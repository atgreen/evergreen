// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Code-owned idle contexts hold no owning code edge or Lisp value. The pool
//! belongs to exactly one TransferCode: its raw code/body/map pointers cannot
//! be used until a new active Lease has reacquired that exact Rc. Retirement
//! destroys idle homes immediately; an active lease independently retains code.
//! CaptureContext drop is pointer-inert: owning options are empty when parked,
//! and its remaining fields are scalars/raw pointers into code or pinned state.
use super::*;

impl ContextHome {
    pub(super) fn park(&mut self) {
        let context = self.get_mut();
        debug_assert!(context.selected.is_none() && context.failure.is_none());
        debug_assert!(context.osr.is_none() && context.recursive.is_null());
        debug_assert!(!context.recursive_escape && !context.completed_deopt);
        debug_assert!(context.completed_cleanup.is_none());
        // Generated publication belongs to the old parent, never to idle code.
        context.generated = std::ptr::null_mut();
        context.generated_owner = None;
        // Controls contain Env/MV and execution/carrier/stack snapshots. Even
        // the same execution may use a different Env on the next checkout.
        context.native_recursive = None;
        context.native_poll = None;
        context.frame = std::ptr::null_mut();
        context.activation = std::ptr::null_mut();
        context.nested = std::ptr::null_mut();
        context.catch_base = 0;
        context.native_bindings = 0;
        context.recursive_profile = false;
        context.landing = SysvNativeLanding {
            stack_pointer: std::ptr::null_mut(),
            entry: std::ptr::null(),
        };
        context.dispatch = DispatchPacket {
            entry: std::ptr::null(),
            request: std::ptr::null_mut(),
        };
    }

    pub(super) fn attach(&mut self, code: &TransferCode, env: &Env, frame: *mut Frame) {
        let context = self.get_mut();
        debug_assert!(std::ptr::eq(context.owner, code));
        debug_assert!(context.frame.is_null() && context.nested.is_null());
        debug_assert!(context.native_recursive.is_none() && context.native_poll.is_none());
        debug_assert!(context.generated.is_null() && context.generated_owner.is_none());
        context.frame = frame;
        context.catch_base = env.catch_stack.len();
        context.native_bindings = egcl_rt::symbols::native_binding_depth();
        context.recursive_profile = !code.is_t2
            && !profiling_disabled()
            && egcl_rt::function::is_interpreted_function(unsafe { (*frame).function });
        // run() initializes a new native control before entry, with the current
        // Env, execution, MV flag, depth and native stack limit. Its ordinary
        // cold poll remains responsible for consumed signal/carrier epochs.
    }
}
