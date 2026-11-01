// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Function-object entry words and lean named calls (bliss-gg3i1.7.15.10.1).
//!
//! A lean caller loads its target's function cell, checks for an interpreted
//! function whose entry word this execution owns, and calls that entry; the
//! callee checks its own stack. Everything else takes the descriptor path.

use super::ordinary_t2::install as install_ordinary;
use super::*;
use std::sync::atomic::Ordering;

fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        install_ordinary(name, params, forms, env)
    })
}

fn mapped(code: &NativeCode) -> &native_transfer_entry::TransferCode {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped ordinary owner")
    };
    mapped
}

fn function(symbol: u32) -> EgclVal {
    egcl_rt::symbols::symbol_function(symbol).expect("defined function")
}

fn owner() -> u64 {
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    unsafe { &*execution }.code_owner()
}

fn entry_word(symbol: u32) -> (usize, u64) {
    egcl_rt::function::entry_word(function(symbol)).expect("interpreted function")
}

fn cold_calls() -> usize {
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test()
}

struct Fixture {
    leaf: u32,
    leaf_code: Rc<NativeCode>,
    caller: u32,
    caller_code: Rc<NativeCode>,
}

/// `leaf` is installed T2 ordinary code; `caller` calls it by name, non-tail.
fn fixture(prefix: &str, leaf_body: &str, env: &mut Env) -> Fixture {
    let leaf_name = format!("{prefix}-LEAF");
    let caller_name = format!("{prefix}-CALLER");
    crate::cli::read_eval_all_env(
        &format!(
            "(defun {leaf_name} (a b c) {leaf_body})
             (defun {caller_name} (a b c) (cons ({leaf_name} a b c) a))"
        ),
        env,
    )
    .unwrap();
    let (leaf, leaf_code) = install(&leaf_name, "(a b c)", &format!("({leaf_body})"), env);
    let (caller, caller_code) = install(
        &caller_name,
        "(a b c)",
        &format!("((cons ({leaf_name} a b c) a))"),
        env,
    );
    for code in [&leaf_code, &caller_code] {
        assert!(
            native_transfer_entry::installed_listing(mapped(code))
                .contains("Ordinary x86-64 Lisp ABI")
        );
    }
    assert!(
        mapped(&leaf_code).ordinary_lean_entry_address().is_some(),
        "ordinary code has a lean entry"
    );
    Fixture {
        leaf,
        leaf_code,
        caller,
        caller_code,
    }
}

fn call(fixture: &Fixture, args: &[EgclVal], env: &mut Env) -> Result<EgclVal, EgclError> {
    run_native(&fixture.caller_code, fixture.caller, args, env)
}

fn warm(fixture: &Fixture, env: &mut Env) {
    let args = [1, 2, 3].map(EgclVal::from_fixnum);
    for _ in 0..3 {
        call(fixture, &args, env).unwrap();
    }
}

/// Desynchronize a cell's revision word from its published descriptor, so
/// descriptor admission rejects it, while the descriptor stays published and
/// its code retained. Restored on drop.
struct StaleRevision {
    cell: Arc<egcl_rt::call_table::CallCell>,
    revision: u64,
}

impl StaleRevision {
    fn new(symbol: u32) -> Self {
        let cell = call_table::resolve(symbol).unwrap();
        assert_ne!(
            unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
            0,
            "a descriptor is published"
        );
        let revision = unsafe { &*cell.revision_address() }.fetch_add(1 << 40, Ordering::AcqRel);
        Self { cell, revision }
    }
}

impl Drop for StaleRevision {
    fn drop(&mut self) {
        unsafe { &*self.cell.revision_address() }.store(self.revision, Ordering::Release);
    }
}

fn leaf_lean_entry(fixture: &Fixture) -> usize {
    mapped(&fixture.leaf_code)
        .ordinary_lean_entry_address()
        .unwrap()
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn lean_named_call_is_emitted_and_enters_the_owned_entry_word() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture("BACKPACK-LEAN", "c", &mut env);
    // The caller's code embeds the lean sequence: `movabs rax, [cell]` with the
    // leaf's function cell address.
    let cell = egcl_rt::symbols::symbol_object_ptr(fixture.leaf).unwrap()
        + std::mem::offset_of!(egcl_rt::object::SymbolData, function);
    let mut needle = vec![0x48, 0xa1];
    needle.extend_from_slice(&(cell as u64).to_le_bytes());
    let code = mapped(&fixture.caller_code).code_bytes_for_test();
    assert!(
        code.windows(needle.len()).any(|window| window == needle),
        "the caller loads the leaf's function cell"
    );
    assert_eq!(entry_word(fixture.leaf), (0, 0), "T0 publication owns no entry");
    warm(&fixture, &mut env);
    assert_eq!(
        entry_word(fixture.leaf),
        (leaf_lean_entry(&fixture), owner()),
        "T2 publication claims the leaf's entry word for this execution"
    );
    // Make the descriptor admission fail its revision check, keeping the
    // descriptor (and so its retained code) published. A call that took that
    // path now reaches the cold helper; the lean call never does.
    let stale = StaleRevision::new(fixture.leaf);
    cold_calls();
    for i in 0..5 {
        let args = [7, 8, i].map(EgclVal::from_fixnum);
        let result = call(&fixture, &args, &mut env).unwrap();
        assert_eq!(crate::cli::cp(result), (args[2], args[0]));
    }
    assert_eq!(cold_calls(), 0, "every call entered the leaf's entry word");
    drop(stale);
    egcl_rt::call_table::invalidate(fixture.leaf);
    assert_eq!(entry_word(fixture.leaf), (0, 0), "invalidation releases the word");
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn a_foreign_owner_or_non_function_takes_the_descriptor_path() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture("BACKPACK-FOREIGN", "c", &mut env);
    warm(&fixture, &mut env);
    assert_eq!(entry_word(fixture.leaf).1, owner());
    // Another execution owns the word, with an entry this one must never
    // enter: a lean call that did would fault.
    let leaf = function(fixture.leaf);
    unsafe {
        egcl_rt::function::release_entry(leaf, owner());
        assert!(egcl_rt::function::claim_entry(leaf, u64::MAX, 0x10));
    }
    cold_calls();
    for i in 0..3 {
        let args = [5, 6, i].map(EgclVal::from_fixnum);
        let result = call(&fixture, &args, &mut env).unwrap();
        assert_eq!(crate::cli::cp(result), (args[2], args[0]));
    }
    assert_eq!(
        cold_calls(),
        0,
        "the owner mismatch takes the published descriptor, not the cold path"
    );
    unsafe { egcl_rt::function::release_entry(leaf, u64::MAX) };
    assert_eq!(entry_word(fixture.leaf), (0, 0));

    // A function cell that holds no function object signals as before.
    crate::cli::read_eval_all_env("(fmakunbound 'backpack-foreign-leaf)", &mut env).unwrap();
    let args = [5, 6, 7].map(EgclVal::from_fixnum);
    let error = call(&fixture, &args, &mut env).unwrap_err();
    assert!(
        format!("{error:?}").contains("BACKPACK-FOREIGN-LEAF"),
        "undefined function error names the callee: {error:?}"
    );
    // A generic function in the cell is called through the descriptor path.
    crate::cli::read_eval_all_env(
        "(defgeneric backpack-foreign-leaf (a b c))
         (defmethod backpack-foreign-leaf (a b c) (list a b c))",
        &mut env,
    )
    .unwrap();
    for _ in 0..3 {
        let result = call(&fixture, &args, &mut env).unwrap();
        let (list, first) = crate::cli::cp(result);
        assert_eq!(first, args[0]);
        assert_eq!(crate::cli::cp(list).0, args[0]);
    }
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn tier_transitions_and_redefinition_update_the_entry_word() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixture = fixture("BACKPACK-TIER", "(+ c 1)", &mut env);
    warm(&fixture, &mut env);
    let claimed = (leaf_lean_entry(&fixture), owner());
    assert_eq!(entry_word(fixture.leaf), claimed);

    // Profiling pins the function to T0 the way the deopt blacklist does.
    profile_pin(fixture.leaf);
    assert_eq!(entry_word(fixture.leaf), (0, 0), "a T0 pin releases the word");
    let args = [1, 2, 40].map(EgclVal::from_fixnum);
    for _ in 0..3 {
        let result = call(&fixture, &args, &mut env).unwrap();
        assert_eq!(crate::cli::cp(result).0, EgclVal::from_fixnum(41));
        assert_eq!(entry_word(fixture.leaf), (0, 0), "a pinned function stays T0");
    }
    profile_unpin(fixture.leaf);

    // Install T2 again. The fixture's freshly compiled body is not the
    // definition's own, so publication leaves the tier the pin reset; record
    // the final tier as a normal promotion does, or the code keeps its
    // caller-side heat accounting and no claim.
    let (_, leaf_code) = install(
        "BACKPACK-TIER-LEAF",
        "(a b c)",
        "((+ c 1))",
        &mut env,
    );
    egcl_rt::function::set_tier(function(fixture.leaf), 2);
    egcl_rt::call_table::invalidate(fixture.leaf);
    let refreshed = Fixture {
        leaf: fixture.leaf,
        leaf_code,
        caller: fixture.caller,
        caller_code: Rc::clone(&fixture.caller_code),
    };
    warm(&refreshed, &mut env);
    assert_eq!(
        entry_word(fixture.leaf),
        (leaf_lean_entry(&refreshed), owner()),
        "republished T2 code claims the word again"
    );

    // Redefinition mid-run: the next call sees the new object, whose word is
    // unowned, and the old object's word is released at once.
    let old = function(fixture.leaf);
    egcl_rt::rooted!(old = old);
    for i in 0..10 {
        if i == 5 {
            crate::cli::read_eval_all_env(
                "(defun backpack-tier-leaf (a b c) (- c 1))",
                &mut env,
            )
            .unwrap();
            assert_eq!(
                egcl_rt::function::entry_word(*old),
                Some((0, 0)),
                "redefinition releases the old object's word"
            );
            assert_eq!(entry_word(fixture.leaf), (0, 0));
        }
        let result = call(&refreshed, &args, &mut env).unwrap();
        let expected = if i < 5 { 41 } else { 39 };
        assert_eq!(crate::cli::cp(result).0, EgclVal::from_fixnum(expected));
    }
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn lean_return_maps_relocate_caller_values_across_a_collecting_callee() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun backpack-gc-force () (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    let fixture = fixture("BACKPACK-GC", "(progn (backpack-gc-force) c)", &mut env);
    warm(&fixture, &mut env);
    assert_eq!(entry_word(fixture.leaf).1, owner());
    // Only the lean path can reach the leaf without the cold helper.
    let stale = StaleRevision::new(fixture.leaf);
    cold_calls();
    let mut moved = 0;
    for i in 0..4 {
        // A fresh nursery cons is live in the caller across the call.
        HeapCollector::new().minor_gc().unwrap();
        egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(i), NIL));
        let before = argument.to_raw();
        let args = [*argument, NIL, EgclVal::from_fixnum(i)];
        egcl_rt::rooted!(result = call(&fixture, &args, &mut env).unwrap());
        let (value, held) = crate::cli::cp(*result);
        assert_eq!(value, EgclVal::from_fixnum(i));
        assert_eq!(held, *argument, "the caller's value was updated in place");
        assert_eq!(crate::cli::cp(held), (EgclVal::from_fixnum(i), NIL));
        if held.to_raw() != before {
            moved += 1;
        }
    }
    // The leaf reaches the interpreted collector helper through its named
    // adapter; a descriptor call from the caller to the leaf would be cold.
    assert_eq!(cold_calls(), 0, "every caller-to-leaf call was lean");
    assert!(moved > 0, "the probe must observe a relocation");
    drop(stale);
    egcl_rt::call_table::invalidate(fixture.leaf);
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn lean_mutual_recursion_overflows_through_the_callee_stack_check() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun backpack-ping (n a b) (if (= n 0) 0 (+ 1 (backpack-pong (- n 1) a b))))
         (defun backpack-pong (n a b) (if (= n 0) 0 (+ 1 (backpack-ping (- n 1) a b))))",
        &mut env,
    )
    .unwrap();
    let (ping, ping_code) = install(
        "BACKPACK-PING",
        "(n a b)",
        "((if (= n 0) 0 (+ 1 (backpack-pong (- n 1) a b))))",
        &mut env,
    );
    let (pong, _pong_code) = install(
        "BACKPACK-PONG",
        "(n a b)",
        "((if (= n 0) 0 (+ 1 (backpack-ping (- n 1) a b))))",
        &mut env,
    );
    let shallow = [EgclVal::from_fixnum(6), NIL, NIL];
    for _ in 0..3 {
        assert_eq!(
            run_native(&ping_code, ping, &shallow, &mut env).unwrap(),
            EgclVal::from_fixnum(6)
        );
    }
    assert_eq!(entry_word(ping).1, owner());
    assert_eq!(entry_word(pong).1, owner());
    cold_calls();
    let deep = [EgclVal::from_fixnum(1 << 26), NIL, NIL];
    native_transfer_entry::ordinary_calls::take_lean_overflows_for_test();
    let error = run_native(&ping_code, ping, &deep, &mut env).unwrap_err();
    assert!(
        native_transfer_entry::ordinary_calls::take_lean_overflows_for_test() > 0,
        "a lean entry's own stack check found the limit: {error:?}"
    );
    // The machine stack is intact afterwards.
    for _ in 0..3 {
        assert_eq!(
            run_native(&ping_code, ping, &shallow, &mut env).unwrap(),
            EgclVal::from_fixnum(6)
        );
    }
}
