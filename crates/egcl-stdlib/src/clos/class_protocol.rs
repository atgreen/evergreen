//! Storage and atomic layout publication for the Lisp class protocol.
//! User methods run before these nonallocating metadata updates.
use super::*;

pub fn class_slot_metaobjects(class: EgclVal, direct: bool) -> (EgclVal, bool) {
    with_state(|st| {
        st.class_meta.get(&class).map_or((NIL, false), |meta| {
            if direct {
                (meta.protocol.direct_slots, meta.protocol.direct_ready)
            } else {
                (meta.protocol.effective_slots, meta.protocol.computed)
            }
        })
    })
}

pub fn class_protocol_default_initargs(class: EgclVal) -> Option<EgclVal> {
    with_state(|st| {
        st.class_meta.get(&class).and_then(|meta| {
            meta.protocol
                .computed
                .then_some(meta.protocol.default_initargs)
        })
    })
}

pub fn class_protocol_runtime_defaults(class: EgclVal) -> Option<EgclVal> {
    with_state(|st| {
        st.class_meta.get(&class).and_then(|m| {
            m.protocol
                .computed
                .then_some(m.protocol.runtime_default_initargs)
        })
    })
}

pub fn class_protocol_runtime_slots(class: EgclVal) -> Option<EgclVal> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .and_then(|m| m.protocol.computed.then_some(m.protocol.runtime_slots))
    })
}

pub fn set_class_direct_slot_metaobjects(class: EgclVal, slots: EgclVal) -> Result<(), EgclError> {
    with_state_mut(|st| {
        let meta = st
            .class_meta
            .get_mut(&class)
            .ok_or_else(|| EgclError::TypeError {
                datum: class,
                expected: "CLASS".into(),
            })?;
        meta.protocol.direct_slots = slots;
        meta.protocol.direct_ready = true;
        Ok(())
    })
}

pub fn begin_class_finalization(class: EgclVal) -> Result<(), EgclError> {
    with_state_mut(|st| {
        let meta = st
            .class_meta
            .get_mut(&class)
            .ok_or_else(|| EgclError::TypeError {
                datum: class,
                expected: "CLASS".into(),
            })?;
        meta.protocol.finalized = false;
        Ok(())
    })
}

pub fn class_protocol_finalized(class: EgclVal) -> bool {
    forward_referenced_superclass(class).is_nil()
        && with_state(|st| {
            st.class_meta
                .get(&class)
                .is_some_and(|m| m.protocol.finalized)
        })
}

pub fn compute_class_precedence_list_default(class: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    with_state(|st| compute_class_precedence_default(st, class))
}

/// Publish a complete, checked CPL and instance-slot order together. The
/// supplied lists are precise roots in ClassMeta; no visitor/user code or
/// Lisp allocation runs with the definitions lock held.
pub fn publish_class_protocol(
    class: EgclVal,
    cpl: &[EgclVal],
    instance_slots: &[EgclVal],
    effective_slots: EgclVal,
    default_initargs: EgclVal,
    runtime_default_initargs: EgclVal,
    runtime_slots: EgclVal,
) -> Result<(), EgclError> {
    with_state_mut(|st| {
        let expected = compute_class_precedence_default(st, class)?;
        let actual: HashSet<_> = cpl.iter().copied().collect();
        let expected_set: HashSet<_> = expected.iter().copied().collect();
        if cpl.first() != Some(&class) || actual.len() != cpl.len() || actual != expected_set {
            return Err(EgclError::ProgramError(
                "invalid computed class precedence list".into(),
            ));
        }
        let names: HashSet<_> = instance_slots.iter().copied().collect();
        if names.len() != instance_slots.len() || instance_slots.iter().any(|v| !v.is_symbol()) {
            return Err(EgclError::ProgramError(
                "invalid computed instance slot layout".into(),
            ));
        }
        let meta = st
            .class_meta
            .get_mut(&class)
            .ok_or_else(|| EgclError::TypeError {
                datum: class,
                expected: "CLASS".into(),
            })?;
        let previous = meta.wrapper;
        meta.protocol.cpl = Some(cpl.to_vec());
        meta.protocol.instance_slots = Some(instance_slots.to_vec());
        meta.protocol.effective_slots = effective_slots;
        meta.protocol.default_initargs = default_initargs;
        meta.protocol.runtime_default_initargs = runtime_default_initargs;
        meta.protocol.runtime_slots = runtime_slots;
        meta.protocol.computed = true;
        meta.protocol.finalized = true;
        st.cpl_cache.get_mut().clear();
        finalize_class_layout(st, class);
        // A parent's computed layout/CPL may change even without a new direct
        // definition. Keep descendant wrappers current until their own checked
        // finalization, but retire their semantic caches and fast eligibility.
        let mut pending = st
            .class_meta
            .get(&class)
            .map_or_else(Vec::new, |m| m.direct_subs.clone());
        let mut visited = HashSet::new();
        visited.insert(class);
        while let Some(descendant) = pending.pop() {
            if !visited.insert(descendant) {
                continue;
            }
            if let Some(meta) = st.class_meta.get_mut(&descendant) {
                pending.extend(meta.direct_subs.iter().copied());
                meta.protocol.invalidate_computed();
            }
        }
        if let Some(wrapper) = previous {
            wrapper.state.store(WRAPPER_OBSOLETE, Ordering::Release);
        }
        Ok(())
    })
}
