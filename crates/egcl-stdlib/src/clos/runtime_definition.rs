// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Runtime definition data belongs to the class identity, not its mutable name.
use super::*;
use std::sync::{Arc, Mutex};

pub type RuntimeClassSlotCells = Arc<Mutex<HashMap<String, Option<EgclVal>>>>;

#[derive(Clone)]
pub struct RuntimeClassDefinition {
    pub owner: EgclVal,
    /// Original source spelling; runtime lookup must use `owner`, not this name.
    pub name: String,
    /// Source/compilation view only; runtime inheritance follows the actual CPL.
    pub supers: Vec<String>,
    pub slots: Vec<RuntimeSlotDefinition>,
    pub class_slot_values: RuntimeClassSlotCells,
    pub default_initargs: Vec<(String, EgclVal)>,
    pub direct_default_metadata: EgclVal,
}

#[derive(Clone)]
pub struct RuntimeSlotDefinition {
    pub name: String,
    pub initargs: Vec<String>,
    pub accessor: Option<String>,
    pub readers: Vec<String>,
    pub writers: Vec<String>,
    pub initform: Option<EgclVal>,
    pub source_initform: Option<EgclVal>,
    pub initfunction: Option<EgclVal>,
    pub mop_options: Vec<EgclVal>,
    pub allocation: SlotAllocation,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SlotAllocation {
    Instance,
    Class,
}

impl egcl_rt::gc::TraceHostRoots for RuntimeSlotDefinition {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Some(value) = &mut self.initform {
            visit(value);
        }
        if let Some(value) = &mut self.source_initform {
            visit(value);
        }
        if let Some(value) = &mut self.initfunction {
            visit(value);
        }
        for value in &mut self.mop_options {
            visit(value);
        }
    }
}

impl egcl_rt::gc::TraceHostRoots for RuntimeClassDefinition {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.owner);
        for slot in &mut self.slots {
            egcl_rt::gc::TraceHostRoots::trace_host_roots(slot, visit);
        }
        for (_, value) in &mut self.default_initargs {
            visit(value);
        }
        visit(&mut self.direct_default_metadata);
        let mut values = self
            .class_slot_values
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for value in values.values_mut().flatten() {
            visit(value);
        }
    }
}

pub fn class_runtime_definition(class: EgclVal) -> Option<RuntimeClassDefinition> {
    with_state(|st| {
        st.class_meta
            .get(&class)
            .and_then(|meta| meta.runtime_definition.clone())
    })
}

/// Clone only the mutable cell owner, avoiding a copy of slot/initform data
/// on the shared-slot read/write path.
pub fn class_runtime_shared_cells(class: EgclVal) -> Option<RuntimeClassSlotCells> {
    with_state(|st| {
        st.class_meta.get(&class).and_then(|meta| {
            meta.runtime_definition
                .as_ref()
                .map(|definition| Arc::clone(&definition.class_slot_values))
        })
    })
}

/// Pure host-data adoption; no Lisp allocation or user callback under the lock.
pub fn set_class_runtime_definition(
    class: EgclVal,
    mut definition: RuntimeClassDefinition,
) -> Result<(), EgclError> {
    definition.owner = class;
    with_state_mut(|st| {
        let meta = st
            .class_meta
            .get_mut(&class)
            .ok_or_else(|| EgclError::TypeError {
                datum: class,
                expected: "CLASS".into(),
            })?;
        meta.runtime_definition = Some(definition);
        Ok(())
    })
}

pub(super) fn snapshot_definition(out: &mut Vec<u8>, definition: &RuntimeClassDefinition) {
    cs_put_val(out, definition.owner);
    cs_put_str(out, &definition.name);
    put_strings(out, &definition.supers);
    cs_put_u32(out, definition.slots.len() as u32);
    for slot in &definition.slots {
        cs_put_str(out, &slot.name);
        put_strings(out, &slot.initargs);
        put_optional_string(out, slot.accessor.as_deref());
        put_strings(out, &slot.readers);
        put_strings(out, &slot.writers);
        put_optional_value(out, slot.initform);
        put_optional_value(out, slot.source_initform);
        put_optional_value(out, slot.initfunction);
        cs_put_vals(out, &slot.mop_options);
        out.push(u8::from(slot.allocation == SlotAllocation::Class));
    }
    cs_put_u32(out, definition.default_initargs.len() as u32);
    for (name, form) in &definition.default_initargs {
        cs_put_str(out, name);
        cs_put_val(out, *form);
    }
    cs_put_val(out, definition.direct_default_metadata);
    let cells = definition
        .class_slot_values
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cs_put_u32(out, cells.len() as u32);
    for (name, value) in cells.iter() {
        cs_put_str(out, name);
        put_optional_value(out, *value);
    }
}

fn put_strings(out: &mut Vec<u8>, values: &[String]) {
    cs_put_u32(out, values.len() as u32);
    for value in values {
        cs_put_str(out, value);
    }
}
fn put_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        cs_put_str(out, value);
    }
}
fn put_optional_value(out: &mut Vec<u8>, value: Option<EgclVal>) {
    out.push(u8::from(value.is_some()));
    if let Some(value) = value {
        cs_put_val(out, value);
    }
}

fn count(data: &[u8], offset: &mut usize, minimum_bytes: usize) -> Option<usize> {
    let count = cs_get_u32(data, offset)? as usize;
    (count.checked_mul(minimum_bytes)? <= data.len().checked_sub(*offset)?).then_some(count)
}
fn strings(data: &[u8], offset: &mut usize) -> Option<Vec<String>> {
    let count = count(data, offset, 4)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(cs_get_str(data, offset)?);
    }
    Some(values)
}
fn optional_string(data: &[u8], offset: &mut usize) -> Option<Option<String>> {
    match cs_get_u8(data, offset)? {
        0 => Some(None),
        1 => Some(Some(cs_get_str(data, offset)?)),
        _ => None,
    }
}
fn optional_value(
    data: &[u8],
    offset: &mut usize,
    remap: &dyn Fn(u64) -> u64,
) -> Option<Option<EgclVal>> {
    match cs_get_u8(data, offset)? {
        0 => Some(None),
        1 => Some(Some(cs_get_val(data, offset, remap)?)),
        _ => None,
    }
}

pub(super) fn restore_definition(
    data: &[u8],
    offset: &mut usize,
    remap: &dyn Fn(u64) -> u64,
) -> Option<RuntimeClassDefinition> {
    let owner = cs_get_val(data, offset, remap)?;
    let name = cs_get_str(data, offset)?;
    let supers = strings(data, offset)?;
    let length = count(data, offset, 25)?;
    let mut slots = Vec::with_capacity(length);
    for _ in 0..length {
        let name = cs_get_str(data, offset)?;
        let initargs = strings(data, offset)?;
        let accessor = optional_string(data, offset)?;
        let readers = strings(data, offset)?;
        let writers = strings(data, offset)?;
        let initform = optional_value(data, offset, remap)?;
        let source_initform = optional_value(data, offset, remap)?;
        let initfunction = optional_value(data, offset, remap)?;
        let count = count(data, offset, 8)?;
        if count % 2 != 0 {
            return None;
        }
        let mut mop_options = Vec::with_capacity(count);
        for _ in 0..count {
            mop_options.push(cs_get_val(data, offset, remap)?);
        }
        let allocation = match cs_get_u8(data, offset)? {
            0 => SlotAllocation::Instance,
            1 => SlotAllocation::Class,
            _ => return None,
        };
        slots.push(RuntimeSlotDefinition {
            name,
            initargs,
            accessor,
            readers,
            writers,
            initform,
            source_initform,
            initfunction,
            mop_options,
            allocation,
        });
    }
    let length = count(data, offset, 12)?;
    let mut default_initargs = Vec::with_capacity(length);
    for _ in 0..length {
        default_initargs.push((cs_get_str(data, offset)?, cs_get_val(data, offset, remap)?));
    }
    let direct_default_metadata = cs_get_val(data, offset, remap)?;
    let length = count(data, offset, 5)?;
    let mut cells = HashMap::with_capacity(length);
    for _ in 0..length {
        let name = cs_get_str(data, offset)?;
        let value = optional_value(data, offset, remap)?;
        if cells.insert(name, value).is_some() {
            return None;
        }
    }
    Some(RuntimeClassDefinition {
        owner,
        name,
        supers,
        slots,
        default_initargs,
        direct_default_metadata,
        class_slot_values: Arc::new(Mutex::new(cells)),
    })
}
