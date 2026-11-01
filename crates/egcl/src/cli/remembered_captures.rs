// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Remembered function captures for minor collections (bliss-gg3i1.7.9).
//!
//! An interpreted function's captured frames and bytecode body live off the
//! heap, so no heap page records a store into them. A minor collection used to
//! trace the captures of every old interpreted function to find nursery
//! references there, which made its cost grow with the number of live
//! functions rather than with what the program wrote.
//!
//! A minor collection empties the nursery, so off-heap storage last written
//! before the most recent completed minor collection cannot refer into it.
//! This module records the storage written in the current epoch
//! (`egcl_rt::gc::minor_epoch`):
//!
//! - every lexical frame ([`FrameCell`]) that was created or locked for
//!   writing, the first time it happens in the epoch (`FrameCell::borrow`
//!   gives read-only access and records nothing);
//! - every bytecode body published where the collector reaches it through an
//!   old function or the bytecode root registry. Bodies are immutable once
//!   shared, so publication is the only write.
//!
//! [`scan_remembered_captures`] visits exactly those as roots of the minor
//! collection, and the collector skips old functions' captures. When a log has
//! to give up (too many live entries in one epoch), or a collection runs with
//! `EGCL_GC_REMEMBERED_CAPTURES=0`, the collection traces every capture as
//! before. Major collections always do.

use super::EnvFrame;
use egcl_rt::bytecode::BytecodeFunction;
use egcl_rt::gc::ExternalRootScanKind;
use egcl_rt::value::EgclVal;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

/// A shared lexical frame. Creation and every mutable lock record the frame as
/// written in the current minor-collection epoch, so a frame the next minor
/// collection must visit is always in [`FRAMES`]. Collector-side visitors use
/// [`FrameCell::gc_borrow`], which records nothing: relocation stores only
/// older addresses.
///
/// Deliberately no larger than the plain `SharedCell<EnvFrame>` it replaces:
/// frames are numerous, and extra bytes per frame shift the host-memory
/// pressure that schedules major collections (measured on SHOW: 16 extra bytes
/// per frame moved one major collection into a warm compilation). The write
/// record is kept beside the frame, in the log and a per-thread cache.
///
/// A guard must never survive a Lisp allocation, evaluation, or safepoint (see
/// `SharedCell`); that is also what makes recording at lock time equivalent to
/// recording each store.
pub(super) struct FrameCell {
    frame: Mutex<EnvFrame>,
}

impl FrameCell {
    pub(super) fn new_arc(frame: EnvFrame) -> Arc<Self> {
        let cell = Arc::new(FrameCell {
            frame: Mutex::new(frame),
        });
        // Always logged: a new frame may reuse a dead frame's address, so the
        // per-thread cache must not suppress it.
        note_frame(&cell, true);
        cell
    }

    /// Lock for reading. Records nothing: a reader stores no reference.
    #[inline]
    pub(super) fn borrow(&self) -> FrameRead<'_> {
        FrameRead(self.gc_borrow())
    }

    /// Lock without recording a write. Only for collector-side visitors and
    /// read-only snapshots, which never store a nursery reference.
    #[inline]
    pub(super) fn gc_borrow(&self) -> MutexGuard<'_, EnvFrame> {
        self.frame.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Mutable access to a frame. Taken through the owning `Arc` so the write can
/// be recorded without a back-pointer in every frame.
pub(super) trait FrameWrite {
    /// Lock for writing, recording the frame as written in this epoch.
    fn borrow_mut(&self) -> MutexGuard<'_, EnvFrame>;
}

impl FrameWrite for Arc<FrameCell> {
    #[inline]
    fn borrow_mut(&self) -> MutexGuard<'_, EnvFrame> {
        note_frame(self, false);
        self.gc_borrow()
    }
}

/// Frames this thread recorded recently, by address, with the epoch: a frame
/// written repeatedly is logged about once per epoch without a stamp in it.
/// A miss only adds a duplicate entry, which the log drops.
const RECENT_FRAMES: usize = 64;

thread_local! {
    static RECENT: [std::cell::Cell<(usize, u64)>; RECENT_FRAMES] =
        const { [const { std::cell::Cell::new((0, u64::MAX)) }; RECENT_FRAMES] };
}

#[inline]
fn note_frame(frame: &Arc<FrameCell>, created: bool) {
    let epoch = egcl_rt::gc::minor_epoch();
    let address = Arc::as_ptr(frame) as usize;
    let slot = (address >> 4) % RECENT_FRAMES;
    let seen = RECENT.with(|recent| recent[slot].replace((address, epoch)) == (address, epoch));
    if created || !seen {
        FRAMES.note(Arc::downgrade(frame), epoch);
    }
}

/// A read-only frame guard: mutation must go through [`FrameCell::borrow_mut`],
/// which records the write for the next minor collection.
pub(super) struct FrameRead<'a>(MutexGuard<'a, EnvFrame>);

impl std::ops::Deref for FrameRead<'_> {
    type Target = EnvFrame;
    #[inline]
    fn deref(&self) -> &EnvFrame {
        &self.0
    }
}

/// Record that `body` became reachable through a closure-body table or the
/// bytecode root registry in the current epoch.
pub(super) fn note_published_body(body: &Arc<BytecodeFunction>) {
    BODIES.note(Arc::downgrade(body), egcl_rt::gc::minor_epoch());
}

/// Live entries one epoch may hold before a log gives up and the next minor
/// collection traces every capture instead. Dead entries are dropped first,
/// so this bounds the frames that stay live across one epoch, not the frames
/// created in it.
const LOG_HARD_LIMIT: usize = 1 << 20;
/// New entries between checks of the recent tail. Most frames die young, and
/// a dead entry's `Weak` keeps its allocation (not its contents), which counts
/// toward host-pressure major collections, so the tail is checked often. Each
/// entry is checked once in the tail, so the cost per entry does not depend on
/// this; re-reading every logged frame's count instead cost about 1% of a SHOW
/// compilation in cache misses.
const LOG_TAIL: usize = 256;

struct WriteLog<T> {
    state: Mutex<LogState<T>>,
    /// Bytes the entry buffer holds (see [`bookkeeping_bytes`]).
    bytes: std::sync::atomic::AtomicUsize,
}

struct LogState<T> {
    /// (epoch written, storage). Entries from earlier epochs were visited by a
    /// completed minor collection and are discarded lazily.
    entries: Vec<(u64, Weak<T>)>,
    /// Entries before this index were live when last checked.
    settled: usize,
    /// Length at which the whole log is checked again (and deduplicated).
    full_at: usize,
    /// The epoch in which this log gave up, if it has not completed since.
    overflowed: Option<u64>,
}

impl<T> WriteLog<T> {
    const fn new() -> Self {
        Self {
            state: Mutex::new(LogState {
                entries: Vec::new(),
                settled: 0,
                full_at: 4 * LOG_TAIL,
                overflowed: None,
            }),
            bytes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn account(&self, log: &LogState<T>) {
        let bytes = log.entries.capacity() * std::mem::size_of::<(u64, Weak<T>)>();
        self.bytes
            .store(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, LogState<T>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn note(&self, item: Weak<T>, epoch: u64) {
        let mut log = self.lock();
        if log.overflowed == Some(epoch) {
            return;
        }
        if log
            .entries
            .last()
            .is_some_and(|(at, last)| *at == epoch && Weak::ptr_eq(last, &item))
        {
            return;
        }
        let capacity = log.entries.capacity();
        log.entries.push((epoch, item));
        if log.entries.len() - log.settled >= LOG_TAIL {
            log.compact_tail(epoch);
        }
        if log.entries.capacity() != capacity {
            self.account(&log);
        }
    }

    fn overflowed_at(&self, epoch: u64) -> bool {
        self.lock().overflowed == Some(epoch)
    }

    /// Drop entries a completed collection already covered and storage that
    /// has died, then return each distinct live item written in `epoch`.
    fn live_since(&self, epoch: u64) -> Vec<Weak<T>> {
        let mut log = self.lock();
        let mut seen = std::collections::HashSet::with_capacity(log.entries.len());
        let mut live = Vec::with_capacity(log.entries.len());
        log.entries.retain(|(at, item)| {
            if *at < epoch || item.strong_count() == 0 {
                return false;
            }
            if seen.insert(item.as_ptr() as usize) {
                live.push(item.clone());
            }
            true
        });
        if log.overflowed.is_some_and(|at| at < epoch) {
            log.overflowed = None;
        }
        log.settle();
        self.account(&log);
        live
    }

    /// Drop every entry written up to `epoch`, once the collection that
    /// visits them is past failing: their `Weak`s would otherwise keep dead
    /// frames' allocations until the next check.
    fn retire(&self, epoch: u64) {
        let mut log = self.lock();
        log.entries.retain(|(at, _)| *at > epoch);
        log.settle();
        self.account(&log);
    }
}

impl<T> LogState<T> {
    /// Check the entries added since the last check; every so often (as the
    /// log doubles) check and deduplicate the whole log.
    fn compact_tail(&mut self, epoch: u64) {
        if self.entries.len() >= self.full_at {
            let mut seen = std::collections::HashSet::with_capacity(self.entries.len());
            self.entries.retain(|(at, item)| {
                *at >= epoch && item.strong_count() > 0 && seen.insert(item.as_ptr() as usize)
            });
            if self.entries.len() > LOG_HARD_LIMIT {
                self.overflowed = Some(epoch);
                self.entries = Vec::new();
            }
        } else {
            let mut kept = self.settled;
            for index in self.settled..self.entries.len() {
                let (at, item) = &self.entries[index];
                if *at >= epoch && item.strong_count() > 0 {
                    self.entries.swap(kept, index);
                    kept += 1;
                }
            }
            self.entries.truncate(kept);
            self.settled = kept;
            return;
        }
        self.settle();
    }

    /// Everything currently logged counts as checked.
    fn settle(&mut self) {
        let len = self.entries.len();
        self.settled = len;
        self.full_at = (2 * len).max(4 * LOG_TAIL);
        if self.entries.capacity() > 2 * self.full_at {
            self.entries.shrink_to(self.full_at);
        }
    }
}

static FRAMES: WriteLog<FrameCell> = WriteLog::new();
static BODIES: WriteLog<BytecodeFunction> = WriteLog::new();

/// Host bytes held by the write logs. They are bookkeeping that every minor
/// collection empties, not private-closure state a major collection could
/// release, so host-pressure accounting leaves them out: counting them made
/// closure-heavy code request extra major collections (21 -> 24 on a
/// 900k-closure loop; one more landing in a warm SHOW compilation).
pub(super) fn bookkeeping_bytes() -> usize {
    use std::sync::atomic::Ordering::Relaxed;
    FRAMES.bytes.load(Relaxed) + BODIES.bytes.load(Relaxed)
}

/// `EGCL_GC_REMEMBERED_CAPTURES=0` traces every capture on every minor
/// collection, as before bliss-gg3i1.7.9.
fn disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| {
        std::env::var("EGCL_GC_REMEMBERED_CAPTURES").is_ok_and(|value| value.trim() == "0")
    })
}

/// The collector's question (see `egcl_rt::gc::set_function_captures_remembered_fn`),
/// asked with the world stopped at the start of each minor collection.
pub(super) fn captures_remembered() -> bool {
    let epoch = egcl_rt::gc::minor_epoch();
    // The first collection traces everything, covering anything published
    // before the logs could see it (a restored image's tables).
    epoch > 0 && !disabled() && !FRAMES.overflowed_at(epoch) && !BODIES.overflowed_at(epoch)
}

/// What one minor collection visits: taken in its marking pass and reused,
/// unchanged, by its relocation pass so both see the same slots. Weak, so an
/// abandoned collection keeps nothing alive; no mutator runs between the two
/// passes, so each upgrade gives the same answer in both.
struct Snapshot {
    epoch: u64,
    frames: Vec<Weak<FrameCell>>,
    bodies: Vec<Weak<BytecodeFunction>>,
}

static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);

/// Root scanner: visit every frame and body written in the current epoch,
/// during a minor collection that skips old functions' captures.
pub(super) fn scan_remembered_captures(visit: &mut dyn FnMut(*mut EgclVal)) {
    let kind = egcl_rt::gc::external_root_scan_kind();
    if kind == ExternalRootScanKind::Full || !egcl_rt::gc::function_captures_remembered() {
        return;
    }
    let epoch = egcl_rt::gc::minor_epoch();
    let mut snapshot = SNAPSHOT.lock().unwrap_or_else(|error| error.into_inner());
    if kind == ExternalRootScanKind::MinorMark || snapshot.as_ref().is_none_or(|s| s.epoch != epoch)
    {
        *snapshot = Some(Snapshot {
            epoch,
            frames: FRAMES.live_since(epoch),
            bodies: BODIES.live_since(epoch),
        });
    }
    let current = snapshot.as_ref().expect("snapshot was just taken");
    if log_enabled() && kind == ExternalRootScanKind::MinorMark {
        eprintln!(
            "[gc-captures] minor epoch {epoch}: {} written frames, {} published bodies",
            current.frames.len(),
            current.bodies.len()
        );
    }
    for body in current.bodies.iter().filter_map(Weak::upgrade) {
        // SAFETY: the world is stopped; the collector rewrites body slots in
        // place, exactly as every other bytecode root scanner does.
        unsafe { super::bytecode::trace_bytecode_function(Arc::as_ptr(&body) as *mut _, visit) };
    }
    for frame in current.frames.iter().filter_map(Weak::upgrade) {
        super::visit_env_frame_slots(&mut frame.gc_borrow(), visit);
    }
    if kind == ExternalRootScanKind::MinorRelocate {
        // Relocation runs after evacuation, when this collection can no
        // longer fail and be retried: what it visited is now old.
        *snapshot = None;
        FRAMES.retire(epoch);
        BODIES.retire(epoch);
    }
}

fn log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_GC_REGION_LOG").is_some())
}

pub(super) fn install() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        egcl_rt::gc::register_root_scanner(scan_remembered_captures);
        egcl_rt::gc::set_function_captures_remembered_fn(captures_remembered);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{arena_cons, cp, heap_test_lock};
    use egcl_rt::value::NIL;

    /// Put the collector in remembered mode: the scanner and decision hook are
    /// installed and at least one minor collection has completed.
    fn remembered_mode() {
        egcl_rt::gc::ensure_heap_initialized();
        install();
        egcl_rt::gc::collect_t0_minor().unwrap();
    }

    fn fixnum(n: i64) -> EgclVal {
        EgclVal::from_fixnum(n)
    }

    /// A frame written after it went clean is visited and relocated by the
    /// remembered scanner, though nothing else reaches it: the log is its only
    /// root. Without the scanner the fresh cons would be reclaimed and the
    /// slot left pointing into the evacuated nursery.
    #[test]
    fn written_frame_is_a_minor_root_and_moves() {
        let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        remembered_mode();
        let frame = FrameCell::new_arc(EnvFrame::default());
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert!(
            captures_remembered(),
            "the next collection must use the logs"
        );

        egcl_rt::rooted!(fresh = arena_cons(fixnum(41), fixnum(42)));
        frame.borrow_mut().vars.insert("HELD".into(), *fresh);
        drop(fresh);
        let unrooted = frame.gc_borrow().vars["HELD"];
        egcl_rt::gc::collect_t0_minor().unwrap();
        let held = frame.gc_borrow().vars["HELD"];
        assert_ne!(
            held.to_raw(),
            unrooted.to_raw(),
            "the capture must actually move"
        );
        assert_eq!(
            cp(held),
            (fixnum(41), fixnum(42)),
            "the moved capture is intact"
        );

        // Clean now: later collections need not visit it, and what it holds
        // is already old, so it stays put and intact.
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert_eq!(frame.gc_borrow().vars["HELD"].to_raw(), held.to_raw());
        assert_eq!(cp(frame.gc_borrow().vars["HELD"]), (fixnum(41), fixnum(42)));
    }

    /// A body published since the last collection is traced by the next one,
    /// and its constants move with their referents.
    #[test]
    fn published_body_is_a_minor_root_and_moves() {
        let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        remembered_mode();
        egcl_rt::rooted!(constant = arena_cons(fixnum(7), NIL));
        let unrooted = *constant;
        let body = Arc::new(BytecodeFunction {
            self_call_sites: Vec::new(),
            notinline_sites: Vec::new(),
            code: Vec::new(),
            constants: vec![*constant],
            load_time_values: Vec::new(),
            handler_cases: Vec::new(),
            handler_binds: Vec::new(),
            names: Vec::new(),
            restart_cases: Vec::new(),
            nested_functions: Vec::new(),
            param_layout: Vec::new(),
            param_types: Vec::new(),
            has_env: false,
            n_locals: 0,
            max_stack: 1,
            arity: 0,
            name: "REMEMBERED-BODY".into(),
            params_form: NIL,
            min_args: 0,
            max_args: Some(0),
            variadic: false,
            binder_cache: Default::default(),
            closure_template: Default::default(),
        });
        note_published_body(&body);
        drop(constant);
        assert!(captures_remembered());
        egcl_rt::gc::collect_t0_minor().unwrap();
        let moved = body.constants[0];
        assert_ne!(
            moved.to_raw(),
            unrooted.to_raw(),
            "the constant must actually move"
        );
        assert_eq!(cp(moved), (fixnum(7), NIL), "the moved constant is intact");
    }

    /// A minor collection no longer traces an interpreted function's captures
    /// through the function (bliss-gg3i1.7.12). A closure that has survived
    /// collections and whose captured frame then receives a young object must
    /// still see that object relocated: the frame's write log is the only
    /// path left from the collector to it.
    #[test]
    fn old_closure_frame_written_later_moves_through_the_log() {
        let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        remembered_mode();
        let body = Arc::new(BytecodeFunction {
            self_call_sites: Vec::new(),
            notinline_sites: Vec::new(),
            code: Vec::new(),
            constants: Vec::new(),
            load_time_values: Vec::new(),
            handler_cases: Vec::new(),
            handler_binds: Vec::new(),
            names: Vec::new(),
            restart_cases: Vec::new(),
            nested_functions: Vec::new(),
            param_layout: Vec::new(),
            param_types: Vec::new(),
            has_env: true,
            n_locals: 0,
            max_stack: 1,
            arity: 0,
            name: "REMEMBERED-CLOSURE".into(),
            params_form: NIL,
            min_args: 0,
            max_args: Some(0),
            variadic: false,
            binder_cache: Default::default(),
            closure_template: Default::default(),
        });
        let frame = FrameCell::new_arc(EnvFrame::default());
        egcl_rt::rooted!(
            closure = super::super::bytecode::make_bytecode_closure(&body, Some(frame))
        );
        // Promote the closure and let its frame's creation record retire.
        egcl_rt::gc::collect_t0_minor().unwrap();
        egcl_rt::gc::collect_t0_minor().unwrap();
        assert!(captures_remembered(), "the next collection uses the logs");

        let captured = super::super::bytecode::closure_captured_env(*closure).unwrap();
        egcl_rt::rooted!(fresh = arena_cons(fixnum(5), fixnum(6)));
        captured.borrow_mut().vars.insert("LATE".into(), *fresh);
        drop(fresh);
        let unrooted = captured.gc_borrow().vars["LATE"];
        egcl_rt::gc::collect_t0_minor().unwrap();
        let held = super::super::bytecode::closure_captured_env(*closure)
            .unwrap()
            .gc_borrow()
            .vars["LATE"];
        assert_ne!(
            held.to_raw(),
            unrooted.to_raw(),
            "the capture must actually move"
        );
        assert_eq!(
            cp(held),
            (fixnum(5), fixnum(6)),
            "the moved capture is intact"
        );
    }

    #[test]
    fn a_log_gives_up_only_on_live_entries_and_recovers_next_epoch() {
        let log: WriteLog<u32> = WriteLog::new();
        let epoch = 1_000_000;
        // Dead entries never make the log give up, however many there are.
        for i in 0..(LOG_HARD_LIMIT as u32 * 2) {
            let item = Arc::new(i);
            log.note(Arc::downgrade(&item), epoch);
        }
        assert!(!log.overflowed_at(epoch));
        // Entries from earlier epochs were covered by a completed collection.
        let earlier: Vec<_> = (0..1000u32).map(Arc::new).collect();
        for item in &earlier {
            log.note(Arc::downgrade(item), epoch - 1);
        }
        assert!(log.live_since(epoch).is_empty());
        // Too many live entries in one epoch make it give up for that epoch.
        let live: Vec<_> = (0..2 * LOG_HARD_LIMIT as u32).map(Arc::new).collect();
        for item in &live {
            log.note(Arc::downgrade(item), epoch);
        }
        assert!(log.overflowed_at(epoch));
        assert!(log.live_since(epoch).is_empty());
        assert!(log.overflowed_at(epoch), "it gives up until the epoch ends");
        // The completed (full) collection ends the epoch.
        assert!(log.live_since(epoch + 1).is_empty());
        assert!(!log.overflowed_at(epoch + 1));
        log.note(Arc::downgrade(&live[0]), epoch + 1);
        log.note(Arc::downgrade(&live[1]), epoch + 1);
        log.note(Arc::downgrade(&live[0]), epoch + 1);
        assert_eq!(
            log.live_since(epoch + 1).len(),
            2,
            "each item is visited once"
        );
    }
}
