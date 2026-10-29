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
        self_call_sites: Vec::new(),
        notinline_sites: Vec::new(),
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
        binder_cache: Default::default(),
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
                assert_eq!(landing.stack_pointer as usize, 0x2040);
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
    unsafe extern "C" fn single_values(_: u64, dst: *mut EgclVal, count: u64) {
        if count == egcl_rt::native_transfer::SNAPSHOT_NATIVE_RETURN_VALUES {
            unsafe {
                dst.cast::<egcl_rt::native_transfer::NativeReturnValues>()
                    .write(egcl_rt::native_transfer::NativeReturnValues::SINGLE);
            }
        } else {
            assert_eq!(count, 0);
        }
    }

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
            single_values as *const () as u64,
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

#[test]
fn native_catch_arithmetic_speculates_without_losing_residual_throw_route() {
    use egcl_compiler::control_scope::{Ownership, ScopeKind};
    use egcl_compiler::t2::speculate::{SpecType, speculate};

    let plus = egcl_rt::symbols::intern("+");
    let throwing = egcl_rt::symbols::intern("NATIVE-CATCH-SPECULATION-RESIDUAL-CALLEE");
    let mut body = cleanup_body();
    body.name = "native-catch-arithmetic".into();
    body.n_locals = 3;
    body.arity = 3;
    body.min_args = 3;
    body.max_args = Some(3);
    body.max_stack = 2;
    body.code = vec![
        Instr::LoadLocal(0),
        Instr::PushCatch {
            resume_bcp: 7,
            sp_restore: 0,
        },
        Instr::LoadLocal(1),
        Instr::LoadLocal(2),
        Instr::CallNamed {
            sym: plus,
            nargs: 2,
        },
        Instr::CallNamed {
            sym: throwing,
            nargs: 1,
        },
        Instr::PopHandler,
        Instr::Return,
    ];
    let mut function = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&function).unwrap();
    let find_call = |symbol| {
        function
            .block_order()
            .iter()
            .find_map(|&block| {
                function.block(block).insts.iter().copied().find_map(|inst| {
                matches!(function.inst(inst).aux, AuxData::CallTarget(target) if target == symbol)
                    .then_some((block, inst))
            })
            })
            .unwrap()
    };
    let (arithmetic_block, arithmetic) = find_call(plus);
    let (_, residual) = find_call(throwing);
    assert_eq!(function.inst(arithmetic).opcode, Opcode::Invoke);
    assert_eq!(function.inst(residual).opcode, Opcode::Invoke);
    let normal = function.inst(arithmetic).targets[0].clone();
    let arithmetic_result = function.inst(arithmetic).results.clone();
    let residual_cold = function.inst(residual).targets[1].block;

    assert_eq!(
        speculate(&mut function, &|bcp| (bcp == 4).then_some(SpecType::Fixnum)),
        1,
        "the native-cleanup Invoke must participate in genuine numeric specialization"
    );
    verify::verify(&function).expect("specializing an Invoke must preserve a valid whole CFG");
    let numeric = function.inst(arithmetic);
    assert_eq!(numeric.opcode, Opcode::FixnumAdd);
    assert_eq!(numeric.results, arithmetic_result);
    assert!(
        numeric.targets.is_empty(),
        "the typed operation is not a CFG terminator"
    );
    assert!(numeric.flags.guard && numeric.flags.effectful);
    assert!(!numeric.flags.call && !numeric.flags.terminator);
    let state = function.frame_states.get(numeric.frame_state.unwrap());
    assert_eq!(state.scopes.len(), 1);
    assert_eq!(state.scopes[0].bcp, 4);
    assert_eq!(
        state.scopes[0].stack.len(),
        2,
        "guard recovery retains both original operands"
    );

    let instructions = &function.block(arithmetic_block).insts;
    let numeric_position = instructions
        .iter()
        .position(|&inst| inst == arithmetic)
        .unwrap();
    assert!(
        !instructions[..numeric_position].iter().any(|&inst| {
            let data = function.inst(inst);
            data.opcode == Opcode::Guard
                && matches!(data.aux, AuxData::BuiltinBinding { .. })
                && data
                    .frame_state
                    .is_some_and(|state| function.frame_states.get(state).scopes[0].bcp == 4)
        }),
        "canonical CL arithmetic has no binding guard; operand guards retain exact recovery"
    );
    assert_eq!(
        function.inst(instructions[numeric_position + 1]).opcode,
        Opcode::ClearMv
    );
    let jump = function.inst(function.terminator(arithmetic_block).unwrap());
    assert_eq!(jump.opcode, Opcode::Jump);
    assert_eq!(jump.targets.len(), 1);
    assert_eq!(jump.targets[0].block, normal.block);
    assert_eq!(
        jump.targets[0].args, normal.args,
        "preserve the normal result projection"
    );

    let invoke = function.inst(residual);
    assert_eq!(invoke.opcode, Opcode::Invoke);
    assert_eq!(invoke.targets[1].block, residual_cold);
    let cold = function.inst(function.terminator(residual_cold).unwrap());
    assert_eq!(cold.opcode, Opcode::NlxTransfer);
    let AuxData::TransferSite { origin_bcp, scopes } = &cold.aux else {
        panic!("residual throwing call must retain its capture metadata")
    };
    assert_eq!(*origin_bcp, 5);
    assert!(matches!(scopes.as_slice(), [scope]
        if scope.push_bcp == 1 && scope.ownership == Ownership::Local
        && matches!(scope.kind, ScopeKind::Catch { resume_bcp: 7 })));
    assert!(
        cold.targets.iter().any(|edge| {
            function.block(edge.block).insts.iter().any(|&inst| {
                let data = function.inst(inst);
                data.opcode == Opcode::CatchLanding
                    && matches!(
                        data.aux,
                        AuxData::CatchDestination {
                            push_bcp: 1,
                            resume_bcp: 7
                        }
                    )
            })
        }),
        "the remaining call must still reach its native catch landing"
    );
}

#[test]
fn native_invoke_speculation_preserves_cold_only_osr_header_and_imports() {
    use egcl_compiler::t2::frame_state::ValueSource;
    use egcl_compiler::t2::ir::{Block, Function, Inst, ValueDef};
    use egcl_compiler::t2::speculate::{SpecType, speculate};
    use std::collections::HashSet;

    fn reachable(function: &Function, normal_only: Option<Inst>) -> HashSet<Block> {
        let mut seen = HashSet::new();
        let mut work = vec![function.entry()];
        while let Some(block) = work.pop() {
            if !seen.insert(block) {
                continue;
            }
            let term = function.terminator(block).unwrap();
            let targets = &function.inst(term).targets;
            let count = if normal_only == Some(term) {
                1
            } else {
                targets.len()
            };
            work.extend(targets[..count].iter().map(|edge| edge.block));
        }
        seen
    }

    let plus = egcl_rt::symbols::intern("+");
    let mut body = cleanup_body();
    body.name = "cold-only-osr-header".into();
    body.n_locals = 4;
    body.arity = 3;
    body.min_args = 3;
    body.max_args = Some(3);
    body.max_stack = 2;
    // The normal path exits the activation. Only a selected catch transfer
    // reaches the loop. Its caught value defines local 3 in the cold preheader.
    body.code = vec![
        Instr::LoadLocal(0),
        Instr::PushCatch {
            resume_bcp: 6,
            sp_restore: 0,
        },
        Instr::LoadLocal(1),
        Instr::LoadLocal(2),
        Instr::CallNamed {
            sym: plus,
            nargs: 2,
        },
        Instr::Return,
        Instr::StoreLocal(3),
        Instr::LoadLocal(3),
        Instr::BrIfFalse(10),
        Instr::Br(7),
        Instr::LoadLocal(3),
        Instr::Return,
    ];
    let mut function = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&function).unwrap();
    let candidate = function
        .block_order()
        .iter()
        .find_map(|&block| {
            function.block(block).insts.iter().copied().find(|&inst| {
            matches!(function.inst(inst).aux, AuxData::CallTarget(symbol) if symbol == plus)
        })
        })
        .unwrap();
    assert_eq!(function.inst(candidate).opcode, Opcode::Invoke);
    assert_eq!(function.osr_entries.len(), 1);
    let entry = function.osr_entries[0].clone();
    assert_eq!(entry.bcp, 7);
    assert!(reachable(&function, None).contains(&entry.block));
    assert!(
        !reachable(&function, Some(candidate)).contains(&entry.block),
        "fixture must make this Invoke the only ordinary-entry route to the OSR header"
    );
    let state = function.frame_states.get(entry.frame_state);
    let ValueSource::Value {
        value: imported, ..
    } = state.scopes[0].locals[3]
    else {
        panic!("the caught value must be a real OSR import, not a constant or dead slot")
    };
    let preheader = match function.value(imported).def {
        ValueDef::Param { block, .. } => block,
        ValueDef::Result { inst, .. } => function
            .block_order()
            .iter()
            .copied()
            .find(|&block| function.block(block).insts.contains(&inst))
            .unwrap(),
    };
    assert_ne!(preheader, function.entry());
    assert_ne!(preheader, entry.block);
    assert!(function.dominators().dominates(preheader, entry.block));
    let preheader_insts = function.block(preheader).insts.clone();
    let candidate_targets = function.inst(candidate).targets.clone();

    assert_eq!(
        speculate(&mut function, &|bcp| (bcp == 4).then_some(SpecType::Fixnum)),
        0,
        "keep the Invoke when specialization would strand an OSR header and its imports"
    );
    verify::verify(&function).unwrap();
    assert_eq!(function.inst(candidate).opcode, Opcode::Invoke);
    assert_eq!(
        function.inst(candidate).targets.len(),
        candidate_targets.len()
    );
    for (actual, expected) in function
        .inst(candidate)
        .targets
        .iter()
        .zip(candidate_targets)
    {
        assert_eq!(actual.block, expected.block);
        assert_eq!(actual.args, expected.args);
    }
    assert_eq!(function.osr_entries.len(), 1);
    assert_eq!(function.osr_entries[0].block, entry.block);
    assert_eq!(function.osr_entries[0].frame_state, entry.frame_state);
    assert!(
        matches!(function.frame_states.get(entry.frame_state).scopes[0].locals[3],
        ValueSource::Value { value, .. } if value == imported)
    );
    assert_eq!(function.block(preheader).insts, preheader_insts);
    assert!(reachable(&function, None).contains(&preheader));
    assert!(reachable(&function, None).contains(&entry.block));
    assert!(function.dominators().dominates(preheader, entry.block));
}

#[test]
fn noncapturing_closure_is_an_argument_free_transfer_invoke() {
    let mut body = cleanup_body();
    body.nested_functions = vec![std::sync::Arc::new(cleanup_body())];
    body.code = vec![
        Instr::MakeClosure {
            func: 0,
            capture_env: false,
        },
        Instr::Return,
    ];
    // The plain builder serves checked-ABI emitters (#478): it lowers the
    // creation to a target-free, argument-free adapter `Call`, never to the
    // mapped `Invoke` with exceptional successors built below.
    let checked = build::build_from_bytecode(&body).unwrap();
    verify::verify(&checked).unwrap();
    let checked_closure = checked
        .block_order()
        .iter()
        .flat_map(|&block| &checked.block(block).insts)
        .copied()
        .find(|&inst| matches!(checked.inst(inst).aux, AuxData::NestedClosure(0)))
        .unwrap();
    let checked_data = checked.inst(checked_closure);
    assert_eq!(checked_data.opcode, Opcode::Call);
    assert!(checked_data.args.is_empty());
    assert!(checked_data.targets.is_empty());
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&ir).unwrap();
    let closure = ir
        .block_order()
        .iter()
        .flat_map(|&block| &ir.block(block).insts)
        .copied()
        .find(|&inst| matches!(ir.inst(inst).aux, AuxData::NestedClosure(0)))
        .unwrap();
    let data = ir.inst(closure);
    assert_eq!(data.opcode, Opcode::Invoke);
    assert!(data.args.is_empty());
    assert_eq!(data.targets.len(), 2);
    assert_ne!(data.targets[0].block, data.targets[1].block);
    assert!(build::build_for_native_scope_optimization(&body, 0).is_ok());

    let mut malformed = ir.clone();
    let argument = ir.block(ir.block_order()[0]).params[0];
    malformed.inst_mut(closure).args = vec![argument];
    assert!(verify::verify(&malformed).is_err());

    body.code[0] = Instr::MakeClosure {
        func: 1,
        capture_env: false,
    };
    assert!(
        build::build_from_bytecode_for_native_cleanups(&body).is_err(),
        "an out-of-range nested function must decline"
    );
    assert!(
        build::build_from_bytecode(&body).is_err(),
        "the checked-ABI lowering must decline an out-of-range nested function too"
    );
    // A capturing closure reaches the activation's EnvFrame through the
    // runtime, identified by its exact bytecode PC (bliss-shih7.27).
    body.code[0] = Instr::MakeClosure {
        func: 0,
        capture_env: true,
    };
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&ir).unwrap();
    assert!(
        ir.block_order()
            .iter()
            .flat_map(|&block| &ir.block(block).insts)
            .any(|&inst| {
                ir.inst(inst).opcode == Opcode::Invoke
                    && matches!(
                        ir.inst(inst).aux,
                        AuxData::LexicalEnvironment {
                            bcp: 0,
                            value: false,
                            result: true
                        }
                    )
            })
    );
}

#[test]
fn captured_variable_operations_are_pc_identified_transfer_invokes() {
    let mut body = cleanup_body();
    body.has_env = true;
    body.names = vec!["X".into()];
    body.code = vec![
        Instr::PushEnvChild,
        Instr::Const(0),
        Instr::DefineEnvVar(0),
        Instr::LoadEnvVar(0),
        Instr::StoreEnvVar(0),
        Instr::PopEnvChild,
        Instr::Const(0),
        Instr::Return,
    ];
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&ir).unwrap();
    let mut seen: Vec<_> = ir
        .block_order()
        .iter()
        .flat_map(|&block| &ir.block(block).insts)
        .filter_map(|&inst| match ir.inst(inst).aux {
            AuxData::LexicalEnvironment { bcp, value, result } => {
                assert_eq!(ir.inst(inst).opcode, Opcode::Invoke);
                assert_eq!(ir.inst(inst).args.len(), usize::from(value));
                Some((bcp, value, result))
            }
            _ => None,
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            (0, false, false),
            (2, true, false),
            (3, false, true),
            (4, true, false),
            (5, false, false)
        ]
    );
    body.code[3] = Instr::LoadEnvVar(1);
    assert!(
        build::build_from_bytecode_for_native_cleanups(&body).is_err(),
        "an unknown name must decline, not become an environment transfer"
    );
}
