// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-owned snapshot of a throwing call's live values, taken while its
//! physical frame still exists and rooted while logical frames are rebuilt.
//!
//! # Place in the transfer pipeline
//!
//! transfer_map.rs decides, per `Invoke`, which locations hold the values a
//! transfer needs and which of those are tagged roots. transfer_sites.rs binds
//! that map to an emitted return PC and physical access recipes. This file is
//! the storage those two hand off to at run time: a [`TransferSnapshot`] is
//! allocated from a [`TransferCaptureMap`] *before* native entry, filled by a
//! non-allocating `capture` after the helper has exited through the veneer,
//! and then used both to `reconstruct` interpreter-visible frames and to
//! `write_back` updated words to the native homes. It does not know physical
//! save recipes, retained code definitions, or the unwind dispatcher.
//!
//! # Contract
//!
//! `new` reserves one `SavedLocation` per distinct location named by the
//! map's slot descriptors, recursing into remat recipe inputs. A location
//! named with `Rebox::None` is a tagged word and is stored in a `Cell<EgclVal>`
//! that `trace_host_roots` visits; any other rebox is a raw word the collector
//! must not see. The map is cross-checked both ways at construction: every
//! tagged location must appear in `map.roots` (`MissingRoot`), every root must
//! correspond to a tagged location (`UnexpectedRoot`), one location may not be
//! both tagged and raw (`ConflictingLocation`), and a `MaterializeConst` naming
//! a movable heap object is refused (`MovingConstant`) because its bits cannot
//! be baked into the snapshot.
//!
//! `capture` copies each word through a caller-supplied reader with no
//! allocation, no GC and no yield, and marks the snapshot captured. After that
//! the buffer must be rooted (`rooted_ref!`) across any allocating work.
//! `reconstruct` roots the snapshot internally and runs `deopt::reconstruct`
//! over a `MachineState` that reads from the saved words, producing
//! [`ReconstructedFrame`]s whose PCs are unwind origins, never instructions to
//! re-execute; the caller roots the result before its next allocation.
//! `write_back` copies every saved word back in its native representation,
//! so a relocated tagged reference reaches its home but an unboxed float slot
//! never receives a reboxed Lisp value. Any use before capture is
//! `NotCaptured`.
//!
//! # Limits
//!
//! The snapshot is per execution and must not be shared between active
//! invocations. Output-allocation failure during reconstruction is not yet
//! handled here; the emergency OOM path belongs to preparation.

use crate::t2::deopt::{
    self, LoweredDeopt, MachineState, Rebox, ReconstructedFrame, SlotDescriptor,
};
use crate::t2::mach::Location;
use crate::t2::transfer_map::TransferCaptureMap;
use std::cell::Cell;
use egcl_rt::gc::TraceHostRoots;
use egcl_rt::value::{NIL, EgclVal};

enum SavedWord {
    Tagged(Cell<EgclVal>),
    Raw(u64),
}

struct SavedLocation {
    location: Location,
    word: SavedWord,
}

#[derive(Debug, PartialEq)]
pub enum CaptureError {
    ConflictingLocation(Location),
    MissingRoot(Location),
    UnexpectedRoot(Location),
    MovingConstant(EgclVal),
    NotCaptured,
}

/// A per-execution buffer, not shared with another active invocation. The
/// metadata and all input slots are allocated by `new`, before native entry.
/// Register this buffer with `rooted_ref!` if it must survive allocating work
/// between `capture` and `reconstruct`; reconstruction roots it internally.
pub struct TransferSnapshot {
    lowered: LoweredDeopt,
    saved: Vec<SavedLocation>,
    captured: bool,
}

impl TransferSnapshot {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    pub(crate) fn locations(&self) -> impl Iterator<Item = Location> + '_ {
        self.saved.iter().map(|slot| slot.location)
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    pub(crate) fn invalidate(&mut self) {
        self.captured = false;
    }

    pub fn new(map: &TransferCaptureMap) -> Result<Self, CaptureError> {
        let mut saved = Vec::new();
        for frame in &map.frames {
            for slot in &frame.slots {
                reserve_slot(slot, &mut saved)?;
            }
        }
        for slot in &saved {
            if matches!(slot.word, SavedWord::Tagged(_)) && !map.roots.contains(&slot.location) {
                return Err(CaptureError::MissingRoot(slot.location));
            }
        }
        for root in &map.roots {
            if !saved
                .iter()
                .any(|slot| slot.location == *root && matches!(slot.word, SavedWord::Tagged(_)))
            {
                return Err(CaptureError::UnexpectedRoot(*root));
            }
        }
        Ok(Self {
            lowered: LoweredDeopt {
                code_offset: 0,
                scopes: map.frames.clone(),
            },
            saved,
            captured: false,
        })
    }

    /// Copy all required words without allocating; no frame references escape.
    ///
    /// # Safety
    /// `read` must read the live word described by each allocated location. It
    /// must not allocate Lisp objects, collect, yield, or retire the source
    /// frame. The source frame/roots must remain valid until capture finishes.
    /// Once captured, root this buffer before any allocating work. This copy
    /// alone does not authorize retiring Lisp cleanup or native root links.
    pub unsafe fn capture(&mut self, mut read: impl FnMut(Location) -> u64) {
        self.captured = false;
        for slot in &mut self.saved {
            let raw = read(slot.location);
            match &mut slot.word {
                SavedWord::Tagged(value) => value.set(EgclVal(raw)),
                SavedWord::Raw(value) => *value = raw,
            }
        }
        self.captured = true;
    }

    /// Copy saved words back in their native representation, including GC's
    /// updated tagged references. Reconstructed Lisp values are not substitutes
    /// for raw native slots: an unboxed float must remain unboxed.
    ///
    /// # Safety
    /// `write` must address the still-live home for each location, without
    /// collecting, allocating Lisp objects or yielding during writeback.
    pub unsafe fn write_back(
        &self,
        mut write: impl FnMut(Location, u64),
    ) -> Result<(), CaptureError> {
        if !self.captured {
            return Err(CaptureError::NotCaptured);
        }
        for slot in &self.saved {
            let raw = match &slot.word {
                SavedWord::Tagged(value) => value.get().to_raw(),
                SavedWord::Raw(value) => *value,
            };
            write(slot.location, raw);
        }
        Ok(())
    }

    /// Rebuild logical values while rooting all captured tagged inputs. The
    /// returned frames must be rooted by the caller before its next allocation.
    /// Their PCs name unwind origins, not instructions to execute again.
    /// `box_double` must return a valid heap double and obey normal root rules.
    /// Output allocation/resource-exhaustion handling belongs to preparation;
    /// this method does not yet provide the emergency OOM fallback.
    pub fn reconstruct(
        &mut self,
        box_double: impl Fn(f64) -> EgclVal,
    ) -> Result<Vec<ReconstructedFrame>, CaptureError> {
        if !self.captured {
            return Err(CaptureError::NotCaptured);
        }
        egcl_rt::rooted_ref!(_saved_roots = &mut *self);
        let machine = SnapshotMachine {
            snapshot: self,
            box_double,
        };
        Ok(deopt::reconstruct(&self.lowered, &machine))
    }
}

impl TraceHostRoots for TransferSnapshot {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for slot in &mut self.saved {
            if let SavedWord::Tagged(value) = &slot.word {
                visit(value.as_ptr());
            }
        }
    }
}

struct SnapshotMachine<'a, F> {
    snapshot: &'a TransferSnapshot,
    box_double: F,
}

impl<F: Fn(f64) -> EgclVal> MachineState for SnapshotMachine<'_, F> {
    fn read(&self, location: Location) -> u64 {
        match &self
            .snapshot
            .saved
            .iter()
            .find(|slot| slot.location == location)
            .expect("reserved capture location")
            .word
        {
            SavedWord::Tagged(value) => value.get().to_raw(),
            SavedWord::Raw(value) => *value,
        }
    }
    fn box_double(&self, value: f64) -> EgclVal {
        (self.box_double)(value)
    }
}

fn reserve_slot(slot: &SlotDescriptor, saved: &mut Vec<SavedLocation>) -> Result<(), CaptureError> {
    match slot {
        SlotDescriptor::MaterializeConst(value)
            if value.is_cons() || value.is_heap_object() || value.is_function() =>
        {
            return Err(CaptureError::MovingConstant(*value));
        }
        SlotDescriptor::InLocation(location, rebox) => {
            let tagged = *rebox == Rebox::None;
            if let Some(existing) = saved.iter().find(|slot| slot.location == *location) {
                if matches!(existing.word, SavedWord::Tagged(_)) != tagged {
                    return Err(CaptureError::ConflictingLocation(*location));
                }
            } else {
                saved.push(SavedLocation {
                    location: *location,
                    word: if tagged {
                        SavedWord::Tagged(Cell::new(NIL))
                    } else {
                        SavedWord::Raw(0)
                    },
                });
            }
        }
        SlotDescriptor::Remat(recipe) => {
            for input in &recipe.inputs {
                reserve_slot(input, saved)?;
            }
        }
        _ => {}
    }
    Ok(())
}
