// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Rooted cold boundaries for ordinary Lisp machine frames.

use super::*;
use egcl_compiler::t2::native_transfer::OrdinaryArityRequest;

use egcl_compiler::t2::emit::{NativeRecursiveControl, OrdinaryColdRequest};
use egcl_compiler::t2::native_transfer::OrdinaryHelperOutcome;
use egcl_rt::native_transfer::NativeReturnValues;

struct ArityRoots(*mut OrdinaryArityRequest);
impl egcl_rt::gc::TraceHostRoots for ArityRoots {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        unsafe {
            let count = (*self.0).nargs;
            visit(std::ptr::addr_of_mut!((*self.0).closure));
            for index in 0..count.min(3) {
                visit(
                    std::ptr::addr_of_mut!((*self.0).register_args)
                        .cast::<EgclVal>()
                        .add(index),
                );
            }
            for index in 0..count.saturating_sub(3) {
                visit((*self.0).overflow_args.add(index));
            }
        }
    }
}

// The assembly request and every actual incoming word stay writable and live
// throughout body. No references to those values cross a possible collection.
unsafe fn with_arity_roots<R>(request: *mut OrdinaryArityRequest, body: impl FnOnce() -> R) -> R {
    let mut roots = ArityRoots(request);
    egcl_rt::rooted_ref!(_roots = &mut roots);
    body()
}

#[cfg(test)]
thread_local! {
    static ARITY_ROOTED_OBSERVER: Cell<Option<unsafe fn(*mut OrdinaryArityRequest)>> = const { Cell::new(None) };
}
#[cfg(test)]
static COLD_CALLS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { Cell::new(0) }) };
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_cold_calls_for_test() -> usize {
    COLD_CALLS.with(|count| count.replace(0))
}

unsafe fn refresh_control(control: *mut NativeRecursiveControl) {
    if control.is_null() {
        return;
    }
    let execution = egcl_rt::thread::current_execution_context_for_native()
        .expect("ordinary helper has an owning execution");
    let control = unsafe { &mut *control };
    assert_eq!(control.execution, execution);
    control.seen_carrier = unsafe { &*execution }.carrier_id();
    control.seen_signal_epoch = egcl_rt::runtime::current_seen_signal_activity()
        .expect("ordinary helper has consumed signal state");
    control.deadline_armed = u8::from(egcl_rt::sandbox_cpu_deadline_armed());
}

fn finish(result: Result<EgclVal, EgclError>, out: *mut OrdinaryHelperOutcome) {
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            stash_native_error(error);
            NIL
        }
    };
    let exit = if native_error_pending() {
        NativeExit::Transfer
    } else {
        NativeExit::Returned
    };
    if exit != NativeExit::Returned {
        let context = CAPTURE.with(Cell::get);
        if !context.is_null() {
            let context = unsafe { &mut *context };
            assert!(
                unsafe { &*context.dynamic_scopes }.is_empty(),
                "ordinary segment escape only admits scope-free frames"
            );
            native_frames::retire_children(context);
            context.recursive_escape = true;
        }
    }
    let values = if exit == NativeExit::Returned {
        let env = NATIVE_ENV.with(Cell::get);
        assert!(!env.is_null());
        unsafe { (*env).native_return_values() }
    } else {
        NativeReturnValues::SINGLE
    };
    // All helper frames and root guards return before assembly consumes this
    // borrowed tuple or jumps to the current segment's universal landing.
    unsafe {
        out.write(OrdinaryHelperOutcome {
            outcome: NativeOutcome { value, exit },
            values,
            anchor: native_transfer::current_segment(),
        });
    }
}

/// Complete a failed ordinary admission through the current named-call cell.
/// The caller maps own args; only the separate closure word is host-rooted
/// here. The copied argument vector has distinct physical root locations.
pub(super) unsafe extern "C" fn cold_call(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    #[cfg(test)]
    COLD_CALLS.with(|count| count.set(count.get() + 1));
    let request = request.cast::<OrdinaryColdRequest>();
    egcl_rt::rooted_ref!(_closure = unsafe { &mut *std::ptr::addr_of_mut!((*request).closure) });
    unsafe {
        root_publication::published(image, || {
            let result = guard_c2i(|| {
                if native_error_pending() {
                    return Ok(NIL);
                }
                egcl_rt::rooted!(args = Vec::new());
                let count = (*request).nargs;
                args.try_reserve_exact(count).map_err(|_| EgclError::Oom)?;
                if count != 0 {
                    args.extend_from_slice(std::slice::from_raw_parts((*request).args, count));
                }
                let mut poll = NativeOutcome {
                    value: NIL,
                    exit: NativeExit::Returned,
                };
                poll_or_transfer_unpublished(std::ptr::null_mut(), &mut poll);
                if poll.exit != NativeExit::Returned {
                    return Ok(NIL);
                }
                super::super::call_table::ordinary_call((*request).symbol as u32, &args)
            });
            refresh_control((*request).control);
            finish(result, out);
        });
    }
}

/// Signal only after all actual incoming arguments and closure are rooted.
/// The callee has no frame; publication starts at the captured caller or ends
/// immediately at the root shim boundary.
pub(super) unsafe extern "C" fn wrong_arity(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    let request = request.cast::<OrdinaryArityRequest>();
    unsafe {
        with_arity_roots(request, || {
            root_publication::published(image, || {
                let result = guard_c2i(|| {
                    if native_error_pending() {
                        return Ok(NIL);
                    }
                    #[cfg(test)]
                    ARITY_ROOTED_OBSERVER.with(|observe| {
                        if let Some(observe) = observe.get() {
                            observe(request);
                        }
                    });
                    egcl_rt::rooted!(args = Vec::new());
                    let count = (*request).nargs;
                    args.try_reserve_exact(count).map_err(|_| EgclError::Oom)?;
                    args.extend_from_slice(&(&(*request).register_args)[..count.min(3)]);
                    if count > 3 {
                        args.extend_from_slice(std::slice::from_raw_parts(
                            (*request).overflow_args,
                            count - 3,
                        ));
                    }
                    Err(EgclError::ProgramError(format!(
                        "wrong number of arguments: expected {}, got {}",
                        (*request).expected_arity,
                        args.len()
                    )))
                });
                finish(result, out);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::Collector;

    #[test]
    fn arity_incoming_roots_relocate_every_actual_and_closure_before_copying() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(values = Vec::new());
        for index in 0..8 {
            values.push(crate::cli::arena_cons(EgclVal::from_fixnum(index), NIL));
        }
        let before: Vec<_> = values.iter().map(|value| value.to_raw()).collect();
        let mut overflow = [values[3], values[4], values[5], values[6]];
        let mut request = OrdinaryArityRequest {
            closure: values[7],
            nargs: 7,
            register_args: [values[0], values[1], values[2]],
            overflow_args: overflow.as_mut_ptr(),
            expected_arity: 1,
            reserved: 0,
        };
        values.clear(); // the incoming view now owns every live Lisp root
        let request = std::ptr::from_mut(&mut request);
        unsafe {
            with_arity_roots(request, || {
                egcl_rt::HeapCollector::new().minor_gc().unwrap();
                let request = &*request;
                let mut moved = false;
                for (index, value) in request
                    .register_args
                    .iter()
                    .chain(overflow.iter())
                    .chain(std::iter::once(&request.closure))
                    .enumerate()
                {
                    // Stress collections during construction can already
                    // promote earlier conses; every value must remain valid.
                    moved |= value.to_raw() != before[index];
                    assert_eq!(
                        crate::cli::cp(*value),
                        (EgclVal::from_fixnum(index as i64), NIL)
                    );
                }
                assert!(moved, "exercise actual incoming-root relocation");
            });
        }
    }

    thread_local! {
        static BEFORE: Cell<[u64; 5]> = const { Cell::new([0; 5]) };
        static MOVED: Cell<bool> = const { Cell::new(false) };
    }

    unsafe fn collect_in_arity_helper(request: *mut OrdinaryArityRequest) {
        egcl_rt::HeapCollector::new().minor_gc().unwrap();
        let request = unsafe { &*request };
        assert_eq!(request.nargs, 4);
        assert_eq!(request.expected_arity, 1);
        let values = [
            request.register_args[0],
            request.register_args[1],
            request.register_args[2],
            unsafe { request.overflow_args.read() },
            request.closure,
        ];
        let before = BEFORE.with(Cell::get);
        for (index, value) in values.into_iter().enumerate() {
            assert_eq!(
                crate::cli::cp(value),
                (EgclVal::from_fixnum(index as i64), NIL)
            );
            if value.to_raw() != before[index] {
                MOVED.with(|moved| moved.set(true));
            }
        }
    }

    #[test]
    fn wrong_arity_root_entry_relocates_actuals_before_leaving_its_segment() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(values = Vec::new());
        for index in 0..5 {
            values.push(crate::cli::arena_cons(EgclVal::from_fixnum(index), NIL));
        }
        BEFORE.with(|before| before.set(std::array::from_fn(|index| values[index].to_raw())));
        MOVED.with(|moved| moved.set(false));
        ARITY_ROOTED_OBSERVER.with(|observe| observe.set(Some(collect_in_arity_helper)));
        let code = JitBuffer::new(
            &egcl_compiler::t2::native_transfer::emit_ordinary_arity_veneer(wrong_arity, 1),
        )
        .unwrap();
        use egcl_compiler::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, ValueRepresentation,
        };
        let mut ir = Function::new("wrong-arity-root");
        let entry = ir.entry();
        let arg = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        ir.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![arg],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        let framed = egcl_compiler::t2::emit::emit_framed_ordinary_leaf(
            &ir,
            egcl_compiler::t2::emit::OrdinaryLispEntryOptions {
                arity: 1,
                arity_error_veneer: code.as_ptr() as u64,
            },
        )
        .unwrap();
        let leaf = JitBuffer::new(&framed.code).unwrap();
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        let result = unsafe {
            native_transfer::invoke_ordinary_native_segment(
                leaf.as_ptr().add(framed.ordinary_entry.unwrap()),
                &*execution,
                values[4],
                &values[..4],
                egcl_rt::current_stack(),
            )
        }
        .unwrap();
        ARITY_ROOTED_OBSERVER.with(|observe| observe.set(None));
        assert_eq!(result.outcome.exit, NativeExit::Transfer);
        let error = NATIVE_ERROR
            .with(|slot| slot.take())
            .expect("arity condition");
        assert!(
            matches!(error.without_backtrace(), EgclError::ProgramError(_)),
            "{error:?}"
        );
        assert!(MOVED.with(Cell::get));
        assert!(native_transfer::current_segment().is_null());
    }

    #[test]
    fn ordinary_cold_fallback_keeps_zero_one_and_many_values() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let mut entry = EntryGuard {
            env: NATIVE_ENV.with(|slot| slot.replace(&mut env)),
            capture: CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())),
            error: NATIVE_ERROR.with(|slot| slot.take()),
        };
        egcl_rt::rooted_ref!(_entry = &mut entry);
        let code = JitBuffer::new(
            &egcl_compiler::t2::native_transfer::emit_ordinary_published_veneer(cold_call),
        )
        .unwrap();
        for count in [0, 1, 3] {
            egcl_rt::rooted!(
                args = (0..count)
                    .map(|n| EgclVal::from_fixnum(n + 10))
                    .collect::<Vec<_>>()
            );
            let mut request = egcl_compiler::t2::emit::OrdinaryColdRequest {
                symbol: u64::from(egcl_rt::symbols::intern("VALUES")),
                nargs: args.len(),
                args: args.as_mut_ptr(),
                closure: NIL,
                control: std::ptr::null_mut(),
                descriptor_slot: 0,
                revision_address: 0,
                reserved: 0,
            };
            let result = unsafe {
                native_transfer::invoke_native_segment(
                    code.as_ptr(),
                    std::ptr::from_mut(&mut request).cast(),
                    egcl_rt::current_stack(),
                )
            }
            .unwrap();
            assert_eq!(result.exit, NativeExit::Returned);
            assert_eq!(result.value, args.first().copied().unwrap_or(NIL));
            assert_eq!(env.native_return_values().count, count as usize);
            assert!(!native_error_pending());
        }
    }
}
