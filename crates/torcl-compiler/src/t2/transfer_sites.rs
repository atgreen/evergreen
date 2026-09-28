//! Bind final frame maps to exact emitted return PCs. These tables do not own
//! executable memory or bytecode definitions: the installed-code owner must
//! retain both, check architecture/ABI compatibility, and publish complete site
//! coverage before execution. No nearest-PC or function-name lookup is allowed.

use crate::t2::deopt::ReconstructedFrame;
use crate::t2::mach::{Location, RegClass};
use crate::t2::native_transfer::{CaptureLocationError, SysvCaptureLocation, SysvTransferCapture};
use crate::t2::transfer_capture::{CaptureError, TransferSnapshot};
use crate::t2::transfer_map::TransferCaptureMap;
use crate::t2::x64_frame::{GPR_X86, ValueHome};
use torcl_rt::gc::TraceHostRoots;
use torcl_rt::value::TorclVal;

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
        activation: &[TorclVal],
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
        box_double: impl Fn(f64) -> TorclVal,
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
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        self.snapshot.trace_host_roots(visit);
    }
}
