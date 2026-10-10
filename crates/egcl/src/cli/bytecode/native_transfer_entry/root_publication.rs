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
//! collection. Stable native homes are authoritative; volatile values retain
//! managed shadows because calls may overwrite their machine registers.

use super::*;
use egcl_compiler::t2::x64_calls::NativeCallValues;
use egcl_compiler::t2::x64_unwind::{NativeFrameCursor, SYSV_PRESERVED_REGISTERS};
use egcl_compiler::t2::x64_value_maps::{NativeFrameValues, NativeValueLocation};
use egcl_rt::native_transfer::NativeSegment;
use std::ops::Range;
use std::sync::Once;

/// Permanent adapter code and the exact PC reached by an ordinary child return.
/// Retain the descriptor with the mapping; scanners must never initialize or
/// wait for a registry while the thread performing initialization is stopped.
pub(in crate::cli::bytecode) struct PublishedAdapter {
    pub(in crate::cli::bytecode) code: JitBuffer,
    child_return_pc: usize,
    frame_bytes: usize,
}

impl PublishedAdapter {
    pub(in crate::cli::bytecode) fn new(
        entry: egcl_compiler::t2::native_transfer::PublishedCallEntry,
    ) -> Option<Self> {
        let code = JitBuffer::new(&entry.code)?;
        let child_return_pc = code.as_ptr() as usize + entry.child_return_offset;
        Some(Self { code, child_return_pc, frame_bytes: entry.frame_bytes })
    }

    pub(in crate::cli::bytecode) fn contains_return(&self, pc: usize) -> bool {
        self.child_return_pc == pc
    }
}

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
    /// Managed stack of the publishing execution, for activation validation.
    /// Captured here because a stop-the-world scan visits other executions'
    /// chains and cannot ask them for their current stack.
    pub(super) stack: *const egcl_rt::stack::EgclStack,
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

/// Separate hook for the cold transfer-preparation route, so a test can
/// distinguish a publication made there from an ordinary poll's.
#[cfg(test)]
pub(super) static OBSERVE_COLD: egcl_rt::execution_local::ExecutionLocal<Cell<ObserveHook>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

#[cfg(test)]
pub(super) fn observe_cold() {
    if let Some(hook) = OBSERVE_COLD.with(Cell::get) {
        hook();
    }
}

/// Boundaries walked that belong to an execution other than the collecting
/// one, and the root slots reached through them. A suspended execution's
/// activations validate only against its own managed stack, so a nonzero
/// visit count is what distinguishes real cross-execution coverage from a
/// walk that silently rejected everything.
#[cfg(test)]
static FOREIGN_WALKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static FOREIGN_VISITED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Why a walk stopped short of a segment's end, or failed to resolve a
/// location the map said was addressable. Native homes may be the only live
/// copies, so production must stop instead of silently dropping a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Bail {
    NullOwner = 0,
    NullSegment = 1,
    NullStack = 2,
    PcOutsideOwner = 3,
    UnwindRefused = 4,
    MapUnavailable = 5,
    MapMissing = 6,
    LocationUnresolved = 7,
    AdapterRecordUnowned = 8,
    AdapterOwnerMissing = 9,
    AdapterCallerUnmapped = 10,
}

#[cfg(test)]
const BAIL_KINDS: usize = 11;
#[cfg(test)]
static BAILS: [std::sync::atomic::AtomicUsize; BAIL_KINDS] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; BAIL_KINDS];

#[cfg(test)]
thread_local! {
    static EXPECTED_BAIL: Cell<Option<Bail>> = const { Cell::new(None) };
    // Negative control: omit all native copies of one object for one collection.
    static OMIT_NATIVE_ROOT_BITS: Cell<u64> = const { Cell::new(0) };
}

fn note_bail(bail: Bail) {
    #[cfg(test)]
    {
        assert_eq!(EXPECTED_BAIL.with(Cell::get), Some(bail),
            "unexpected native root-walk bailout: {bail:?}");
        BAILS[bail as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(not(test))]
    panic!("invalid native root publication: {bail:?}");
}

/// Only deliberately malformed publications may decline. Keep the permission
/// local to one collector invocation and restore it even if its fixture panics.
#[cfg(test)]
fn with_expected_bail<R>(bail: Bail, body: impl FnOnce() -> R) -> R {
    struct Restore(Option<Bail>);
    impl Drop for Restore {
        fn drop(&mut self) {
            EXPECTED_BAIL.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(EXPECTED_BAIL.with(|slot| slot.replace(Some(bail))));
    body()
}

/// Locations the maps said were addressable, and how many the walk actually
/// resolved. These must be equal: a shortfall can drop a sole native root.
#[cfg(test)]
static EXPECTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static RESOLVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// (expected, resolved, bails per kind) since the last call.
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_completeness() -> (usize, usize, [usize; BAIL_KINDS]) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        EXPECTED.swap(0, Relaxed),
        RESOLVED.swap(0, Relaxed),
        std::array::from_fn(|i| BAILS[i].swap(0, Relaxed)),
    )
}

/// Describe any nonzero bail counts, for a test failure message.
#[cfg(test)]
pub(in crate::cli::bytecode) fn describe_bails(bails: [usize; BAIL_KINDS]) -> String {
    const NAMES: [&str; BAIL_KINDS] = [
        "NullOwner", "NullSegment", "NullStack", "PcOutsideOwner",
        "UnwindRefused", "MapUnavailable", "MapMissing", "LocationUnresolved",
        "AdapterRecordUnowned", "AdapterOwnerMissing", "AdapterCallerUnmapped",
    ];
    let mut parts = Vec::new();
    for (name, count) in NAMES.iter().zip(bails) {
        if count > 0 {
            parts.push(format!("{name}={count}"));
        }
    }
    if parts.is_empty() { "none".into() } else { parts.join(" ") }
}

/// Root slots the walk actually visited, split by home kind. Only the
/// non-activation kinds are words the EgclStack scan does not already visit,
/// so a nonzero stack/register count is the mechanical statement that the
/// published walk is load-bearing rather than redundant.
#[cfg(test)]
static VISIT_ACTIVATION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static VISIT_STACK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static VISIT_REGISTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// (activation, stack, register) visits since the last call.
#[cfg(test)]
pub(super) fn take_visits_by_kind() -> (usize, usize, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        VISIT_ACTIVATION.swap(0, Relaxed),
        VISIT_STACK.swap(0, Relaxed),
        VISIT_REGISTER.swap(0, Relaxed),
    )
}

#[cfg(all(test, debug_assertions))]
static VERIFIED_ALIASES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static ADAPTER_CROSSINGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(in crate::cli::bytecode) fn take_adapter_crossings() -> usize {
    ADAPTER_CROSSINGS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
pub(super) fn take_foreign_walk_counts() -> (usize, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        FOREIGN_WALKS.swap(0, Relaxed),
        FOREIGN_VISITED.swap(0, Relaxed),
    )
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
    // Copy the geometry out, then let the reference go before `body` writes
    // through the aliasing outcome pointer.
    let cursor = unsafe {
        let image_ref = &*image;
        NativeFrameCursor {
            pc: image_ref.return_pc as usize,
            call_sp: image_ref.caller_sp as usize,
            registers: std::array::from_fn(|index| {
                Some(std::ptr::addr_of!(image_ref.preserved[index]) as usize)
            }),
        }
    };
    unsafe { publish_with(cursor, None, image as usize, body) }
}

/// Link one suspended caller for the extent of `body`. `low` is the lowest
/// native address the publication owns: its own machine storage, below the
/// caller's save words and stack slots. `owner` names the code holding
/// `cursor.pc` when the caller is not the current CAPTURE's own activation.
unsafe fn publish_with<R>(
    cursor: NativeFrameCursor,
    owner: Option<*const TransferCode>,
    low: usize,
    body: impl FnOnce() -> R,
) -> R {
    static SCANNER: Once = Once::new();
    SCANNER.call_once(|| egcl_rt::gc::register_root_scanner(scan_published_roots));
    let context = CAPTURE.with(Cell::get);
    let segment = native_transfer::current_segment();
    debug_assert!(!context.is_null(), "publication needs a captured activation");
    debug_assert!(!segment.is_null(), "publication needs an active native segment");
    let owner = owner.unwrap_or_else(|| {
        if context.is_null() { std::ptr::null() } else { unsafe { (*context).owner } }
    });
    debug_assert!(
        owner.is_null() || {
            let code = unsafe { &*owner };
            let offset = cursor.pc.wrapping_sub(code.code.as_ptr() as usize);
            offset < code.code_len && code._native_calls.value_map(offset).is_some()
        },
        "a publication's PC must be a recorded site of its own owner"
    );
    let saved_sp = if segment.is_null() { 0 } else { unsafe { (*segment).saved_sp } };
    let mut boundary = PublishedBoundary {
        owner,
        cursor,
        bounds: low..saved_sp,
        segment,
        stack: egcl_rt::current_stack(),
        previous: ACTIVE.with(Cell::get),
        #[cfg(test)]
        visited: Cell::new(0),
    };
    debug_assert!(
        low < cursor.call_sp && cursor.call_sp <= saved_sp,
        "machine storage, caller RSP and segment entry must nest"
    );
    ACTIVE.with(|slot| slot.set(&mut boundary));
    let _linked = Linked(&mut boundary);
    body()
}

/// Publish the generated caller suspended beneath a permanent mapped-call
/// adapter frame. The adapter's own callbacks have no capture image, but the
/// record sits at the bottom of that frame, so the caller's exact return PC,
/// pre-CALL RSP and writable save words are all recoverable from it.
///
/// Use this before a child exists — preparation, and the checked and
/// interpreted fallbacks — where `CAPTURE` still names the caller itself.
///
/// # Safety
/// `record` must be the live record of an executing adapter frame, and the
/// caller must not retain a Rust reference to it across the body's writes.
pub(super) unsafe fn published_mapped<R>(
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
    body: impl FnOnce() -> R,
) -> R {
    if unsafe { super::super::native_callable::outer::owns_record(record) } {
        // The registered Rust boundary has no generated Lisp frame or live
        // tagged register homes. Its callable and arguments are host roots;
        // any enclosing mapped publication remains linked throughout reentry.
        return body();
    }
    unsafe { publish_with(mapped_cursor(record), None, record as usize, body) }
}

/// Use this once a child activation is live, i.e. from cold resumption.
/// `CAPTURE` then names the CHILD, so the owner of the caller's PC has to come
/// from the record's own activation instead.
///
/// # Safety
/// As `published_mapped`, and `record.owner` must already hold this adapter's
/// child activation — the adapter leaves that word UNINITIALIZED until
/// preparation writes it, so this must never run before preparation.
pub(super) unsafe fn published_mapped_resuming<R>(
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
    body: impl FnOnce() -> R,
) -> R {
    // Never fall back to CAPTURE here: it names the child while this cursor
    // belongs to the parent. A missing owner publishes null, which makes any
    // collection fail explicitly before it can lose the parent's native roots.
    let owner = unsafe { super::nested::record_parent_owner(record) };
    debug_assert!(owner.is_some(), "a live child must yield its parent's owner");
    let owner = owner.unwrap_or(std::ptr::null());
    unsafe { publish_with(mapped_cursor(record), Some(owner), record as usize, body) }
}

/// The suspended caller's exact geometry, read out of the adapter frame.
unsafe fn mapped_cursor(
    record: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
) -> NativeFrameCursor {
    use egcl_compiler::t2::native_transfer::{ADAPTER_FRAME_BYTES, MappedCallRecord};
    let base = record as usize;
    NativeFrameCursor {
        // SAFETY: the adapter reserved ADAPTER_FRAME_BYTES below the caller's
        // return address, which the tail-jumping veneer left in place.
        pc: unsafe { ((base + ADAPTER_FRAME_BYTES) as *const usize).read() },
        call_sp: base + ADAPTER_FRAME_BYTES + 8,
        registers: std::array::from_fn(|index| {
            Some(base + std::mem::offset_of!(MappedCallRecord, preserved) + index * 8)
        }),
    }
}

/// Verify, mutator-side, that every value recorded in more than one home has
/// the SAME word in all of them. Returns (values checked, dual-homed values
/// checked, mismatches).
///
/// This is an oracle on the walk's address arithmetic. The shadow activation
/// slot is written by generated code from the value's own home immediately
/// before the safepoint, so if `body_sp + offset`, an inherited register save
/// word, or the activation pointer were computed wrongly, the words would
/// disagree. It deliberately runs in the mutator rather than in the collector:
/// during the relocate pass the EgclStack scan forwards the shadow before the
/// external scanners run, so a scanner-side comparison would report spurious
/// mismatches. It compares two homes at ONE instant, never a word before
/// against after.
///
/// # Safety
/// Call only from a published helper on the publishing execution, with the
/// chain live -- e.g. from inside a published poll.
#[cfg(test)]
pub(super) unsafe fn verify_dual_homes() -> (usize, usize, usize) {
    let mut checked = 0;
    let mut dual = 0;
    let mut mismatches = 0;
    let mut boundary = ACTIVE.with(Cell::get);
    while !boundary.is_null() {
        let published = unsafe { &*boundary };
        if published.owner.is_null() || published.segment.is_null() || published.stack.is_null() {
            boundary = published.previous;
            continue;
        }
        let code = unsafe { &*published.owner };
        let base = code.code.as_ptr() as usize;
        let segment = unsafe { &*published.segment };
        let walk = Walk::new(published.bounds.clone(), unsafe { &*published.stack });
        let mut cursor = published.cursor;
        loop {
            let Some(offset) =
                cursor.pc.checked_sub(base).filter(|offset| *offset < code.code_len)
            else {
                break;
            };
            let Some(step) = code._native_calls.unwind(base, &cursor, walk.bounds.clone(), |a| {
                walk.word(a)
            }) else {
                break;
            };
            if let Some(NativeCallValues::Frame(map)) = code._native_calls.value_map(offset) {
                let layout = map.layout();
                let activation = unsafe { frame_activation(map, step.body_sp, &walk) };
                for value in map.values().iter().filter(|v| v.may_reference_heap()) {
                    let words: Vec<_> = value
                        .locations()
                        .iter()
                        .filter_map(|location| unsafe {
                            resolve_location(
                                *location, &cursor, step.body_sp, activation,
                                layout.activation_slots, &walk,
                            )
                        })
                        .map(|slot| unsafe { slot.read() })
                        .collect();
                    checked += 1;
                    if words.len() > 1 {
                        dual += 1;
                        if words.iter().any(|w| *w != words[0]) {
                            mismatches += 1;
                        }
                    }
                }
            }
            if step.caller.call_sp == segment.saved_sp && step.caller.pc == segment.return_pc() {
                break;
            }
            cursor = step.caller;
        }
        boundary = published.previous;
    }
    (checked, dual, mismatches)
}

/// Walk every execution's chain under stop-the-world. Each boundary unwinds
/// through its owner's exact recipes until the segment entry; frames whose
/// roots T0 already owns (deoptimizing or retired) are skipped, and a malformed
/// or missing map is a fatal invariant failure.
fn scan_published_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    #[cfg(test)]
    let here = egcl_rt::current_stack() as *const egcl_rt::stack::EgclStack;
    unsafe {
        ACTIVE.scan(|slot| {
            let mut boundary = slot.get();
            while !boundary.is_null() {
                let visited = walk_boundary(&*boundary, visit);
                #[cfg(test)]
                if !std::ptr::eq((*boundary).stack, here) {
                    use std::sync::atomic::Ordering::Relaxed;
                    FOREIGN_WALKS.fetch_add(1, Relaxed);
                    FOREIGN_VISITED.fetch_add(visited, Relaxed);
                }
                #[cfg(not(test))]
                let _ = visited;
                boundary = (*boundary).previous;
            }
        });
    }
}

/// Validated extent of one publication: native stack words it may read, and
/// the managed stack its activations must live in.
struct Walk<'a> {
    bounds: Range<usize>,
    stack: &'a egcl_rt::stack::EgclStack,
    // Only valid during this uninterrupted walk over a fixed managed chain.
    // Native frames normally request activations from youngest to oldest.
    frame_cursor: Cell<*const egcl_rt::stack::Frame>,
}

impl<'a> Walk<'a> {
    fn new(bounds: Range<usize>, stack: &'a egcl_rt::stack::EgclStack) -> Self {
        Self { bounds, stack, frame_cursor: Cell::new(stack.fp()) }
    }

    /// Match an actual linked header before reading its declared slot count.
    /// Cache the last match so a native walk follows each managed link at most
    /// once, regardless of the number of roots or aliases in each frame.
    fn activation(&self, activation: *mut EgclVal, slots: u16) -> Option<*mut EgclVal> {
        use egcl_rt::stack::Frame;
        let address = activation as usize;
        if address % std::mem::align_of::<Frame>() != 0 {
            return None;
        }
        let header = address.checked_sub(std::mem::size_of::<Frame>())?;
        let end = address.checked_add(usize::from(slots) * std::mem::size_of::<EgclVal>())?;
        let base = self.stack.base() as usize;
        let top = self.stack.sp() as usize;
        if header < base || end > top {
            return None;
        }
        let mut frame = self.frame_cursor.get();
        // Support an out-of-order diagnostic lookup without treating the cache
        // as proof of membership. Ordinary outward native walks never restart.
        if header > frame as usize {
            frame = self.stack.fp();
        }
        while !frame.is_null() && frame as usize >= header {
            let current = frame as usize;
            if current < base || current % std::mem::align_of::<Frame>() != 0
                || current.checked_add(std::mem::size_of::<Frame>())? > top {
                return None;
            }
            // Start at the real top and follow only its actual frame links.
            if current == header {
                if unsafe { (*frame).num_locals } < slots {
                    return None;
                }
                self.frame_cursor.set(frame);
                return Some(activation);
            }
            let previous = unsafe { (*frame).prev_fp };
            if previous as usize >= current {
                return None;
            }
            frame = previous;
        }
        None
    }

    /// A native stack word this publication actually owns.
    fn word(&self, address: usize) -> Option<usize> {
        self.slot(address).map(|slot| unsafe { (slot as *const usize).read() })
    }

    /// Address of an owned, aligned native stack word, without reading it.
    fn slot(&self, address: usize) -> Option<usize> {
        (address % 8 == 0
            && address >= self.bounds.start
            && address.checked_add(8)? <= self.bounds.end)
            .then_some(address)
    }
}

/// Unit-test entry point using a fresh managed-frame cursor.
#[cfg(test)]
fn validated_activation(
    activation: *mut EgclVal,
    slots: u16,
    stack: &egcl_rt::stack::EgclStack,
) -> Option<*mut EgclVal> {
    Walk::new(0..0, stack).activation(activation, slots)
}

/// Returns the number of root slots visited through this boundary's frames.
unsafe fn walk_boundary(
    boundary: &PublishedBoundary,
    visit: &mut dyn FnMut(*mut EgclVal),
) -> usize {
    if boundary.owner.is_null() || boundary.segment.is_null() || boundary.stack.is_null() {
        note_bail(if boundary.owner.is_null() {
            Bail::NullOwner
        } else if boundary.segment.is_null() {
            Bail::NullSegment
        } else {
            Bail::NullStack
        });
        return 0;
    }
    let mut code = unsafe { &*boundary.owner };
    let segment = unsafe { &*boundary.segment };
    let walk = Walk::new(boundary.bounds.clone(), unsafe { &*boundary.stack });
    let mut cursor = boundary.cursor;
    let mut total = 0;
    loop {
        let base = code.code.as_ptr() as usize;
        let Some(offset) = cursor.pc.checked_sub(base).filter(|offset| *offset < code.code_len)
        else {
            let adapter = super::mapped_adapter(cursor.pc)
                .or_else(|| super::super::native_callable::mapped_adapter(cursor.pc));
            let Some(adapter) = adapter else {
                note_bail(Bail::PcOutsideOwner);
                return total;
            };
            let record = cursor.call_sp;
            // Validate the whole adapter record and its caller return slot
            // before reading its initialized child owner or any saved word.
            if walk.slot(record).is_none()
                || record.checked_add(adapter.frame_bytes).and_then(|p| walk.slot(p)).is_none() {
                note_bail(Bail::AdapterRecordUnowned);
                return total;
            }
            let record = record as *mut egcl_compiler::t2::native_transfer::MappedCallRecord;
            let Some(owner) = (unsafe { super::nested::record_parent_owner(record) }) else {
                note_bail(Bail::AdapterOwnerMissing);
                return total;
            };
            let caller = unsafe { mapped_cursor(record) };
            let parent = unsafe { &*owner };
            let Some(offset) = caller.pc.checked_sub(parent.code.as_ptr() as usize) else {
                note_bail(Bail::AdapterCallerUnmapped);
                return total;
            };
            if offset >= parent.code_len || parent._native_calls.value_map(offset).is_none() {
                note_bail(Bail::AdapterCallerUnmapped);
                return total;
            }
            // The adapter restores ALL preserved words, even registers the
            // child never saved. Replace every inherited register location.
            cursor = caller;
            code = parent;
            #[cfg(test)]
            ADAPTER_CROSSINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        };
        let Some(step) =
            code._native_calls
                .unwind(base, &cursor, walk.bounds.clone(), |address| walk.word(address))
        else {
            note_bail(Bail::UnwindRefused);
            return total;
        };
        match code._native_calls.value_map(offset) {
            Some(NativeCallValues::Frame(map)) => {
                let visited = unsafe { visit_frame(map, &cursor, step.body_sp, &walk, visit) };
                total += visited;
                #[cfg(test)]
                boundary.visited.set(boundary.visited.get() + visited);
            }
            // T0 owns these roots while the native frame is being replaced.
            Some(NativeCallValues::Deoptimizing { .. } | NativeCallValues::Retired) => {}
            Some(NativeCallValues::Unavailable) => {
                note_bail(Bail::MapUnavailable);
                return total;
            }
            None => {
                note_bail(Bail::MapMissing);
                return total;
            }
        }
        if step.caller.call_sp == segment.saved_sp && step.caller.pc == segment.return_pc() {
            return total;
        }
        cursor = step.caller;
    }
}

/// Visit every writable heap-referencing copy at one suspended call and return
/// how many slots were visited. Register copies resolve through the cursor's
/// inherited save-word addresses; stack copies are body-RSP relative;
/// activation copies are the managed shadow slots the stack scan also visits,
/// so relocation stays idempotent.
/// Resolve one recorded location to the writable word that holds the value,
/// or `None` when this publication cannot prove it owns that word.
///
/// Shared by the walk and the dual-home verifier on purpose: a verifier with
/// its own copy of this arithmetic would validate itself rather than the walk.
unsafe fn resolve_location(
    location: NativeValueLocation,
    cursor: &NativeFrameCursor,
    body_sp: Option<usize>,
    activation: Option<*mut EgclVal>,
    activation_slots: u16,
    walk: &Walk<'_>,
) -> Option<*mut EgclVal> {
    match location {
        NativeValueLocation::Activation(slot) => match activation {
            Some(activation) if slot < activation_slots => {
                Some(unsafe { activation.add(slot as usize) })
            }
            _ => None,
        },
        NativeValueLocation::Stack(offset) => body_sp
            .and_then(|body_sp| body_sp.checked_add_signed(offset as isize))
            .and_then(|address| walk.slot(address))
            .map(|address| address as *mut EgclVal),
        NativeValueLocation::Register(register) => SYSV_PRESERVED_REGISTERS
            .iter()
            .position(|&r| r == register)
            .and_then(|index| cursor.registers[index])
            .and_then(|address| walk.slot(address))
            .map(|address| address as *mut EgclVal),
        NativeValueLocation::Constant(_) | NativeValueLocation::Unavailable => None,
    }
}

/// The activation pointer for a frame, read from an owned native stack word and
/// accepted only if it names a real frame of the publishing execution.
unsafe fn frame_activation(
    map: &NativeFrameValues,
    body_sp: Option<usize>,
    walk: &Walk<'_>,
) -> Option<*mut EgclVal> {
    let layout = map.layout();
    match (layout.activation_base_slot, body_sp) {
        (Some(slot), Some(body_sp)) => walk
            .word(body_sp.checked_add(slot as usize * 8)?)
            .and_then(|activation| {
                walk.activation(activation as *mut EgclVal, layout.activation_slots)
            }),
        _ => None,
    }
}

unsafe fn visit_frame(
    map: &NativeFrameValues,
    cursor: &NativeFrameCursor,
    body_sp: Option<usize>,
    walk: &Walk<'_>,
    visit: &mut dyn FnMut(*mut EgclVal),
) -> usize {
    #[cfg(debug_assertions)]
    if egcl_rt::gc::marking_external_roots() {
        let compared = verify_frame_aliases(map, cursor, body_sp, walk)
            .unwrap_or_else(|error| panic!("native root aliases at PC {:#x}: {error:?}", cursor.pc));
        #[cfg(test)]
        VERIFIED_ALIASES.fetch_add(compared, std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(test))]
        let _ = compared;
    }
    let layout = map.layout();
    let activation = unsafe { frame_activation(map, body_sp, walk) };
    let mut visited = 0;
    for &location in map.gc_locations() {
        let resolved = unsafe {
            resolve_location(location, cursor, body_sp, activation, layout.activation_slots, walk)
        };
        #[cfg(test)]
        if !matches!(location, NativeValueLocation::Constant(_) | NativeValueLocation::Unavailable) {
            use std::sync::atomic::Ordering::Relaxed;
            EXPECTED.fetch_add(1, Relaxed);
            if resolved.is_some() {
                RESOLVED.fetch_add(1, Relaxed);
            }
        }
        let Some(slot) = resolved else {
            if !matches!(location, NativeValueLocation::Constant(_) | NativeValueLocation::Unavailable) {
                note_bail(Bail::LocationUnresolved);
            }
            continue;
        };
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering::Relaxed;
            match location {
                NativeValueLocation::Activation(_) => &VISIT_ACTIVATION,
                NativeValueLocation::Stack(_) => &VISIT_STACK,
                NativeValueLocation::Register(_) => &VISIT_REGISTER,
                _ => unreachable!("filtered above"),
            }
            .fetch_add(1, Relaxed);
        }
        #[cfg(test)]
        if matches!(location, NativeValueLocation::Stack(_) | NativeValueLocation::Register(_))
            && OMIT_NATIVE_ROOT_BITS.with(Cell::get) == unsafe { slot.read() }.to_raw() {
            continue;
        }
        visited += 1;
        visit(slot);
    }
    visited
}

/// Compare before the collector changes ANY root. During relocation another
/// scanner may already have updated a shadow while the native copy is still
/// old; comparing then would incorrectly reject a valid relocation in progress.
#[cfg(any(debug_assertions, test))]
fn verify_frame_aliases(
    map: &NativeFrameValues,
    cursor: &NativeFrameCursor,
    body_sp: Option<usize>,
    walk: &Walk<'_>,
) -> Result<usize, String> {
    let activation = unsafe { frame_activation(map, body_sp, walk) };
    let mut compared = 0;
    for value in map.values().iter().filter(|value| value.may_reference_heap()) {
        let mut first = None;
        for &location in value.locations() {
            let slot = unsafe { resolve_location(location, cursor, body_sp, activation,
                map.layout().activation_slots, walk) }
                .ok_or_else(|| format!("unresolved {:?} at {location:?}", value.value))?;
            let bits = unsafe { slot.read() };
            if let Some((original_slot, original)) = first {
                if original != bits {
                    return Err(format!("{:?}: {original_slot:p} holds {original:?}, \
                        {slot:p} ({location:?}) holds {bits:?}", value.value));
                }
                compared += 1;
            } else {
                first = Some((slot, bits));
            }
        }
    }
    Ok(compared)
}

#[cfg(test)]
#[path = "root_publication_matrix_tests.rs"]
mod matrix_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::Collector;

    #[test]
    #[should_panic(expected = "unexpected native root-walk bailout")]
    fn unexpected_walk_bailout_fails_the_test() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        note_bail(Bail::PcOutsideOwner);
    }

    #[test]
    #[should_panic(expected = "unexpected native root-walk bailout")]
    fn expected_bailout_does_not_allow_another_failure_reason() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        with_expected_bail(Bail::NullOwner, || note_bail(Bail::PcOutsideOwner));
    }

    #[test]
    fn expected_bailout_permission_ends_when_the_fixture_unwinds() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        assert!(std::panic::catch_unwind(|| {
            with_expected_bail(Bail::NullOwner, || panic!("leave malformed fixture"));
        }).is_err());
        assert!(std::panic::catch_unwind(|| note_bail(Bail::NullOwner)).is_err());
    }

    struct Probe {
        code: *const TransferCode,
        env: *mut Env,
        args: *const Vec<EgclVal>,
        calls: usize,
        moved: bool,
        /// Root slots reached through the published chain by the nested collection.
        visited: usize,
        /// Of those, ones in a native stack slot or register save word -- i.e.
        /// words the EgclStack activation scan does not visit at all.
        native_visits: usize,
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

    pub(super) fn force_minor_gc() {
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
        // Oracle on the walk's address arithmetic: a value's shadow slot and
        // its native home must agree before anything collects.
        let (checked, dual, mismatches) = unsafe { verify_dual_homes() };
        eprintln!("  dual-home check: values={checked} dual-homed={dual} mismatches={mismatches}");
        assert_eq!(mismatches, 0, "a value's shadow and native home disagree");
        assert!(checked > 0, "the fixture must exercise mapped heap values");
        let code = unsafe { &*publication.owner };
        let offset = publication.cursor.pc - code.code.as_ptr() as usize;
        let Some(NativeCallValues::Frame(map)) = code._native_calls.value_map(offset) else {
            panic!("inner poll needs a frame map");
        };
        let step = code._native_calls.unwind(code.code.as_ptr() as usize,
            &publication.cursor, publication.bounds.clone(), read_stack_word).unwrap();
        let walk = Walk::new(publication.bounds.clone(), unsafe { &*publication.stack });
        verify_frame_aliases(map, &publication.cursor, step.body_sp, &walk).unwrap();
        let native = map.gc_locations().find_map(|location| match location {
            NativeValueLocation::Stack(_) | NativeValueLocation::Register(_) =>
                unsafe { resolve_location(*location, &publication.cursor, step.body_sp,
                    frame_activation(map, step.body_sp, &walk), map.layout().activation_slots, &walk) }
                    .filter(|slot| unsafe { **slot == before }),
            _ => None,
        }).expect("a real native home");
        take_visits_by_kind();
        take_completeness();
        force_minor_gc();
        let (activation, stack, register) = take_visits_by_kind();
        // Every location the maps declared addressable must have been reached.
        // A shortfall could discard the only copy of a live native root.
        let (expected, resolved, bails) = take_completeness();
        eprintln!("  completeness: expected={expected} resolved={resolved} bails=[{}]",
                  describe_bails(bails));
        assert!(expected > 0, "the collection must reach addressable locations");
        assert_eq!(expected, resolved,
                   "the walk failed to resolve a location its own map declared addressable");
        assert_eq!(bails.iter().sum::<usize>(), 0,
                   "the walk bailed: [{}]", describe_bails(bails));

        assert!(verify_frame_aliases(map, &publication.cursor, step.body_sp, &walk).is_ok(),
            "relocate native homes and shadows alike");
        assert_eq!(unsafe { native.read() }, unsafe { (&*probe.args)[0] },
            "the native home must contain the relocated input");
        probe.visited += chain_visited(current);
        probe.native_visits += stack + register;
        eprintln!(
            "  nested collection visits: activation={activation} stack={stack} register={register}"
        );
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
        let mut probe = Probe { code: &code, env: &mut env, args: &*args, calls: 0, moved: false, visited: 0, native_visits: 0, frames: 0 };
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
        assert!(
            probe.native_visits > 0,
            "the walk must relocate through at least one native stack slot or register save \
             word -- a word the EgclStack activation scan never visits. Zero here means the \
             walk is redundant with that scan and proves nothing (bliss-shih7.2.7.3)."
        );
        assert_eq!(value, args[0]);
    }

    struct NativeOnlyProbe {
        env: *mut Env,
        observed: bool,
        moved: bool,
    }
    thread_local! {
        static NATIVE_ONLY_PROBE: Cell<*mut NativeOnlyProbe> = const { Cell::new(std::ptr::null_mut()) };
    }

    pub(super) fn native_only_slot_for(
        published: &PublishedBoundary,
        marker: i64,
    ) -> Option<*mut EgclVal> {
        let mut native = None;
        // Include older publications when Rust/checked code reenters Lisp.
        // Each boundary retains the bounds and exact owner for its own frames.
        let mut boundary = published as *const PublishedBoundary;
        while !boundary.is_null() {
            let current = unsafe { &*boundary };
            unsafe { walk_boundary(current, &mut |slot| {
                if !current.bounds.contains(&(slot as usize)) { return; }
                let value = slot.read();
                if value.is_cons() && crate::cli::cp(value) == (EgclVal::from_fixnum(marker), NIL) {
                    native = Some(slot);
                }
            }); }
            boundary = current.previous;
        }
        let native = native?;
        let before = unsafe { native.read() }.to_raw();
        let mut managed_copies = 0;
        unsafe { egcl_rt::stack::visit_stack_refs((*published.stack).fp(), |value| {
            managed_copies += usize::from(value.to_raw() == before);
        }); }
        assert_eq!(managed_copies, 0, "the target must have no scanned activation copy");
        let env = NATIVE_ENV.with(Cell::get);
        assert!(!unsafe { &*env }.mv.iter().any(|value| value.to_raw() == before),
            "the current native environment must not retain the target");
        Some(native)
    }

    fn native_only_slot(published: &PublishedBoundary) -> Option<*mut EgclVal> {
        native_only_slot_for(published, 314159)
    }

    fn observe_native_only() {
        let probe = unsafe { &mut *NATIVE_ONLY_PROBE.with(Cell::get) };
        if probe.observed { return; }
        let published = unsafe { &*ACTIVE.with(Cell::get) };
        let Some(native) = native_only_slot(published) else { return };
        // Keep only diagnostic bits in Rust. The fixture's fresh cons is never
        // an argument, literal, or rooted host local.
        let before = unsafe { native.read() }.to_raw();
        assert!(!unsafe { &*probe.env }.mv.iter().any(|value| value.to_raw() == before),
            "the target must not survive through multiple-value state");
        probe.observed = true;
        let omit = std::env::var_os("EGCL_TEST_OMIT_NATIVE_ONLY_ROOT").is_some();
        if omit { OMIT_NATIVE_ROOT_BITS.with(|slot| slot.set(before)); }
        force_minor_gc();
        OMIT_NATIVE_ROOT_BITS.with(|slot| slot.set(0));
        probe.moved = unsafe { native.read() }.to_raw() != before;
    }

    #[test]
    fn native_only_value_moves_at_a_published_poll() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(forms = reader::read_from_string(
            "((let ((x (cons 314159 nil)))
                (values nil)
                (let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 2) (go again))))
                x))"
        ).unwrap().0);
        let body = Arc::new(compile_function("PUBLICATION-NATIVE-ONLY", NIL, *forms,
            &env, false, false).unwrap());
        let code = TransferCode::compile(body).expect("native-only fixture must be admitted");
        code.run(&[], &mut env).unwrap();
        env.mv.clear();
        force_minor_gc();
        let mut probe = NativeOnlyProbe { env: &mut env, observed: false, moved: false };
        NATIVE_ONLY_PROBE.with(|slot| slot.set(&mut probe));
        OBSERVE.with(|slot| slot.set(Some(observe_native_only)));
        let value = code.run(&[], &mut env).unwrap();
        OBSERVE.with(|slot| slot.set(None));
        NATIVE_ONLY_PROBE.with(|slot| slot.set(std::ptr::null_mut()));
        assert!(probe.observed, "the poll must expose the fresh cons in a native-only home");
        assert!(probe.moved, "native-only root must move");
        assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
    }

    #[test]
    fn omitting_the_native_only_root_fails_the_movement_probe() {
        // A separate process contains the intentionally dead return value. Its
        // fixture checks movement before dereferencing that value, so failure
        // must be the explicit assertion, not a crash or an unrelated error.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::bytecode::native_transfer_entry::root_publication::tests::native_only_value_moves_at_a_published_poll",
                "--nocapture"])
            .env("EGCL_TEST_OMIT_NATIVE_ONLY_ROOT", "1")
            .output().unwrap();
        assert!(!output.status.success(), "omitting the sole roots must fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("native-only root must move"), "{stderr}");
        assert_eq!(output.status.code(), Some(101), "must fail the movement assertion: {stderr}");
    }

    #[test]
    fn native_only_caller_value_moves_across_a_mapped_child() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let loop_forms = "((let ((n 0))
            (tagbody again (setq n (+ n 1)) (if (< n 2) (go again)))))";
        let child_forms = "((publication-native-loop)
            (if (eq fail t) (publication-native-throw) (if fail (+ fail 1) nil)))";
        crate::cli::read_eval_all_env(&format!(
            "(defun publication-native-loop () {})
             (defun publication-native-throw () (throw :outer nil))
             (defun publication-native-child (fail) {})",
            &loop_forms[1..loop_forms.len()-1], &child_forms[1..child_forms.len()-1]),
            &mut env).unwrap();
        egcl_rt::rooted!(forms = reader::read_from_string(loop_forms).unwrap().0);
        let body = Arc::new(compile_function("PUBLICATION-NATIVE-LOOP", NIL, *forms,
            &env, false, false).unwrap());
        let symbol = crate::cli::resolve_sym("PUBLICATION-NATIVE-LOOP").unwrap().as_symbol_index();
        registry_put(symbol, Arc::clone(&body));
        let installed_loop = install_baseline(symbol, body).expect("install collecting loop");
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &installed_loop);
        egcl_rt::rooted!(params = reader::read_from_string("(fail)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(child_forms).unwrap().0);
        let body = Arc::new(compile_function("PUBLICATION-NATIVE-CHILD", *params, *forms,
            &env, false, false).unwrap());
        let symbol = crate::cli::resolve_sym("PUBLICATION-NATIVE-CHILD").unwrap().as_symbol_index();
        registry_put(symbol, Arc::clone(&body));
        let child = TransferCode::compile(body).expect("compile guarded child");
        assert!(child.has_deopt && child.deopt_metadata.is_some(),
            "the numeric child path must use optimized speculative guards");
        let installed = install_baseline_code(symbol, child).expect("install mapped child");
        let NativeCodeStorage::Mapped(child) = &installed._storage else { unreachable!() };
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &installed);
        egcl_rt::rooted!(caller_params = reader::read_from_string("(mode)").unwrap().0);
        for (fail, argument) in [("nil", NIL), ("t", T), ("1.5", EgclVal::from_single_float(1.5))] {
            egcl_rt::rooted!(forms = reader::read_from_string(
                "((let ((x (cons 314159 nil))) (values nil)
                    (catch :outer (publication-native-child mode)) x))"
            ).unwrap().0);
            let body = Arc::new(compile_function("PUBLICATION-NATIVE-CALLER", *caller_params, *forms,
                &env, false, false).unwrap());
            let code = TransferCode::compile(body).expect("admit caller-only root");
            code.run(&[NIL], &mut env).unwrap();
            env.mv.clear();
            force_minor_gc();
            let mut probe = NativeOnlyProbe { env: &mut env, observed: false, moved: false };
            NATIVE_ONLY_PROBE.with(|slot| slot.set(&mut probe));
            take_adapter_crossings();
            take_nested_entries();
            let deopts = child.deopt_count();
            OBSERVE.with(|slot| slot.set(Some(observe_native_only)));
            let value = code.run(&[argument], &mut env).unwrap();
            OBSERVE.with(|slot| slot.set(None));
            NATIVE_ONLY_PROBE.with(|slot| slot.set(std::ptr::null_mut()));
            assert!(probe.observed, "child poll must find its caller's sole native root");
            assert!(probe.moved, "caller-only native root must move, fail={fail}");
            assert!(take_adapter_crossings() > 0, "root walk must cross the mapped child adapter");
            assert_eq!(take_nested_entries(), 2, "child and loop must share the caller's segment");
            assert_eq!(child.deopt_count() - deopts, u32::from(fail == "1.5"),
                "the float path must really deoptimize the child");
            assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
        }
    }

    fn observe_native_only_recursion() {
        if RECURSIVE_ENTRIES.with(Cell::get) >= 2 {
            observe_native_only();
        }
    }

    #[test]
    fn native_only_values_move_across_recursive_callers() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let name = "PUBLICATION-NATIVE-RECURSION";
        let form = "(if (= n 0) nil (let ((x (if (= n 3) (cons 314159 nil) nil)))
            (publication-clear) (publication-native-recursion (- n 1)) x))";
        crate::cli::read_eval_all_env(&format!("(defun publication-clear () nil) (defun {name} (n) {form})"), &mut env).unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
        let symbol = crate::cli::resolve_sym(name).unwrap().as_symbol_index();
        registry_put(symbol, Arc::clone(&body));
        let code = TransferCode::compile(body).expect("admit native-only recursion");
        let args = [EgclVal::from_fixnum(3)];
        code.run(&args, &mut env).unwrap();
        env.mv.clear();
        force_minor_gc();
        let mut probe = NativeOnlyProbe { env: &mut env, observed: false, moved: false };
        NATIVE_ONLY_PROBE.with(|slot| slot.set(&mut probe));
        take_recursive_entries();
        OBSERVE.with(|slot| slot.set(Some(observe_native_only_recursion)));
        let value = code.run(&args, &mut env).unwrap();
        OBSERVE.with(|slot| slot.set(None));
        NATIVE_ONLY_PROBE.with(|slot| slot.set(std::ptr::null_mut()));
        assert_eq!(take_recursive_entries(), 3, "all calls must recurse natively");
        assert!(probe.observed && probe.moved, "an older caller's sole native root must move");
        assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL));
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
        let offset = published.cursor.pc - base;
        let Some(NativeCallValues::Frame(map)) = code._native_calls.value_map(offset) else {
            panic!("recursive preparation needs a frame map");
        };
        let step = code._native_calls.unwind(base, &published.cursor,
            published.bounds.clone(), read_stack_word).unwrap();
        let walk = Walk::new(published.bounds.clone(), unsafe { &*published.stack });
        assert!(verify_frame_aliases(map, &published.cursor, step.body_sp, &walk).unwrap() > 0,
            "outgoing arguments must duplicate a native home");
        let native = map.values().iter().filter(|value| value.locations().len() > 1)
            .flat_map(|value| value.locations()).find_map(|location| match location {
                NativeValueLocation::Stack(_) | NativeValueLocation::Register(_) =>
                    unsafe { resolve_location(*location, &published.cursor, step.body_sp,
                        frame_activation(map, step.body_sp, &walk), map.layout().activation_slots, &walk) }
                        .filter(|slot| unsafe { **slot == (&*probe.args)[1] }),
                _ => None,
            }).expect("a duplicated native heap home");
        let original = unsafe { native.read() };
        unsafe { native.write(NIL) };
        assert!(verify_frame_aliases(map, &published.cursor, step.body_sp, &walk).is_err(),
            "a wrong native home must fail the alias oracle");
        unsafe { native.write(original) };
        #[cfg(debug_assertions)]
        let checked = VERIFIED_ALIASES.load(std::sync::atomic::Ordering::Relaxed);
        force_minor_gc();
        #[cfg(debug_assertions)]
        assert!(VERIFIED_ALIASES.load(std::sync::atomic::Ordering::Relaxed) > checked,
            "the collector must compare aliases in its mark pass");
        assert_eq!(unsafe { native.read() }, unsafe { (&*probe.args)[1] });
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
        let mut probe = Probe { code: &code, env: &mut env, args: &*args, calls: 0, moved: false, visited: 0, native_visits: 0, frames: 0 };
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

    /// The cold transfer-preparation route signals and allocates after the
    /// helper's own publication has ended with its Rust frame. It must publish
    /// the equivalent image the capture stub built for the same caller.
    static COLD_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static COLD_VISITED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn observe_cold_publication() {
        use std::sync::atomic::Ordering::SeqCst;
        let current = ACTIVE.with(Cell::get);
        assert!(!current.is_null(), "publish before cold preparation can signal");
        COLD_CALLS.fetch_add(1, SeqCst);
        force_minor_gc();
        COLD_VISITED.fetch_add(chain_visited(current), SeqCst);
    }

    #[test]
    fn cold_transfer_preparation_publishes_before_it_signals() {
        use std::sync::atomic::Ordering::SeqCst;
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let token = crate::cli::next_control_token("PUBLICATION-COLD");
        let tag = reader::read_from_string(":publication-cold-tag").unwrap().0;
        env.catch_stack.push((tag, token.clone()));
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(
            "((progn (let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again))))
                     (throw :publication-cold-tag x)))"
        ).unwrap().0);
        let body = Arc::new(
            compile_function("PUBLICATION-COLD", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).unwrap();
        egcl_rt::rooted!(args = vec![crate::cli::arena_cons(T, NIL)]);
        COLD_CALLS.store(0, SeqCst);
        COLD_VISITED.store(0, SeqCst);
        OBSERVE_COLD.with(|slot| slot.set(Some(observe_cold_publication)));
        let result = code.run(&args, &mut env);
        OBSERVE_COLD.with(|slot| slot.set(None));
        assert!(ACTIVE.with(Cell::get).is_null(), "retire before the stub discards its image");
        assert!(COLD_CALLS.load(SeqCst) > 0, "the throw must enter the cold route");
        assert!(
            COLD_VISITED.load(SeqCst) > 0,
            "a collection during cold preparation must reach roots through the publication"
        );
        assert!(
            matches!(&result, Err(EgclError::Internal(t)) if t == &token),
            "the throw still reaches its catch: {result:?}"
        );
        assert_eq!(crate::cli::take_control_mv(&token, &mut env), args[0]);
        assert_eq!(crate::cli::cp(args[0]).0, T, "the thrown value survived collection");
        env.catch_stack.pop();
    }

    /// Fibers park inside a live publication; the collector then walks their
    /// chains from another execution and must validate each activation against
    /// that fiber's own managed stack, not the collector's.
    static SUSPENDED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static PARKED_MOVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static PARKED: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
        unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };

    fn park_while_published() {
        if PARKED.with(|parked| parked.replace(true)) {
            return; // park once per fiber, on its first published poll
        }
        let publication = ACTIVE.with(Cell::get);
        assert!(!publication.is_null(), "parked inside a publication");
        let slot = native_only_slot(unsafe { &*publication }).expect("park with a sole native root");
        let before = unsafe { slot.read() }.to_raw();
        SUSPENDED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        egcl_rt::sync::fiber_sleep(std::time::Duration::from_millis(400)).unwrap();
        if unsafe { slot.read() }.to_raw() != before {
            PARKED_MOVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn publication_fiber() -> EgclVal {
        // Match thread_entry_runner: workers share initialized classes/packages.
        let mut env = Env::new_impl(false, false, false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(forms = reader::read_from_string(
            "((let ((x (cons 314159 nil))) (values nil)
                (let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again)))) x))"
        ).unwrap().0);
        let body = Arc::new(
            compile_function("PUBLICATION-FIBER", NIL, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).expect("fiber publication body");
        code.run(&[], &mut env).unwrap();
        env.mv.clear();
        force_minor_gc();
        OBSERVE.with(|slot| slot.set(Some(park_while_published)));
        let value = code.run(&[], &mut env).unwrap();
        OBSERVE.with(|slot| slot.set(None));
        assert!(ACTIVE.with(Cell::get).is_null(), "retire before the fiber returns");
        assert_eq!(crate::cli::cp(value), (EgclVal::from_fixnum(314159), NIL), "the private value stayed intact");
        EgclVal::from_fixnum(1)
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn suspended_fiber_publications_are_walked_against_their_own_stack() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        assert!(egcl_rt::native_transfer::is_supported());
        let mut startup = Env::new(false);
        egcl_rt::rooted_ref!(_startup = &mut startup);
        SUSPENDED.store(0, std::sync::atomic::Ordering::SeqCst);
        PARKED_MOVED.store(0, std::sync::atomic::Ordering::SeqCst);
        const FIBERS: usize = 4;
        let group = egcl_rt::SchedulerGroup::init(
            &egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
        for _ in 0..FIBERS {
            let entry = unsafe {
                EgclVal::from_function_ptr(publication_fiber as *const () as *mut u8)
            };
            group.submit(egcl_rt::thread::make_fiber(entry).unwrap()).unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while SUSPENDED.load(std::sync::atomic::Ordering::SeqCst) < 2 {
            egcl_rt::poll_safepoint();
            assert!(
                std::time::Instant::now() < deadline,
                "two fibers must park inside a live publication"
            );
            std::thread::yield_now();
        }
        // Collect from outside the fibers while their publications are live on
        // distinct native and managed stacks.
        take_foreign_walk_counts();
        egcl_rt::HeapCollector::new().minor_gc().unwrap();
        let (walks, visited) = take_foreign_walk_counts();
        assert!(walks >= 2, "walk every suspended execution's chain, saw {walks}");
        assert!(
            visited > 0,
            "activations must validate against each fiber's own stack, not the collector's"
        );
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(1); FIBERS]);
        assert!(PARKED_MOVED.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "both parked fibers must relocate their sole native roots");
    }

    /// Compiled maps must expose stable native-only homes, while retaining
    /// activation copies wherever calls need them for arguments or volatility.
    #[test]
    fn relocatable_values_record_their_real_native_home() {
        let _lock = crate::cli::heap_test_lock()
            .lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        crate::cli::read_eval_all_env(
            "(defun publication-callee (y) y)
             (defun publication-callee2 (a b) (cons a b))", &mut env).unwrap();
        let shapes = [
            // a plain mapped call with a value live across it
            ("MAPPED", "(x)", "((progn (publication-callee x) (car x)))"),
            // a loop poll, the shape every other fixture here uses
            ("POLL", "(x)",
             "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again))) x))"),
            // several simultaneously live heap values across two calls
            ("MANYLIVE", "(x)",
             "((let ((a (cons x x)) (b (cons x nil)) (c (list x x)))
                 (publication-callee2 a b) (publication-callee c) (list a b c)))"),
        ];
        let mut non_activation = 0;
        let mut native_only = 0;
        let mut frames = 0;
        for (name, params, forms) in shapes {
            egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
            egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
            let Some(body) = compile_function(
                &format!("PUBLICATION-SHAPE-{name}"), *params, *forms, &env, false, false)
            else {
                panic!("{name}: fixture must compile");
            };
            let code = TransferCode::compile(Arc::new(body))
                .unwrap_or_else(|| panic!("{name}: fixture must retain native admission"));
            for site in code._native_calls.iter() {
                let described = match code._native_calls.value_map(site.return_offset) {
                    Some(NativeCallValues::Frame(map)) => {
                        frames += 1;
                        for value in map.values().iter().filter(|v| v.may_reference_heap()) {
                            if !value.locations().is_empty() && value.locations().iter().all(|location|
                                matches!(location, NativeValueLocation::Stack(_) | NativeValueLocation::Register(_))) {
                                native_only += 1;
                            }
                        }
                        let mut act = 0;
                        let mut stk = 0;
                        let mut reg = 0;
                        let mut other = 0;
                        for location in map.gc_locations() {
                            match location {
                                NativeValueLocation::Activation(_) => act += 1,
                                NativeValueLocation::Stack(_) => { stk += 1; non_activation += 1 }
                                NativeValueLocation::Register(_) => { reg += 1; non_activation += 1 }
                                _ => other += 1,
                            }
                        }
                        format!("activation={act} stack={stk} register={reg} other={other}")
                    }
                    other => format!("{other:?}"),
                };
                eprintln!(
                    "  {name} off={:<5} base={:?} origin={:?} -> {described}",
                    site.return_offset, site.stack_base, site.origin,
                );
            }
        }
        assert!(frames > 0, "the shapes must produce real value maps");
        assert!(native_only > 0, "stable native homes must not retain redundant shadows");
        assert!(
            non_activation > 0,
            "every heap location is an activation slot again: for_call has gone back to masking \
             the real home, so the published walk is redundant with the EgclStack scan and \
             nothing it does can be proven (bliss-shih7.2.7.3)"
        );
    }

    /// An activation pointer is honoured only when a real frame header backs
    /// it on the owning stack and declares at least the addressed slots.
    #[test]
    fn activation_validation_requires_a_real_frame_with_enough_slots() {
        use egcl_rt::stack::{EgclStack, Frame};
        let stack = EgclStack::new(64 * 1024);
        let frame = stack.push_frame(NIL, std::ptr::null(), 4, 0).expect("push a frame");
        let activation = unsafe { frame.add(1) }.cast::<EgclVal>();

        assert_eq!(validated_activation(activation, 4, &stack), Some(activation),
            "the frame's own slots are addressable");
        assert_eq!(validated_activation(activation, 3, &stack), Some(activation),
            "addressing fewer slots than declared is fine");
        assert_eq!(validated_activation(activation, 5, &stack), None,
            "a map claiming more slots than the header declares is rejected");
        assert_eq!(validated_activation(std::ptr::null_mut(), 1, &stack), None, "null");
        assert_eq!(validated_activation(activation.wrapping_byte_add(1), 1, &stack), None,
            "misaligned");
        assert_eq!(validated_activation(stack.base() as *mut EgclVal, 1, &stack), None,
            "no room for a header below the stack base");
        assert_eq!(validated_activation(stack.sp() as *mut EgclVal, 1, &stack), None,
            "slots must end at or below the stack pointer");
        assert_eq!(validated_activation(usize::MAX as *mut EgclVal, 1, &stack), None,
            "an address whose header or slot area would overflow is rejected");
        // A frame header inside the stack but belonging to no pushed frame is
        // still rejected when it cannot declare the slots.
        let unpushed = unsafe { (stack.base() as *const Frame).add(1) } as usize;
        assert_eq!(validated_activation(
            (unpushed + std::mem::size_of::<Frame>()) as *mut EgclVal, u16::MAX, &stack), None);
    }

    #[test]
    fn activation_validation_rejects_interior_slots_that_look_like_headers() {
        use egcl_rt::stack::EgclStack;
        let stack = EgclStack::new(64 * 1024);
        let frame = stack.push_frame(NIL, std::ptr::null(), 4, 0).unwrap();
        let activation = unsafe { frame.add(1) }.cast::<EgclVal>();
        // At a header shifted by one slot, num_locals reads these high bits.
        // The address and full claimed extent are inside the owning stack.
        unsafe { activation.write(EgclVal::from_fixnum(1 << 29)) };
        let interior = unsafe { activation.add(1) };
        assert_eq!(validated_activation(activation, 4, &stack), Some(activation));
        assert_eq!(validated_activation(interior, 1, &stack), None,
            "in-bounds bytes resembling a header do not identify a linked frame");
    }

    #[test]
    fn activation_cursor_accepts_linked_frames_and_repeated_alias_lookups() {
        use egcl_rt::stack::EgclStack;
        let stack = EgclStack::new(64 * 1024);
        let older = stack.push_frame(NIL, std::ptr::null(), 4, 0).unwrap();
        let younger = stack.push_frame(NIL, std::ptr::null(), 2, 0).unwrap();
        let older = unsafe { older.add(1) }.cast::<EgclVal>();
        let younger = unsafe { younger.add(1) }.cast::<EgclVal>();
        let walk = Walk::new(0..0, &stack);
        for activation in [younger, younger, older, older, younger, older] {
            assert_eq!(walk.activation(activation, 2), Some(activation));
        }
        assert_eq!(walk.activation(younger, 3), None);
        assert_eq!(walk.activation(older, 4), Some(older));
        unsafe { older.write(EgclVal::from_fixnum(1 << 29)) };
        assert_eq!(walk.activation(unsafe { older.add(1) }, 1), None);
        assert_eq!(walk.activation(older, 4), Some(older));
    }

    /// A publication the walker cannot trust must end its chain without
    /// dereferencing anything, while the real chain keeps working.
    fn observe_malformed() {
        let probe = probe();
        let real = ACTIVE.with(Cell::get);
        let published = unsafe { &*real };
        let code = unsafe { &*published.owner };
        let malformed = |owner, pc, bounds, stack| PublishedBoundary {
            owner,
            cursor: NativeFrameCursor { pc, ..published.cursor },
            bounds,
            segment: published.segment,
            stack,
            // Keep the genuine publication live: the malformed head must
            // visit nothing, but GC must still relocate the native aliases
            // this test's executing frame will compare and reload later.
            previous: real,
            visited: Cell::new(0),
        };
        let past_the_code = code.code.as_ptr() as usize + code.code_len;
        let mut cases = [
            ("PC outside the owner", Bail::PcOutsideOwner,
                malformed(published.owner, past_the_code, published.bounds.clone(), published.stack)),
            ("empty stack bounds", Bail::UnwindRefused,
                malformed(published.owner, published.cursor.pc, 0..0, published.stack)),
            ("no owner", Bail::NullOwner,
                malformed(std::ptr::null(), published.cursor.pc, published.bounds.clone(), published.stack)),
            ("no managed stack", Bail::NullStack,
                malformed(published.owner, published.cursor.pc, published.bounds.clone(), std::ptr::null())),
        ];
        for (label, reason, boundary) in &mut cases {
            ACTIVE.with(|slot| slot.set(boundary));
            take_completeness();
            with_expected_bail(*reason, force_minor_gc);
            // Positive control for the accounting itself: a malformed
            // publication must not merely visit nothing, it must RECORD why it
            // declined. Silence here would mean the bail counters cannot see
            // the very paths that become dropped roots after a shadow is
            // dropped.
            let (_, _, bails) = take_completeness();
            ACTIVE.with(|slot| slot.set(real));
            assert_eq!(boundary.visited.get(), 0, "{label}: the walk must stop without guessing");
            let mut expected = [0; BAIL_KINDS];
            // Exactly the malformed head in the mark and relocation scans;
            // an unrelated bad publication cannot hide behind this allowance.
            expected[*reason as usize] = 2;
            assert_eq!(bails, expected,
                "{label}: only the deliberate malformed publication may decline");
            eprintln!("  {label}: bails=[{}]", describe_bails(bails));
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
        let mut probe = Probe { code: &code, env: &mut env, args: &*args, calls: 0, moved: false, visited: 0, native_visits: 0, frames: 0 };
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
