// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A guard completes its exact bytecode continuation before returning to its
//! native caller. No pre-guard SSA home is read after that continuation runs.

use super::*;
use egcl_compiler::t2::emit::TransferDeoptRequest;

/// T0 owns and pops the current frame while running the continuation. Restore
/// the exact native-owned extent, with empty slots, before its caller retires
/// it. This also bounds any frames left behind by an intercepted Rust panic.
struct RestoreNativeFrame {
    frame: *mut Frame,
    header: Frame,
    sp: *const u8,
}

impl egcl_rt::gc::TraceHostRoots for RestoreNativeFrame {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.header.function);
    }
}

impl Drop for RestoreNativeFrame {
    fn drop(&mut self) {
        let stack = egcl_rt::current_stack();
        while stack.fp() != self.header.prev_fp {
            assert!(
                !stack.fp().is_null(),
                "deopt crossed its owning frame boundary"
            );
            stack.pop_frame();
        }
        // The identical extent was occupied immediately before resumption;
        // this cannot exhaust the stack and does not allocate or safepoint.
        let frame = stack
            .push_frame(
                self.header.function,
                self.header.code_info,
                self.header.num_locals,
                self.header.flags,
            )
            .expect("restore occupied native extent");
        assert_eq!(frame, self.frame);
        assert_eq!(stack.sp(), self.sp);
        unsafe {
            (*frame).return_pc = self.header.return_pc;
        }
    }
}

fn note_segment_deopt(code: &TransferCode) {
    DEOPT_COUNT.fetch_add(1, AtomicOrdering::Relaxed);
    let count = code.deopts.get().saturating_add(1);
    code.deopts.set(count);
    let key = Arc::as_ptr(&code.body) as usize;
    let current = SEGMENT_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&key)
            .and_then(|entry| entry.code.as_ref())
            .is_some_and(|entry| std::ptr::eq(entry.as_ref(), code))
    });
    let installed = code.installed_symbol.is_some_and(|symbol| {
        NATIVE_REGISTRY.with(|registry| {
            registry.borrow().get(&symbol).is_some_and(|native| {
                matches!(&native._storage, NativeCodeStorage::Mapped(mapped)
                if std::ptr::eq(mapped.as_ref(), code))
            })
        })
    });
    if !(current || installed || call_table::owns_mapped(code)) {
        return;
    }
    if count == 1 {
        decay_failed_speculation(
            &code.body,
            None,
            code.deopt_metadata
                .as_ref()
                .map(|m| m.speculations.as_slice()),
        );
    }
    // Recompile only this cached version. Resumed T0 samples the retained
    // original body; unsupported numeric phases then leave their calls generic.
    // An older suspended activation must not retire its replacement.
    if count >= deopt_blacklist_threshold() {
        code.recompile.set(true);
    }
}

pub(super) unsafe extern "C" fn resume_guard(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut egcl_compiler::t2::native_transfer::SysvTransferCapture,
) {
    unsafe {
        super::root_publication::published(image, || resume_guard_unpublished(request, out))
    }
}

unsafe fn resume_guard_unpublished(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &*request.cast::<TransferDeoptRequest>() };
    let context = CAPTURE.with(Cell::get);
    let owner = unsafe { (*context).owner };
    let code = unsafe { &*owner };
    let frame = egcl_rt::current_stack().fp() as *mut Frame;
    let expected = unsafe {
        if (*context).recursive.is_null() {
            (*context).frame
        } else {
            (*(*context).recursive).frame
        }
    };
    let result = guard_c2i(|| {
        if frame.is_null()
            || frame != expected
            || unsafe { frame.add(1).cast::<EgclVal>() } != request.activation
        {
            return Err(invalid_capture());
        }
        let header = unsafe { std::ptr::read(frame) };
        // Accept only this retained code's native metadata. A child header
        // also retains the exact heap callable while T0 owns/pops its frame.
        if !std::ptr::eq(header.code_info, code.code_info) || !header.return_pc.is_null() {
            return Err(invalid_capture());
        }
        // Resumed Lisp owns the frame; it must not borrow the abandoned
        // native capture. Restore that capture only after restoring the frame.
        let _capture_pause = CapturePause(CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())));
        let mut restore = RestoreNativeFrame {
            frame,
            header,
            sp: egcl_rt::current_stack().sp(),
        };
        egcl_rt::rooted_ref!(_restore_root = &mut restore);
        // Materialized T0 locals no longer follow native debugger recipes.
        // T0 scans every tagged slot; the guard restores native metadata when
        // its exact frame extent is recreated after the continuation returns.
        unsafe { (*frame).code_info = std::ptr::null(); }
        let metadata = code.deopt_metadata.clone().ok_or_else(invalid_capture)?;
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        let (scopes, metadata) = materialize_t2_deopt_scopes(
            request.n_scopes,
            request.n_words,
            request.words,
            Some(frame),
            Some(metadata),
        )
        .map_err(|message| EgclError::Internal(message.into()))?;
        note_segment_deopt(code);
        // Every stream word is now in scanned slots. There is no mutable
        // CaptureContext reference or RefCell borrow across resumed Lisp.
        resume_inlined_in_t0(scopes, metadata, None, unsafe { &mut *env })
    });
    // Fiber mounting establishes carrier compatibility before resumed Lisp or
    // Rust executes. Publish the value/transfer without another capability test.
    let outcome = match result {
        Ok(value) => NativeOutcome {
            value,
            exit: NativeExit::Returned,
        },
        Err(error) => {
            stash_native_error(error);
            unsafe {
                (*context).completed_deopt = true;
            }
            NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            }
        }
    };
    unsafe {
        out.write(outcome);
    }
}

/// Called only after the deopt Rust adapter has returned. Its precise state
/// was already consumed; ordinary Invoke capture would replay stale state.
pub(super) unsafe extern "C" fn prepare_completed_deopt(capture: *mut SysvTransferCapture) {
    // Publish the same suspended caller the capture stub imaged. The deopt
    // return PC maps to Deoptimizing, so the walk skips this retired top frame
    // — whose roots T0 already owns — and continues through older ones.
    unsafe {
        super::root_publication::published(capture, || prepare_completed_deopt_unpublished(capture))
    }
}

unsafe fn prepare_completed_deopt_unpublished(capture: *mut SysvTransferCapture) {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    let expected = if context.recursive.is_null() {
        context.frame
    } else {
        unsafe { (*context.recursive).frame }
    };
    let sites = unsafe { &*context.sites };
    if !context.completed_deopt
        || egcl_rt::current_stack().fp() != expected
        || !sites.is_deopt_return(context.code_base, capture.return_pc as usize)
        || capture.exit != NativeExit::Transfer
    {
        context.failure = Some(TransferSiteError::InvalidLandingCapture);
    }
    while !context.recursive.is_null() {
        unsafe {
            retire_recursive(context, context.recursive);
        }
    }
    context.completed_deopt = false;
    context.recursive_escape = true;
    prepare_escape(context);
    capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
}

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::Collector;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CACHE_MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
    static CACHE_ROOTS: std::sync::Mutex<Vec<std::sync::Weak<ActiveBytecodeRoot>>> =
        std::sync::Mutex::new(Vec::new());

    fn cache_fiber() -> EgclVal {
        // Keep an assertion failure inside Rust rather than crossing the
        // scheduler's non-unwinding machine trampoline.
        std::panic::catch_unwind(cache_fiber_checked).unwrap_or(EgclVal::from_fixnum(0))
    }

    fn cache_fiber_checked() -> EgclVal {
        assert!(
            SEGMENT_CACHE.with(|cache| cache.borrow().is_empty()),
            "a new fiber must not inherit another execution's code cache"
        );
        let mut env = Env::new_impl(false, false, false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string("((+ x 1))").unwrap().0);
        let body = Arc::new(
            compile_function(
                "MIGRATING-SEGMENT-CACHE",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let old = cached_code(&body).expect("optimized segment");
        assert!(old.has_deopt);
        for _ in 0..200 {
            let carrier = egcl_rt::current_thread_id();
            egcl_rt::thread::fiber_yield().unwrap();
            if carrier != egcl_rt::current_thread_id() {
                CACHE_MIGRATIONS.fetch_add(1, Ordering::Relaxed);
            }
            let selected = cached_code(&body).unwrap();
            assert!(
                Rc::ptr_eq(&old, &selected),
                "migration changed the retained code version"
            );
            assert_eq!(
                SEGMENT_CACHE.with(|cache| cache.borrow().len()),
                1,
                "other fibers must own separate caches"
            );
        }
        let argument = EgclVal::from_single_float(1.5);
        for _ in 0..deopt_blacklist_threshold() {
            assert_eq!(
                old.run(&[argument], &mut env).unwrap(),
                EgclVal::from_single_float(2.5)
            );
        }
        assert!(
            old.recompile.get(),
            "migrated deopts must update the owning cache version"
        );
        let replacement = cached_code(&body).unwrap();
        assert!(!Rc::ptr_eq(&old, &replacement));
        assert_eq!(
            replacement.run(&[argument], &mut env).unwrap(),
            EgclVal::from_single_float(2.5)
        );
        assert_eq!(replacement.deopt_count(), 0);
        assert_eq!(
            old.run(&[argument], &mut env).unwrap(),
            EgclVal::from_single_float(2.5)
        );
        assert!(
            !replacement.recompile.get(),
            "old code must not retire its replacement"
        );
        // Only atomic weak root tokens cross executions, never the code's Rc.
        CACHE_ROOTS.lock().unwrap().extend([
            Arc::downgrade(&old._body_roots),
            Arc::downgrade(&replacement._body_roots),
        ]);
        EgclVal::from_fixnum(1)
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn native_v2_segment_cache_follows_fiber_and_retires_exact_version() {
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(egcl_rt::native_transfer::is_supported());
        let mut startup = Env::new(false);
        egcl_rt::rooted_ref!(_startup = &mut startup);
        for num_workers in [1, 4] {
            CACHE_MIGRATIONS.store(0, Ordering::Relaxed);
            let group =
                egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers }).unwrap();
            for _ in 0..12 {
                let entry =
                    unsafe { EgclVal::from_function_ptr(cache_fiber as *const () as *mut u8) };
                group
                    .submit(egcl_rt::thread::make_fiber(entry).unwrap())
                    .unwrap();
            }
            assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(1); 12]);
            let roots = std::mem::take(&mut *CACHE_ROOTS.lock().unwrap());
            assert_eq!(roots.len(), 24);
            assert!(
                roots.iter().all(|root| root.upgrade().is_none()),
                "stopped fibers must release cached code and its root ownership"
            );
            if num_workers == 4 {
                assert!(
                    CACHE_MIGRATIONS.load(Ordering::Relaxed) > 0,
                    "probe must actually migrate across workers"
                );
            }
        }
    }

    #[test]
    fn native_v2_saved_frame_header_is_a_moving_root() {
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        // Interpreted callable objects are pinned today. A movable sentinel
        // exercises the saved header's root contract without claiming that a
        // stress run over an immovable callable proves relocation safety.
        let stack = egcl_rt::current_stack();
        let parent = stack.fp();
        let (old_address, frame) = {
            egcl_rt::rooted!(sentinel = super::super::super::super::arena_cons(T, NIL));
            let frame = stack
                .push_frame(*sentinel, std::ptr::null(), 3, FLAG_CALL)
                .unwrap();
            (sentinel.to_raw(), frame)
        };
        {
            let mut restore = RestoreNativeFrame {
                frame,
                header: unsafe { std::ptr::read(frame) },
                sp: stack.sp(),
            };
            egcl_rt::rooted_ref!(_restore_root = &mut restore);
            stack.pop_frame();
            egcl_rt::gc::HeapCollector::new().minor_gc().unwrap();
            assert_ne!(restore.header.function.to_raw(), old_address);
            assert_eq!(
                super::super::super::super::cp(restore.header.function),
                (T, NIL)
            );
        }
        assert_eq!(stack.fp(), frame);
        assert_ne!(unsafe { (*frame).function.to_raw() }, old_address);
        assert_eq!(
            super::super::super::super::cp(unsafe { (*frame).function }),
            (T, NIL)
        );
        stack.pop_frame();
        assert_eq!(stack.fp(), parent);
    }

    #[test]
    fn native_v2_deopt_restores_frame_extent_after_caught_panic() {
        let stack = egcl_rt::current_stack();
        let parent = stack.fp();
        let parent_sp = stack.sp();
        let frame = stack
            .push_frame(NIL, std::ptr::null(), 12, FLAG_CALL)
            .unwrap();
        let sp = stack.sp();
        let result = guard_c2i(|| {
            // Resumed Lisp owns the frame; it must not borrow the abandoned
            // native capture. Restore that capture only after restoring the frame.
            let _capture_pause =
                CapturePause(CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())));
            let mut restore = RestoreNativeFrame {
                frame,
                header: unsafe { std::ptr::read(frame) },
                sp,
            };
            egcl_rt::rooted_ref!(_restore_root = &mut restore);
            // T0 may pop the owned frame, and an OSR/resume path can use a
            // larger extent. Retirement must still see the original extent.
            stack.pop_frame();
            stack
                .push_frame(NIL, std::ptr::null(), 48, FLAG_CALL)
                .unwrap();
            panic!("injected continuation panic");
        });
        assert!(matches!(result, Err(EgclError::Internal(_))));
        assert_eq!(stack.fp(), frame);
        assert_eq!(stack.sp(), sp);
        assert_eq!(unsafe { (*frame).num_locals }, 12);
        assert!(
            unsafe { egcl_rt::EgclStack::frame_slots_mut(frame) }
                .iter()
                .all(|v| v.is_nil())
        );
        stack.pop_frame();
        assert_eq!(stack.fp(), parent);
        assert_eq!(stack.sp(), parent_sp);
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn native_v2_optimized_cache_recompiles_numeric_phase_without_retiring_replacement() {
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::super::read_eval_all_env(
            "(defun segment-phase-touch (x) nil)
             (defun segment-phase (x) (segment-phase-touch x) (+ x 1))",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(
            forms = reader::read_from_string("((segment-phase-touch x) (+ x 1))")
                .unwrap()
                .0
        );
        let symbol = super::super::super::super::resolve_sym("SEGMENT-PHASE")
            .unwrap()
            .as_symbol_index();
        let body = Arc::new(
            compile_function("SEGMENT-PHASE", *params, *forms, &env, false, false).unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        let key = Arc::as_ptr(&body) as usize;
        let old = Rc::new(TransferCode::compile(Arc::clone(&body)).unwrap());
        assert!(old.has_deopt);
        SEGMENT_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .insert(key, SegmentCacheEntry::new(&body, Some(Rc::clone(&old))))
        });
        let argument = EgclVal::from_single_float(1.5);
        for _ in 0..deopt_blacklist_threshold() {
            assert_eq!(
                old.run(&[argument], &mut env).unwrap(),
                EgclVal::from_single_float(2.5)
            );
        }
        assert!(old.recompile.get());
        // Use the actual dispatch-cache replacement path without changing
        // the process-wide opt-in environment in this test.
        let replacement = cached_code(&body).unwrap();
        assert!(!Rc::ptr_eq(&old, &replacement));
        for _ in 0..12 {
            assert_eq!(
                replacement.run(&[argument], &mut env).unwrap(),
                EgclVal::from_single_float(2.5)
            );
        }
        assert_eq!(
            replacement.deopt_count(),
            0,
            "new numeric phase must stop failing old guards"
        );
        assert_eq!(
            old.run(&[argument], &mut env).unwrap(),
            EgclVal::from_single_float(2.5)
        );
        assert!(
            !replacement.recompile.get(),
            "old suspended code cannot retire the replacement"
        );
        SEGMENT_CACHE.with(|cache| cache.borrow_mut().remove(&key));
    }
}
