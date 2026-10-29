// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;

thread_local! {
    static COLD_SELF_CALLS: Cell<usize> = const { Cell::new(0) };
}

fn request_cold_self() {
    // Arrive after the helper's return poll so the generated self edge
    // must still publish its caller and take cold admission.
    ordinary_t2::request_yield_after_return();
}

fn replace_at_cold_self() {
    COLD_SELF_CALLS.with(|calls| calls.set(calls.get() + 1));
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!env.is_null());
    unsafe {
        super::super::super::read_eval_all_env("(defun ordinary-guardless-self (n) 99)", &mut *env)
            .unwrap();
    }
}

struct RestoreHooks {
    observer: Option<fn()>,
    cold_self: Option<fn()>,
}

impl Drop for RestoreHooks {
    fn drop(&mut self) {
        native_transfer_entry::ordinary_calls::YIELD_AFTER_RETURN_TO_OWNER
            .with(|target| target.set(0));
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.observer));
        native_transfer_entry::ordinary_calls::SELF_COLD_OBSERVER
            .with(|hook| hook.set(self.cold_self));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_guardless_self_cold_keeps_retained_callable_after_redefinition() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun ordinary-guardless-observer () (%ordinary-t2-entry-for-test))
         (defun ordinary-guardless-self (n)
           (if n
               (progn (ordinary-guardless-observer)
                      (if (ordinary-guardless-self nil) 7 11))
               nil))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = ordinary_t2::install(
        "ORDINARY-GUARDLESS-SELF",
        "(n)",
        "((if n (progn (ordinary-guardless-observer)
                      (if (ordinary-guardless-self nil) 7 11)) nil))",
        &mut env,
    );
    assert!(code.is_t2);
    assert!(
        !code.has_deopt,
        "fixture must not rely on a deopt identity reservation"
    );
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("ordinary native code owner")
    };
    assert!(mapped.ordinary_entry.is_some());
    let body = code.body.as_ref().expect("installed source body");
    let self_sites: Vec<_> = body
        .code
        .iter()
        .enumerate()
        .filter_map(|(pc, instruction)| {
            matches!(instruction, Instr::CallNamed { sym, .. } if *sym == symbol)
                .then_some(pc as u32)
        })
        .collect();
    assert_eq!(
        self_sites.len(),
        1,
        "fixture must retain a non-tail self call"
    );
    assert_eq!(
        body.self_call_sites, self_sites,
        "source must prove direct self binding"
    );

    // Verify the installed bytes contain a relative call to the ordinary
    // in-body prologue, rather than merely trusting the source whitelist.
    let base = code.entry as u64;
    let bytes = unsafe { std::slice::from_raw_parts(code.entry, code.code_len) };
    let instructions: Vec<_> =
        iced_x86::Decoder::with_ip(64, bytes, base, iced_x86::DecoderOptions::NONE)
            .into_iter()
            .collect();
    let self_calls: Vec<_> = instructions
        .iter()
        .filter(|instruction| {
            instruction.code() == iced_x86::Code::Call_rel32_64
                && instructions.iter().any(|target| {
                    target.ip() == instruction.near_branch_target()
                        && target.mnemonic() == iced_x86::Mnemonic::Push
                        && target.op0_register() == iced_x86::Register::RBP
                })
        })
        .collect();
    assert_eq!(
        self_calls.len(),
        1,
        "installed code must contain the direct self edge"
    );

    ordinary_t2::expect_entry(&code);
    COLD_SELF_CALLS.with(|calls| calls.set(0));
    let _restore = RestoreHooks {
        observer: ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|hook| hook.replace(Some(request_cold_self))),
        cold_self: native_transfer_entry::ordinary_calls::SELF_COLD_OBSERVER
            .with(|hook| hook.replace(Some(replace_at_cold_self))),
    };
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    let result = run_native(&code, symbol, &[T], &mut env).unwrap();
    assert!(
        ordinary_t2::observed_entry(),
        "observer must see the exact installed owner"
    );
    assert_eq!(
        COLD_SELF_CALLS.with(Cell::get),
        1,
        "real cold-self hook must execute"
    );
    assert!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test() > 0);
    assert_eq!(
        result,
        EgclVal::from_fixnum(11),
        "retained old self returns NIL; resolving the replacement would return 7"
    );
    egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_eq!(
        super::super::super::apply_function(*replacement, &[NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(99),
        "fresh external calls must see the replacement"
    );
}
