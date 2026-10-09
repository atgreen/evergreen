// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::{
    build,
    ir::{AuxData, Opcode},
    verify,
};
use egcl_rt::EgclVal;
use egcl_rt::bytecode::{BytecodeFunction, Instr};

fn cleanup_body() -> BytecodeFunction {
    let mut body = BytecodeFunction {
        name: "cleanup-merge".into(),
        code: vec![],
        constants: vec![EgclVal::from_fixnum(99)],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 1,
        arity: 1,
        params_form: egcl_rt::value::NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
    };
    body.code = vec![
        Instr::PushUnwind {
            cleanup_bcp: 8,
            sp_restore: 0,
        },
        Instr::CallNamed {
            sym: egcl_rt::symbols::intern("MAY-THROW"),
            nargs: 0,
        },
        Instr::Pop,
        Instr::Const(0),
        Instr::StoreLocal(0),
        Instr::LoadLocal(0),
        Instr::PopHandler,
        Instr::EnterCleanupNormal {
            cleanup_bcp: 8,
            resume_bcp: 12,
        },
        Instr::LoadLocal(0),
        Instr::CallNamed {
            sym: egcl_rt::symbols::intern("OBSERVE-CLEANUP"),
            nargs: 1,
        },
        Instr::Pop,
        Instr::CleanupReturn,
        Instr::Return,
    ];
    body
}

#[test]
fn handler_initializers_use_transfer_invokes_with_live_stack_maps() {
    use egcl_rt::bytecode::HandlerBindInfo;
    let mut body = cleanup_body();
    let symbol = egcl_rt::symbols::intern("HANDLER-INITIALIZER");
    body.code = vec![
        Instr::LoadFunction(symbol),
        Instr::EvalHost(0),
        Instr::PushHandlerBind { hb: 0 },
        Instr::Const(0),
        Instr::PopHandlerBind,
        Instr::Return,
    ];
    body.max_stack = 2;
    body.handler_binds = vec![HandlerBindInfo {
        types: vec!["ERROR".into(), "WARNING".into()],
    }];
    assert!(build::build_from_bytecode(&body).is_err());
    assert!(build::build_from_bytecode_for_transfers(&body).is_err());
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&ir).unwrap();
    let calls: Vec<_> = ir
        .block_order()
        .iter()
        .flat_map(|&block| &ir.block(block).insts)
        .copied()
        .filter(|&inst| ir.inst(inst).opcode == Opcode::Invoke)
        .collect();
    let lookup = calls
        .iter()
        .copied()
        .find(|&inst| matches!(ir.inst(inst).aux, AuxData::FunctionLookup(s) if s == symbol))
        .unwrap();
    let eval = calls
        .iter()
        .copied()
        .find(|&inst| matches!(ir.inst(inst).aux, AuxData::HostEval(0)))
        .unwrap();
    let enter = calls
        .iter()
        .copied()
        .find(|&inst| {
            matches!(
                ir.inst(inst).aux,
                AuxData::HandlerBindScope { enter: true, .. }
            )
        })
        .unwrap();
    assert!(ir.inst(lookup).args.is_empty());
    assert!(ir.inst(eval).args.is_empty());
    assert_eq!(ir.inst(enter).args.len(), 2);
    let frame = ir.frame_states.get(ir.inst(eval).frame_state.unwrap());
    assert_eq!(
        frame.scopes.last().unwrap().stack.len(),
        1,
        "the first handler must remain live during the second initializer"
    );

    let mut malformed = ir.clone();
    malformed.inst_mut(eval).args = ir.inst(enter).args.clone();
    assert!(verify::verify(&malformed).is_err());

    body.code[1] = Instr::EvalHost(1);
    assert!(build::build_from_bytecode_for_native_cleanups(&body).is_err());
    body.code.remove(1);
    assert!(
        build::build_from_bytecode_for_native_cleanups(&body).is_err(),
        "registration must reject a missing handler operand"
    );
}

#[test]
fn throwing_predecessor_preserves_the_local_before_a_later_assignment() {
    for initializer in [
        Instr::CallNamed {
            sym: egcl_rt::symbols::intern("MAY-THROW"),
            nargs: 0,
        },
        Instr::EvalHost(0),
        Instr::LoadFunction(egcl_rt::symbols::intern("HANDLER-LOOKUP")),
    ] {
        check_throwing_predecessor(initializer);
    }
}

fn check_throwing_predecessor(initializer: Instr) {
    let mut body = cleanup_body();
    body.code[1] = initializer;
    let f = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&f).unwrap();
    let landing = f
        .block_order()
        .iter()
        .copied()
        .find(|&b| {
            f.block(b)
                .insts
                .iter()
                .any(|&i| f.inst(i).opcode == Opcode::CleanupLanding)
        })
        .expect("native cleanup entry");
    assert!(
        !f.block(landing).params.is_empty(),
        "old and new local must merge"
    );
    let incoming: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .flat_map(|&i| f.inst(i).targets.iter())
        .filter(|edge| edge.block == landing)
        .collect();
    assert_eq!(incoming.len(), 2);
    assert_ne!(
        incoming[0].args, incoming[1].args,
        "exception sees old local; normal sees 99"
    );
    assert!(
        incoming
            .iter()
            .any(|edge| edge.args.contains(&f.block(f.entry()).params[0]))
    );
    let assigned = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .find_map(|&i| {
            matches!(f.inst(i).aux, AuxData::FixnumImm(99)).then(|| f.inst(i).results[0])
        })
        .unwrap();
    assert!(incoming.iter().any(|edge| edge.args.contains(&assigned)));
    assert!(
        f.block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .any(|&i| f.inst(i).opcode == Opcode::Invoke
                && matches!(f.inst(i).aux, AuxData::CleanupContinuation { .. })),
        "cleanup completion must represent normal and pending-transfer routes"
    );
    let mut bad = f.clone();
    let transfer = bad
        .block_order()
        .iter()
        .flat_map(|&b| &bad.block(b).insts)
        .copied()
        .find(|&i| bad.inst(i).opcode == Opcode::NlxTransfer && !bad.inst(i).targets.is_empty())
        .unwrap();
    if let AuxData::TransferSite { scopes, .. } = &mut bad.inst_mut(transfer).aux {
        scopes.retain(|scope| {
            !matches!(
                scope.kind,
                egcl_compiler::control_scope::ScopeKind::Unwind { .. }
            )
        });
    }
    assert!(
        verify::verify(&bad)
            .unwrap_err()
            .iter()
            .any(|error| error.check == "V13 cleanup"),
        "a native landing requires the selected unwind scope"
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn emitted_cleanup_edges_have_exact_sites_and_checked_native_destinations() {
    use egcl_compiler::t2::emit::{
        emit_framed_native_cleanups, emit_framed_transfers_with_cleanup,
    };
    use egcl_compiler::t2::native_transfer::SysvTransferCapture;
    use egcl_rt::native_transfer::NativeExit;
    let body = cleanup_body();
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    assert!(emit_framed_transfers_with_cleanup(&ir, 1, 2, Some((2, 3))).is_err());
    let (code, sites) = emit_framed_native_cleanups(&ir, 1, 2, 2, 3, 4).unwrap();
    assert_eq!(sites.sites().count(), 3);
    assert_eq!(code.root_sync_sites.len(), 4); // 3 invokes and normal save
    for site in sites.sites() {
        let capture = SysvTransferCapture {
            request: std::ptr::null_mut(),
            value: egcl_rt::value::NIL,
            exit: NativeExit::Transfer,
            preserved: [0; 6],
            caller_sp: 0x2000 as *const u64,
            return_pc: (0x1000 + site.return_offset() as usize) as *const u8,
        };
        let landing = site.native_cleanup_landing(0x1000, &capture).unwrap();
        match site.map().origin_bcp {
            1 => {
                let landing = landing.expect("protected call must enter native cleanup");
                let offset = landing.entry as usize - 0x1000;
                assert_eq!(&code.code[offset..offset + 4], &[0xf3, 0x0f, 0x1e, 0xfa]);
                // Invoke request plus its explicit native call context.
                assert_eq!(landing.stack_pointer as usize, 0x2030);
            }
            9 | 11 => assert!(landing.is_none(), "no enclosing cleanup remains"),
            bcp => panic!("unexpected site {bcp}"),
        }
    }
    // Landing maps currently enter the cold edge's terminator. They must not
    // silently skip work added earlier in that block by a future IR transform.
    let mut changed = ir.clone();
    let cold = changed
        .block_order()
        .iter()
        .copied()
        .find(|&block| {
            changed.terminator(block).is_some_and(|inst| {
                changed.inst(inst).opcode == Opcode::NlxTransfer
                    && !changed.inst(inst).targets.is_empty()
            })
        })
        .unwrap();
    use egcl_compiler::t2::ir::{IRType, InstData, InstFlags, ValueRepresentation};
    changed.push_inst(
        cold,
        InstData {
            opcode: Opcode::ConstNil,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    changed.block_mut(cold).insts.swap(0, 1);
    verify::verify(&changed).unwrap();
    assert!(emit_framed_native_cleanups(&changed, 1, 2, 2, 3, 4).is_err());
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod execution {
    use super::*;
    use egcl_compiler::t2::emit::{
        TransferCallRequest, TransferCleanupRequest, emit_framed_native_cleanups,
    };
    use egcl_compiler::t2::native_transfer::{
        SysvNativeLanding, SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
        emit_native_landing_stub,
    };
    use egcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable};
    use egcl_rt::jit::JitBuffer;
    use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome};
    use egcl_rt::value::NIL;
    use egcl_rt::{Collector, EgclStack, HeapCollector};
    use std::cell::Cell;

    #[repr(C)]
    struct Dispatch {
        entry: *const u8,
        request: *mut u8,
    }
    struct State {
        table: *const SysvTransferTable,
        base: usize,
        snapshots: *mut u8,
        activation: *mut EgclVal,
        slots: usize,
        expected: *const EgclVal,
        original: u64,
        throwing: bool,
        calls: usize,
        drops: usize,
        observed: bool,
        pending: bool,
        saves: usize,
        completions: usize,
        landings: usize,
        landing: SysvNativeLanding,
        landing_stub: *const u8,
        dispatch: Dispatch,
    }
    thread_local! { static STATE: Cell<*mut State> = const { Cell::new(std::ptr::null_mut()) }; }
    struct Finished(*mut usize);
    impl Drop for Finished {
        fn drop(&mut self) {
            unsafe {
                *self.0 += 1;
            }
        }
    }

    unsafe extern "C" fn call(request: *mut u8, out: *mut NativeOutcome) {
        let state = unsafe { &mut *STATE.with(Cell::get) };
        let request = unsafe { &*request.cast::<TransferCallRequest>() };
        let _finished = Finished(&mut state.drops);
        state.calls += 1;
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(
            unsafe { (*state.expected).to_raw() },
            state.original,
            "heap local must actually move"
        );
        if state.calls == 2 {
            assert_eq!(request.nargs, 1);
            assert_eq!(
                unsafe { request.args.read() },
                if state.throwing {
                    unsafe { state.expected.read() }
                } else {
                    EgclVal::from_fixnum(99)
                }
            );
            state.observed = true;
        } else {
            assert_eq!(state.calls, 1);
        }
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: if state.calls == 1 && state.throwing {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
    }
    extern "C" fn clear_mv() {}

    unsafe extern "C" fn save(cleanup: u32, resume: u32, value: EgclVal) -> EgclVal {
        assert_eq!((cleanup, resume, value), (8, 12, EgclVal::from_fixnum(99)));
        unsafe {
            (*STATE.with(Cell::get)).saves += 1;
        }
        NIL
    }
    unsafe extern "C" fn complete(request: *mut u8, out: *mut NativeOutcome) {
        let state = unsafe { &mut *STATE.with(Cell::get) };
        let request = unsafe { &*request.cast::<TransferCleanupRequest>() };
        assert_eq!(
            (request.cleanup_bcp, request.resume_bcp, request.reserved),
            (8, 12, 0)
        );
        assert_eq!(request.activation, state.activation);
        state.completions += 1;
        unsafe {
            out.write(NativeOutcome {
                value: EgclVal::from_fixnum(99),
                exit: if state.pending {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
    }
    unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
        let state = unsafe { &mut *STATE.with(Cell::get) };
        let capture = unsafe { &mut *capture };
        assert_eq!(
            state.drops, state.calls,
            "helper must return through Rust first"
        );
        let table = unsafe { &*state.table };
        let (index, site) = table
            .sites()
            .enumerate()
            .find(|(_, site)| {
                state.base + site.return_offset() as usize == capture.return_pc as usize
            })
            .unwrap();
        let snapshot = unsafe { &mut *state.snapshots.cast::<SysvSiteSnapshot>().add(index) };
        unsafe {
            snapshot
                .capture_from_activation(
                    state.base,
                    capture,
                    std::slice::from_raw_parts(state.activation, state.slots),
                )
                .unwrap();
        }
        if let Some(landing) = site.native_cleanup_landing(state.base, capture).unwrap() {
            assert!(!state.pending);
            state.pending = true;
            state.landings += 1;
            unsafe {
                snapshot.write_back(state.base, capture).unwrap();
            }
            state.landing = landing;
            state.dispatch = Dispatch {
                entry: state.landing_stub,
                request: std::ptr::from_mut(&mut state.landing).cast(),
            };
        } else {
            assert_eq!(
                site.map().origin_bcp,
                11,
                "leave only after native cleanup completion"
            );
            state.dispatch = Dispatch {
                entry: native_transfer::leave_native_segment as *const u8,
                request: native_transfer::current_segment().cast(),
            };
        }
        capture.request = std::ptr::from_mut(&mut state.dispatch).cast();
    }
    #[unsafe(naked)]
    unsafe extern "C" fn dispatch(_packet: *mut u8, _value: u64, _exit: NativeExit) -> ! {
        core::arch::naked_asm!("endbr64", "mov rax, [rdi]", "mov rdi, [rdi + 8]", "jmp rax");
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn generated_cleanup_uses_exceptional_phi_and_completes_without_bytecode() {
        assert!(native_transfer::is_supported());
        let body = cleanup_body();
        let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
        let landing_stub = JitBuffer::new(&emit_native_landing_stub()).unwrap();
        let capture = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8)).unwrap();
        let call_veneer = JitBuffer::new(&emit_helper_veneer(call, capture.as_ptr())).unwrap();
        let complete_veneer =
            JitBuffer::new(&emit_helper_veneer(complete, capture.as_ptr())).unwrap();
        let (emitted, table) = emit_framed_native_cleanups(
            &ir,
            call_veneer.as_ptr() as u64,
            2,
            save as *const () as u64,
            complete_veneer.as_ptr() as u64,
            clear_mv as *const () as u64,
        )
        .unwrap();
        let code = JitBuffer::new(&emitted.code).unwrap();
        for throwing in [false, true] {
            let mut snapshots: Vec<_> = table
                .sites()
                .map(|site| site.reserve_snapshot().unwrap())
                .collect();
            egcl_rt::rooted_ref!(_snapshots = &mut snapshots);
            let object = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
            unsafe {
                object.cast::<f64>().write(42.5);
            }
            egcl_rt::rooted!(expected = unsafe { EgclVal::from_heap_ptr(object.sub(8)) });
            egcl_rt::rooted!(activation = vec![NIL; 2 + usize::from(emitted.shadow_root_slots)]);
            activation[0] = *expected;
            let mut state = State {
                table: &table,
                base: code.as_ptr() as usize,
                snapshots: snapshots.as_mut_ptr().cast(),
                activation: activation.as_mut_ptr(),
                slots: activation.len(),
                expected: &*expected,
                original: expected.to_raw(),
                throwing,
                calls: 0,
                drops: 0,
                observed: false,
                pending: false,
                saves: 0,
                completions: 0,
                landings: 0,
                landing: SysvNativeLanding {
                    stack_pointer: std::ptr::null_mut(),
                    entry: std::ptr::null(),
                },
                landing_stub: landing_stub.as_ptr(),
                dispatch: Dispatch {
                    entry: std::ptr::null(),
                    request: std::ptr::null_mut(),
                },
            };
            STATE.with(|slot| slot.set(&mut state));
            let stack = EgclStack::new(64 * 1024);
            let outcome = unsafe {
                native_transfer::invoke_native_segment(
                    code.as_ptr(),
                    activation.as_mut_ptr().cast(),
                    &stack,
                )
            }
            .unwrap();
            STATE.with(|slot| slot.set(std::ptr::null_mut()));
            assert_eq!(
                outcome.exit,
                if throwing {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                }
            );
            assert_eq!(outcome.value, EgclVal::from_fixnum(99));
            assert!(state.observed);
            assert_eq!((state.calls, state.drops, state.completions), (2, 2, 1));
            assert_eq!(state.landings, usize::from(throwing));
            assert_eq!(state.saves, usize::from(!throwing));
        }
    }
}
