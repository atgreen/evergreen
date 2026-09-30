// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bind final frame maps to exact emitted return PCs. These tables do not own
//! executable memory or bytecode definitions: the installed-code owner must
//! retain both, check architecture/ABI compatibility, and publish complete site
//! coverage before execution. No nearest-PC or function-name lookup is allowed.

use crate::t2::deopt::ReconstructedFrame;
use crate::t2::mach::{Location, RegClass};
use crate::t2::native_transfer::{
    CaptureLocationError, SysvCaptureLocation, SysvNativeLanding, SysvTransferCapture,
};
use crate::t2::transfer_capture::{CaptureError, TransferSnapshot};
use crate::t2::transfer_map::TransferCaptureMap;
use crate::t2::x64_frame::{GPR_X86, ValueHome};
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::value::EgclVal;

/// Emission records the offset immediately following the actual CALL, along
/// with final homes and any temporary stack space still present at that point.
pub struct SysvTransferSite {
    pub return_offset: u32,
    pub stack_slots: u32,
    pub call_stack_adjust: u32,
    /// Total tagged slots in the still-rooted owning activation.
    pub activation_slots: u16,
    /// Native location -> canonical activation slot. A collecting helper may
    /// have updated these shadows without restoring the native homes yet.
    /// Emission must include every moving root synchronized before this call.
    pub shadow_roots: Vec<(Location, u16)>,
    pub map: TransferCaptureMap,
}

/// Compiler-selected cold edge that performs the phi moves into a cleanup.
/// The entry is not the cleanup body itself: different source calls can require
/// different moves. Bind these descriptors only after final code emission.
#[derive(Clone, Copy, Debug)]
pub struct SysvCleanupLanding {
    pub return_offset: u32,
    pub entry_offset: u32,
    pub cleanup_bcp: u32,
}

/// Source-specific cold edge into one exact catch in the retained activation.
#[derive(Clone, Copy, Debug)]
pub struct SysvCatchLanding {
    pub return_offset: u32,
    pub entry_offset: u32,
    pub push_bcp: u32,
    pub resume_bcp: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct SysvHandlerLanding {
    pub return_offset: u32,
    pub entry_offset: u32,
    pub push_bcp: u32,
    pub table_index: u32,
    pub clause_index: u32,
}

#[derive(Debug, PartialEq)]
pub enum TransferSiteError {
    InvalidReturnOffset(u32),
    DuplicateReturnOffset(u32),
    InvalidLogicalFrame(u32),
    UnsupportedRegister(Location),
    Capture(CaptureError),
    Physical(CaptureLocationError),
    WrongReturnPc,
    InvalidShadowRoot(Location),
    MissingActivation,
    InvalidCleanupLanding(u32),
    InvalidCatchLanding(u32),
    InvalidHandlerLanding(u32),
    InvalidLandingCapture,
}

struct CaptureRecipe {
    native: SysvCaptureLocation,
    shadow_slot: Option<usize>,
}

pub struct CheckedSysvSite {
    return_offset: u32,
    map: TransferCaptureMap,
    recipes: Vec<(Location, CaptureRecipe)>,
    activation_slots: usize,
    call_stack_adjust: u32,
    cleanup_landing: Option<u32>,
    catch_landings: Vec<(u32, u32)>,
    handler_landings: Vec<(u32, u32, u32)>,
}

pub struct SysvTransferTable {
    code_len: usize,
    sites: Vec<CheckedSysvSite>,
}

impl SysvTransferTable {
    pub fn sites(&self) -> impl Iterator<Item = &CheckedSysvSite> {
        self.sites.iter()
    }
    /// Validate all descriptor inputs, including raw words and nested remat
    /// inputs, before the first native entry. Construction may allocate.
    pub fn new(code_len: usize, sites: Vec<SysvTransferSite>) -> Result<Self, TransferSiteError> {
        let mut checked = Vec::with_capacity(sites.len());
        for site in sites {
            if site.return_offset == 0 || site.return_offset as usize >= code_len {
                return Err(TransferSiteError::InvalidReturnOffset(site.return_offset));
            }
            // Inline scope composition is not supported by transfer-map
            // lowering yet. Do not silently accept uncomposed frames here.
            if site.map.frames.len() != 1
                || site.map.frames[0].resume_pc != site.map.origin_bcp
                || site.map.frames[0].num_locals > site.map.frames[0].slots.len()
                || site.map.frames[0].live_ref_bitmap.len() != site.map.frames[0].slots.len()
            {
                return Err(TransferSiteError::InvalidLogicalFrame(site.return_offset));
            }
            // Validate adjustment even if all logical values are constants.
            if site.call_stack_adjust % 8 != 0 {
                return Err(TransferSiteError::Physical(
                    CaptureLocationError::UnalignedStackAdjustment,
                ));
            }
            let snapshot = TransferSnapshot::new(&site.map).map_err(TransferSiteError::Capture)?;
            for (index, &(location, slot)) in site.shadow_roots.iter().enumerate() {
                if !site.map.roots.contains(&location)
                    || slot >= site.activation_slots
                    || site.shadow_roots[..index]
                        .iter()
                        .any(|&(prior_location, prior_slot)| {
                            prior_location == location || prior_slot == slot
                        })
                {
                    return Err(TransferSiteError::InvalidShadowRoot(location));
                }
            }
            let mut recipes = Vec::new();
            for location in snapshot.locations() {
                let home = match location {
                    Location::Stack(slot) => ValueHome::Stack(slot.0),
                    Location::Register(register) if register.class == RegClass::Gpr => {
                        ValueHome::Reg(
                            *GPR_X86
                                .get(register.encoding as usize)
                                .ok_or(TransferSiteError::UnsupportedRegister(location))?,
                        )
                    }
                    _ => return Err(TransferSiteError::UnsupportedRegister(location)),
                };
                let recipe =
                    SysvCaptureLocation::for_home(home, site.stack_slots, site.call_stack_adjust)
                        .map_err(TransferSiteError::Physical)?;
                let shadow_slot = site
                    .shadow_roots
                    .iter()
                    .find(|(native, _)| *native == location)
                    .map(|(_, slot)| usize::from(*slot));
                recipes.push((
                    location,
                    CaptureRecipe {
                        native: recipe,
                        shadow_slot,
                    },
                ));
            }
            checked.push(CheckedSysvSite {
                return_offset: site.return_offset,
                map: site.map,
                recipes,
                activation_slots: usize::from(site.activation_slots),
                call_stack_adjust: site.call_stack_adjust,
                cleanup_landing: None,
                catch_landings: Vec::new(),
                handler_landings: Vec::new(),
            });
        }
        checked.sort_unstable_by_key(|site| site.return_offset);
        for pair in checked.windows(2) {
            if pair[0].return_offset == pair[1].return_offset {
                return Err(TransferSiteError::DuplicateReturnOffset(
                    pair[0].return_offset,
                ));
            }
        }
        Ok(Self {
            code_len,
            sites: checked,
        })
    }

    /// Bind compiler-verified cleanup edges to the exact bytes being installed.
    /// Unknown sites, duplicate destinations for one site, inherited cleanups,
    /// and entries without ENDBR64 are refused. Failure consumes the table, so
    /// partially checked destinations cannot be published. Sites omitted here
    /// retain explicit fallback. This does not infer CFG semantics from bytes;
    /// the emitter must supply edges validated by the IR verifier.
    pub fn with_cleanup_landings(
        mut self,
        code: &[u8],
        landings: &[SysvCleanupLanding],
    ) -> Result<Self, TransferSiteError> {
        use crate::control_scope::{Ownership, ScopeKind};
        if code.len() != self.code_len {
            return Err(TransferSiteError::InvalidCleanupLanding(u32::MAX));
        }
        for landing in landings {
            let invalid = || TransferSiteError::InvalidCleanupLanding(landing.return_offset);
            let index = self
                .sites
                .binary_search_by_key(&landing.return_offset, |site| site.return_offset)
                .map_err(|_| invalid())?;
            let site = &mut self.sites[index];
            let scope = site
                .map
                .control_scopes
                .iter()
                .rev()
                .find(|scope| matches!(scope.kind, ScopeKind::Unwind { .. }))
                .ok_or_else(invalid)?;
            let start = landing.entry_offset as usize;
            let end = start.checked_add(4).ok_or_else(invalid)?;
            if site.cleanup_landing.is_some()
                || site.call_stack_adjust % 16 != 0
                || scope.ownership != Ownership::Local
                || scope.kind
                    != (ScopeKind::Unwind {
                        cleanup_bcp: landing.cleanup_bcp,
                    })
                || code.get(start..end) != Some(&[0xf3, 0x0f, 0x1e, 0xfa])
            {
                return Err(invalid());
            }
            site.cleanup_landing = Some(landing.entry_offset);
        }
        Ok(self)
    }

    /// Bind catch destinations to exact source sites and scope identities,
    /// validating their machine-code entry markers before publishing the table.
    pub fn with_catch_landings(
        mut self,
        code: &[u8],
        landings: &[SysvCatchLanding],
    ) -> Result<Self, TransferSiteError> {
        use crate::control_scope::{Ownership, ScopeKind};
        if code.len() != self.code_len {
            return Err(TransferSiteError::InvalidCatchLanding(u32::MAX));
        }
        for landing in landings {
            let invalid = || TransferSiteError::InvalidCatchLanding(landing.return_offset);
            let index = self
                .sites
                .binary_search_by_key(&landing.return_offset, |site| site.return_offset)
                .map_err(|_| invalid())?;
            let site = &mut self.sites[index];
            let mut candidates = site
                .map
                .control_scopes
                .iter()
                .enumerate()
                .filter(|(_, s)| s.push_bcp == landing.push_bcp);
            let (scope_index, scope) = candidates.next().ok_or_else(invalid)?;
            let start = landing.entry_offset as usize;
            let end = start.checked_add(4).ok_or_else(invalid)?;
            if candidates.next().is_some()
                || scope.ownership != Ownership::Local
                || scope.kind
                    != (ScopeKind::Catch {
                        resume_bcp: landing.resume_bcp,
                    })
                || site.map.control_scopes[scope_index + 1..].iter().any(|s| {
                    s.ownership != Ownership::Local || matches!(s.kind, ScopeKind::Unwind { .. })
                })
                || site.call_stack_adjust % 16 != 0
                || site
                    .catch_landings
                    .iter()
                    .any(|(push, _)| *push == landing.push_bcp)
                || code.get(start..end) != Some(&[0xf3, 0x0f, 0x1e, 0xfa])
            {
                return Err(invalid());
            }
            site.catch_landings
                .push((landing.push_bcp, landing.entry_offset));
        }
        Ok(self)
    }

    /// Bind each selected clause to its source site and retained definition.
    pub fn with_handler_landings(
        mut self,
        code: &[u8],
        definitions: &[egcl_rt::bytecode::HandlerCaseInfo],
        landings: &[SysvHandlerLanding],
    ) -> Result<Self, TransferSiteError> {
        use crate::control_scope::{Ownership, ScopeKind};
        if code.len() != self.code_len {
            return Err(TransferSiteError::InvalidHandlerLanding(u32::MAX));
        }
        for landing in landings {
            let invalid = || TransferSiteError::InvalidHandlerLanding(landing.return_offset);
            let index = self
                .sites
                .binary_search_by_key(&landing.return_offset, |site| site.return_offset)
                .map_err(|_| invalid())?;
            let site = &mut self.sites[index];
            let mut candidates = site
                .map
                .control_scopes
                .iter()
                .enumerate()
                .filter(|(_, scope)| scope.push_bcp == landing.push_bcp);
            let (scope_index, scope) = candidates.next().ok_or_else(invalid)?;
            let start = landing.entry_offset as usize;
            let end = start.checked_add(4).ok_or_else(invalid)?;
            if candidates.next().is_some()
                || scope.ownership != Ownership::Local
                || scope.kind
                    != (ScopeKind::HandlerCase {
                        table_index: landing.table_index,
                    })
                || definitions
                    .get(landing.table_index as usize)
                    .and_then(|info| info.clauses.get(landing.clause_index as usize))
                    .is_none()
                || site.map.control_scopes[scope_index + 1..]
                    .iter()
                    .any(|scope| {
                        scope.ownership != Ownership::Local
                            || matches!(scope.kind, ScopeKind::Unwind { .. })
                    })
                || site.call_stack_adjust % 16 != 0
                || site.handler_landings.iter().any(|(push, clause, _)| {
                    *push == landing.push_bcp && *clause == landing.clause_index
                })
                || code.get(start..end) != Some(&[0xf3, 0x0f, 0x1e, 0xfa])
            {
                return Err(invalid());
            }
            site.handler_landings.push((
                landing.push_bcp,
                landing.clause_index,
                landing.entry_offset,
            ));
        }
        Ok(self)
    }

    /// Integer address checks only; this does not dereference code or allocate.
    /// The caller must supply the base of the code owning this table.
    pub fn lookup(&self, code_base: usize, return_pc: usize) -> Option<&CheckedSysvSite> {
        let end = code_base.checked_add(self.code_len)?;
        if return_pc >= end {
            return None;
        }
        let offset = u32::try_from(return_pc.checked_sub(code_base)?).ok()?;
        let index = self
            .sites
            .binary_search_by_key(&offset, |site| site.return_offset)
            .ok()?;
        Some(&self.sites[index])
    }
}

impl CheckedSysvSite {
    pub fn return_offset(&self) -> u32 {
        self.return_offset
    }
    pub fn map(&self) -> &TransferCaptureMap {
        &self.map
    }

    /// Construct a same-frame landing packet without allocating or reading raw
    /// memory. The caller must retain the installed code at `code_base`, prove
    /// this is its live frame in the current segment, root the pending cursor,
    /// and repair captured native homes before dispatch. Address arithmetic and
    /// exact-PC checks cannot prove those ownership/lifetime obligations.
    pub fn native_cleanup_landing(
        &self,
        code_base: usize,
        capture: &SysvTransferCapture,
    ) -> Result<Option<SysvNativeLanding>, TransferSiteError> {
        self.native_landing(code_base, capture, self.cleanup_landing)
    }

    /// Same lifetime/rooting obligations as native_cleanup_landing. A missing
    /// exact scope identity means explicit fallback, never a nearest destination.
    pub fn native_catch_landing(
        &self,
        code_base: usize,
        capture: &SysvTransferCapture,
        push_bcp: u32,
    ) -> Result<Option<SysvNativeLanding>, TransferSiteError> {
        self.native_landing(
            code_base,
            capture,
            self.catch_landings
                .iter()
                .find(|(push, _)| *push == push_bcp)
                .map(|(_, offset)| *offset),
        )
    }

    pub fn native_handler_landing(
        &self,
        code_base: usize,
        capture: &SysvTransferCapture,
        push_bcp: u32,
        clause_index: u32,
    ) -> Result<Option<SysvNativeLanding>, TransferSiteError> {
        self.native_landing(
            code_base,
            capture,
            self.handler_landings
                .iter()
                .find(|(push, clause, _)| *push == push_bcp && *clause == clause_index)
                .map(|(_, _, offset)| *offset),
        )
    }

    fn native_landing(
        &self,
        code_base: usize,
        capture: &SysvTransferCapture,
        offset: Option<u32>,
    ) -> Result<Option<SysvNativeLanding>, TransferSiteError> {
        if (capture.return_pc as usize).checked_sub(code_base) != Some(self.return_offset as usize)
        {
            return Err(TransferSiteError::WrongReturnPc);
        }
        let Some(offset) = offset else {
            return Ok(None);
        };
        let invalid = || TransferSiteError::InvalidLandingCapture;
        let caller_sp = capture.caller_sp as usize;
        if caller_sp == 0
            || caller_sp % 16 != 0
            || capture.exit != egcl_rt::native_transfer::NativeExit::Transfer
        {
            return Err(invalid());
        }
        let stack_pointer = caller_sp
            .checked_add(self.call_stack_adjust as usize)
            .ok_or_else(invalid)? as *mut u64;
        let entry = code_base.checked_add(offset as usize).ok_or_else(invalid)? as *const u8;
        Ok(Some(SysvNativeLanding {
            stack_pointer,
            entry,
        }))
    }

    /// Reserve execution-owned storage before native entry. The borrow keeps
    /// the exact checked site alive, independent of any later name redefinition.
    /// Retaining the executing bytecode/code remains the installer's obligation.
    pub fn reserve_snapshot(&self) -> Result<SysvSiteSnapshot<'_>, CaptureError> {
        Ok(SysvSiteSnapshot {
            site: self,
            snapshot: TransferSnapshot::new(&self.map)?,
        })
    }
}

/// Cannot be paired with an unrelated site's physical access recipes. Root this
/// buffer across allocating work between capture, reconstruction and writeback.
pub struct SysvSiteSnapshot<'a> {
    site: &'a CheckedSysvSite,
    snapshot: TransferSnapshot,
}

impl SysvSiteSnapshot<'_> {
    fn check_pc(
        &self,
        code_base: usize,
        capture: &SysvTransferCapture,
    ) -> Result<(), TransferSiteError> {
        if (capture.return_pc as usize).checked_sub(code_base)
            == Some(self.site.return_offset as usize)
        {
            Ok(())
        } else {
            Err(TransferSiteError::WrongReturnPc)
        }
    }

    /// # Safety
    /// Code base and capture must describe the still-live frame whose layout
    /// produced this table. All raw reads must finish without GC or yielding.
    pub unsafe fn capture(
        &mut self,
        code_base: usize,
        capture: &SysvTransferCapture,
    ) -> Result<(), TransferSiteError> {
        unsafe { self.capture_from_activation(code_base, capture, &[]) }
    }

    /// Capture updated activation shadows for roots and native homes for raw
    /// words. Normal-path shadow restoration has not run when a helper exits
    /// through the veneer, so reading native root homes here can resurrect stale
    /// pointers. Bounds and mapping checks finish before any native read.
    ///
    /// # Safety
    /// Same live-frame contract as `capture`. `activation` must be the owning
    /// activation, kept rooted across the helper and subsequent reconstruction;
    /// its mapped shadows must contain the current values for this exact call.
    pub unsafe fn capture_from_activation(
        &mut self,
        code_base: usize,
        capture: &SysvTransferCapture,
        activation: &[EgclVal],
    ) -> Result<(), TransferSiteError> {
        self.snapshot.invalidate();
        self.check_pc(code_base, capture)?;
        if activation.len() < self.site.activation_slots {
            return Err(TransferSiteError::MissingActivation);
        }
        unsafe {
            self.snapshot.capture(|location| {
                let recipe = &self
                    .site
                    .recipes
                    .iter()
                    .find(|(key, _)| *key == location)
                    .expect("validated descriptor location")
                    .1;
                match recipe.shadow_slot {
                    Some(slot) => activation[slot].to_raw(),
                    None => recipe.native.read(capture),
                }
            });
        }
        Ok(())
    }

    pub fn reconstruct(
        &mut self,
        box_double: impl Fn(f64) -> EgclVal,
    ) -> Result<Vec<ReconstructedFrame>, CaptureError> {
        self.snapshot.reconstruct(box_double)
    }

    /// # Safety
    /// The captured frame is still live and its homes are writable. No GC or
    /// yield may intervene while copying updated words back to native homes.
    pub unsafe fn write_back(
        &self,
        code_base: usize,
        capture: &mut SysvTransferCapture,
    ) -> Result<(), TransferSiteError> {
        self.check_pc(code_base, capture)?;
        unsafe {
            self.snapshot
                .write_back(|location, word| {
                    self.site
                        .recipes
                        .iter()
                        .find(|(key, _)| *key == location)
                        .expect("validated descriptor location")
                        .1
                        .native
                        .write(capture, word)
                })
                .map_err(TransferSiteError::Capture)
        }
    }
}

impl TraceHostRoots for SysvSiteSnapshot<'_> {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.snapshot.trace_host_roots(visit);
    }
}
