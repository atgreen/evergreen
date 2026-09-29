#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use torcl_compiler::t2::build::build_from_bytecode_for_transfers;
use torcl_compiler::t2::emit::{emit_framed, emit_framed_transfers};
use torcl_rt::bytecode::{BytecodeFunction, Instr};
use torcl_rt::value::NIL;

fn body() -> BytecodeFunction {
    BytecodeFunction {
        code: vec![
            Instr::LoadLocal(0),
            Instr::CallNamed {
                sym: 123456,
                nargs: 1,
            },
            Instr::CallNamed {
                sym: 123457,
                nargs: 1,
            },
            Instr::Return,
        ],
        constants: vec![],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        names: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 2,
        max_stack: 2,
        arity: 2,
        name: "native-invoke-emission".into(),
        params_form: NIL,
        min_args: 2,
        max_args: Some(2),
        variadic: false,
    }
}

#[test]
fn rich_emitter_produces_one_exact_recovery_site_per_invoke() {
    let ir = build_from_bytecode_for_transfers(&body()).unwrap();
    assert!(emit_framed(&ir, 0, 0, 0, 0, 0, 0, 0, 0, None).is_err());
    let (emitted, table) = emit_framed_transfers(&ir, 0x12345678, 4).unwrap();
    assert_eq!(table.sites().count(), 2);
    assert!(!emitted.has_deopt);
    assert_eq!(emitted.emitted_safepoints, 2);
    for site in table.sites() {
        let offset = site.return_offset() as usize;
        assert_eq!(&emitted.code[offset - 2..offset], &[0xff, 0xd0]);
        assert!(table.lookup(0x1000, 0x1000 + offset).is_some());
        assert!(!site.map().roots.is_empty());
    }
}

#[test]
fn transfer_entry_refuses_missing_adapter_storage_and_raw_argument_representation() {
    let mut ir = build_from_bytecode_for_transfers(&body()).unwrap();
    assert!(emit_framed_transfers(&ir, 0, 4).is_err());
    assert!(emit_framed_transfers(&ir, 0x12345678, 1).is_err());
    let argument = ir.block(ir.entry()).params[0];
    ir.set_repr(
        argument,
        torcl_compiler::t2::ir::ValueRepresentation::UnboxedFixnum,
    );
    assert!(emit_framed_transfers(&ir, 0x12345678, 4).is_err());
}

use std::cell::Cell;
use torcl_compiler::t2::emit::TransferCallRequest;
use torcl_compiler::t2::native_transfer::{
    SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
};
use torcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable};
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
use torcl_rt::value::TorclVal;
use torcl_rt::{Collector, HeapCollector, TorclStack};

struct RunState {
    anchor: *mut NativeSegment,
    table: *const SysvTransferTable,
    code_base: usize,
    snapshots: *mut u8,
    snapshot_count: usize,
    activation_len: usize,
    expected: *const TorclVal,
    expected_len: usize,
    first_nargs: usize,
    transfer_bcp: u32,
    calls: usize,
    drops: usize,
    recovered: usize,
    exit: NativeExit,
}
thread_local! { static RUN: Cell<*mut RunState> = const { Cell::new(std::ptr::null_mut()) }; }
struct Finished(*mut usize);
impl Drop for Finished {
    fn drop(&mut self) {
        unsafe {
            *self.0 += 1;
        }
    }
}
unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let state = unsafe { &mut *RUN.with(Cell::get) };
    let request = unsafe { &*request.cast::<TransferCallRequest>() };
    state.calls += 1;
    let _finished = Finished(&mut state.drops);
    assert_eq!(
        request.symbol,
        if state.calls == 1 { 123456 } else { 123457 }
    );
    assert_eq!(
        request.nargs,
        if state.calls == 1 {
            state.first_nargs
        } else {
            1
        }
    );
    state.anchor = native_transfer::current_segment();
    HeapCollector::new().minor_gc().unwrap();
    for index in 0..request.nargs {
        assert_eq!(unsafe { request.args.add(index).read() }, unsafe {
            state
                .expected
                .add(if state.calls == 1 { index } else { 1 })
                .read()
        });
    }
    // Return a different value than argument zero. Normal root restoration
    // must not overwrite this result with the old argument's shadow.
    let value = unsafe { state.expected.add(1).read() };
    unsafe {
        out.write(NativeOutcome {
            value,
            exit: if state.calls == 1 {
                NativeExit::Returned
            } else {
                state.exit
            },
        });
    }
}
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let state = unsafe { &mut *RUN.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    let request = unsafe { &*capture.request.cast::<TransferCallRequest>() };
    let table = unsafe { &*state.table };
    let site = table
        .lookup(state.code_base, capture.return_pc as usize)
        .unwrap();
    assert_eq!(
        site.map().origin_bcp,
        state.transfer_bcp,
        "recover second call, never replay first"
    );
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
    torcl_rt::rooted!(payload = capture.value);
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
            .reconstruct(|_| panic!("no unboxed floats"))
            .unwrap()
    );
    assert_eq!(frames[0].locals.as_slice(), unsafe {
        std::slice::from_raw_parts(state.expected, state.expected_len)
    });
    assert_eq!(frames[0].stack.as_slice(), &[*payload]);
    unsafe {
        snapshot.write_back(state.code_base, capture).unwrap();
    }
    state.recovered += 1;
    capture.value = *payload;
    capture.request = state.anchor.cast();
}
#[unsafe(naked)]
unsafe extern "C" fn dispatch(_anchor: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "jmp {leave}", leave = sym native_transfer::leave_native_segment);
}
fn double(value: f64) -> TorclVal {
    let body = torcl_rt::alloc_typed(8, torcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(value);
        TorclVal::from_heap_ptr(body.sub(8))
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn compiler_emitted_calls_return_or_capture_after_collecting_helpers() {
    assert!(native_transfer::is_supported());
    for nargs in [0usize, 1, 4, 8] {
        for exit in [
            NativeExit::Returned,
            NativeExit::Transfer,
            NativeExit::Deopt,
        ] {
            eprintln!("compiler invoke nargs={nargs} exit={exit:?}");
            let mut source = body();
            let locals = nargs.max(2);
            source.n_locals = locals as u16;
            source.arity = locals as u16;
            source.min_args = locals as u16;
            source.max_args = Some(locals as u16);
            source.max_stack = nargs.max(1) as u16;
            source.code = (0..nargs).map(|i| Instr::LoadLocal(i as u16)).collect();
            source.code.extend([
                Instr::CallNamed {
                    sym: 123456,
                    nargs: nargs as u16,
                },
                Instr::CallNamed {
                    sym: 123457,
                    nargs: 1,
                },
                Instr::Return,
            ]);
            let activation_slots = source.n_locals + source.max_stack;
            let ir = build_from_bytecode_for_transfers(&source).unwrap();
            let capture =
                JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8)).unwrap();
            let veneer = JitBuffer::new(&emit_helper_veneer(helper, capture.as_ptr())).unwrap();
            let (emitted, table) =
                emit_framed_transfers(&ir, veneer.as_ptr() as u64, activation_slots).unwrap();
            let code = JitBuffer::new(&emitted.code).unwrap();
            let mut snapshots: Vec<_> = table
                .sites()
                .map(|site| (site.return_offset(), site.reserve_snapshot().unwrap()))
                .collect();
            torcl_rt::rooted!(expected = vec![NIL; locals]);
            for i in 1..locals {
                expected[i] = double((i + 1) as f64 * 101.0);
            }
            expected[0] = double(101.0);
            let original = expected[0].to_raw();
            torcl_rt::rooted!(
                activation = vec![
                    NIL;
                    usize::from(activation_slots)
                        + usize::from(emitted.shadow_root_slots)
                ]
            );
            activation[..locals].copy_from_slice(&expected);
            let mut state = RunState {
                anchor: std::ptr::null_mut(),
                table: &table,
                code_base: code.as_ptr() as usize,
                snapshots: snapshots.as_mut_ptr().cast(),
                snapshot_count: snapshots.len(),
                activation_len: activation.len(),
                expected: expected.as_ptr(),
                expected_len: locals,
                first_nargs: nargs,
                transfer_bcp: nargs as u32 + 1,
                calls: 0,
                drops: 0,
                recovered: 0,
                exit,
            };
            RUN.with(|slot| slot.set(&mut state));
            let stack = TorclStack::new(64 * 1024);
            let outcome = unsafe {
                native_transfer::invoke_native_segment(
                    code.as_ptr(),
                    activation.as_mut_ptr().cast(),
                    &stack,
                )
            }
            .unwrap();
            RUN.with(|slot| slot.set(std::ptr::null_mut()));
            assert_eq!(outcome.exit, exit);
            assert_eq!(outcome.value, expected[1]);
            assert_eq!(outcome.value.as_double_float(), 202.0);
            assert_eq!((state.calls, state.drops), (2, 2));
            assert_eq!(state.recovered, usize::from(exit != NativeExit::Returned));
            assert_ne!(
                original,
                expected[0].to_raw(),
                "helper really relocated the argument"
            );
            assert!(native_transfer::current_segment().is_null());
        }
    }
}
