// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! C layouts shared by x86-64 targets, plus SysV AMD64 argument placement.
//! SysV classification follows psABI §3.2.3. There are no vector or x87 descriptors,
//! so aggregates above two eightbytes use MEMORY rather than SSEUP registers.
#[cfg(unix)]
use super::call::Location;
use super::{AlienType, call::Scalar};
use crate::EgclError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Class {
    None,
    Integer,
    Sse,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Layout {
    pub size: u32,
    pub alignment: u32,
    pub scalar: Option<Scalar>,
    /// None is MEMORY; Some contains one class per eightbyte.
    pub classes: Option<Vec<Class>>,
}

pub(super) fn too_large() -> EgclError {
    EgclError::FfiError("foreign layout exceeds adapter displacement limits".into())
}

pub(super) fn align_up(size: u32, alignment: u32) -> Result<u32, EgclError> {
    size.checked_add(alignment - 1)
        .map(|n| n & !(alignment - 1))
        .filter(|n| *n <= i32::MAX as u32)
        .ok_or_else(too_large)
}

impl Layout {
    pub fn scalar(value: Scalar) -> Self {
        let (size, class) = match value {
            Scalar::Void => (0, Class::None),
            Scalar::Integer { bits, .. } => (u32::from(bits) / 8, Class::Integer),
            Scalar::Float => (4, Class::Sse),
            Scalar::Double => (8, Class::Sse),
        };
        Self {
            size,
            alignment: size.max(1),
            scalar: Some(value),
            classes: Some(if size == 0 { vec![] } else { vec![class] }),
        }
    }

    pub fn new(ty: &AlienType) -> Result<Self, EgclError> {
        let shape = shape(ty, 0)?;
        let scalar = match ty {
            AlienType::Struct { .. } | AlienType::Union { .. } => None,
            _ => Some(Scalar::from_type(ty)?),
        };
        let mut classes = vec![Class::None; (shape.size as usize).div_ceil(8).min(2)];
        let mut memory = shape.size > 16;
        if !memory {
            for (offset, leaf) in shape.leaves {
                let field = Self::scalar(leaf);
                if offset % field.alignment != 0 {
                    memory = true;
                    break;
                }
                let class = field.classes.unwrap()[0];
                for index in offset / 8..=(offset + field.size - 1) / 8 {
                    let old = &mut classes[index as usize];
                    *old = if *old == Class::Integer || class == Class::Integer {
                        Class::Integer
                    } else {
                        class
                    };
                }
            }
        }
        Ok(Self {
            size: shape.size,
            alignment: shape.alignment,
            scalar,
            classes: (!memory).then_some(classes),
        })
    }

    pub fn slot_bytes(&self) -> Result<u32, EgclError> {
        align_up(self.size, 8)
    }
}

struct Shape {
    size: u32,
    alignment: u32,
    leaves: Vec<(u32, Scalar)>,
}

fn shape(ty: &AlienType, depth: usize) -> Result<Shape, EgclError> {
    if depth > 64 {
        return Err(EgclError::FfiError(
            "foreign aggregate nesting exceeds 64 levels".into(),
        ));
    }
    let (fields, packed, union) = match ty {
        AlienType::Struct { fields, packed } => (fields, *packed, false),
        AlienType::Union { variants } => (variants, false, true),
        _ => {
            let scalar = Scalar::from_type(ty)?;
            let layout = Layout::scalar(scalar);
            return Ok(Shape {
                size: layout.size,
                alignment: layout.alignment,
                leaves: if scalar == Scalar::Void {
                    vec![]
                } else {
                    vec![(0, scalar)]
                },
            });
        }
    };
    if fields.is_empty() {
        return Err(EgclError::FfiError(
            "empty foreign aggregates are unsupported".into(),
        ));
    }
    let mut shape = Shape {
        size: 0,
        alignment: 1,
        leaves: vec![],
    };
    for field in fields {
        let child = shape_field(field, depth + 1)?;
        let alignment = if packed { 1 } else { child.alignment };
        shape.alignment = shape.alignment.max(alignment);
        let offset = if union {
            0
        } else {
            align_up(shape.size, alignment)?
        };
        let end = offset
            .checked_add(child.size)
            .filter(|n| *n <= i32::MAX as u32)
            .ok_or_else(too_large)?;
        shape.size = shape.size.max(end);
        shape.leaves.extend(
            child
                .leaves
                .into_iter()
                .map(|(at, scalar)| (offset + at, scalar)),
        );
    }
    shape.size = align_up(shape.size, shape.alignment)?;
    Ok(shape)
}

fn shape_field(ty: &AlienType, depth: usize) -> Result<Shape, EgclError> {
    let shape = shape(ty, depth)?;
    if shape.size == 0 {
        return Err(EgclError::FfiError(
            "void is not an aggregate field type".into(),
        ));
    }
    Ok(shape)
}

#[cfg(unix)]
pub(super) enum Placement {
    Registers(Vec<(u32, Location)>),
    Stack(u32),
}
#[cfg(unix)]
pub(super) struct Assignment {
    pub placements: Vec<Placement>,
    pub stack_bytes: u32,
    pub sse: u8,
}

#[cfg(unix)]
pub(super) fn assign(arguments: &[Layout], hidden_result: bool) -> Result<Assignment, EgclError> {
    const GPRS: [u8; 6] = [7, 6, 2, 1, 8, 9];
    let mut integer = usize::from(hidden_result);
    let mut sse = 0u8;
    let mut stack_bytes = 0;
    let mut placements = Vec::with_capacity(arguments.len());
    for argument in arguments {
        if argument.size == 0 {
            return Err(EgclError::FfiError("void is not an argument type".into()));
        }
        if let Some(classes) = &argument.classes {
            let integers = classes
                .iter()
                .filter(|class| **class == Class::Integer)
                .count();
            let vectors = classes.iter().filter(|class| **class == Class::Sse).count();
            // Reserve only when the entire argument fits, so exhaustion of one
            // bank never consumes space in the other bank for a spilled value.
            if integer + integers <= 6 && usize::from(sse) + vectors <= 8 {
                let mut pieces = Vec::new();
                for (index, class) in classes.iter().enumerate() {
                    let location = match class {
                        Class::None => continue,
                        Class::Integer => {
                            let register = GPRS[integer];
                            integer += 1;
                            Location::Integer(register)
                        }
                        Class::Sse => {
                            let register = sse;
                            sse += 1;
                            Location::Sse(register)
                        }
                    };
                    pieces.push((index as u32 * 8, location));
                }
                placements.push(Placement::Registers(pieces));
                continue;
            }
        }
        stack_bytes = align_up(stack_bytes, argument.alignment.max(8))?;
        placements.push(Placement::Stack(stack_bytes));
        stack_bytes = stack_bytes
            .checked_add(argument.slot_bytes()?)
            .filter(|n| *n <= i32::MAX as u32)
            .ok_or_else(too_large)?;
    }
    Ok(Assignment {
        placements,
        stack_bytes,
        sse,
    })
}
