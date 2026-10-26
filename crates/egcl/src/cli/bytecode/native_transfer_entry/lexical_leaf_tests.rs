// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

static COUNTS: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0, 0))) };
static REQUEST_POLL: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };
pub(super) fn record_hit(store: bool) {
    COUNTS.with(|counts| {
        let (read, write, published) = counts.get();
        counts.set((
            read + usize::from(!store),
            write + usize::from(store),
            published,
        ));
    });
}
static PUBLISHED_ACCESSES: egcl_rt::execution_local::ExecutionLocal<Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new((0, 0))) };
pub(super) fn record_published(request: *mut u8) {
    use egcl_compiler::t2::emit::TransferCallRequest;
    COUNTS.with(|counts| {
        let (read, write, published) = counts.get();
        counts.set((read, write, published + 1));
    });
    // Observe the actual bytecode operation, not the request arity: other
    // lexical requests (such as closure creation) can also take one value.
    let context = unsafe { CAPTURE.with(Cell::get).as_ref() };
    let instruction = context.and_then(|context| unsafe {
        let bcp = (*request.cast::<TransferCallRequest>()).symbol as u32 as usize;
        context.body.as_ref()?.code.get(bcp)
    });
    PUBLISHED_ACCESSES.with(|counts| {
        let (reads, stores) = counts.get();
        match instruction {
            Some(Instr::LoadEnvVar(_)) => counts.set((reads + 1, stores)),
            Some(Instr::StoreEnvVar(_)) => counts.set((reads, stores + 1)),
            _ => {}
        }
    });
}
pub(super) fn before_request(request: *mut u8) {
    use egcl_compiler::t2::emit::{TRANSFER_LEXICAL_ENVIRONMENT_REQUEST, TransferCallRequest};
    let kind = unsafe { (*request.cast::<TransferCallRequest>()).symbol } & !u64::from(u32::MAX);
    if kind == TRANSFER_LEXICAL_ENVIRONMENT_REQUEST
        && REQUEST_POLL.with(|request| request.replace(false))
    {
        egcl_rt::thread::request_current_execution_poll();
    }
}
// Addresses are diagnostic integers only, deliberately not roots. The outgoing
// native slot itself must be updated through the published call's exact map.
static COLLECT_NEXT_STORE: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };
type OutgoingMoveObservation = (u64, u64, bool, bool);
static OUTGOING_MOVE: egcl_rt::execution_local::ExecutionLocal<
    Cell<Option<OutgoingMoveObservation>>,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };
pub(super) fn inside_published(request: *mut u8) {
    use egcl_compiler::t2::emit::{TRANSFER_LEXICAL_ENVIRONMENT_REQUEST, TransferCallRequest};
    use egcl_rt::Collector;
    let (kind, count, args) = unsafe {
        let call = &*request.cast::<TransferCallRequest>();
        (call.symbol & !u64::from(u32::MAX), call.nargs, call.args)
    };
    if kind != TRANSFER_LEXICAL_ENVIRONMENT_REQUEST
        || count != 1
        || args.is_null()
        || !COLLECT_NEXT_STORE.with(|collect| collect.replace(false))
    {
        return;
    }
    let before = unsafe { (*args).to_raw() };
    let published = !root_publication::ACTIVE.with(Cell::get).is_null();
    // Do not panic inside the extern-C helper. Record diagnostics and assert
    // after native execution returns, including collection failure.
    let collected = egcl_rt::HeapCollector::new().minor_gc().is_ok();
    let after = unsafe { (*args).to_raw() };
    OUTGOING_MOVE.with(|moved| moved.set(Some((before, after, published, collected))));
}
fn reset() {
    PUBLISHED_ACCESSES.with(|counts| counts.set((0, 0)));
    COLLECT_NEXT_STORE.with(|collect| collect.set(false));
    OUTGOING_MOVE.with(|moved| moved.set(None));
    COUNTS.with(|counts| counts.set((0, 0, 0)));
    REQUEST_POLL.with(|request| request.set(false));
}

fn code(forms: &str, env: &Env) -> TransferCode {
    egcl_rt::rooted!(params = reader::read_from_string("(next)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let captures = std::collections::HashSet::from(["LEXICAL-LEAF-CAPTURE".to_owned()]);
    let empty = std::collections::HashSet::new();
    let body = Arc::new(
        compile_function_in(
            "LEXICAL-LEAF-TEST",
            *params,
            *forms,
            env,
            &HashMap::new(),
            false,
            false,
            &empty,
            &empty,
            &empty,
            &captures,
            None,
        )
        .expect("compile capturing bytecode"),
    );
    assert!(body.has_env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    TransferCode::compile_mapped_for_test(body).expect("compile actual native capturing body")
}

#[test]
fn lexical_leaf_native_reads_writes_and_shadowing() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = resolve_sym("LEXICAL-LEAF-CAPTURE").unwrap();
    env.define_local_symbol(symbol, EgclVal::from_fixnum(11));
    let parent = Arc::clone(&env.frame);
    env.frame = Arc::new(SharedCell::new(EnvFrame {
        vars: super::super::super::VecMap::default(),
        symbol_vars: super::super::super::VecMap::default(),
        parent: Some(Arc::clone(&parent)),
    }));
    env.define_local_symbol(symbol, EgclVal::from_fixnum(22));
    let native = code(
        "((setq lexical-leaf-capture next) lexical-leaf-capture)",
        &env,
    );
    reset();
    let result = native.run(&[EgclVal::from_fixnum(42)], &mut env).unwrap();
    assert_eq!(result, EgclVal::from_fixnum(42));
    assert_eq!(
        Env::lookup_frame(&parent, "LEXICAL-LEAF-CAPTURE"),
        Some(EgclVal::from_fixnum(11))
    );
    assert_eq!(
        env.lookup_var("LEXICAL-LEAF-CAPTURE"),
        Some(EgclVal::from_fixnum(42))
    );
    assert_eq!(
        COUNTS.with(Cell::get),
        (2, 1, 0),
        "SETQ readback and final read, plus the store, must take the leaf"
    );
}

#[test]
fn lexical_leaf_publishes_when_a_poll_is_requested() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = resolve_sym("LEXICAL-LEAF-CAPTURE").unwrap();
    env.define_local_symbol(symbol, EgclVal::from_fixnum(11));
    let native = code(
        "((setq lexical-leaf-capture next) lexical-leaf-capture)",
        &env,
    );
    reset();
    REQUEST_POLL.with(|request| request.set(true));
    assert_eq!(
        native.run(&[EgclVal::from_fixnum(42)], &mut env).unwrap(),
        EgclVal::from_fixnum(42)
    );
    let (reads, writes, published) = COUNTS.with(Cell::get);
    assert_eq!(reads, 2, "SETQ readback plus the final lexical read");
    assert_eq!(
        writes, 0,
        "a request posted after entry must bypass the leaf"
    );
    assert_eq!(published, 1);
    assert!(
        !egcl_rt::thread::current_execution_poll_pending(),
        "the published operation must cooperate"
    );
}

#[test]
fn lexical_leaf_missing_binding_keeps_published_error_path() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // Resolve slot names before the failing call and root the actual error
    // before inspecting a condition: its instance can move during slot access.
    let name_slot = resolve_sym("NAME").unwrap();
    let missing = resolve_sym("LEXICAL-LEAF-CAPTURE").unwrap();
    let native = code("(lexical-leaf-capture)", &env);
    reset();
    egcl_rt::rooted!(
        error = native
            .run(&[NIL], &mut env)
            .expect_err("an unbound capture must signal")
    );
    let condition = match error.without_backtrace() {
        EgclError::Signalled(details) => details.condition,
        other => panic!("expected signalled UNBOUND-VARIABLE, got {other:?}"),
    };
    egcl_rt::rooted!(condition = condition);
    assert!(
        super::super::super::condition_matches_handler(&env, *condition, "UNBOUND-VARIABLE"),
        "wrong condition: {:?}",
        &*error
    );
    assert_eq!(
        super::super::super::read_slot_value(*condition, name_slot, &env).unwrap(),
        missing,
        "the condition NAME must be the exact missing symbol"
    );
    assert_eq!(COUNTS.with(Cell::get), (0, 0, 1));
}

#[test]
fn lexical_leaf_stored_capture_relocates_across_a_collecting_call() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = resolve_sym("LEXICAL-LEAF-CAPTURE").unwrap();
    env.define_local_symbol(symbol, NIL);
    super::super::super::read_eval_all_env(
        "(defun lexical-leaf-collect () (%force-minor-gc-for-test))",
        &mut env,
    )
    .unwrap();
    let native = code(
        "((setq lexical-leaf-capture next) (lexical-leaf-collect) lexical-leaf-capture)",
        &env,
    );
    egcl_rt::rooted!(datum = super::super::super::arena_str("leaf capture moves"));
    let before = datum.to_raw();
    reset();
    egcl_rt::rooted!(result = native.run(&[*datum], &mut env).unwrap());
    assert_ne!(
        before,
        datum.to_raw(),
        "the referent must actually relocate"
    );
    assert_eq!(*result, *datum);
    assert_eq!(env.lookup_var("LEXICAL-LEAF-CAPTURE"), Some(*datum));
    let (reads, writes, _) = COUNTS.with(Cell::get);
    assert_eq!((reads, writes), (2, 1));
}

#[test]
fn lexical_leaf_published_poll_relocates_outgoing_value_before_store() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = resolve_sym("LEXICAL-LEAF-CAPTURE").unwrap();
    env.define_local_symbol(symbol, NIL);
    let native = code(
        "((setq lexical-leaf-capture next) lexical-leaf-capture)",
        &env,
    );
    // Allocate after lowering so this is a nursery object at the cooperation
    // point, and post the poll only after native entry has already polled.
    egcl_rt::rooted!(datum = super::super::super::arena_str("outgoing lexical store moves"));
    reset();
    COLLECT_NEXT_STORE.with(|collect| collect.set(true));
    REQUEST_POLL.with(|request| request.set(true));
    egcl_rt::rooted!(result = native.run(&[*datum], &mut env).unwrap());
    let (before, after, published, collected) = OUTGOING_MOVE
        .with(Cell::get)
        .expect("the published store must reach the pre-setter collection hook");
    assert!(
        published,
        "the outgoing argument must be scanned through a live publication"
    );
    assert!(collected, "the forced minor collection must succeed");
    assert_ne!(
        before, after,
        "the actual outgoing argument must relocate before StoreEnvVar"
    );
    assert_eq!(after, datum.to_raw());
    assert_eq!(*result, *datum);
    assert_eq!(env.lookup_var("LEXICAL-LEAF-CAPTURE"), Some(*datum));
    let (reads, writes, published) = COUNTS.with(Cell::get);
    let (published_reads, published_stores) = PUBLISHED_ACCESSES.with(Cell::get);
    assert_eq!(
        published_stores, 1,
        "the armed store must publish exactly once"
    );
    assert_eq!(writes, 0, "the armed store must use the published path");
    assert_eq!(
        reads + published_reads,
        2,
        "SETQ readback and final read may publish to service GC rearming"
    );
    assert_eq!(
        published,
        published_reads + published_stores,
        "the total must account for every published lexical operation"
    );
}
