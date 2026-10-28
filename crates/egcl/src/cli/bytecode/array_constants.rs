// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Portable array graph metadata; this snapshot path never allocates Lisp data.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct ComplexMetadata {
    pub(super) fill_pointer: u64,
    pub(super) adjustable: bool,
    pub(super) element_type: u8,
    pub(super) has_fill_pointer: bool,
    pub(super) displacement: Option<(u64, u64)>,
}

pub(super) fn complex_parts(value: EgclVal) -> Option<(EgclVal, ComplexMetadata)> {
    if !egcl_stdlib::is_complex_vector(value) {
        return None;
    }
    let storage = unsafe { *value.as_ptr().add(8).cast::<EgclVal>() };
    let element = unsafe { *value.as_ptr().add(32).cast::<EgclVal>() };
    if !element.is_fixnum() {
        return None;
    }
    let fill_pointer = unsafe { *value.as_ptr().add(16).cast::<EgclVal>() };
    if !fill_pointer.is_fixnum() {
        return None;
    }
    let metadata = ComplexMetadata {
        fill_pointer: u64::try_from(fill_pointer.as_fixnum()).ok()?,
        adjustable: egcl_stdlib::cvec_adjustable(value),
        element_type: u8::try_from(element.as_fixnum()).ok()?,
        has_fill_pointer: egcl_stdlib::cvec_has_fill_pointer(value),
        displacement: egcl_stdlib::cvec_displacement(value)
            .map(|(_, offset, size)| (offset as u64, size as u64)),
    };
    metadata.validate().ok()?;
    Some((storage, metadata))
}

impl ComplexMetadata {
    fn validate(&self) -> Result<(), EgclError> {
        let limit = (1u64 << 60) - 1;
        if self.fill_pointer > limit
            || !matches!(self.element_type, 0 | 1 | 2 | 11 | 12)
            || self
                .displacement
                .is_some_and(|(offset, size)| offset > limit || size > limit)
        {
            return Err(bbu_error("invalid complex array metadata"));
        }
        Ok(())
    }

    pub(super) fn encode(&self, bytes: &mut Vec<u8>) {
        put_u64(bytes, self.fill_pointer);
        put_u8(bytes, u8::from(self.adjustable));
        put_u8(bytes, self.element_type);
        put_u8(bytes, u8::from(self.has_fill_pointer));
        put_u8(bytes, u8::from(self.displacement.is_some()));
        if let Some((offset, size)) = self.displacement {
            put_u64(bytes, offset);
            put_u64(bytes, size);
        }
    }

    pub(super) fn parse(cursor: &mut BbuCursor<'_>) -> Result<Self, EgclError> {
        fn boolean(cursor: &mut BbuCursor<'_>) -> Result<bool, EgclError> {
            match cursor.u8()? {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(bbu_error("invalid array flag")),
            }
        }
        let metadata = Self {
            fill_pointer: cursor.u64()?,
            adjustable: boolean(cursor)?,
            element_type: cursor.u8()?,
            has_fill_pointer: boolean(cursor)?,
            displacement: if boolean(cursor)? {
                Some((cursor.u64()?, cursor.u64()?))
            } else {
                None
            },
        };
        metadata.validate()?;
        Ok(metadata)
    }
}

/// Fixed-width array leaves need fresh, mutable storage after materialization.
/// Ordinary symbol/name strings keep the existing pooled string encoding.
pub(super) fn leaf_payload(value: EgclVal) -> Option<Vec<u8>> {
    if let Some(length) = egcl_rt::packed_u8::len(value) {
        let mut bytes = vec![23];
        put_u32(&mut bytes, u32::try_from(length).ok()?);
        for index in 0..length {
            bytes.push(egcl_rt::packed_u8::get(value, index)?);
        }
        return Some(bytes);
    }
    if let Some(length) = egcl_rt::packed_f64::len(value) {
        let mut bytes = vec![27];
        put_u32(&mut bytes, u32::try_from(length).ok()?);
        for index in 0..length {
            put_u64(
                &mut bytes,
                egcl_rt::packed_f64::get(value, index)?.to_bits(),
            );
        }
        return Some(bytes);
    }
    if let Some(length) = egcl_rt::types::bit_vector_len(value) {
        let mut bytes = vec![28];
        put_u32(&mut bytes, u32::try_from(length).ok()?);
        for index in 0..length {
            bytes.push(egcl_rt::types::bit_vector_ref(value, index)?);
        }
        return Some(bytes);
    }
    if value.is_string() {
        let text = value.as_string();
        let mut bytes = vec![29, u8::from(value.is_base_string())];
        put_u32(&mut bytes, u32::try_from(text.len()).ok()?);
        bytes.extend_from_slice(text.as_bytes());
        return Some(bytes);
    }
    None
}

/// Validate references and logical sizes using the constant descriptors, before
/// any shells are exposed. Only storage edges participate in this acyclic walk;
/// self-references through ordinary array *elements* remain legal.
pub(super) fn validate_graph(constants: &[BbuConstant]) -> Result<(), EgclError> {
    if !constants.iter().any(|c| {
        matches!(
            c,
            BbuConstant::MdStorage { .. } | BbuConstant::ComplexArray { .. }
        )
    }) {
        return Ok(());
    }
    let mut states = vec![0u8; constants.len()];
    let mut sizes = vec![0u64; constants.len()];
    for root in 0..constants.len() {
        if !matches!(
            constants[root],
            BbuConstant::MdStorage { .. } | BbuConstant::ComplexArray { .. }
        ) {
            continue;
        }
        let mut pending = vec![(root, false)];
        while let Some((index, finish)) = pending.pop() {
            if !finish {
                if states[index] == 2 {
                    continue;
                }
                if states[index] == 1 {
                    return Err(bbu_error("circular array storage"));
                }
                states[index] = 1;
                pending.push((index, true));
                if let BbuConstant::MdStorage { storage_ref, .. }
                | BbuConstant::MdPackedU8 { storage_ref, .. }
                | BbuConstant::ComplexArray { storage_ref, .. } = &constants[index]
                {
                    let child = bbu_index(*storage_ref, constants.len(), "array storage")?;
                    pending.push((child, false));
                }
                continue;
            }
            let size = match &constants[index] {
                BbuConstant::Vector(values) => values.len() as u64,
                BbuConstant::PackedU8(values) => values.len() as u64,
                BbuConstant::PackedF64(values) => values.len() as u64,
                BbuConstant::BitArray(values) => values.len() as u64,
                BbuConstant::ArrayString { text, .. } | BbuConstant::String(text) => {
                    text.chars().count() as u64
                }
                BbuConstant::MdArray {
                    dimensions,
                    element_refs,
                } => {
                    let total = dimensions
                        .iter()
                        .try_fold(1u64, |n, d| n.checked_mul(*d))
                        .ok_or_else(|| bbu_error("array dimension product overflow"))?;
                    if total != element_refs.len() as u64 {
                        return Err(bbu_error("invalid array element count"));
                    }
                    total
                }
                BbuConstant::MdPackedU8 {
                    dimensions,
                    storage_ref,
                }
                | BbuConstant::MdStorage {
                    dimensions,
                    storage_ref,
                } => {
                    if dimensions.len() > 1024
                        || dimensions
                            .iter()
                            .any(|&d| d > ((1u64 << 60) - 1) || usize::try_from(d).is_err())
                    {
                        return Err(bbu_error("invalid array dimensions"));
                    }
                    if dimensions.len() == 1
                        && matches!(constants[index], BbuConstant::MdStorage { .. })
                    {
                        return Err(bbu_error("invalid multidimensional array rank"));
                    }
                    let total = dimensions
                        .iter()
                        .try_fold(1u64, |n, d| n.checked_mul(*d))
                        .ok_or_else(|| bbu_error("array dimension product overflow"))?;
                    let child = bbu_index(*storage_ref, constants.len(), "array storage")?;
                    if matches!(
                        constants[child],
                        BbuConstant::MdArray { .. }
                            | BbuConstant::MdStorage { .. }
                            | BbuConstant::MdPackedU8 { .. }
                    ) {
                        return Err(bbu_error("multidimensional backing is not a vector"));
                    }
                    if sizes[child] != total {
                        return Err(bbu_error("array shape differs from storage"));
                    }
                    total
                }
                BbuConstant::ComplexArray {
                    storage_ref,
                    metadata,
                } => {
                    metadata.validate()?;
                    let capacity =
                        sizes[bbu_index(*storage_ref, constants.len(), "array storage")?];
                    let size = if let Some((offset, size)) = metadata.displacement {
                        if offset.checked_add(size).is_none_or(|end| end > capacity) {
                            return Err(bbu_error("array displacement exceeds storage"));
                        }
                        size
                    } else {
                        capacity
                    };
                    if metadata.fill_pointer > size
                        || (!metadata.has_fill_pointer && metadata.fill_pointer != size)
                    {
                        return Err(bbu_error("invalid array fill pointer"));
                    }
                    size
                }
                _ => return Err(bbu_error("array storage is not an array")),
            };
            if size > ((1u64 << 60) - 1) || usize::try_from(size).is_err() {
                return Err(bbu_error("array size exceeds host fixnum range"));
            }
            sizes[index] = size;
            states[index] = 2;
        }
    }
    Ok(())
}

/// All shells and edges now exist. Check actual storage element compatibility
/// before hash tables or load actions can observe the reconstructed objects.
pub(super) fn validate_values(
    constants: &[BbuConstant],
    values: &[EgclVal],
) -> Result<(), EgclError> {
    use egcl_stdlib::sequences::ArrayElementKind as Kind;
    for (index, constant) in constants.iter().enumerate() {
        let BbuConstant::ComplexArray {
            storage_ref,
            metadata,
        } = constant
        else {
            continue;
        };
        let storage = values[bbu_index(*storage_ref, values.len(), "array storage")?];
        // Direct accessors require physical flat storage. Only displaced
        // wrappers interpret another array through its row-major accessors.
        if metadata.displacement.is_none() {
            let general = storage.is_heap_object()
                && unsafe { (*storage.as_ptr().cast::<egcl_rt::ObjectHeader>()).type_id() }
                    == egcl_rt::object::type_id::SIMPLE_VECTOR;
            let direct = match metadata.element_type {
                0 => general,
                1 => storage.is_string() || general,
                2 => egcl_rt::types::bit_vector_len(storage).is_some() || general,
                11 => egcl_rt::packed_f64::is_vector(storage),
                12 => egcl_rt::packed_u8::is_vector(storage),
                _ => false,
            };
            if !direct {
                return Err(bbu_error("nondisplaced array requires flat storage"));
            }
        }
        let kind = egcl_stdlib::sequences::array_element_kind(storage)
            .ok_or_else(|| bbu_error("invalid array backing"))?;
        let matches = match metadata.element_type {
            0 => kind == Kind::General,
            1 => matches!(kind, Kind::BaseChar | Kind::Character),
            2 => kind == Kind::Bit,
            11 => kind == Kind::DoubleFloat,
            12 => kind == Kind::U8,
            _ => false,
        };
        // Legacy complex bit/character arrays used a general vector internally.
        // Accept only checked values, never a mismatched displaced base.
        if !matches {
            if metadata.displacement.is_some()
                || kind != Kind::General
                || !matches!(metadata.element_type, 1 | 2)
            {
                return Err(bbu_error("complex array element type differs from storage"));
            }
            let elements = bbu_vector_elements(storage)
                .ok_or_else(|| bbu_error("invalid legacy typed storage"))?;
            if elements.iter().any(|value| {
                if metadata.element_type == 1 {
                    !value.is_character()
                } else {
                    !value.is_fixnum() || !matches!(value.as_fixnum(), 0 | 1)
                }
            }) {
                return Err(bbu_error("invalid typed array element"));
            }
        }
        if egcl_stdlib::array_total_size(values[index]).is_none() {
            return Err(bbu_error("invalid restored array"));
        }
    }
    Ok(())
}
