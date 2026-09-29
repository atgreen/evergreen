use super::*;
use std::cell::Cell;
use torcl_compiler::t2::native_transfer::{
    SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
};
use torcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable};
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer;

struct CaptureState {
    code_base: usize,
    table: *const SysvTransferTable,
    snapshots: *mut u8,
    snapshot_count: usize,
    activation_len: usize,
    expected: *const TorclVal,
    calls: usize,
    drops: usize,
    captures: usize,
}
thread_local! { static STATE: Cell<*mut CaptureState> = const { Cell::new(std::ptr::null_mut()) }; }
struct Finished(*mut usize);
impl Drop for Finished {
    fn drop(&mut self) {
        unsafe {
            *self.0 += 1;
        }
    }
}
unsafe extern "C" fn observed_bridge(request: *mut u8, out: *mut NativeOutcome) {
    let state = unsafe { &mut *STATE.with(Cell::get) };
    state.calls += 1;
    let _finished = Finished(&mut state.drops);
    unsafe {
        c2i_call_legacy_v2(request, out);
    }
}
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let state = unsafe { &mut *STATE.with(Cell::get) };
    assert_eq!(
        (state.calls, state.drops),
        (2, 2),
        "all helper Rust scopes finished before capture"
    );
    assert!(native_error_pending());
    let capture = unsafe { &mut *capture };
    let request = unsafe { &*capture.request.cast::<TransferCallRequest>() };
    let table = unsafe { &*state.table };
    let site = table
        .lookup(state.code_base, capture.return_pc as usize)
        .unwrap();
    let snapshots = unsafe {
        std::slice::from_raw_parts_mut(
            state.snapshots.cast::<(u32, SysvSiteSnapshot<'_>)>(),
            state.snapshot_count,
        )
    };
    let snapshot = &mut snapshots
        .iter_mut()
        .find(|(offset, _)| *offset == site.return_offset())
        .unwrap()
        .1;
    unsafe {
        snapshot
            .capture_from_activation(
                state.code_base,
                capture,
                std::slice::from_raw_parts(request.activation, state.activation_len),
            )
            .unwrap();
    }
    torcl_rt::rooted_ref!(_snapshot = &mut *snapshot);
    HeapCollector::new().minor_gc().unwrap();
    torcl_rt::rooted!(
        frames = snapshot
            .reconstruct(|_| panic!("tagged inputs only"))
            .unwrap()
    );
    assert_eq!(frames[0].locals[0], unsafe { state.expected.read() });
    assert_eq!(
        frames[0].stack.as_slice(),
        &[unsafe { state.expected.read() }]
    );
    unsafe {
        snapshot.write_back(state.code_base, capture).unwrap();
    }
    state.captures += 1;
    capture.request = native_transfer::current_segment().cast();
}
#[unsafe(naked)]
unsafe extern "C" fn dispatch(_anchor: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "jmp {leave}", leave = sym native_transfer::leave_native_segment);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_bridge_runs_compiled_lisp_callers_through_real_callees() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(native_transfer::is_supported());
    for (case, target) in [
        "(values (cons :answer x) (list :secondary))",
        "(car 7)",
        "(throw :v2-tag (values x (list :secondary)))",
    ]
    .into_iter()
    .enumerate()
    {
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *v2-ticks* 0 *v2-cleanup* 0)
             (defun v2-tick (x) (setq *v2-ticks* (+ *v2-ticks* 1)) x)
             (defun v2-target (x) (unwind-protect {target}
               (setq *v2-cleanup* (+ *v2-cleanup* 1))))"
            ),
            &mut env,
        )
        .unwrap();
        let tag = reader::read_from_string(":v2-tag").unwrap().0;
        let token = super::super::super::next_control_token("V2-COMPILED-CATCH");
        env.catch_stack.push((tag, token.clone()));
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(
            forms = reader::read_from_string("((v2-target (v2-tick x)))")
                .unwrap()
                .0
        );
        let body =
            Arc::new(compile_function("V2-COMPILED", *params, *forms, &env, false, false).unwrap());
        let _body = ActiveBytecodeRoot::new(&body);
        let ir = torcl_compiler::t2::build::build_from_bytecode_for_transfers(&body).unwrap();
        let capture = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8)).unwrap();
        let veneer =
            JitBuffer::new(&emit_helper_veneer(observed_bridge, capture.as_ptr())).unwrap();
        let base_slots = body.n_locals + body.max_stack;
        let (emitted, table) = torcl_compiler::t2::emit::emit_framed_transfers(
            &ir,
            veneer.as_ptr() as u64,
            base_slots,
        )
        .unwrap();
        assert_eq!(table.sites().count(), 2);
        let code = JitBuffer::new(&emitted.code).unwrap();
        let mut snapshots: Vec<_> = table
            .sites()
            .map(|site| (site.return_offset(), site.reserve_snapshot().unwrap()))
            .collect();
        torcl_rt::rooted!(
            expected = vec![super::super::super::arena_cons(
                TorclVal::from_fixnum(31),
                NIL
            )]
        );
        let original = expected[0].to_raw();
        torcl_rt::rooted!(
            activation =
                vec![NIL; usize::from(base_slots) + usize::from(emitted.shadow_root_slots)]
        );
        activation[0] = expected[0];
        let mut state = CaptureState {
            code_base: code.as_ptr() as usize,
            table: &table,
            snapshots: snapshots.as_mut_ptr().cast(),
            snapshot_count: snapshots.len(),
            activation_len: activation.len(),
            expected: expected.as_ptr(),
            calls: 0,
            drops: 0,
            captures: 0,
        };
        STATE.with(|slot| slot.set(&mut state));
        let _native = NativeEnvGuard::enter(&mut env);
        let stack = torcl_rt::TorclStack::new(64 * 1024);
        let outcome = unsafe {
            native_transfer::invoke_native_segment(
                code.as_ptr(),
                activation.as_mut_ptr().cast(),
                &stack,
            )
        }
        .unwrap();
        STATE.with(|slot| slot.set(std::ptr::null_mut()));
        torcl_rt::rooted!(primary = outcome.value);
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(
            expected[0].to_raw(),
            original,
            "argument relocated during real Lisp calls/recovery"
        );
        if case == 0 {
            assert_eq!(outcome.exit, NativeExit::Returned);
            assert_eq!(super::super::super::cp(*primary).1, expected[0]);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(env.mv[0], *primary);
            assert!(env.mv[1].is_cons());
            assert!(!native_error_pending());
        } else {
            assert_eq!(outcome.exit, NativeExit::Transfer);
            torcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()).unwrap());
            if case == 2 {
                assert!(matches!(&*error, TorclError::Internal(message) if message == &token));
                assert_eq!(
                    super::super::super::take_control_mv(&token, &mut env),
                    expected[0]
                );
                assert!(env.mv_active && env.mv.len() == 2);
                assert!(env.mv[1].is_cons());
            } else {
                assert!(!matches!(&*error, TorclError::Internal(_)));
            }
        }
        assert_eq!((state.calls, state.drops), (2, 2));
        assert_eq!(state.captures, usize::from(case != 0));
        for name in ["*v2-ticks*", "*v2-cleanup*"] {
            assert_eq!(
                super::super::super::read_eval_all_env(name, &mut env).unwrap(),
                TorclVal::from_fixnum(1),
                "{name} ran once"
            );
        }
        env.catch_stack.pop();
        assert!(native_transfer::current_segment().is_null());
    }
}
