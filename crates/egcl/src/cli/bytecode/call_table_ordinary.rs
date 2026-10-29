// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary entry ownership is execution-local, but stack walkers receive an
//! explicit directory for the execution they are inspecting. No hot machine
//! call allocates a managed activation or increments a Rust reference count.

use super::{CallCell, linkage};
use egcl_rt::call_table::{OrdinaryCallHeat, OrdinaryEntryDescriptor};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::super::native_transfer_entry::TransferCode;

struct RetainedEntry<C> {
    descriptor: OrdinaryEntryDescriptor,
    // Past policies can still be loaded by suspended native activations.
    #[allow(clippy::vec_box)]
    heat: Vec<Box<OrdinaryCallHeat>>,
    cell: Arc<CallCell>,
    code: Rc<C>,
    callable: Option<Arc<egcl_rt::CrossThreadRoot<egcl_rt::value::EgclVal>>>,
}

// Every descriptor field participates, just as in its derived equality.
// Indices/pointers are metadata only; ownership remains exclusively in entries.
#[derive(Debug, PartialEq, Eq, Hash)]
struct RetainedKey {
    cell: usize,
    wide: [u64; 4],
    native: [usize; 6],
}

impl RetainedKey {
    fn new(cell: &Arc<CallCell>, d: OrdinaryEntryDescriptor) -> Self {
        Self {
            cell: Arc::as_ptr(cell) as usize,
            wide: [d.kind, d.version, d.revision, d.environment],
            native: [
                d.entry,
                d.code_start,
                d.code_len,
                d.arity,
                d.owner,
                d.stack_required,
            ],
        }
    }
}

// Disjoint address clusters. Identical shared ranges stay uniform and need
// one index lookup regardless of the number of callable/descriptor aliases.
// Nonidentical overlapping ranges retain exact first-insertion semantics by
// searching only their cluster's insertion-ordered members.
struct PcRange {
    end: usize,
    uniform: bool,
    members: Vec<usize>,
}

struct RetainedEntries<C> {
    segments: usize,
    // Descriptor addresses are embedded in atomic slots and stay fixed while
    // publication/redefinition grows this directory.
    #[allow(clippy::vec_box)]
    entries: Vec<Box<RetainedEntry<C>>>,
    by_descriptor: HashMap<RetainedKey, usize>,
    by_pc: BTreeMap<usize, PcRange>,
    #[cfg(test)]
    lookup_candidates: std::cell::Cell<usize>,
    #[cfg(test)]
    duplicate_queries: usize,
    /// The publication epoch and entry count the last completed sweep saw.
    /// While both are unchanged a sweep would find every entry still
    /// published, so the outermost segment exit skips it (GitHub #487).
    swept: Option<(u64, usize)>,
}

impl<C> Default for RetainedEntries<C> {
    fn default() -> Self {
        Self {
            segments: 0,
            entries: Vec::new(),
            by_descriptor: HashMap::new(),
            by_pc: BTreeMap::new(),
            #[cfg(test)]
            lookup_candidates: std::cell::Cell::new(0),
            #[cfg(test)]
            duplicate_queries: 0,
            swept: None,
        }
    }
}

impl<C> RetainedEntries<C> {
    fn begin_segment(&mut self) {
        self.segments = self
            .segments
            .checked_add(1)
            .expect("native segment depth overflow");
    }

    fn finish_segment(&mut self) {
        self.segments = self
            .segments
            .checked_sub(1)
            .expect("unbalanced native segment retirement");
        if self.swept != Some((linkage::ordinary_epoch(), self.entries.len())) {
            self.sweep();
        }
    }

    #[cfg(test)]
    fn retain(
        &mut self,
        cell: Arc<CallCell>,
        descriptor: OrdinaryEntryDescriptor,
        code: Rc<C>,
    ) -> Option<*const OrdinaryEntryDescriptor> {
        self.retain_with_callable(cell, descriptor, code, None)
    }

    fn retain_with_callable(
        &mut self,
        cell: Arc<CallCell>,
        descriptor: OrdinaryEntryDescriptor,
        code: Rc<C>,
        callable: Option<Arc<egcl_rt::CrossThreadRoot<egcl_rt::value::EgclVal>>>,
    ) -> Option<*const OrdinaryEntryDescriptor> {
        if !descriptor.is_compatible() || descriptor.owner != Rc::as_ptr(&code) as usize {
            return None;
        }
        #[cfg(test)]
        {
            self.duplicate_queries += 1;
        }
        if let Some(&index) = self.by_descriptor.get(&RetainedKey::new(&cell, descriptor)) {
            return Some(&self.entries[index].descriptor);
        }
        let entry = Box::new(RetainedEntry {
            descriptor,
            heat: Vec::new(),
            cell,
            code,
            callable,
        });
        let pointer = std::ptr::from_ref(&entry.descriptor);
        self.entries.push(entry);
        self.index_entry(self.entries.len() - 1);
        Some(pointer)
    }

    fn index_entry(&mut self, index: usize) {
        let entry = &self.entries[index];
        let d = entry.descriptor;
        self.by_descriptor
            .insert(RetainedKey::new(&entry.cell, d), index);
        let mut start = d.code_start;
        let mut end = start
            .checked_add(d.code_len)
            .expect("compatible descriptor range");
        if let Some(group) = self.by_pc.get_mut(&start)
            && group.uniform
            && group.end == end
        {
            group.members.push(index);
            return;
        }
        let mut members = vec![index];
        let mut uniform = true;
        let previous = self
            .by_pc
            .range(..=start)
            .next_back()
            .filter(|(_, group)| group.end > start)
            .map(|(&key, _)| key);
        if let Some(key) = previous {
            let group = self.by_pc.remove(&key).unwrap();
            start = key;
            end = end.max(group.end);
            members.extend(group.members);
            uniform = false;
        }
        loop {
            let next = self.by_pc.range(start..end).next().map(|(&key, _)| key);
            let Some(key) = next else {
                break;
            };
            let group = self.by_pc.remove(&key).unwrap();
            end = end.max(group.end);
            members.extend(group.members);
            uniform = false;
        }
        if !uniform {
            members.sort_unstable();
        }
        self.by_pc.insert(
            start,
            PcRange {
                end,
                uniform,
                members,
            },
        );
    }

    fn first_index_for_pc(&self, pc: usize) -> Option<usize> {
        let (_, group) = self.by_pc.range(..=pc).next_back()?;
        if pc >= group.end {
            return None;
        }
        if group.uniform {
            #[cfg(test)]
            self.lookup_candidates.set(self.lookup_candidates.get() + 1);
            return group.members.first().copied();
        }
        group.members.iter().copied().find(|&index| {
            #[cfg(test)]
            self.lookup_candidates.set(self.lookup_candidates.get() + 1);
            self.entries[index].descriptor.contains_pc(pc)
        })
    }

    fn sweep(&mut self) {
        if self.segments != 0 {
            return;
        }
        let epoch = linkage::ordinary_epoch();
        let before = self.entries.len();
        self.entries.retain(|entry| {
            let published =
                unsafe { &*entry.cell.ordinary_entry_address() }.load(Ordering::Acquire);
            published == std::ptr::from_ref(&entry.descriptor) as usize
        });
        if self.entries.len() != before {
            self.by_descriptor.clear();
            self.by_pc.clear();
            for index in 0..self.entries.len() {
                self.index_entry(index);
            }
        }
        self.swept = Some((epoch, self.entries.len()));
    }

    fn code_for_pc(&self, pc: usize) -> Option<Rc<C>> {
        self.first_index_for_pc(pc)
            .map(|index| Rc::clone(&self.entries[index].code))
    }
}

/// Borrow only for the duration of lookup. The explicit handle may be read by
/// a collector/debugger while its owning execution is stopped, just like the
/// existing PublishedBoundary owner; it must not be read concurrently with it.
pub(in crate::cli::bytecode) struct RootIdentity {
    owner: *const TransferCode,
    start: usize,
    len: usize,
    callable: Arc<egcl_rt::CrossThreadRoot<egcl_rt::value::EgclVal>>,
}

pub(in crate::cli::bytecode) struct OrdinaryCodeDirectory {
    roots: RefCell<Vec<Rc<RootIdentity>>>,
    retained: RefCell<RetainedEntries<TransferCode>>,
}

impl OrdinaryCodeDirectory {
    #[cfg(test)]
    pub(in crate::cli::bytecode) fn for_walk_test(
        codes: &[(Rc<TransferCode>, usize, usize)],
    ) -> Self {
        let mut retained = RetainedEntries::default();
        for (code, start, len) in codes {
            // Test-only ownership registration. Legacy fixture entries are
            // never published or executed through the ordinary ABI.
            let descriptor = OrdinaryEntryDescriptor {
                kind: linkage::ORDINARY_X64_LISP_KIND,
                version: linkage::ORDINARY_X64_LISP_VERSION,
                revision: 0,
                entry: *start,
                code_start: *start,
                code_len: *len,
                arity: 0,
                environment: linkage::ORDINARY_ENV_NONE,
                owner: Rc::as_ptr(code) as usize,
                stack_required: 8,
            };
            assert!(
                retained
                    .retain(
                        Arc::new(CallCell::new(0, 11, 12)),
                        descriptor,
                        Rc::clone(code)
                    )
                    .is_some()
            );
        }
        Self {
            retained: RefCell::new(retained),
            roots: RefCell::new(Vec::new()),
        }
    }

    /// Clone handles under the directory borrow; every GC-gated read happens
    /// afterward because a Running mutator may park while acquiring that gate.
    fn accepts_callable(&self, code: &TransferCode, function: egcl_rt::value::EgclVal) -> bool {
        self.accepts_callable_root(code, function, None)
    }

    /// [`Self::accepts_callable`], given `callable`, a root already known to
    /// hold `function`. A handle that IS that root agrees without a GC-gated
    /// read, so the steady state of one function calling through its own
    /// published code takes no lock and allocates nothing (GitHub #487).
    fn accepts_callable_root(
        &self,
        code: &TransferCode,
        function: egcl_rt::value::EgclVal,
        callable: Option<&Arc<egcl_rt::CrossThreadRoot<egcl_rt::value::EgclVal>>>,
    ) -> bool {
        if callable.is_some_and(|callable| self.held_only_by(code, callable)) {
            return true;
        }
        egcl_rt::rooted!(function = function);
        let mut retained: Vec<_> = self
            .retained
            .borrow()
            .entries
            .iter()
            .filter(|entry| std::ptr::eq(Rc::as_ptr(&entry.code), code))
            .map(|entry| entry.callable.clone())
            .collect();
        retained.extend(
            self.roots
                .borrow()
                .iter()
                .filter(|root| std::ptr::eq(root.owner, code))
                .map(|root| Some(Arc::clone(&root.callable))),
        );
        retained.into_iter().all(|root| {
            root.is_some_and(|root| root.with_gc_stable_mutator(|value| *value == *function))
        })
    }

    /// Whether every handle constraining `code`'s identity is `callable`
    /// itself. Pointer comparisons only: no gate, no allocation, no parking.
    fn held_only_by(
        &self,
        code: &TransferCode,
        callable: &Arc<egcl_rt::CrossThreadRoot<egcl_rt::value::EgclVal>>,
    ) -> bool {
        self.retained
            .borrow()
            .entries
            .iter()
            .filter(|entry| std::ptr::eq(Rc::as_ptr(&entry.code), code))
            .all(|entry| {
                entry
                    .callable
                    .as_ref()
                    .is_some_and(|root| Arc::ptr_eq(root, callable))
            })
            && self
                .roots
                .borrow()
                .iter()
                .filter(|root| std::ptr::eq(root.owner, code))
                .all(|root| Arc::ptr_eq(&root.callable, callable))
    }

    pub(in crate::cli::bytecode) fn callable_for_pc(
        &self,
        pc: usize,
    ) -> Option<Arc<egcl_rt::CrossThreadRoot<egcl_rt::value::EgclVal>>> {
        {
            let retained = self.retained.borrow();
            retained
                .first_index_for_pc(pc)
                .and_then(|index| retained.entries[index].callable.clone())
        }
        .or_else(|| {
            self.roots
                .borrow()
                .iter()
                .find(|root| {
                    pc.checked_sub(root.start)
                        .is_some_and(|offset| offset < root.len)
                })
                .map(|root| Arc::clone(&root.callable))
        })
    }

    pub(in crate::cli::bytecode) fn code_for_pc(&self, pc: usize) -> Option<Rc<TransferCode>> {
        self.retained.borrow().code_for_pc(pc)
    }
}

static DIRECTORY: egcl_rt::execution_local::ExecutionLocal<Rc<OrdinaryCodeDirectory>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| {
        Rc::new(OrdinaryCodeDirectory {
            retained: RefCell::new(RetainedEntries::default()),
            roots: RefCell::new(Vec::new()),
        })
    })
};

/// Capture on the publishing execution, never during a foreign stack walk.
/// The enclosing segment lease keeps retired owners until every frame returns.
pub(in crate::cli::bytecode) fn active_ordinary_directory() -> Rc<OrdinaryCodeDirectory> {
    DIRECTORY.with(|directory| {
        assert!(
            directory.retained.borrow().segments > 0,
            "native publication needs a segment lease"
        );
        Rc::clone(directory)
    })
}

/// Owns a lease for the whole segment, including suspension and Rust callbacks.
/// Drop uses the saved directory, never whichever execution is currently cached.
pub(in crate::cli::bytecode) struct OrdinarySegmentGuard {
    directory: Rc<OrdinaryCodeDirectory>,
}

/// The borrowed code remains alive through machine execution and cold recovery.
/// Retain its callable independently of named-cell publication or replacement.
pub(in crate::cli::bytecode) struct OrdinaryRootGuard<'a> {
    directory: Rc<OrdinaryCodeDirectory>,
    identity: Option<Rc<RootIdentity>>,
    code: &'a TransferCode,
}

impl Drop for OrdinaryRootGuard<'_> {
    fn drop(&mut self) {
        let Some(identity) = self.identity.take() else {
            return;
        };
        {
            let mut roots = self.directory.roots.borrow_mut();
            // Guards nest, so this is nearly always the newest root.
            if roots.last().is_some_and(|root| Rc::ptr_eq(root, &identity)) {
                roots.pop();
            } else {
                roots.retain(|root| !Rc::ptr_eq(root, &identity));
            }
        }
        // No longer a root of this directory: keep it for the next call.
        if Rc::strong_count(&identity) == 1 {
            self.code.store_spare_root_identity(identity);
        }
    }
}

impl OrdinarySegmentGuard {
    pub(in crate::cli::bytecode) fn retain_root<'a>(
        &self,
        code: &'a TransferCode,
        function: egcl_rt::value::EgclVal,
    ) -> Option<OrdinaryRootGuard<'a>> {
        let (start, len) = code.ordinary_code_bounds()?;
        // Steady state: the same pinned function as the previous activation,
        // whose released identity is still spare. Pointer checks only, so no
        // root, gate, reference count or allocation (GitHub #487).
        let reused = code
            .with_pinned_callable_root(function, |callable| {
                if !self.directory.held_only_by(code, callable) {
                    return None;
                }
                code.take_spare_root_identity().filter(|identity| {
                    std::ptr::eq(identity.owner, code)
                        && identity.start == start
                        && identity.len == len
                        && Arc::ptr_eq(&identity.callable, callable)
                })
            })
            .flatten();
        if let Some(identity) = reused {
            self.directory.roots.borrow_mut().push(Rc::clone(&identity));
            return Some(OrdinaryRootGuard {
                directory: Rc::clone(&self.directory),
                identity: Some(identity),
                code,
            });
        }
        egcl_rt::rooted!(function = function);
        let cached = code.cached_callable_root(&function);
        if !self
            .directory
            .accepts_callable_root(code, *function, cached.as_ref())
        {
            return None;
        }
        let callable = cached.unwrap_or_else(|| code.callable_root(*function));
        // The previous activation's identity, when it names the same root and
        // code range, saves an allocation and free on every call.
        let identity = code
            .take_spare_root_identity()
            .filter(|identity| {
                std::ptr::eq(identity.owner, code)
                    && identity.start == start
                    && identity.len == len
                    && Arc::ptr_eq(&identity.callable, &callable)
            })
            .unwrap_or_else(|| {
                #[cfg(test)]
                ROOT_IDENTITIES.with(|count| count.set(count.get() + 1));
                Rc::new(RootIdentity {
                    owner: std::ptr::from_ref(code),
                    start,
                    len,
                    callable,
                })
            });
        self.directory.roots.borrow_mut().push(Rc::clone(&identity));
        Some(OrdinaryRootGuard {
            directory: Rc::clone(&self.directory),
            identity: Some(identity),
            code,
        })
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "ordinary entry consumers follow in bliss-t8a48")
    )]
    pub(in crate::cli::bytecode) fn directory(&self) -> Rc<OrdinaryCodeDirectory> {
        Rc::clone(&self.directory)
    }
}

impl Drop for OrdinarySegmentGuard {
    fn drop(&mut self) {
        self.directory.retained.borrow_mut().finish_segment();
    }
}

pub(in crate::cli::bytecode) fn enter_ordinary_segment() -> OrdinarySegmentGuard {
    DIRECTORY.with(|directory| {
        directory.retained.borrow_mut().begin_segment();
        OrdinarySegmentGuard {
            directory: Rc::clone(directory),
        }
    })
}

/// Install ownership before publishing the descriptor address. The caller has
/// already validated the emitted code and provided its true code range/entry.
/// Failed revision checks discard only unreferenced quiescent versions.
///
/// # Safety
/// `cell` identifies this execution's live CallCell; descriptor entry/range and
/// ABI metadata must describe `code` and have passed installation validation.
#[cfg(test)]
pub(in crate::cli::bytecode) unsafe fn publish_ordinary(
    cell: u64,
    descriptor: OrdinaryEntryDescriptor,
    code: Rc<TransferCode>,
    function: egcl_rt::value::EgclVal,
) -> bool {
    unsafe { publish_ordinary_with_heat(cell, descriptor, code, function, None) }
}

/// Publish optional heat metadata under the descriptor's same lifetime lease.
/// Safety requirements are identical to `publish_ordinary`; counters must name
/// pinned fields of `function`, which this directory retains as a traced root.
pub(in crate::cli::bytecode) unsafe fn publish_ordinary_with_heat(
    cell: u64,
    descriptor: OrdinaryEntryDescriptor,
    code: Rc<TransferCode>,
    function: egcl_rt::value::EgclVal,
    heat: Option<OrdinaryCallHeat>,
) -> bool {
    egcl_rt::rooted!(function = function);
    let state = unsafe { super::state(cell) };
    DIRECTORY.with(|directory| {
        if code.needs_callable_identity() && !directory.accepts_callable(&code, *function) {
            return false;
        }
        // Share the code's own callable root, so ordinary entries for the same
        // function recognize this published identity by pointer.
        let callable = code.callable_root(*function);
        let pointer = directory.retained.borrow_mut().retain_with_callable(
            Arc::clone(&state.cell),
            descriptor,
            code,
            Some(callable),
        );
        let Some(pointer) = pointer else { return false };
        // Retention and publication do not allocate Lisp objects or yield.
        let heat_pointer = {
            let mut retained = directory.retained.borrow_mut();
            let index = retained.by_descriptor[&RetainedKey::new(&state.cell, descriptor)];
            let entry = &mut retained.entries[index];
            debug_assert!(std::ptr::eq(&entry.descriptor, pointer));
            if let Some(mut heat) = heat {
                heat.descriptor = pointer as usize;
                if entry.heat.last().is_none_or(|current| **current != heat) {
                    entry.heat.push(Box::new(heat));
                }
                std::ptr::from_ref(&**entry.heat.last().unwrap()) as usize
            } else {
                0
            }
        };
        let published =
            unsafe { linkage::publish_ordinary_with_heat(&state.cell, &*pointer, heat_pointer) };
        directory.retained.borrow_mut().sweep();
        published
    })
}

/// Run with all executions stopped, alongside call-table cache root scanning.
/// TransferCode owns its bytecode/constant roots; callable handles are traced
/// by the cross-thread root scanner independently of directory retirement.
pub(super) unsafe fn sweep_all() {
    unsafe {
        DIRECTORY.scan(|directory| directory.retained.borrow_mut().sweep());
    }
}

#[cfg(test)]
thread_local! {
    static ROOT_IDENTITIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Root identities allocated by `retain_root` on this thread since the last call.
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_root_identities_for_test() -> usize {
    ROOT_IDENTITIES.with(|count| count.replace(0))
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn ordinary_segment_depth_for_test() -> usize {
    DIRECTORY.with(|directory| directory.retained.borrow().segments)
}

/// Exercise actual guarded descriptor publication using a fixture's emitted
/// code. The caller supplies two distinct interpreted callable identities.
#[cfg(test)]
pub(in crate::cli::bytecode) fn assert_guarded_callable_publication_for_test(
    code: Rc<TransferCode>,
    first: egcl_rt::value::EgclVal,
    second: egcl_rt::value::EgclVal,
) {
    egcl_rt::rooted!(first = first);
    egcl_rt::rooted!(second = second);
    assert_ne!(*first, *second);
    assert!(code.ordinary_entry.is_some() && code.has_deopt);
    let symbol = crate::cli::resolve_sym("ORDINARY-IDENTITY-PUBLICATION-FIRST")
        .unwrap()
        .as_symbol_index();
    let other = crate::cli::resolve_sym("ORDINARY-IDENTITY-PUBLICATION-OTHER")
        .unwrap()
        .as_symbol_index();
    let cell = super::resolve(symbol).unwrap();
    let other_cell = super::resolve(other).unwrap();
    linkage::invalidate(symbol);
    linkage::invalidate(other);
    let segment = enter_ordinary_segment();
    // An unpublished root already reserves the exact identity of this code.
    let root = segment
        .retain_root(&code, *first)
        .expect("first root identity");
    assert!(segment.retain_root(&code, *second).is_none());
    let nested_root = segment
        .retain_root(&code, *first)
        .expect("same callable reentry");
    drop(nested_root);
    let root_pc = code.ordinary_descriptor(0).unwrap().entry;
    segment
        .directory
        .callable_for_pc(root_pc)
        .expect("unpublished root lookup")
        .with_gc_stable_mutator(|value| assert_eq!(*value, *first));
    let rejected = code
        .ordinary_descriptor(linkage::revision(other_cell.ordinal()))
        .unwrap();
    assert!(!unsafe {
        publish_ordinary(
            Arc::as_ptr(&other_cell) as u64,
            rejected,
            Rc::clone(&code),
            *second,
        )
    });
    let descriptor = code
        .ordinary_descriptor(linkage::revision(cell.ordinal()))
        .unwrap();
    let address = Arc::as_ptr(&cell) as u64;
    assert!(unsafe { publish_ordinary(address, descriptor, Rc::clone(&code), *first) });
    let retained = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
    assert_ne!(retained, 0);
    let other_descriptor = code
        .ordinary_descriptor(linkage::revision(other_cell.ordinal()))
        .unwrap();
    assert!(!unsafe {
        publish_ordinary(
            Arc::as_ptr(&other_cell) as u64,
            other_descriptor,
            Rc::clone(&code),
            *second,
        )
    });
    assert_eq!(
        unsafe { &*other_cell.ordinary_entry_address() }.load(Ordering::Acquire),
        0
    );
    assert_eq!(
        unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
        retained
    );
    // Redefinition clears the old slot, but the live segment still owns its
    // exact descriptor and callable. Legacy publication cannot bypass the gate.
    linkage::invalidate(symbol);
    let revision = linkage::revision(cell.ordinal());
    let bridges = super::bridge_entries().unwrap();
    assert!(linkage::publish(&cell, revision, bridges[2], bridges[3]));
    assert_eq!(
        unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
        0
    );
    let replacement = code.ordinary_descriptor(revision).unwrap();
    assert!(!unsafe { publish_ordinary(address, replacement, Rc::clone(&code), *second) });
    assert_eq!(
        unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
        0
    );
    assert!(unsafe { publish_ordinary(address, replacement, Rc::clone(&code), *first) });
    assert_ne!(
        unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
        0
    );
    linkage::invalidate(symbol);
    linkage::invalidate(other);
    assert!(segment.retain_root(&code, *second).is_none());
    drop(root);
    drop(segment);
    // Once both the root and retired descriptors disappear, a new identity
    // may enter the same code without inheriting the previous callable.
    let segment = enter_ordinary_segment();
    let replacement_root = segment
        .retain_root(&code, *second)
        .expect("retired identity released");
    segment
        .directory
        .callable_for_pc(root_pc)
        .unwrap()
        .with_gc_stable_mutator(|value| assert_eq!(*value, *second));
    drop(replacement_root);
    drop(segment);
}

/// Descriptors loaded by generated code remain borrowed until all native
/// segments in this execution retire, including suspended Rust callbacks.
pub(super) fn has_active_segments() -> bool {
    DIRECTORY.with(|directory| directory.retained.borrow().segments != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(owner: &Rc<()>, revision: u64, start: usize) -> OrdinaryEntryDescriptor {
        OrdinaryEntryDescriptor {
            kind: linkage::ORDINARY_X64_LISP_KIND,
            version: linkage::ORDINARY_X64_LISP_VERSION,
            revision,
            entry: start + 8,
            code_start: start,
            code_len: 64,
            arity: 0,
            environment: linkage::ORDINARY_ENV_NONE,
            owner: Rc::as_ptr(owner) as usize,
            stack_required: 8,
        }
    }

    #[test]
    fn ordinary_index_key_preserves_every_descriptor_field_and_cell_identity() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let other_cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let original = descriptor(&owner, 1, 0x1000);
        let key = RetainedKey::new(&cell, original);
        assert_ne!(key, RetainedKey::new(&other_cell, original));
        for field in 0..10 {
            let mut changed = original;
            match field {
                0 => changed.kind += 1,
                1 => changed.version += 1,
                2 => changed.revision += 1,
                3 => changed.entry += 1,
                4 => changed.code_start += 1,
                5 => changed.code_len += 1,
                6 => changed.arity += 1,
                7 => changed.environment += 1,
                8 => changed.owner += 1,
                9 => changed.stack_required += 8,
                _ => unreachable!(),
            }
            assert_ne!(
                key,
                RetainedKey::new(&cell, changed),
                "descriptor field {field}"
            );
        }
    }

    #[test]
    fn ordinary_index_matches_linear_reference_for_mixed_overlaps_and_boundaries() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let mut retained = RetainedEntries::default();
        let mut seed = 17u64;
        for revision in 0..200 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = 0x1000 + (seed as usize & 0xfff);
            let mut d = descriptor(&owner, revision, start);
            d.code_len = 16 + ((seed >> 32) as usize & 0x7f);
            retained
                .retain(Arc::clone(&cell), d, Rc::clone(&owner))
                .unwrap();
        }
        for pc in 0xff0..0x2100 {
            let expected = retained
                .entries
                .iter()
                .position(|entry| entry.descriptor.contains_pc(pc));
            assert_eq!(retained.first_index_for_pc(pc), expected, "PC {pc:#x}");
        }
    }

    #[test]
    fn ordinary_index_handles_thirteen_thousand_disjoint_and_shared_entries_without_scan() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let mut retained = RetainedEntries::default();
        const COUNT: usize = 13_000;
        for index in 0..COUNT {
            let d = descriptor(&owner, index as u64, 0x1000 + index * 128);
            let pointer = retained
                .retain(Arc::clone(&cell), d, Rc::clone(&owner))
                .unwrap();
            assert_eq!(
                retained.retain(Arc::clone(&cell), d, Rc::clone(&owner)),
                Some(pointer)
            );
        }
        assert_eq!(retained.entries.len(), COUNT);
        assert_eq!(retained.duplicate_queries, COUNT * 2);
        for index in 0..COUNT {
            let start = 0x1000 + index * 128;
            assert_eq!(retained.first_index_for_pc(start + 1), Some(index));
            assert_eq!(retained.first_index_for_pc(start + 63), Some(index));
            assert_eq!(retained.first_index_for_pc(start + 64), None);
        }
        assert_eq!(retained.lookup_candidates.get(), COUNT * 2);
        // Arbitrarily many descriptors/callable aliases of one exact code
        // range stay in its uniform bucket, rather than becoming an overlap scan.
        for revision in 0..COUNT {
            let d = descriptor(&owner, (COUNT + revision) as u64, 0x1000);
            retained
                .retain(Arc::clone(&cell), d, Rc::clone(&owner))
                .unwrap();
        }
        assert_eq!(retained.by_pc.len(), COUNT);
        assert!(retained.by_pc.get(&0x1000).unwrap().uniform);
        let before = retained.lookup_candidates.get();
        for _ in 0..COUNT {
            assert_eq!(retained.first_index_for_pc(0x1010), Some(0));
        }
        assert_eq!(retained.lookup_candidates.get() - before, COUNT);
        assert_eq!(retained.duplicate_queries, COUNT * 3);
    }

    #[test]
    fn ordinary_index_overlaps_preserve_first_retained_match_without_penalizing_other_ranges() {
        let owner = Rc::new(());
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let mut retained = RetainedEntries::default();
        let mut first = descriptor(&owner, 1, 0x1040);
        first.code_len = 16;
        retained
            .retain(Arc::clone(&cell), first, Rc::clone(&owner))
            .unwrap();
        let mut enclosing = descriptor(&owner, 2, 0x1000);
        enclosing.code_len = 256;
        retained
            .retain(Arc::clone(&cell), enclosing, Rc::clone(&owner))
            .unwrap();
        let mut extension = descriptor(&owner, 3, 0x10f0);
        extension.code_len = 64;
        retained
            .retain(Arc::clone(&cell), extension, Rc::clone(&owner))
            .unwrap();
        retained
            .retain(cell, descriptor(&owner, 4, 0x2000), Rc::clone(&owner))
            .unwrap();
        assert_eq!(retained.by_pc.len(), 2);
        assert!(!retained.by_pc.get(&0x1000).unwrap().uniform);
        assert_eq!(retained.first_index_for_pc(0x1048), Some(0));
        assert_eq!(retained.first_index_for_pc(0x1000), Some(1));
        assert_eq!(retained.first_index_for_pc(0x10ff), Some(1));
        assert_eq!(retained.first_index_for_pc(0x1100), Some(2));
        assert_eq!(retained.first_index_for_pc(0x1130), None);
        let before = retained.lookup_candidates.get();
        assert_eq!(retained.first_index_for_pc(0x2010), Some(3));
        assert_eq!(retained.lookup_candidates.get() - before, 1);
    }

    #[test]
    fn ordinary_index_sweep_reindexes_survivors_without_retaining_dropped_owners() {
        let mut retained = RetainedEntries::default();
        let old = Rc::new(());
        let old_weak = Rc::downgrade(&old);
        let live = Rc::new(());
        let dead_cell = Arc::new(CallCell::new(0, 11, 12));
        let live_cell = Arc::new(CallCell::new(1, 11, 12));
        retained.begin_segment();
        retained
            .retain(dead_cell, descriptor(&old, 1, 0x1000), old)
            .unwrap();
        let d = descriptor(&live, 2, 0x2000);
        let pointer = retained
            .retain(Arc::clone(&live_cell), d, Rc::clone(&live))
            .unwrap();
        unsafe { &*live_cell.ordinary_entry_address() }.store(pointer as usize, Ordering::Release);
        retained.sweep();
        assert!(
            old_weak.upgrade().is_some(),
            "suspended segment retains its exact owner"
        );
        retained.finish_segment();
        assert!(old_weak.upgrade().is_none());
        assert_eq!(retained.first_index_for_pc(0x2010), Some(0));
        assert!(retained.code_for_pc(0x1010).is_none());
        assert_eq!(
            retained.retain(Arc::clone(&live_cell), d, Rc::clone(&live)),
            Some(pointer)
        );
        unsafe { &*live_cell.ordinary_entry_address() }.store(0, Ordering::Release);
        retained.sweep();
        assert!(retained.by_pc.is_empty());
        assert!(retained.by_descriptor.is_empty());
    }

    #[test]
    fn ordinary_explicit_directory_retains_the_exact_compiled_owner() {
        use crate::cli::{Env, reader};
        use egcl_rt::value::NIL;
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(forms = reader::read_from_string("(42)").unwrap().0);
        let body = Arc::new(
            crate::cli::bytecode::compile_function(
                "ORDINARY-DIRECTORY-OWNER",
                NIL,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let code = Rc::new(TransferCode::compile(body).expect("retained code fixture"));
        let owner = Rc::as_ptr(&code) as usize;
        let weak = Rc::downgrade(&code);
        let guard = enter_ordinary_segment();
        let directory = guard.directory();
        // Synthetic PC geometry tests ownership lookup only. This descriptor
        // is never published or executed: the fixture still has the old ABI.
        let descriptor = OrdinaryEntryDescriptor {
            kind: linkage::ORDINARY_X64_LISP_KIND,
            version: linkage::ORDINARY_X64_LISP_VERSION,
            revision: 0,
            entry: 0x1010,
            code_start: 0x1000,
            code_len: 64,
            arity: 0,
            environment: linkage::ORDINARY_ENV_NONE,
            owner,
            stack_required: 8,
        };
        assert!(
            directory
                .retained
                .borrow_mut()
                .retain(Arc::new(CallCell::new(0, 11, 12)), descriptor, code,)
                .is_some()
        );
        let unrelated = OrdinaryCodeDirectory {
            roots: RefCell::new(Vec::new()),
            retained: RefCell::new(RetainedEntries::default()),
        };
        assert!(unrelated.code_for_pc(0x1010).is_none());
        assert_eq!(
            Rc::as_ptr(&directory.code_for_pc(0x1010).unwrap()) as usize,
            owner
        );
        assert!(weak.upgrade().is_some());
        drop(guard);
        assert!(directory.code_for_pc(0x1010).is_none());
        assert!(
            weak.upgrade().is_none(),
            "directory handle alone does not retain retired code"
        );
    }

    #[test]
    fn ordinary_index_first_none_callable_keeps_root_fallback_instead_of_later_entry() {
        use crate::cli::{Env, reader};
        use egcl_rt::value::{NIL, T};
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(forms = reader::read_from_string("(42)").unwrap().0);
        let body = Arc::new(
            crate::cli::bytecode::compile_function(
                "ORDINARY-FIRST-NONE",
                NIL,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let code = Rc::new(TransferCode::compile(body).unwrap());
        let directory = OrdinaryCodeDirectory {
            retained: RefCell::new(RetainedEntries::default()),
            roots: RefCell::new(Vec::new()),
        };
        let descriptor = OrdinaryEntryDescriptor {
            kind: linkage::ORDINARY_X64_LISP_KIND,
            version: linkage::ORDINARY_X64_LISP_VERSION,
            revision: 1,
            entry: 0x1010,
            code_start: 0x1000,
            code_len: 64,
            arity: 0,
            environment: linkage::ORDINARY_ENV_NONE,
            owner: Rc::as_ptr(&code) as usize,
            stack_required: 8,
        };
        let later = Arc::new(egcl_rt::CrossThreadRoot::new(NIL));
        directory
            .retained
            .borrow_mut()
            .retain_with_callable(
                Arc::new(CallCell::new(0, 11, 12)),
                descriptor,
                Rc::clone(&code),
                None,
            )
            .unwrap();
        let mut next = descriptor;
        next.revision += 1;
        directory
            .retained
            .borrow_mut()
            .retain_with_callable(
                Arc::new(CallCell::new(1, 11, 12)),
                next,
                Rc::clone(&code),
                Some(later),
            )
            .unwrap();
        assert!(
            directory.callable_for_pc(0x1010).is_none(),
            "first matching None is authoritative"
        );
        let fallback = Arc::new(egcl_rt::CrossThreadRoot::new(T));
        directory.roots.borrow_mut().push(Rc::new(RootIdentity {
            owner: Rc::as_ptr(&code),
            start: 0x1000,
            len: 64,
            callable: Arc::clone(&fallback),
        }));
        let selected = directory.callable_for_pc(0x1010).unwrap();
        assert!(Arc::ptr_eq(&selected, &fallback));
        drop(directory.retained.borrow_mut()); // no borrow escapes the lookup
    }

    #[test]
    fn ordinary_callable_identity_includes_retired_entries_and_roots_lookup() {
        use crate::cli::{Env, reader};
        use egcl_rt::value::NIL;
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(forms = reader::read_from_string("(42)").unwrap().0);
        let body = Arc::new(
            crate::cli::bytecode::compile_function(
                "ORDINARY-CALLABLE-OWNER",
                NIL,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let code = Rc::new(TransferCode::compile(body).unwrap());
        egcl_rt::rooted!(
            first = crate::cli::read_eval_all_env("(function (lambda () 42))", &mut env).unwrap()
        );
        egcl_rt::rooted!(
            second = crate::cli::read_eval_all_env("(function (lambda () 42))", &mut env).unwrap()
        );
        assert_ne!(*first, *second, "distinct callable identities");
        let directory = OrdinaryCodeDirectory {
            retained: RefCell::new(RetainedEntries::default()),
            roots: RefCell::new(Vec::new()),
        };
        let descriptor = OrdinaryEntryDescriptor {
            kind: linkage::ORDINARY_X64_LISP_KIND,
            version: linkage::ORDINARY_X64_LISP_VERSION,
            revision: 0,
            entry: 0x1010,
            code_start: 0x1000,
            code_len: 64,
            arity: 0,
            environment: linkage::ORDINARY_ENV_NONE,
            owner: Rc::as_ptr(&code) as usize,
            stack_required: 8,
        };
        directory.retained.borrow_mut().begin_segment();
        assert!(directory.accepts_callable(&code, *first));
        let callable = Arc::new(egcl_rt::CrossThreadRoot::new(*first));
        assert!(
            directory
                .retained
                .borrow_mut()
                .retain_with_callable(
                    Arc::new(CallCell::new(0, 11, 12)),
                    descriptor,
                    Rc::clone(&code),
                    Some(callable),
                )
                .is_some()
        );
        directory.retained.borrow_mut().sweep();
        assert!(directory.accepts_callable(&code, *first));
        assert!(
            !directory.accepts_callable(&code, *second),
            "unpublished but in-flight owner still constrains identity"
        );
        let retained = directory.callable_for_pc(0x1010).unwrap();
        // Lookup must release the directory borrow before callers enter a GC gate.
        drop(directory.retained.borrow_mut());
        retained.with_gc_stable_mutator(|value| assert_eq!(*value, *first));
        directory.retained.borrow_mut().finish_segment();
        assert!(directory.callable_for_pc(0x1010).is_none());
        assert!(
            directory.accepts_callable(&code, *second),
            "retired last segment releases identity"
        );
        // A returned handle retains its callable independently of directory sweeping.
        egcl_rt::gc::collect_t0_minor().unwrap();
        retained.with_gc_stable_mutator(|value| assert_eq!(*value, *first));
    }

    #[test]
    fn ordinary_guard_restores_depth_after_nested_rust_unwind() {
        let _thread = egcl_rt::thread::current_thread_id();
        assert_eq!(ordinary_segment_depth_for_test(), 0);
        let outer = enter_ordinary_segment();
        let directory = outer.directory();
        assert_eq!(ordinary_segment_depth_for_test(), 1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inner = enter_ordinary_segment();
            assert!(Rc::ptr_eq(&directory, &inner.directory()));
            assert_eq!(ordinary_segment_depth_for_test(), 2);
            panic!("selected Rust error");
        }));
        assert!(result.is_err());
        assert_eq!(ordinary_segment_depth_for_test(), 1);
        drop(outer);
        assert_eq!(ordinary_segment_depth_for_test(), 0);
    }

    #[test]
    fn ordinary_retired_owner_survives_suspension_and_nested_segments() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let weak = Rc::downgrade(&owner);
        let mut entries = RetainedEntries::default();
        entries.begin_segment();
        // A failed/invalidated publication is retired, but may still be in a
        // loaded native frame. The lease remains live while a fiber is parked.
        assert!(
            entries
                .retain(Arc::clone(&cell), descriptor(&owner, 0, 0x1000), owner)
                .is_some()
        );
        entries.sweep();
        assert!(
            weak.upgrade().is_some(),
            "suspended outer segment retains code"
        );
        entries.begin_segment();
        entries.finish_segment();
        assert!(
            weak.upgrade().is_some(),
            "nested callback cannot retire outer code"
        );
        assert!(entries.code_for_pc(0x1010).is_some());
        assert!(entries.code_for_pc(0x1040).is_none());
        entries.finish_segment();
        assert!(
            weak.upgrade().is_none(),
            "last segment retires invalidated owners"
        );
    }

    #[test]
    fn ordinary_published_descriptor_remains_live_when_execution_is_quiescent() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let weak = Rc::downgrade(&owner);
        let mut entries = RetainedEntries::default();
        let pointer = entries
            .retain(Arc::clone(&cell), descriptor(&owner, 0, 0x1000), owner)
            .unwrap();
        // The runtime publication tests exercise the locked revision check;
        // here the same atomic slot controls the owner's retirement decision.
        unsafe { &*cell.ordinary_entry_address() }.store(pointer as usize, Ordering::Release);
        entries.sweep();
        assert!(weak.upgrade().is_some());
        entries.begin_segment();
        entries.finish_segment();
        assert!(weak.upgrade().is_some());
        unsafe { &*cell.ordinary_entry_address() }.store(0, Ordering::Release);
        entries.sweep();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn ordinary_directory_keeps_multiple_code_owners_and_validates_identity() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let old = Rc::new(());
        let replacement = Rc::new(());
        let mut entries = RetainedEntries::default();
        entries.begin_segment();
        let wrong = descriptor(&old, 0, 0x1000);
        assert!(
            entries
                .retain(Arc::clone(&cell), wrong, Rc::clone(&replacement))
                .is_none()
        );
        assert!(
            entries
                .retain(
                    Arc::clone(&cell),
                    descriptor(&old, 0, 0x1000),
                    Rc::clone(&old)
                )
                .is_some()
        );
        assert!(
            entries
                .retain(
                    cell,
                    descriptor(&replacement, 1, 0x2000),
                    Rc::clone(&replacement)
                )
                .is_some()
        );
        assert!(Rc::ptr_eq(&entries.code_for_pc(0x1010).unwrap(), &old));
        assert!(Rc::ptr_eq(
            &entries.code_for_pc(0x2010).unwrap(),
            &replacement
        ));
        assert!(entries.code_for_pc(0x3000).is_none());
        entries.finish_segment();
    }
}
