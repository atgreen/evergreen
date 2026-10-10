// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{arena_cons, heap_test_lock, read_eval_all_env, vec_to_list};
use egcl_rt::gc::TraceHostRoots;
use std::cell::RefCell;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Position {
    segment: usize,
    segment_chain: Vec<usize>,
    capture: Option<(usize, bool)>,
    adapter_callers: Option<super::super::super::native_callable::outer::SuspendedCallers>,
    frame: usize,
    stack: usize,
    depth: u32,
}
impl Position {
    fn here() -> Self {
        let mut segment = egcl_rt::native_transfer::current_segment();
        let mut segment_chain = Vec::new();
        while !segment.is_null() {
            segment_chain.push(segment as usize);
            segment = unsafe { (*segment).previous() };
        }
        Self {
            segment: egcl_rt::native_transfer::current_segment() as usize,
            segment_chain,
            capture: super::super::super::native_transfer_entry::child_capture_for_test(),
            adapter_callers:
                super::super::super::native_callable::outer::suspended_callers_for_test(),
            frame: egcl_rt::current_stack().fp() as usize,
            stack: egcl_rt::current_stack().sp() as usize,
            depth: NATIVE_DEPTH.with(|depth| depth.get()),
        }
    }

    // Marker and EVAL callbacks can cross several published Rust adapters.
    // Require every intermediate physical segment link and the nearest real
    // mapped capture, recorded before each adapter hid its caller's CAPTURE.
    fn mapped_caller(&self) -> (usize, Option<(usize, bool)>) {
        if let Some(callers) = &self.adapter_callers {
            assert!(
                self.capture.is_none(),
                "adapter must hide its caller capture: {self:?}"
            );
            assert_eq!(self.segment_chain.first(), Some(&self.segment));
            assert!(
                !callers.is_empty(),
                "adapter must retain its caller: {self:?}"
            );
            for (index, &(segment, capture)) in callers.iter().enumerate() {
                assert_eq!(self.segment_chain.get(index + 1), Some(&segment));
                assert_ne!(self.segment_chain[index], segment);
                assert_ne!(segment, 0);
                if index + 1 < callers.len() {
                    assert!(
                        capture.is_none(),
                        "intermediate adapter must hide CAPTURE: {self:?}"
                    );
                } else {
                    assert!(
                        capture.is_some(),
                        "this fixture needs a real mapped caller: {self:?}"
                    );
                    return (segment, capture);
                }
            }
            unreachable!("nonempty caller chain returns its final mapped capture")
        } else {
            assert!(
                self.capture.is_some(),
                "marker must execute above mapped code: {self:?}"
            );
            (self.segment, self.capture)
        }
    }
}

#[derive(Default)]
struct Observations {
    armed: bool,
    events: Vec<(i64, Position)>,
    moved: bool,
    restored: bool,
}
static OBSERVATIONS: egcl_rt::execution_local::ExecutionLocal<RefCell<Observations>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Observations::default()))
};

// The guard lives in the real EVAL Rust frame. It contains a fresh movable
// root, so observed relocation and destructor execution are both required.
// The enclosing EVAL scope roots the guard before it evaluates the inner form.
pub(in crate::cli) struct NativeEvalProbe {
    active: bool,
    held: EgclVal,
    address: u64,
    position: Position,
}
impl NativeEvalProbe {
    pub(in crate::cli) fn enter() -> Self {
        let active = OBSERVATIONS.with(|state| state.borrow().armed);
        let position = Position::here();
        egcl_rt::rooted!(held = if active { arena_cons(T, NIL) } else { NIL });
        if active {
            OBSERVATIONS.with(|state| state.borrow_mut().events.push((-1, position.clone())));
        }
        Self {
            active,
            held: *held,
            address: held.to_raw(),
            position,
        }
    }
}
impl TraceHostRoots for NativeEvalProbe {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.held);
    }
}
impl Drop for NativeEvalProbe {
    fn drop(&mut self) {
        if self.active {
            let position = Position::here();
            OBSERVATIONS.with(|state| {
                let mut state = state.borrow_mut();
                state.moved = self.held.to_raw() != self.address && cp(self.held) == (T, NIL);
                state.restored = position == self.position;
                state.events.push((-2, position));
            });
        }
    }
}

pub(in crate::cli) fn native_reentry_event_for_test(phase: EgclVal) -> Result<EgclVal, EgclError> {
    let position = Position::here();
    assert_eq!(
        super::super::super::native_transfer_entry::pending_argument_count(),
        0,
        "expanded argument owner must retire while its parent segment is still live"
    );
    OBSERVATIONS.with(|state| {
        let mut state = state.borrow_mut();
        if state.armed {
            state.events.push((phase.as_fixnum(), position));
        }
    });
    Ok(NIL)
}

struct ProbeScope;
impl ProbeScope {
    fn arm() -> Self {
        OBSERVATIONS.with(|state| {
            *state.borrow_mut() = Observations {
                armed: true,
                ..Observations::default()
            }
        });
        Self
    }
}
impl Drop for ProbeScope {
    fn drop(&mut self) {
        OBSERVATIONS.with(|state| *state.borrow_mut() = Observations::default());
    }
}

fn prepare_mapped(name: &str, params: &str, forms: &str, env: &Env) -> (u32, TransferCode) {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(
        compile_function(name, *params, *forms, env, false, false)
            .unwrap_or_else(|| panic!("{name} failed bytecode lowering")),
    );
    registry_put(symbol, Arc::clone(&body));
    (
        symbol,
        TransferCode::compile_nested_protected(Arc::clone(&body))
            .unwrap_or_else(|| panic!("{name} not admitted: {:?}", body.code)),
    )
}

fn install(symbol: u32, code: TransferCode) -> Rc<TransferCode> {
    let installed =
        super::super::super::native_transfer_entry::install_baseline_code(symbol, code).unwrap();
    publish_native(
        symbol,
        egcl_rt::symbols::symbol_function(symbol),
        &installed,
    );
    let NativeCodeStorage::Mapped(code) = &installed._storage else {
        unreachable!()
    };
    Rc::clone(code)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_mapped_child_reenters_through_rust_and_retires_in_order() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let inner_forms = "((unwind-protect
        (progn (reentry-mark 0) (reentry-collect)
          (if fail (throw :reentry (values x (list x))) (values x (list x))))
        (reentry-mark 1)))";
    let child_forms = "((unwind-protect (eval form) (reentry-mark 2)))";
    read_eval_all_env(
        &format!(
            "(defun reentry-mark (phase) (%native-reentry-event-for-test phase))
         (defun reentry-collect () (%force-minor-gc-for-test))
         (defun reentry-inner (x fail) {})
         (defun reentry-child (form) {})",
            &inner_forms[1..inner_forms.len() - 1],
            &child_forms[1..child_forms.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    let (inner_symbol, inner) = prepare_mapped("REENTRY-INNER", "(x fail)", inner_forms, &env);
    let inner = install(inner_symbol, inner);
    let (child_symbol, child) = prepare_mapped("REENTRY-CHILD", "(form)", child_forms, &env);
    let child = install(child_symbol, child);
    for invocation in [
        "(reentry-child form)",
        "(funcall #'reentry-child form)",
        "(apply #'reentry-child (list form))",
    ] {
        let caller = compile_caller(
            "(form)",
            &format!("((catch :reentry (unwind-protect {invocation} (reentry-mark 3))))"),
            &env,
        );
        let quote = EgclVal::from_symbol_index(egcl_rt::symbols::intern("QUOTE"));
        for fail in [NIL, T] {
            egcl_rt::rooted!(value = arena_cons(T, NIL));
            egcl_rt::rooted!(quoted = vec_to_list(&[quote, *value]));
            egcl_rt::rooted!(
                form = vec_to_list(&[EgclVal::from_symbol_index(inner_symbol), *quoted, fail])
            );
            // Warm the caller's cell without arming observation. The measured
            // invocation must then own a mapped child rather than a cold fallback.
            egcl_rt::rooted!(
                warm_form = vec_to_list(&[EgclVal::from_symbol_index(inner_symbol), *quoted, NIL])
            );
            caller.run(&[*warm_form], &mut env).unwrap();
            assert!(
                !call_table::resolve(child_symbol).unwrap().is_cold(),
                "warm child fail={fail:?}"
            );
            let _probe = ProbeScope::arm();
            take_nested_entries();
            let before = Position::here();
            let result = caller.run(&[*form], &mut env).unwrap();
            assert_eq!(result, *value);
            assert!(env.mv_active);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(cp(env.mv[1]), (*value, NIL));
            assert_eq!(Position::here(), before);
            assert!(
                env.handlers.is_empty() && env.restarts.is_empty() && env.catch_stack.is_empty()
            );
            assert_eq!(
                take_nested_entries(),
                1,
                "fail={fail:?}, events={:?}",
                OBSERVATIONS.with(|state| state.borrow().events.clone())
            );
            OBSERVATIONS.with(|state| {
                let state = state.borrow();
                assert_eq!(
                    state.events.iter().map(|e| e.0).collect::<Vec<_>>(),
                    [-1, 0, 1, -2, 2, 3]
                );
                assert!(state.moved, "live EVAL guard root must relocate");
                assert!(
                    state.restored,
                    "inner entry must return normally to its Rust owner"
                );
                let outer = &state.events[0].1;
                let inner_position = &state.events[1].1;
                assert_ne!(outer.segment, 0);
                let (outer_mapped_segment, outer_capture) = outer.mapped_caller();
                let (inner_mapped_segment, inner_capture) = inner_position.mapped_caller();
                assert_eq!(outer_capture, Some((Rc::as_ptr(&child) as usize, true)));
                assert_ne!(
                    inner_mapped_segment, outer_mapped_segment,
                    "EVAL must create a separate native segment"
                );
                assert!(
                    inner_position.segment_chain.contains(&outer.segment),
                    "inner execution must retain the real EVAL segment: {inner_position:?}"
                );
                assert_eq!(inner_capture, Some((Rc::as_ptr(&inner) as usize, false)));
                assert_eq!(
                    state.events[2].1.mapped_caller(),
                    (inner_mapped_segment, inner_capture)
                );
                assert_eq!(
                    state.events[4].1.mapped_caller(),
                    (outer_mapped_segment, outer_capture)
                );
                assert_eq!(
                    state.events[5].1.mapped_caller(),
                    (
                        outer_mapped_segment,
                        Some((std::ptr::from_ref(&caller) as usize, false))
                    )
                );
            });
        }
    }
}
