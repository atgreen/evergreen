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
pub(in crate::cli::bytecode) unsafe fn with_entry_roots<R>(
    request: *mut OrdinaryArityRequest,
    body: impl FnOnce() -> R,
) -> R {
    let mut roots = ArityRoots(request);
    egcl_rt::rooted_ref!(_roots = &mut roots);
    body()
}

#[cfg(test)]
thread_local! {
    static ARITY_ROOTED_OBSERVER: Cell<Option<unsafe fn(*mut OrdinaryArityRequest)>> = const { Cell::new(None) };
    pub(in crate::cli::bytecode) static RETURN_ROOTED_OBSERVER: Cell<Option<unsafe fn(*mut EgclVal)>> = const { Cell::new(None) };
    // Model a notification arriving after the return poll, before native resume.
    pub(in crate::cli::bytecode) static YIELD_AFTER_RETURN_TO_OWNER: Cell<usize> = const { Cell::new(0) };
}
#[cfg(test)]
static COLD_CALLS: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { Cell::new(0) }) };
#[cfg(test)]
static NAMED_ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { Cell::new(0) }) };
#[cfg(test)]
type SelfColdObserver = Cell<Option<fn()>>;
#[cfg(test)]
pub(in crate::cli::bytecode) static SELF_COLD_OBSERVER: egcl_rt::execution_local::ExecutionLocal<
    SelfColdObserver,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| const { Cell::new(None) }) };
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_named_entries_for_test() -> usize {
    NAMED_ENTRIES.with(|count| count.replace(0))
}

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

/// Return to generated code with notifications serviced and result roots live.
///
/// The caller must retain its `root_publication::published` boundary.
pub(in crate::cli::bytecode) unsafe fn finish(
    result: Result<EgclVal, EgclError>,
    out: *mut OrdinaryHelperOutcome,
) {
    let result = result.and_then(|value| {
        guard_c2i(|| {
            egcl_rt::rooted!(value = value);
            if !native_error_pending() && egcl_rt::thread::current_execution_poll_pending() {
                #[cfg(test)]
                if let Some(observe) = RETURN_ROOTED_OBSERVER.with(Cell::get) {
                    unsafe {
                        observe(std::ptr::from_mut(&mut *value));
                    }
                }
                let mut poll = NativeOutcome {
                    value: NIL,
                    exit: NativeExit::Returned,
                };
                unsafe {
                    poll_or_transfer_unpublished(std::ptr::null_mut(), &mut poll);
                }
            }
            Ok(*value)
        })
    });
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

/// Common noncollecting prefix for both installed and still-cold adapters.
/// All argument storage remains owned by the suspended machine request.
unsafe fn try_lexical_leaf(
    symbol: u32,
    args: &[EgclVal],
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) -> bool {
    let Some(value) = (unsafe { super::ordinary_closure_context::leaf(symbol, args, image) })
    else {
        return false;
    };
    let env = NATIVE_ENV.with(Cell::get);
    if !env.is_null() {
        unsafe {
            (*env).clear_mv();
        }
    }
    unsafe {
        out.write(OrdinaryHelperOutcome {
            outcome: NativeOutcome {
                value,
                exit: NativeExit::Returned,
            },
            values: NativeReturnValues::SINGLE,
            anchor: native_transfer::current_segment(),
        });
    }
    true
}

/// Complete a failed ordinary admission through a named cell or the exact
/// callable retained by a directly recursive caller's native code owner.
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
    // A private primitive may stay cold: ordinary_call handles it before
    // warming its named cell. Share the exact same lexical success protocol.
    if unsafe {
        (*request).reserved == 0
            && (*request).symbol <= u64::from(u32::MAX)
            && matches!((*request).nargs, 2 | 3)
            && !(*request).args.is_null()
    } {
        let args = unsafe { std::slice::from_raw_parts((*request).args, (*request).nargs) };
        if unsafe { try_lexical_leaf((*request).symbol as u32, args, out, image) } {
            return;
        }
    }
    egcl_rt::rooted_ref!(_closure = unsafe { &mut *std::ptr::addr_of_mut!((*request).closure) });
    unsafe {
        root_publication::published(image, || {
            let result = guard_c2i(|| {
                if native_error_pending() {
                    return Ok(NIL);
                }
                egcl_rt::rooted!(self_callable = NIL);
                match (*request).reserved {
                    0 => {}
                    1 => {
                        let boundary = root_publication::ACTIVE.with(Cell::get);
                        if boundary.is_null() {
                            return Err(invalid_capture());
                        }
                        let boundary = &*boundary;
                        if boundary.owner.is_null() || (*boundary.owner).ordinary_entry.is_none() {
                            return Err(invalid_capture());
                        }
                        // Both root ingress and published callees reserve an
                        // exact identity. Release the directory borrow before
                        // the GC gate can park; resolve before polling can
                        // replace the public definition.
                        if (*boundary.owner).native_callable_slot.is_some() {
                            *self_callable = root_publication::frame_callable(boundary)
                                .ok_or_else(invalid_capture)?;
                        } else {
                            let retained = boundary
                                .directory
                                .callable_for_pc(boundary.cursor.pc)
                                .ok_or_else(invalid_capture)?;
                            retained.with_gc_stable_mutator(|value| *self_callable = *value);
                        }
                        if !egcl_rt::function::is_interpreted_function(*self_callable) {
                            return Err(invalid_capture());
                        }
                    }
                    _ => return Err(invalid_capture()),
                }
                egcl_rt::rooted!(args = Vec::new());
                let count = (*request).nargs;
                args.try_reserve_exact(count).map_err(|_| EgclError::Oom)?;
                if count != 0 {
                    args.extend_from_slice(std::slice::from_raw_parts((*request).args, count));
                }
                #[cfg(test)]
                if (*request).reserved == 1
                    && let Some(observe) = SELF_COLD_OBSERVER.with(Cell::get)
                {
                    observe();
                }
                let mut poll = NativeOutcome {
                    value: NIL,
                    exit: NativeExit::Returned,
                };
                poll_or_transfer_unpublished(std::ptr::null_mut(), &mut poll);
                if poll.exit != NativeExit::Returned {
                    return Ok(NIL);
                }
                if (*request).reserved == 1 {
                    let env = NATIVE_ENV.with(Cell::get);
                    if env.is_null() {
                        return Err(invalid_capture());
                    }
                    apply_function(*self_callable, &args, &mut *env)
                } else {
                    super::super::call_table::ordinary_call((*request).symbol as u32, &args)
                }
            });
            refresh_control((*request).control);
            finish(result, out);
        });
    }
}

/// Ordinary ingress for runtime-bound named targets. The request contains the
/// actual register/overflow arguments, including excess arguments needed for
/// diagnostics. Every source remains a writable root until the copy is rooted.
pub(in crate::cli::bytecode) unsafe extern "C" fn named_entry(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    let request = request.cast::<OrdinaryArityRequest>();
    // The adapter's entry data is its call cell (call_table_adapter::ensure).
    let cell = unsafe { (*request).entry_data } as u64;
    // Lexical adapter actuals all fit the captured register array. A hit
    // cannot collect, signal or poll; every miss uses the original boundary.
    let count = unsafe { (*request).nargs };
    if count <= 3 {
        let symbol = unsafe { super::super::call_table::cell_symbol(cell) };
        let args = unsafe { &(&(*request).register_args)[..count] };
        if unsafe { try_lexical_leaf(symbol, args, out, image) } {
            return;
        }
    }
    #[cfg(test)]
    if !ordinary_primitives::is_private(unsafe { super::super::call_table::cell_symbol(cell) }) {
        NAMED_ENTRIES.with(|count| count.set(count.get() + 1));
    }
    unsafe {
        with_entry_roots(request, || {
            root_publication::published(image, || {
                let execution = egcl_rt::thread::current_execution_context_for_native()
                    .expect("ordinary named adapter owns an execution");
                let control = (*execution).active_native_control() as *mut NativeRecursiveControl;
                let result = guard_c2i(|| {
                    if native_error_pending() {
                        return Ok(NIL);
                    }
                    let count = (*request).nargs;
                    let registers = &(&(*request).register_args)[..count.min(3)];
                    let overflow = if count > 3 {
                        std::slice::from_raw_parts((*request).overflow_args, count - 3)
                    } else {
                        &[]
                    };
                    // Common arities need no heap buffer (GitHub #487).
                    egcl_rt::rooted!(args = EntryArgs::from_parts(registers, overflow));
                    let mut poll = NativeOutcome {
                        value: NIL,
                        exit: NativeExit::Returned,
                    };
                    poll_or_transfer_unpublished(std::ptr::null_mut(), &mut poll);
                    if poll.exit != NativeExit::Returned {
                        return Ok(NIL);
                    }
                    super::super::call_table::ordinary_call_cell(cell, args.as_slice())
                });
                refresh_control(control);
                finish(result, out);
            });
        });
    }
}

/// Complete a call whose ordinary `&key` entry could not bind its arguments
/// natively: a keyword it has not matched before, `:allow-other-keys`, or an
/// argument error. The entry data is the body the code retains. The call
/// runs through the general binder, which signals any error exactly as
/// before and teaches the entry's binder the keywords used. The installed
/// definition of this body enters its compiled code through the managed
/// activation, which binds before entering; any other callable runs this
/// exact body interpreted.
pub(super) unsafe extern "C" fn keyword_decline(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    let request = request.cast::<OrdinaryArityRequest>();
    unsafe {
        with_entry_roots(request, || {
            root_publication::published(image, || {
                let execution = egcl_rt::thread::current_execution_context_for_native()
                    .expect("ordinary keyword entry owns an execution");
                let control = (*execution).active_native_control() as *mut NativeRecursiveControl;
                let result = guard_c2i(|| {
                    if native_error_pending() {
                        return Ok(NIL);
                    }
                    // The retained code owns this body for as long as its
                    // entry can be called.
                    let body = (*request).entry_data as *const BytecodeFunction;
                    Arc::increment_strong_count(body);
                    let body = Arc::from_raw(body);
                    let count = (*request).nargs;
                    let registers = &(&(*request).register_args)[..count.min(3)];
                    let overflow = if count > 3 {
                        std::slice::from_raw_parts((*request).overflow_args, count - 3)
                    } else {
                        &[]
                    };
                    egcl_rt::rooted!(args = EntryArgs::from_parts(registers, overflow));
                    let mut poll = NativeOutcome {
                        value: NIL,
                        exit: NativeExit::Returned,
                    };
                    poll_or_transfer_unpublished(std::ptr::null_mut(), &mut poll);
                    if poll.exit != NativeExit::Returned {
                        return Ok(NIL);
                    }
                    let env = NATIVE_ENV.with(Cell::get);
                    if env.is_null() {
                        return Err(invalid_capture());
                    }
                    let symbol = super::super::resolve_sym(&body.name)
                        .map(EgclVal::as_symbol_index)
                        .ok_or_else(invalid_capture)?;
                    egcl_rt::rooted!(
                        function = egcl_rt::symbols::symbol_function(symbol).unwrap_or(NIL)
                    );
                    if callable_definition_matches(symbol, *function, Arc::as_ptr(&body)) {
                        super::super::super::apply_function_impl(
                            *function,
                            args.as_slice(),
                            &mut *env,
                        )
                    } else {
                        super::super::run_with_sym(body, args.as_slice(), NIL, symbol, &mut *env)
                    }
                });
                refresh_control(control);
                finish(result, out);
            });
        });
    }
}

/// Finish a precise guard continuation through the ordinary tuple protocol.
pub(super) unsafe extern "C" fn guard(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    unsafe {
        root_publication::published(image, || {
            let result = guard_c2i(|| deopt::resume_ordinary_guard(request));
            if result.is_ok() {
                refresh_control(
                    (*request.cast::<egcl_compiler::t2::emit::OrdinaryGuardRequest>()).control,
                );
            }
            finish(result, out);
        });
    }
}

/// The rejection veneer of an entry that checks declared parameter types.
/// The entry data is the body its code retains. A wrong count signals as
/// `wrong_arity` does; otherwise a declared type failed, and the same
/// validation root ingress runs signals its TYPE-ERROR (bliss-6m6ca).
pub(super) unsafe extern "C" fn wrong_arguments(
    request: *mut u8,
    out: *mut OrdinaryHelperOutcome,
    image: *mut SysvTransferCapture,
) {
    let request = request.cast::<OrdinaryArityRequest>();
    unsafe {
        with_entry_roots(request, || {
            root_publication::published(image, || {
                let result = guard_c2i(|| {
                    if native_error_pending() {
                        return Ok(NIL);
                    }
                    let body = &*((*request).entry_data as *const BytecodeFunction);
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
                    if !super::super::arity_accepts(body, args.len()) {
                        let (min, max) = (usize::from(body.min_args), usize::from(body.arity));
                        return Err(EgclError::ProgramError(if min == max {
                            format!(
                                "wrong number of arguments: expected {max}, got {}",
                                args.len()
                            )
                        } else {
                            format!(
                                "wrong number of arguments: expected {min} to {max}, got {}",
                                args.len()
                            )
                        }));
                    }
                    validate_declared_args(body, &args)?;
                    // The generated check and the declaration disagree.
                    Err(invalid_capture())
                });
                finish(result, out);
            });
        });
    }
}

/// The arity veneer's entry data for an entry accepting `min..=max`
/// arguments: the count itself for a fixed entry, as before.
pub(super) fn expected_arguments(min: u16, max: u16) -> usize {
    if min == max {
        usize::from(max)
    } else {
        (usize::from(min) + 1) << 32 | usize::from(max)
    }
}

fn expected_range(data: usize) -> (usize, usize) {
    let max = data & 0xffff_ffff;
    match data >> 32 {
        0 => (max, max),
        min => (min - 1, max),
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
        with_entry_roots(request, || {
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
                    let (min, max) = expected_range((*request).entry_data);
                    Err(EgclError::ProgramError(if min == max {
                        format!(
                            "wrong number of arguments: expected {max}, got {}",
                            args.len()
                        )
                    } else {
                        format!(
                            "wrong number of arguments: expected {min} to {max}, got {}",
                            args.len()
                        )
                    }))
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
            entry_data: 1,
            reserved: 0,
        };
        values.clear(); // the incoming view now owns every live Lisp root
        let request = std::ptr::from_mut(&mut request);
        unsafe {
            with_entry_roots(request, || {
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
        assert_eq!(request.entry_data, 1);
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
                require_callable_home: false,
                arity: 1,
                arity_error_veneer: code.as_ptr() as u64,
                binding: None,
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

    #[repr(C)]
    struct SevenActualInvocation {
        entry: *const u8,
        execution: *const u8,
        closure: EgclVal,
        arguments: [EgclVal; 7],
    }

    // Test-only outer entry: the production ordinary adapter remains limited
    // to four actuals. Four overflow words precede private anchor homes. The
    // generic segment shim saves/restores all preserved registers and FP state.
    #[unsafe(naked)]
    unsafe extern "C" fn enter_seven_actuals() {
        core::arch::naked_asm!(
            "endbr64",
            "sub rsp, 56",
            "mov rax, [rdx + {sp}]", "mov [rsp + 32], rax",
            "mov rax, [rdx + {landing}]", "mov [rsp + 40], rax",
            "mov [rsp + 48], rdx",
            "mov [rdx + {sp}], rsp",
            "lea rax, [rip + 2f]", "mov [rdx + {landing}], rax",
            "mov r10, rdi", "mov r11, [r10 + {entry}]",
            "mov r13, [r10 + {execution}]",
            "mov rax, [r10 + {args} + 24]", "mov [rsp], rax",
            "mov rax, [r10 + {args} + 32]", "mov [rsp + 8], rax",
            "mov rax, [r10 + {args} + 40]", "mov [rsp + 16], rax",
            "mov rax, [r10 + {args} + 48]", "mov [rsp + 24], rax",
            "mov rax, [r10 + {closure}]", "mov ecx, 7",
            "mov rdi, [r10 + {args}]", "mov rsi, [r10 + {args} + 8]",
            "mov rdx, [r10 + {args} + 16]", "call r11",
            // Generic NativeSegment::return_pc requires exactly these two
            // bytes between the CALL return address and its cold landing.
            "xor edx, edx",
            "2:", "endbr64",
            // Only generated frames are discarded. Restore the original
            // anchor and enter its existing epilogue with RAX/RDX intact.
            "mov r10, [rsp + 48]", "mov r11, [rsp + 40]",
            "mov rcx, [rsp + 32]",
            "mov [r10 + {sp}], rcx", "mov [r10 + {landing}], r11",
            "mov rsp, rcx", "jmp r11",
            sp = const std::mem::offset_of!(native_transfer::NativeSegment, saved_sp),
            landing = const std::mem::offset_of!(native_transfer::NativeSegment, landing_pc),
            entry = const std::mem::offset_of!(SevenActualInvocation, entry),
            execution = const std::mem::offset_of!(SevenActualInvocation, execution),
            closure = const std::mem::offset_of!(SevenActualInvocation, closure),
            args = const std::mem::offset_of!(SevenActualInvocation, arguments),
        );
    }

    thread_local! {
        static OVERFLOW_BEFORE: Cell<[u64; 8]> = const { Cell::new([0; 8]) };
        static OVERFLOW_OBSERVED: Cell<bool> = const { Cell::new(false) };
    }

    unsafe fn collect_seven_in_arity_helper(request: *mut OrdinaryArityRequest) {
        assert_eq!(unsafe { (*request).nargs }, 7);
        assert_eq!(unsafe { (*request).entry_data }, 1);
        let anchor = native_transfer::current_segment();
        assert!(!anchor.is_null());
        assert_eq!(unsafe { (*request).overflow_args } as usize, unsafe {
            (*anchor).saved_sp
        });
        let before = OVERFLOW_BEFORE.with(Cell::get);
        // Stress allocation may already age these objects into Survivor.
        egcl_rt::HeapCollector::new().full_gc().unwrap();
        let request = unsafe { &*request };
        for (index, value) in request
            .register_args
            .iter()
            .chain(unsafe { std::slice::from_raw_parts(request.overflow_args, 4) })
            .chain(std::iter::once(&request.closure))
            .enumerate()
        {
            assert_ne!(
                value.to_raw(),
                before[index],
                "incoming home {index} must relocate"
            );
            assert_eq!(
                crate::cli::cp(*value),
                (EgclVal::from_fixnum(index as i64), NIL)
            );
        }
        OVERFLOW_OBSERVED.with(|observed| observed.set(true));
    }

    #[test]
    fn wrong_arity_seven_actual_machine_entry_relocates_all_incoming_homes() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(values = Vec::new());
        for index in 0..8 {
            values.push(crate::cli::arena_cons(EgclVal::from_fixnum(index), NIL));
        }
        OVERFLOW_BEFORE
            .with(|before| before.set(std::array::from_fn(|index| values[index].to_raw())));
        OVERFLOW_OBSERVED.with(|moved| moved.set(false));
        struct RestoreObserver(Option<unsafe fn(*mut OrdinaryArityRequest)>);
        impl Drop for RestoreObserver {
            fn drop(&mut self) {
                ARITY_ROOTED_OBSERVER.with(|observe| observe.set(self.0));
            }
        }
        let _observer = RestoreObserver(
            ARITY_ROOTED_OBSERVER
                .with(|observe| observe.replace(Some(collect_seven_in_arity_helper))),
        );
        let code = JitBuffer::new(
            &egcl_compiler::t2::native_transfer::emit_ordinary_arity_veneer(wrong_arity, 1),
        )
        .unwrap();
        use egcl_compiler::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, ValueRepresentation,
        };
        let mut ir = Function::new("wrong-arity-seven-actuals");
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
                require_callable_home: false,
                arity: 1,
                arity_error_veneer: code.as_ptr() as u64,
                binding: None,
            },
        )
        .unwrap();
        let leaf = JitBuffer::new(&framed.code).unwrap();
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        // These comparison roots keep the objects live even in the negative
        // mutation; every incoming machine home must independently be repaired.
        let mut request = SevenActualInvocation {
            entry: unsafe { leaf.as_ptr().add(framed.ordinary_entry.unwrap()) },
            execution: execution.cast(),
            closure: values[7],
            arguments: std::array::from_fn(|index| values[index]),
        };
        let result = unsafe {
            native_transfer::invoke_native_segment(
                enter_seven_actuals as *const u8,
                std::ptr::from_mut(&mut request).cast(),
                egcl_rt::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(result.exit, NativeExit::Transfer);
        let error = NATIVE_ERROR
            .with(|slot| slot.take())
            .expect("arity condition");
        assert!(
            matches!(error.without_backtrace(), EgclError::ProgramError(message)
                if message == "wrong number of arguments: expected 1, got 7"),
            "{error:?}"
        );
        assert!(OVERFLOW_OBSERVED.with(Cell::get));
        assert!(native_transfer::current_segment().is_null());
    }

    #[test]
    fn ordinary_cold_self_requires_a_retained_native_owner() {
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
        for reserved in [1, 2] {
            let mut request = OrdinaryColdRequest {
                symbol: u64::from(egcl_rt::symbols::intern("VALUES")),
                nargs: 0,
                args: std::ptr::null_mut(),
                closure: NIL,
                control: std::ptr::null_mut(),
                descriptor_slot: 0,
                revision_address: 0,
                reserved,
            };
            let result = unsafe {
                native_transfer::invoke_native_segment(
                    code.as_ptr(),
                    std::ptr::from_mut(&mut request).cast(),
                    egcl_rt::current_stack(),
                )
            }
            .unwrap();
            assert_eq!(
                result.exit,
                NativeExit::Transfer,
                "self without an owner and unknown flags must not resolve the global VALUES cell"
            );
            assert!(NATIVE_ERROR.with(|slot| slot.take()).is_some());
        }
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
