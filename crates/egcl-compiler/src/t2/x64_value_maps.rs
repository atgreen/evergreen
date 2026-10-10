// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Authoritative value copies while an x86-64 SysV caller is suspended.

use super::ir::{Value, ValueRepresentation};
use super::x64_calls::{NativeCallSite, NativeStackBase};
use super::x64_frame::{FrameValue, FrameValueLocation, ValueHome};
use egcl_rt::value::EgclVal;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeValueLocation {
    /// Slot in the activation addressed by `activation_base_slot`.
    Activation(u16),
    /// Byte displacement from body RSP, before the call's temporary area.
    Stack(i32),
    /// SysV callee-saved hardware register, recovered by unwinding the callee.
    Register(u8),
    Constant(EgclVal),
    /// A nonroot whose volatile home was overwritten by call setup or the callee.
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct NativeValue {
    pub value: Value,
    pub repr: ValueRepresentation,
    may_reference_heap: bool,
    locations: Vec<NativeValueLocation>,
}

impl NativeValue {
    pub fn locations(&self) -> &[NativeValueLocation] {
        &self.locations
    }
    pub fn may_reference_heap(&self) -> bool {
        self.may_reference_heap
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFrameLayout {
    pub native_slots: u32,
    /// Body-RSP-relative native slot holding the owning activation pointer.
    pub activation_base_slot: Option<u32>,
    /// Total activation slots, including shadows and outgoing arguments.
    pub activation_slots: u16,
}

#[derive(Clone, Debug)]
pub struct NativeFrameValues {
    layout: NativeFrameLayout,
    values: Vec<NativeValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeValueMapError {
    InvalidLocation,
    ConflictingLocation,
    InvalidValues,
    MissingRoot(Value),
}

impl NativeFrameValues {
    pub fn layout(&self) -> NativeFrameLayout {
        self.layout
    }
    pub fn values(&self) -> &[NativeValue] {
        &self.values
    }
    pub fn ssa_value(&self, value: Value) -> Option<&NativeValue> {
        self.values.iter().find(|entry| entry.value == value)
    }
    /// Every writable copy, including duplicate outgoing arguments. Raw words
    /// and proven nonmoving tagged values never become collector roots.
    pub fn gc_locations(&self) -> impl Iterator<Item = &NativeValueLocation> {
        self.values
            .iter()
            .filter(|value| value.may_reference_heap)
            .flat_map(|value| value.locations.iter())
    }

    // Root order is the actual shadow write order; outgoing order is the ABI
    // slice order. Neither may be reconstructed by sorting values after emission.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_call(
        values: &[FrameValue],
        roots: &[Value],
        shadow_base: u16,
        outgoing: &[Value],
        outgoing_base: Option<u16>,
        layout: NativeFrameLayout,
        poll_spill_start: Option<u32>,
        nonmoving: impl Fn(Value) -> bool,
    ) -> Result<Self, NativeValueMapError> {
        use NativeValueMapError as Error;
        let known: HashSet<_> = values.iter().map(|v| v.value).collect();
        if known.len() != values.len()
            || roots.iter().any(|v| !known.contains(v))
            || outgoing.iter().any(|v| !known.contains(v))
            || roots.iter().copied().collect::<HashSet<_>>().len() != roots.len()
            || (!outgoing.is_empty() && outgoing_base.is_none())
        {
            return Err(Error::InvalidValues);
        }
        let shadows = roots
            .iter()
            .enumerate()
            .map(|(index, &value)| {
                let index = u16::try_from(index).map_err(|_| Error::InvalidLocation)?;
                let slot = shadow_base
                    .checked_add(index)
                    .ok_or(Error::InvalidLocation)?;
                Ok((value, slot))
            })
            .collect::<Result<HashMap<_, _>, Error>>()?;
        let mut raw_slot = poll_spill_start.unwrap_or(0);
        let mut mapped = Vec::with_capacity(values.len());
        for entry in values {
            let shadow = shadows.get(&entry.value).copied();
            if shadow.is_some() && entry.gc_home().is_none() {
                return Err(Error::InvalidValues);
            }
            let moving = entry.gc_home().is_some() && !nonmoving(entry.value);
            if moving && shadow.is_none() {
                return Err(Error::MissingRoot(entry.value));
            }
            let location = match entry.location {
                FrameValueLocation::Immediate(value) => NativeValueLocation::Constant(value),
                FrameValueLocation::Home(_) if shadow.is_some() => {
                    NativeValueLocation::Activation(shadow.unwrap())
                }
                FrameValueLocation::Home(ValueHome::Reg(_)) if poll_spill_start.is_some() => {
                    if entry.repr == ValueRepresentation::Tagged {
                        return Err(Error::InvalidValues);
                    }
                    let offset = stack_offset(raw_slot)?;
                    raw_slot = raw_slot.checked_add(1).ok_or(Error::InvalidLocation)?;
                    NativeValueLocation::Stack(offset)
                }
                FrameValueLocation::Home(ValueHome::Stack(slot)) => {
                    NativeValueLocation::Stack(stack_offset(slot)?)
                }
                FrameValueLocation::Home(ValueHome::Reg(reg)) if matches!(reg, 3 | 12..=15) => {
                    NativeValueLocation::Register(reg)
                }
                FrameValueLocation::Home(ValueHome::Reg(_)) => NativeValueLocation::Unavailable,
            };
            let mut locations = vec![location];
            // Record the value's real native home ALONGSIDE its shadow slot.
            //
            // The `Home(_) if shadow.is_some()` arm above MASKS the Stack and
            // Register arms, and a moving value is required to have a shadow, so
            // until this a map could not describe a non-activation home at all
            // and the precise walk could reach nothing the managed-stack scan
            // already reached (bliss-shih7.2.7.3). Appending rather than
            // reordering keeps the primary location, and the poll-spill arm's
            // `raw_slot` counter, exactly as they were: that arm rejects a
            // Tagged value and is unreachable only because the shadow arm
            // precedes it.
            //
            // Only a home that SURVIVES the helper call may be added. A
            // caller-saved register's contents are destroyed by the call, which
            // is what the shadow store and restore exist to preserve; the
            // Register arm is already restricted to the callee-saved set, whose
            // save words the published capture image makes writable.
            if moving {
                let native_home = match entry.location {
                    FrameValueLocation::Home(ValueHome::Stack(slot)) => {
                        Some(NativeValueLocation::Stack(stack_offset(slot)?))
                    }
                    FrameValueLocation::Home(ValueHome::Reg(reg)) if matches!(reg, 3 | 12..=15) => {
                        Some(NativeValueLocation::Register(reg))
                    }
                    _ => None,
                };
                if let Some(home) = native_home
                    && !locations.contains(&home)
                {
                    locations.push(home);
                }
            }
            if let Some(base) = outgoing_base {
                for (index, &arg) in outgoing.iter().enumerate() {
                    if arg == entry.value {
                        let index = u16::try_from(index).map_err(|_| Error::InvalidLocation)?;
                        let slot = base.checked_add(index).ok_or(Error::InvalidLocation)?;
                        let location = NativeValueLocation::Activation(slot);
                        if !locations.contains(&location) {
                            locations.push(location);
                        }
                    }
                }
            }
            if locations.len() > 1 {
                locations.retain(|loc| *loc != NativeValueLocation::Unavailable);
            }
            mapped.push(NativeValue {
                value: entry.value,
                repr: entry.repr,
                may_reference_heap: moving,
                locations,
            });
        }
        Self::checked(layout, mapped)
    }

    fn checked(
        layout: NativeFrameLayout,
        values: Vec<NativeValue>,
    ) -> Result<Self, NativeValueMapError> {
        use NativeValueMapError as Error;
        let stack_bytes = stack_offset(layout.native_slots)?;
        if layout
            .activation_base_slot
            .is_some_and(|slot| slot >= layout.native_slots)
        {
            return Err(Error::InvalidLocation);
        }
        let mut copies = HashMap::new();
        let mut identities = HashSet::new();
        for value in &values {
            if !identities.insert(value.value)
                || value.locations.is_empty()
                || (value.may_reference_heap && value.repr != ValueRepresentation::Tagged)
            {
                return Err(Error::InvalidValues);
            }
            for &location in &value.locations {
                match location {
                    NativeValueLocation::Activation(slot)
                        if layout.activation_base_slot.is_none()
                            || slot >= layout.activation_slots =>
                    {
                        return Err(Error::InvalidLocation);
                    }
                    NativeValueLocation::Stack(offset)
                        if offset < 0
                            || offset % 8 != 0
                            || offset >= stack_bytes
                            || layout
                                .activation_base_slot
                                .is_some_and(|slot| offset == (slot * 8) as i32) =>
                    {
                        return Err(Error::InvalidLocation);
                    }
                    NativeValueLocation::Register(reg) if !matches!(reg, 3 | 12..=15) => {
                        return Err(Error::InvalidLocation);
                    }
                    NativeValueLocation::Constant(constant)
                        if egcl_rt::gc::is_heap_ref(constant) =>
                    {
                        return Err(Error::InvalidValues);
                    }
                    NativeValueLocation::Constant(_) | NativeValueLocation::Unavailable => {
                        if value.may_reference_heap {
                            return Err(Error::InvalidValues);
                        }
                        continue;
                    }
                    _ => {}
                }
                if let Some(previous) = copies.insert(location, value.value)
                    && previous != value.value
                {
                    return Err(Error::ConflictingLocation);
                }
            }
        }
        Ok(Self { layout, values })
    }

    /// Check temporary locations against this exact call's stack adjustment.
    pub(crate) fn validate_at(&self, call: &NativeCallSite) -> Result<(), NativeValueMapError> {
        if call.stack_base != NativeStackBase::Body || self.values.iter().flat_map(|v| &v.locations)
            .any(|loc| matches!(loc, NativeValueLocation::Stack(offset) if i64::from(*offset) < -i64::from(call.stack_adjust))) {
            return Err(NativeValueMapError::InvalidLocation);
        }
        Ok(())
    }
}

fn stack_offset(slot: u32) -> Result<i32, NativeValueMapError> {
    slot.checked_mul(8)
        .and_then(|bytes| i32::try_from(bytes).ok())
        .ok_or(NativeValueMapError::InvalidLocation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::ir::{Value, ValueRepresentation as Repr};
    use crate::t2::x64_frame::{FrameValue, FrameValueLocation, ValueHome};

    fn home(value: u32, repr: Repr, location: ValueHome) -> FrameValue {
        FrameValue {
            value: Value(value),
            repr,
            location: FrameValueLocation::Home(location),
        }
    }
    fn layout() -> NativeFrameLayout {
        NativeFrameLayout {
            native_slots: 8,
            activation_base_slot: Some(7),
            activation_slots: 12,
        }
    }

    #[test]
    fn native_roots_retain_stack_and_preserved_register_aliases() {
        let values = [
            home(0, Repr::Tagged, ValueHome::Stack(0)),
            home(1, Repr::Tagged, ValueHome::Reg(12)),
            home(2, Repr::Tagged, ValueHome::Reg(1)),
            home(3, Repr::UnboxedFixnum, ValueHome::Reg(8)),
        ];
        for poll in [None, Some(2)] {
            let map = NativeFrameValues::for_call(
                &values, &[Value(0), Value(1), Value(2)], 4,
                &[Value(0)], Some(8), layout(), poll, |_| false,
            ).unwrap();
            assert_eq!(map.ssa_value(Value(0)).unwrap().locations(), &[
                NativeValueLocation::Activation(4), NativeValueLocation::Stack(0),
                NativeValueLocation::Activation(8),
            ]);
            assert_eq!(map.ssa_value(Value(1)).unwrap().locations(), &[
                NativeValueLocation::Activation(5), NativeValueLocation::Register(12),
            ]);
            assert_eq!(map.ssa_value(Value(2)).unwrap().locations(), &[
                NativeValueLocation::Activation(6),
            ]);
            assert_eq!(map.ssa_value(Value(3)).unwrap().locations(), &[
                if poll.is_some() { NativeValueLocation::Stack(16) }
                else { NativeValueLocation::Unavailable },
            ], "tagged poll registers must not consume raw spill positions");
        }
    }

    #[test]
    fn calls_use_updated_shadows_and_every_outgoing_argument_copy() {
        let values = [
            home(0, Repr::Tagged, ValueHome::Reg(1)),
            home(1, Repr::UnboxedFixnum, ValueHome::Stack(0)),
            home(2, Repr::Tagged, ValueHome::Reg(3)),
        ];
        let map = NativeFrameValues::for_call(
            &values,
            &[Value(0)],
            4,
            &[Value(0), Value(0)],
            Some(6),
            layout(),
            None,
            |v| v == Value(2),
        )
        .unwrap();
        assert_eq!(
            map.ssa_value(Value(0)).unwrap().locations(),
            &[
                NativeValueLocation::Activation(4),
                NativeValueLocation::Activation(6),
                NativeValueLocation::Activation(7),
            ]
        );
        assert_eq!(
            map.ssa_value(Value(1)).unwrap().locations(),
            &[NativeValueLocation::Stack(0)]
        );
        assert_eq!(
            map.ssa_value(Value(2)).unwrap().locations(),
            &[NativeValueLocation::Register(3)]
        );
        assert_eq!(
            map.gc_locations().copied().collect::<Vec<_>>(),
            vec![
                NativeValueLocation::Activation(4),
                NativeValueLocation::Activation(6),
                NativeValueLocation::Activation(7),
            ]
        );
    }

    #[test]
    fn poll_spills_are_raw_even_when_their_bits_look_like_heap_references() {
        let values = [
            home(0, Repr::Tagged, ValueHome::Reg(1)),
            home(1, Repr::UnboxedFixnum, ValueHome::Reg(8)),
            home(2, Repr::UnboxedF64, ValueHome::Stack(0)),
        ];
        let map = NativeFrameValues::for_call(
            &values,
            &[Value(0)],
            4,
            &[],
            None,
            layout(),
            Some(2),
            |_| false,
        )
        .unwrap();
        assert_eq!(
            map.ssa_value(Value(1)).unwrap().locations(),
            &[NativeValueLocation::Stack(16)]
        );
        assert_eq!(
            map.ssa_value(Value(2)).unwrap().locations(),
            &[NativeValueLocation::Stack(0)]
        );
        assert_eq!(
            map.gc_locations().copied().collect::<Vec<_>>(),
            vec![NativeValueLocation::Activation(4)]
        );
    }

    #[test]
    fn reject_missing_roots_and_aliasing_or_out_of_bounds_copies() {
        let values = [
            home(0, Repr::Tagged, ValueHome::Reg(1)),
            home(1, Repr::Tagged, ValueHome::Reg(8)),
        ];
        for (roots, base, args, arg_base) in [
            (vec![Value(0)], 4, vec![], None),
            (vec![Value(0), Value(1)], 11, vec![], None),
            (vec![Value(0), Value(1)], 4, vec![Value(1)], Some(4)),
            (vec![Value(0), Value(1)], 4, vec![Value(2)], Some(6)),
        ] {
            assert!(
                NativeFrameValues::for_call(
                    &values,
                    &roots,
                    base,
                    &args,
                    arg_base,
                    layout(),
                    None,
                    |_| false
                )
                .is_err()
            );
        }
    }

    #[test]
    fn clobbered_nonroots_are_truthfully_unavailable() {
        let values = [home(0, Repr::Tagged, ValueHome::Reg(6))];
        let map = NativeFrameValues::for_call(&values, &[], 0, &[], None, layout(), None, |_| true)
            .unwrap();
        assert_eq!(
            map.ssa_value(Value(0)).unwrap().locations(),
            &[NativeValueLocation::Unavailable]
        );
        assert_eq!(map.gc_locations().count(), 0);
    }
}
