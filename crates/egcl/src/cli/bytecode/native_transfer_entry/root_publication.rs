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
/// location the map said was addressable. Every one of these is harmless only
/// while the activation shadows still cover the roots: once a value's sole
/// home is published, each becomes a silently dropped live root.
#[cfg(test)]
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
}

#[cfg(test)]
const BAIL_KINDS: usize = 8;
#[cfg(test)]
static BAILS: [std::sync::atomic::AtomicUsize; BAIL_KINDS] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; BAIL_KINDS];

#[cfg(test)]
fn note_bail(bail: Bail) {
    BAILS[bail as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Locations the maps said were addressable, and how many the walk actually
/// resolved. These must be equal: a shortfall is a root the walk would have
/// dropped if the shadows were not still covering it.
#[cfg(test)]
static EXPECTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static RESOLVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// (expected, resolved, bails per kind) since the last call.
#[cfg(test)]
pub(super) fn take_completeness() -> (usize, usize, [usize; BAIL_KINDS]) {
    use std::sync::atomic::Ordering::Relaxed;
    (
        EXPECTED.swap(0, Relaxed),
        RESOLVED.swap(0, Relaxed),
        std::array::from_fn(|i| BAILS[i].swap(0, Relaxed)),
    )
}

/// Describe any nonzero bail counts, for a test failure message.
#[cfg(test)]
pub(super) fn describe_bails(bails: [usize; BAIL_KINDS]) -> String {
    const NAMES: [&str; BAIL_KINDS] = [
        "NullOwner", "NullSegment", "NullStack", "PcOutsideOwner",
        "UnwindRefused", "MapUnavailable", "MapMissing", "LocationUnresolved",
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
    // Fail CLOSED. Passing None here would let publish_with fall back to
    // CAPTURE's owner, which during a live child is the CHILD's code while this
    // cursor is the parent's PC. In release, where the owner identity check is
    // only a debug_assert, that misattribution would index the wrong value map
    // and hand the collector an arbitrary machine word -- an unboxed double in
    // r13, say -- as a heap reference. A null owner makes the walk decline the
    // boundary instead, which costs coverage the shadows still provide.
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
        let walk = Walk { bounds: published.bounds.clone(), stack: unsafe { &*published.stack } };
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
/// or missing map ends that chain rather than guessing.
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
}

impl Walk<'_> {
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

/// Accept an activation pointer only as the slot area of a real frame on this
/// execution's managed stack: a `Frame` header must precede it inside the
/// stack, the addressed slots must end at or below the stack pointer, and the
/// header must itself declare at least that many slots. Nothing is
/// dereferenced until all of that holds.
fn validated_activation(
    activation: *mut EgclVal,
    slots: u16,
    stack: &egcl_rt::stack::EgclStack,
) -> Option<*mut EgclVal> {
    use egcl_rt::stack::Frame;
    let address = activation as usize;
    if address % std::mem::align_of::<Frame>() != 0 {
        return None;
    }
    let header = address.checked_sub(std::mem::size_of::<Frame>())?;
    let end = address.checked_add(usize::from(slots) * std::mem::size_of::<EgclVal>())?;
    if header < stack.base() as usize || end > stack.sp() as usize {
        return None;
    }
    // SAFETY: the header lies wholly inside this execution's managed stack.
    (unsafe { (*(header as *const Frame)).num_locals } >= slots).then_some(activation)
}

/// Returns the number of root slots visited through this boundary's frames.
unsafe fn walk_boundary(
    boundary: &PublishedBoundary,
    visit: &mut dyn FnMut(*mut EgclVal),
) -> usize {
    if boundary.owner.is_null() || boundary.segment.is_null() || boundary.stack.is_null() {
        #[cfg(test)]
        note_bail(if boundary.owner.is_null() {
            Bail::NullOwner
        } else if boundary.segment.is_null() {
            Bail::NullSegment
        } else {
            Bail::NullStack
        });
        return 0;
    }
    let code = unsafe { &*boundary.owner };
    let base = code.code.as_ptr() as usize;
    let segment = unsafe { &*boundary.segment };
    let walk = Walk {
        bounds: boundary.bounds.clone(),
        stack: unsafe { &*boundary.stack },
    };
    let mut cursor = boundary.cursor;
    let mut total = 0;
    loop {
        let Some(offset) = cursor.pc.checked_sub(base).filter(|offset| *offset < code.code_len)
        else {
            #[cfg(test)]
            note_bail(Bail::PcOutsideOwner);
            return total;
        };
        let Some(step) =
            code._native_calls
                .unwind(base, &cursor, walk.bounds.clone(), |address| walk.word(address))
        else {
            #[cfg(test)]
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
                #[cfg(test)]
                note_bail(Bail::MapUnavailable);
                return total;
            }
            None => {
                #[cfg(test)]
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
            .map(|body_sp| body_sp.wrapping_add_signed(offset as isize))
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
            .word(body_sp.wrapping_add(slot as usize * 8))
            .and_then(|activation| {
                validated_activation(activation as *mut EgclVal, layout.activation_slots, walk.stack)
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
    let layout = map.layout();
    let activation = unsafe { frame_activation(map, body_sp, walk) };
    let mut visited = 0;
    for location in map.gc_locations() {
        let resolved = unsafe {
            resolve_location(*location, cursor, body_sp, activation, layout.activation_slots, walk)
        };
        // Constant and Unavailable are legitimately not addressable; every
        // other kind the map records is one the walk is obliged to reach.
        #[cfg(test)]
        if !matches!(
            location,
            NativeValueLocation::Constant(_) | NativeValueLocation::Unavailable
        ) {
            use std::sync::atomic::Ordering::Relaxed;
            EXPECTED.fetch_add(1, Relaxed);
            if resolved.is_some() {
                RESOLVED.fetch_add(1, Relaxed);
            } else {
                note_bail(Bail::LocationUnresolved);
            }
        }
        let Some(slot) = resolved else { continue };
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering::Relaxed;
            match *location {
                NativeValueLocation::Activation(_) => &VISIT_ACTIVATION,
                NativeValueLocation::Stack(_) => &VISIT_STACK,
                NativeValueLocation::Register(_) => &VISIT_REGISTER,
                _ => unreachable!("filtered above"),
            }
            .fetch_add(1, Relaxed);
        }
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
        // Oracle on the walk's address arithmetic: a value's shadow slot and
        // its native home must agree before anything collects.
        let (checked, dual, mismatches) = unsafe { verify_dual_homes() };
        eprintln!("  dual-home check: values={checked} dual-homed={dual} mismatches={mismatches}");
        assert_eq!(mismatches, 0, "a value's shadow and native home disagree");
        assert!(dual > 0, "the fixture must exercise at least one dual-homed value");
        take_visits_by_kind();
        take_completeness();
        force_minor_gc();
        let (activation, stack, register) = take_visits_by_kind();
        // Every location the maps declared addressable must have been reached.
        // A shortfall here is a root the walk would have dropped silently if
        // the activation shadows were not still covering it.
        let (expected, resolved, bails) = take_completeness();
        eprintln!("  completeness: expected={expected} resolved={resolved} bails=[{}]",
                  describe_bails(bails));
        assert!(expected > 0, "the collection must reach addressable locations");
        assert_eq!(expected, resolved,
                   "the walk failed to resolve a location its own map declared addressable");
        assert_eq!(bails.iter().sum::<usize>(), 0,
                   "the walk bailed: [{}]", describe_bails(bails));
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
    static PARKED: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
        unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };

    fn park_while_published() {
        if PARKED.with(|parked| parked.replace(true)) {
            return; // park once per fiber, on its first published poll
        }
        assert!(!ACTIVE.with(Cell::get).is_null(), "parked inside a publication");
        SUSPENDED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        egcl_rt::sync::fiber_sleep(std::time::Duration::from_millis(400)).unwrap();
    }

    fn publication_fiber() -> EgclVal {
        // Match thread_entry_runner: workers share initialized classes/packages.
        let mut env = Env::new_impl(false, false, false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(
            "((let ((n 0)) (tagbody again (setq n (+ n 1)) (if (< n 3) (go again))) x))"
        ).unwrap().0);
        let body = Arc::new(
            compile_function("PUBLICATION-FIBER", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).expect("fiber publication body");
        egcl_rt::rooted!(args = vec![crate::cli::arena_cons(T, NIL)]);
        OBSERVE.with(|slot| slot.set(Some(park_while_published)));
        let value = code.run(&args, &mut env).unwrap();
        OBSERVE.with(|slot| slot.set(None));
        assert_eq!(value, args[0]);
        assert!(ACTIVE.with(Cell::get).is_null(), "retire before the fiber returns");
        assert_eq!(crate::cli::cp(args[0]).0, T, "the parked input stayed intact");
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
    }

    /// Every relocatable value records its real native home alongside its
    /// shadow slot, so the published walk can reach a word the EgclStack scan
    /// does not.
    ///
    /// This did not hold before the home was unmasked: `for_call`'s
    /// `Home(_) if shadow.is_some()` arm masked the `Stack`/`Register` arms and
    /// a moving value is required to have a shadow, so every heap location was
    /// an `Activation` slot the managed-stack scan already visited and the walk
    /// was redundant by construction. A drop back to zero means the masking has
    /// returned and the walk has gone redundant again, which would make the
    /// crossing's negative control unwritable (bliss-shih7.2.7.3.2.1) and
    /// shadow removal unjustifiable (bliss-shih7.2.7.3).
    ///
    /// This does NOT by itself license dropping a shadow: both words are live
    /// and must agree, and the completeness accounting that would let the walk
    /// be the sole mechanism is still missing.
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
        let mut frames = 0;
        for (name, params, forms) in shapes {
            egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
            egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
            let Some(body) = compile_function(
                &format!("PUBLICATION-SHAPE-{name}"), *params, *forms, &env, false, false)
            else {
                panic!("{name}: fixture must compile");
            };
            let Some(code) = TransferCode::compile(Arc::new(body)) else {
                eprintln!("  {name}: not admitted for native transfer, skipped");
                continue;
            };
            for site in code._native_calls.iter() {
                let described = match code._native_calls.value_map(site.return_offset) {
                    Some(NativeCallValues::Frame(map)) => {
                        frames += 1;
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
            previous: std::ptr::null_mut(),
            visited: Cell::new(0),
        };
        let past_the_code = code.code.as_ptr() as usize + code.code_len;
        let mut cases = [
            ("PC outside the owner",
                malformed(published.owner, past_the_code, published.bounds.clone(), published.stack)),
            ("empty stack bounds",
                malformed(published.owner, published.cursor.pc, 0..0, published.stack)),
            ("no owner",
                malformed(std::ptr::null(), published.cursor.pc, published.bounds.clone(), published.stack)),
            ("no managed stack",
                malformed(published.owner, published.cursor.pc, published.bounds.clone(), std::ptr::null())),
        ];
        for (label, boundary) in &mut cases {
            ACTIVE.with(|slot| slot.set(boundary));
            take_completeness();
            force_minor_gc();
            // Positive control for the accounting itself: a malformed
            // publication must not merely visit nothing, it must RECORD why it
            // declined. Silence here would mean the bail counters cannot see
            // the very paths that become dropped roots after a shadow is
            // dropped.
            let (_, _, bails) = take_completeness();
            ACTIVE.with(|slot| slot.set(real));
            assert_eq!(boundary.visited.get(), 0, "{label}: the walk must stop without guessing");
            assert!(bails.iter().sum::<usize>() > 0,
                    "{label}: the walk declined without recording a reason");
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
