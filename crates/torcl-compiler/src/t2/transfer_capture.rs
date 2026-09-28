//! Owned saved words for a transfer whose physical frame is still available.
//! Reserve storage before native entry, capture without collecting, then root
//! saved tagged words while rebuilding logical frames. Physical save recipes,
//! retained code definitions and the unwind dispatcher are separate contracts.

use crate::t2::deopt::{
    self, LoweredDeopt, MachineState, Rebox, ReconstructedFrame, SlotDescriptor,
};
use crate::t2::mach::Location;
use crate::t2::transfer_map::TransferCaptureMap;
use std::cell::Cell;
use torcl_rt::gc::TraceHostRoots;
use torcl_rt::value::{NIL, TorclVal};

enum SavedWord {
    Tagged(Cell<TorclVal>),
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
    MovingConstant(TorclVal),
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
                SavedWord::Tagged(value) => value.set(TorclVal(raw)),
                SavedWord::Raw(value) => *value = raw,
            }
        }
        self.captured = true;
    }

    /// Rebuild logical values while rooting all captured tagged inputs. The
    /// returned frames must be rooted by the caller before its next allocation.
    /// Their PCs name unwind origins, not instructions to execute again.
    /// `box_double` must return a valid heap double and obey normal root rules.
    /// Output allocation/resource-exhaustion handling belongs to preparation;
    /// this method does not yet provide the emergency OOM fallback.
    pub fn reconstruct(
        &mut self,
        box_double: impl Fn(f64) -> TorclVal,
    ) -> Result<Vec<ReconstructedFrame>, CaptureError> {
        if !self.captured {
            return Err(CaptureError::NotCaptured);
        }
        torcl_rt::rooted_ref!(_saved_roots = &mut *self);
        let machine = SnapshotMachine {
            snapshot: self,
            box_double,
        };
        Ok(deopt::reconstruct(&self.lowered, &machine))
    }
}

impl TraceHostRoots for TransferSnapshot {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
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

impl<F: Fn(f64) -> TorclVal> MachineState for SnapshotMachine<'_, F> {
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
    fn box_double(&self, value: f64) -> TorclVal {
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
