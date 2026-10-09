// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Publication of a suspended native caller before a Rust helper may collect.
//!
//! A published helper veneer captures the exact return PC, the pre-CALL RSP and
//! writable save words for the six SysV preserved registers into a
//! `SysvTransferCapture` image before entering Rust. `published` links that
//! image into an execution-local chain for the helper's dynamic extent so the
//! collector can walk every suspended native frame of every execution under
//! stop-the-world, through the exact per-site maps the code owner retains.
//!
//! The chain is constant-sized bookkeeping on the helper's own stack: nothing
//! is walked on an ordinary helper entry. The full walk happens only during a
//! collection. Managed shadow-root slots remain authoritative; this walk is
//! additive until complete coverage is verified (bliss-shih7.2.7.3).

use super::*;
use egcl_compiler::t2::x64_calls::NativeCallValues;
use egcl_compiler::t2::x64_unwind::{NativeFrameCursor, SYSV_PRESERVED_REGISTERS};
use egcl_compiler::t2::x64_value_maps::{NativeFrameValues, NativeValueLocation};
use egcl_rt::native_transfer::NativeSegment;
use std::ops::Range;
use std::sync::Once;

/// One suspended native caller, valid for the dynamic extent of the helper
/// that published it. `owner` is frozen at machine-helper entry, before the
/// logical CAPTURE context can change; `segment` is the active native anchor
/// whose `saved_sp`/`return_pc` end the physical walk.
pub(super) struct PublishedBoundary {
    pub(super) owner: *const TransferCode,
    pub(super) cursor: NativeFrameCursor,
    /// Actual native stack ownership: from the capture image up to the segment
    /// entry. The six save words live below the caller's `call_sp`.
    pub(super) bounds: Range<usize>,
    pub(super) segment: *mut NativeSegment,
    pub(super) previous: *mut PublishedBoundary,
    /// Root slots the collector reached through this boundary's own frames.
    #[cfg(test)]
    pub(super) visited: Cell<usize>,
}

// SAFETY: only the owning execution links or unlinks its chain; the collector
// reads it with every mutator stopped. A boundary lives on its helper's stack
// and is unlinked before that frame returns, so no dangling link survives.
pub(super) static ACTIVE: egcl_rt::execution_local::ExecutionLocal<Cell<*mut PublishedBoundary>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };

/// Test hook fired inside a published poll, after linking and before the real
/// safepoint, so a test can reenter Lisp or force a collection at that point.
#[cfg(test)]
type ObserveHook = Option<fn()>;
#[cfg(test)]
pub(super) static OBSERVE: egcl_rt::execution_local::ExecutionLocal<Cell<ObserveHook>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

#[cfg(test)]
pub(super) fn observe() {
    if let Some(hook) = OBSERVE.with(Cell::get) {
        hook();
    }
}

/// Restore the enclosing publication on every exit, including unwinding.
struct Linked(*mut PublishedBoundary);

impl Drop for Linked {
    fn drop(&mut self) {
        let previous = unsafe { (*self.0).previous };
        let current = ACTIVE.with(|slot| slot.replace(previous));
        debug_assert_eq!(current, self.0, "publications must unlink in LIFO order");
    }
}

/// Run `body` with the captured caller published. Publish before any poll or
/// allocation; the image's save words are the writable homes the collector
/// relocates through, and the veneer reloads them after `body` returns.
///
/// # Safety
/// `image` must be the live capture the published veneer built for this
/// helper invocation, and the caller must not hold a Rust reference to it
/// across writes through the overlapping outcome pointer.
pub(super) unsafe fn published<R>(
    image: *mut egcl_compiler::t2::native_transfer::SysvTransferCapture,
    body: impl FnOnce() -> R,
) -> R {
    static SCANNER: Once = Once::new();
    SCANNER.call_once(|| egcl_rt::gc::register_root_scanner(scan_published_roots));
    // Copy the geometry out, then let the reference go before `body` writes
    // through the aliasing outcome pointer.
    let (cursor, context, segment) = unsafe {
        let image_ref = &*image;
        let registers = std::array::from_fn(|index| {
            Some(std::ptr::addr_of!(image_ref.preserved[index]) as usize)
        });
        let cursor = NativeFrameCursor {
            pc: image_ref.return_pc as usize,
            call_sp: image_ref.caller_sp as usize,
            registers,
        };
        (cursor, CAPTURE.with(Cell::get), native_transfer::current_segment())
    };
    debug_assert!(!context.is_null(), "helpers run inside a captured activation");
    debug_assert!(!segment.is_null(), "helpers run inside an active native segment");
    let owner = if context.is_null() { std::ptr::null() } else { unsafe { (*context).owner } };
    let saved_sp = if segment.is_null() { 0 } else { unsafe { (*segment).saved_sp } };
    let mut boundary = PublishedBoundary {
        owner,
        cursor,
        bounds: (image as usize)..saved_sp,
        segment,
        previous: ACTIVE.with(Cell::get),
        #[cfg(test)]
        visited: Cell::new(0),
    };
    debug_assert!(
        boundary.bounds.start < cursor.call_sp && cursor.call_sp <= boundary.bounds.end,
        "capture image, caller RSP and segment entry must nest"
    );
    ACTIVE.with(|slot| slot.set(&mut boundary));
    let _linked = Linked(&mut boundary);
    body()
}

/// Walk every execution's chain under stop-the-world. Each boundary unwinds
/// through its owner's exact recipes until the segment entry; frames whose
/// roots T0 already owns (deoptimizing or retired) are skipped, and a malformed
/// or missing map ends that chain rather than guessing.
fn scan_published_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    unsafe {
        ACTIVE.scan(|slot| {
            let mut boundary = slot.get();
            while !boundary.is_null() {
                walk_boundary(&*boundary, visit);
                boundary = (*boundary).previous;
            }
        });
    }
}

unsafe fn walk_boundary(boundary: &PublishedBoundary, visit: &mut dyn FnMut(*mut EgclVal)) {
    if boundary.owner.is_null() || boundary.segment.is_null() {
        return;
    }
    let code = unsafe { &*boundary.owner };
    let base = code.code.as_ptr() as usize;
    let segment = unsafe { &*boundary.segment };
    let bounds = boundary.bounds.clone();
    let read_word = |address: usize| {
        (address % 8 == 0 && address >= bounds.start && address.checked_add(8)? <= bounds.end)
            .then(|| unsafe { (address as *const usize).read() })
    };
    let mut cursor = boundary.cursor;
    loop {
        let Some(offset) = cursor.pc.checked_sub(base).filter(|offset| *offset < code.code_len)
        else {
            return;
        };
        let Some(step) = code._native_calls.unwind(base, &cursor, bounds.clone(), read_word)
        else {
            return;
        };
        match code._native_calls.value_map(offset) {
            Some(NativeCallValues::Frame(map)) => {
                let visited = unsafe { visit_frame(map, &cursor, step.body_sp, visit) };
                #[cfg(test)]
                boundary.visited.set(boundary.visited.get() + visited);
                #[cfg(not(test))]
                let _ = visited;
            }
            // T0 owns these roots while the native frame is being replaced.
            Some(NativeCallValues::Deoptimizing { .. } | NativeCallValues::Retired) => {}
            Some(NativeCallValues::Unavailable) | None => return,
        }
        if step.caller.call_sp == segment.saved_sp && step.caller.pc == segment.return_pc() {
            return;
        }
        cursor = step.caller;
    }
}

/// Visit every writable heap-referencing copy at one suspended call and return
/// how many slots were visited. Register copies resolve through the cursor's
/// inherited save-word addresses; stack copies are body-RSP relative;
/// activation copies are the managed shadow slots the stack scan also visits,
/// so relocation stays idempotent.
unsafe fn visit_frame(
    map: &NativeFrameValues,
    cursor: &NativeFrameCursor,
    body_sp: Option<usize>,
    visit: &mut dyn FnMut(*mut EgclVal),
) -> usize {
    let layout = map.layout();
    let activation = match (layout.activation_base_slot, body_sp) {
        (Some(slot), Some(body_sp)) => {
            let home = body_sp.wrapping_add(slot as usize * 8) as *const *mut EgclVal;
            let activation = unsafe { home.read() };
            (!activation.is_null() && activation as usize % 8 == 0).then_some(activation)
        }
        _ => None,
    };
    let mut visited = 0;
    for location in map.gc_locations() {
        let slot = match *location {
            NativeValueLocation::Activation(slot) => match activation {
                Some(activation) if slot < layout.activation_slots => {
                    unsafe { activation.add(slot as usize) }
                }
                _ => continue,
            },
            NativeValueLocation::Stack(offset) => match body_sp {
                Some(body_sp) => body_sp.wrapping_add_signed(offset as isize) as *mut EgclVal,
                None => continue,
            },
            NativeValueLocation::Register(register) => {
                let Some(index) = SYSV_PRESERVED_REGISTERS.iter().position(|&r| r == register)
                else {
                    continue;
                };
                match cursor.registers[index] {
                    Some(address) => address as *mut EgclVal,
                    None => continue,
                }
            }
            NativeValueLocation::Constant(_) | NativeValueLocation::Unavailable => continue,
        };
        visited += 1;
        visit(slot);
    }
    visited
}

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::Collector;

    struct Probe {
        code: *const TransferCode,
        env: *mut Env,
        args: *const Vec<EgclVal>,
        calls: usize,
        moved: bool,
        /// Root slots reached through the published chain by the nested collection.
        visited: usize,
        /// Same-owner native frames the walk crossed before the segment entry.
        frames: usize,
    }
    thread_local! { static PROBE: Cell<*mut Probe> = const { Cell::new(std::ptr::null_mut()) }; }

    fn probe() -> &'static mut Probe {
        unsafe { &mut *PROBE.with(Cell::get) }
    }

    fn read_stack_word(address: usize) -> Option<usize> {
        Some(unsafe { (address as *const usize).read() })
    }

    /// Slots visited along the whole chain starting at `boundary`.
    fn chain_visited(mut boundary: *mut PublishedBoundary) -> usize {
        let mut total = 0;
        while !boundary.is_null() {
            total += unsafe { (*boundary).visited.get() };
            boundary = unsafe { (*boundary).previous };
        }
        total
    }

    fn force_minor_gc() {
        egcl_rt::HeapCollector::new().minor_gc().unwrap();
    }

    fn observe() {
        let probe = probe();
        let publication = ACTIVE.with(Cell::get);
        assert!(!publication.is_null(), "publish before the helper can collect");
        let published = unsafe { &*publication };
        let code = unsafe { &*published.owner };
        let offset = published.cursor.pc - code.code.as_ptr() as usize;
        let step = code._native_calls.unwind(code.code.as_ptr() as usize,
            &published.cursor, published.bounds.clone(), read_stack_word).unwrap();
        assert_eq!(step.caller.call_sp, unsafe { (*published.segment).saved_sp });
        assert_eq!(step.caller.pc, unsafe { (*published.segment).return_pc() });
        probe.calls += 1;
        if probe.calls == 1 {
            assert!(matches!(code._native_calls.value_map(offset),
                Some(egcl_compiler::t2::x64_calls::NativeCallValues::Frame(_))));
            // Rust reenters another generated segment while this publication
            // remains live. The inner poll must link to this physical caller.
            let previous_observer = OBSERVE.with(|slot| slot.replace(Some(observe_inner)));
            let value = unsafe { (&*probe.code).run(&*probe.args, &mut *probe.env) }.unwrap();
            OBSERVE.with(|slot| slot.set(previous_observer));
            assert_eq!(value, unsafe { (&*probe.args)[0] });
            assert_eq!(ACTIVE.with(Cell::get), publication, "restore outer publication after reentry");
        }
    }

    fn observe_inner() {
        let current = ACTIVE.with(Cell::get);
        let publication = unsafe { &*current };
        assert!(!publication.previous.is_null());
        assert_ne!(publication.segment, unsafe { (*publication.previous).segment });
        let probe = probe();
        let before = unsafe { (&*probe.args)[0] };
        force_minor_gc();
        probe.visited += chain_visited(current);
        let after = unsafe { (&*probe.args)[0] };
        probe.moved |= before != after;
    }

    #[test]
    fn native_helper_publication_survives_rust_lisp_reentry_and_gc() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(
            "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again))) x))"
        ).unwrap().0);
        let body = Arc::new(compile_function("PUBLICATION-REENTRY", *params, *forms, &env, false, false).unwrap());
        let code = TransferCode::compile(body).unwrap();
        egcl_rt::rooted!(args = vec![crate::cli::arena_cons(T, NIL)]);
        let mut probe = Probe { code: &code, env: &mut env, args: &*args, calls: 0, moved: false, visited: 0, frames: 0 };
        PROBE.with(|slot| slot.set(&mut probe));
        assert!(ACTIVE.with(Cell::get).is_null());
        OBSERVE.with(|slot| slot.set(Some(observe)));
        let value = code.run(&args, &mut env).unwrap();
        OBSERVE.with(|slot| slot.set(None));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
        assert!(ACTIVE.with(Cell::get).is_null(), "retire publication before machine storage disappears");
        assert!(probe.calls > 0, "the compiled loop must enter a published poll");
        assert!(probe.moved, "the nested collection must relocate a live input");
        assert!(probe.visited > 0, "the collection must reach roots through the published chain");
        assert_eq!(value, args[0]);
    }

    const RECURSION_DEPTH: usize = 6;

    /// At each recursive preparation, every enclosing activation is a native
    /// frame of the same owner. The walk must cross each one through an exact
    /// map and stop only at the segment entry; the deepest preparation crosses
    /// one frame per activation.
    fn observe_recursion() {
        let probe = probe();
        let publication = ACTIVE.with(Cell::get);
        let published = unsafe { &*publication };
        assert!(published.previous.is_null(), "native self-calls publish no Rust frame");
        let code = unsafe { &*published.owner };
        let base = code.code.as_ptr() as usize;
        let segment = unsafe { &*published.segment };
        let mut cursor = published.cursor;
        let mut frames = 0;
        loop {
            let offset = cursor.pc - base;
            assert!(offset < code.code_len, "every crossed PC stays inside the owner");
            assert!(matches!(code._native_calls.value_map(offset),
                Some(egcl_compiler::t2::x64_calls::NativeCallValues::Frame(_))),
                "exact value map at crossed frame {frames}");
            let step = code._native_calls.unwind(base, &cursor, published.bounds.clone(), read_stack_word)
                .unwrap_or_else(|| panic!("exact unwind recipe at crossed frame {frames}"));
            frames += 1;
            if step.caller.call_sp == segment.saved_sp && step.caller.pc == segment.return_pc() {
                break;
            }
            assert!(frames <= RECURSION_DEPTH + 1, "the walk must end at the segment entry");
            cursor = step.caller;
        }
        probe.frames = probe.frames.max(frames);
        force_minor_gc();
        probe.visited += chain_visited(publication);
        probe.calls += 1;
    }

    #[test]
    fn published_chain_crosses_direct_recursion_to_the_segment_entry() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let name = "PUBLICATION-RECURSION";
        // The base case stays scope-free so direct recursion is admitted; the
        // published recursive preparation is where the hook observes the chain.
        let form = "(if (= n 0) x (progn (PUBLICATION-RECURSION (- n 1) x) (car x)))";
        crate::cli::read_eval_all_env(&format!("(defun {name} (n x) {form})"), &mut env).unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
        let symbol = crate::cli::resolve_sym(name).unwrap().as_symbol_index();
        crate::cli::bytecode::registry_put(symbol, Arc::clone(&body));
        let code = TransferCode::compile(Arc::clone(&body)).expect("admit recursive source body");
        egcl_rt::rooted!(args = vec![
            EgclVal::from_fixnum(RECURSION_DEPTH as i64),
            crate::cli::arena_cons(EgclVal::from_fixnum(42), NIL),
        ]);
        let old_pointer = args[1].to_raw();
        let mut probe = Probe { code: &code, env: &mut env, args: &*args, calls: 0, moved: false, visited: 0, frames: 0 };
        PROBE.with(|slot| slot.set(&mut probe));
        take_recursive_entries();
        OBSERVE.with(|slot| slot.set(Some(observe_recursion)));
        let value = code.run(&args, &mut env);
        OBSERVE.with(|slot| slot.set(None));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
        assert_eq!(take_recursive_entries(), RECURSION_DEPTH, "all recursive calls entered native code");
        assert!(ACTIVE.with(Cell::get).is_null());
        assert_eq!(probe.calls, RECURSION_DEPTH, "every recursive call runs its published preparation");
        assert_eq!(probe.frames, RECURSION_DEPTH, "one native frame per preparing activation, then the segment");
        assert!(probe.visited > 0, "the collection must reach roots through the recursive chain");
        assert_ne!(args[1].to_raw(), old_pointer, "the suspended callers' heap value actually moved");
        assert_eq!(crate::cli::cp(args[1]), (EgclVal::from_fixnum(42), NIL));
        assert_eq!(value.unwrap(), EgclVal::from_fixnum(42));
    }

    /// A publication the walker cannot trust must end its chain without
    /// dereferencing anything, while the real chain keeps working.
    fn observe_malformed() {
        let probe = probe();
        let real = ACTIVE.with(Cell::get);
        let published = unsafe { &*real };
        let code = unsafe { &*published.owner };
        let malformed = |owner, pc, bounds| PublishedBoundary {
            owner,
            cursor: NativeFrameCursor { pc, ..published.cursor },
            bounds,
            segment: published.segment,
            previous: std::ptr::null_mut(),
            visited: Cell::new(0),
        };
        let past_the_code = code.code.as_ptr() as usize + code.code_len;
        let mut cases = [
            ("PC outside the owner", malformed(published.owner, past_the_code, published.bounds.clone())),
            ("empty stack bounds", malformed(published.owner, published.cursor.pc, 0..0)),
            ("no owner", malformed(std::ptr::null(), published.cursor.pc, published.bounds.clone())),
        ];
        for (label, boundary) in &mut cases {
            ACTIVE.with(|slot| slot.set(boundary));
            force_minor_gc();
            ACTIVE.with(|slot| slot.set(real));
            assert_eq!(boundary.visited.get(), 0, "{label}: the walk must stop without guessing");
        }
        force_minor_gc();
        assert!(published.visited.get() > 0, "the real publication still reaches its roots");
        probe.visited += published.visited.get();
        probe.calls += 1;
    }

    #[test]
    fn malformed_publications_end_the_walk_without_dereferencing() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(
            "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again))) x))"
        ).unwrap().0);
        let body = Arc::new(compile_function("PUBLICATION-MALFORMED", *params, *forms, &env, false, false).unwrap());
        let code = TransferCode::compile(body).unwrap();
        egcl_rt::rooted!(args = vec![crate::cli::arena_cons(T, NIL)]);
        let mut probe = Probe { code: &code, env: &mut env, args: &*args, calls: 0, moved: false, visited: 0, frames: 0 };
        PROBE.with(|slot| slot.set(&mut probe));
        OBSERVE.with(|slot| slot.set(Some(observe_malformed)));
        let value = code.run(&args, &mut env).unwrap();
        OBSERVE.with(|slot| slot.set(None));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
        assert!(ACTIVE.with(Cell::get).is_null());
        assert!(probe.calls > 0, "the compiled loop must enter a published poll");
        assert!(probe.visited > 0);
        assert_eq!(value, args[0]);
    }
}
