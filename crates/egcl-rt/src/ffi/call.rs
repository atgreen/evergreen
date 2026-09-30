// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled foreign-call adapters. Scalar slots contain raw C value bits,
//! never Lisp heap references. The caller owns rooting and native transitions.

use super::AlienType;
use crate::{error::EgclError, jit::JitBuffer};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Scalar {
    Void,
    Integer { bits: u8, signed: bool },
    Float,
    Double,
}

impl Scalar {
    pub(super) fn from_type(ty: &AlienType) -> Result<Self, EgclError> {
        match ty {
            AlienType::Void => Ok(Self::Void),
            AlienType::Int {
                bits: bits @ (8 | 16 | 32 | 64),
                signed,
            } => Ok(Self::Integer {
                bits: *bits,
                signed: *signed,
            }),
            AlienType::Pointer(_) | AlienType::FnPtr { .. } => Ok(Self::Integer {
                bits: 64,
                signed: false,
            }),
            AlienType::Float => Ok(Self::Float),
            AlienType::Double => Ok(Self::Double),
            _ => Err(EgclError::FfiError(format!(
                "unsupported foreign call type: {ty:?}"
            ))),
        }
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct Signature {
    result: Scalar,
    arguments: Vec<Scalar>,
    fixed_count: Option<usize>,
}

pub(super) struct CallAdapter {
    code: JitBuffer,
}

static CACHE: OnceLock<Mutex<HashMap<Signature, Arc<CallAdapter>>>> = OnceLock::new();

impl CallAdapter {
    pub(super) fn get(
        result: &AlienType,
        arguments: &[AlienType],
        fixed_count: Option<usize>,
    ) -> Result<Arc<Self>, EgclError> {
        let signature = Signature {
            fixed_count,
            result: Scalar::from_type(result)?,
            arguments: arguments
                .iter()
                .map(Scalar::from_type)
                .collect::<Result<_, _>>()?,
        };
        if signature.arguments.contains(&Scalar::Void) {
            return Err(EgclError::FfiError("void is not an argument type".into()));
        }
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        if let Some(adapter) = cache.get(&signature) {
            return Ok(Arc::clone(adapter));
        }
        let code = emit(&signature)?;
        let adapter = Arc::new(Self { code });
        // Bound retained executable mappings. An evicted adapter remains alive
        // until its active calls release their Arc, including reentrant calls.
        if cache.len() >= 256 {
            let victim = cache.keys().next().unwrap().clone();
            cache.remove(&victim);
        }
        cache.insert(signature, Arc::clone(&adapter));
        Ok(adapter)
    }

    /// # Safety
    /// The target and argument slots must match the signature used by `get`.
    pub(super) unsafe fn invoke(&self, target: *const (), arguments: &[u64]) -> u64 {
        // SAFETY: emit builds this entry ABI; the RX mapping lives through this
        // borrow. The caller guarantees the foreign target's actual signature.
        let entry: unsafe extern "C" fn(*const (), *const u64) -> u64 =
            unsafe { std::mem::transmute(self.code.as_ptr()) };
        unsafe { entry(target, arguments.as_ptr()) }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Location {
    Integer(u8),
    Sse(u8),
    Stack(u32),
}

pub(super) struct ScalarPlan {
    pub(super) locations: Vec<Location>,
    pub(super) stack_bytes: u32,
    #[cfg(unix)]
    pub(super) sse: u8,
}

/// The same scalar ABI assignment is used by outbound calls and inbound entries.
#[cfg(unix)]
pub(super) fn classify(arguments: &[Scalar]) -> Result<ScalarPlan, EgclError> {
    if arguments.len() > (i32::MAX as usize - 16) / 8 {
        return Err(signature_too_large());
    }
    // Scalars are the one-eightbyte case of the same placement used for
    // aggregates. Callback entries consume this plan as well as outbound calls.
    let layouts: Vec<_> = arguments
        .iter()
        .copied()
        .map(super::abi::Layout::scalar)
        .collect();
    let plan = super::abi::assign(&layouts, false)?;
    let locations = plan
        .placements
        .into_iter()
        .map(|placement| match placement {
            super::abi::Placement::Registers(pieces) => pieces.into_iter().next().unwrap().1,
            super::abi::Placement::Stack(offset) => Location::Stack(offset),
        })
        .collect();
    Ok(ScalarPlan {
        locations,
        stack_bytes: plan.stack_bytes,
        sse: plan.sse,
    })
}

#[cfg(windows)]
pub(super) fn classify(arguments: &[Scalar]) -> Result<ScalarPlan, EgclError> {
    if arguments.contains(&Scalar::Void) {
        return Err(EgclError::FfiError("void is not an argument type".into()));
    }
    // Callback displacements include both the local slots and incoming stack.
    if arguments.len() > (i32::MAX as usize - 64) / 16 {
        return Err(signature_too_large());
    }
    let locations = arguments
        .iter()
        .enumerate()
        .map(|(index, scalar)| {
            if index >= 4 {
                Location::Stack(index as u32 * 8)
            } else if matches!(scalar, Scalar::Float | Scalar::Double) {
                Location::Sse(index as u8)
            } else {
                Location::Integer(super::win64::ARGUMENT_REGISTERS[index])
            }
        })
        .collect();
    Ok(ScalarPlan {
        locations,
        stack_bytes: arguments.len().max(4) as u32 * 8,
    })
}

fn emit(signature: &Signature) -> Result<JitBuffer, EgclError> {
    let plan = classify(&signature.arguments)?;
    let frame_bytes = plan
        .stack_bytes
        .checked_add(15)
        .ok_or_else(signature_too_large)?
        & !15;
    if frame_bytes > i32::MAX as u32 || signature.arguments.len() > i32::MAX as usize / 8 {
        return Err(signature_too_large());
    }
    // Entry: rdi=target, rsi=slots. Keep these in caller-saved scratch registers
    // while constructing arguments. push rbp aligns rsp to 16 before the call.
    #[cfg(unix)]
    let mut code = vec![
        0xf3, 0x0f, 0x1e, 0xfa, // endbr64 (also valid without CET)
        0x55, // push rbp
        0x48, 0x89, 0xe5, // mov rbp,rsp
        0x49, 0x89, 0xfb, // mov r11,rdi
        0x49, 0x89, 0xf2, // mov r10,rsi
        0x48, 0x81, 0xec, // sub rsp,frame_bytes
    ];
    #[cfg(unix)]
    code.extend_from_slice(&frame_bytes.to_le_bytes());
    #[cfg(windows)]
    let (mut code, unwind) = super::win64::prologue(frame_bytes)?;
    #[cfg(windows)]
    code.extend_from_slice(&[
        0x49, 0x89, 0xcb, // mov r11,rcx (target)
        0x49, 0x89, 0xd2, // mov r10,rdx (slots)
    ]);
    for (index, location) in plan.locations.iter().enumerate() {
        code.extend_from_slice(&[0x49, 0x8b, 0x82]); // mov rax,[r10+disp32]
        code.extend_from_slice(&((index * 8) as u32).to_le_bytes());
        // Extend narrow C integers to 32 bits. Clang callees may rely on the
        // caller doing this, even though the storage slot carries only raw bits.
        match signature.arguments[index] {
            Scalar::Integer {
                bits: 8,
                signed: true,
            } => code.extend_from_slice(&[0x0f, 0xbe, 0xc0]),
            Scalar::Integer {
                bits: 16,
                signed: true,
            } => code.extend_from_slice(&[0x0f, 0xbf, 0xc0]),
            Scalar::Integer {
                bits: 8,
                signed: false,
            } => code.extend_from_slice(&[0x0f, 0xb6, 0xc0]),
            Scalar::Integer {
                bits: 16,
                signed: false,
            } => code.extend_from_slice(&[0x0f, 0xb7, 0xc0]),
            _ => {}
        }
        match location {
            Location::Integer(register) => {
                code.extend_from_slice(&[0x48 | (register >> 3), 0x89, 0xc0 | (register & 7)]);
            }
            Location::Sse(register) => {
                code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0 | (register << 3)]);
                #[cfg(windows)]
                if signature.fixed_count.is_some() {
                    let gpr = super::win64::ARGUMENT_REGISTERS[*register as usize];
                    code.extend_from_slice(&[0x48 | (gpr >> 3), 0x89, 0xc0 | (gpr & 7)]);
                }
            }
            Location::Stack(offset) => {
                code.extend_from_slice(&[0x48, 0x89, 0x84, 0x24]); // mov [rsp+disp32],rax
                code.extend_from_slice(&offset.to_le_bytes());
            }
        }
    }
    #[cfg(unix)]
    if signature.fixed_count.is_some() {
        // SysV variadic calls pass the number of used vector registers in AL,
        // including those occupied by named arguments. All argument moves are
        // complete, so eax is available as scratch without clobbering a value.
        code.push(0xb8); // mov eax,imm32
        code.extend_from_slice(&(plan.sse as u32).to_le_bytes());
    }
    code.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11
    match signature.result {
        Scalar::Void => code.extend_from_slice(&[0x31, 0xc0]), // xor eax,eax
        Scalar::Float => code.extend_from_slice(&[0x66, 0x0f, 0x7e, 0xc0]), // movd eax,xmm0
        Scalar::Double => code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x7e, 0xc0]), // movq rax,xmm0
        Scalar::Integer { bits: 8, .. } => code.extend_from_slice(&[0x0f, 0xb6, 0xc0]), // movzx eax,al
        Scalar::Integer { bits: 16, .. } => code.extend_from_slice(&[0x0f, 0xb7, 0xc0]), // movzx eax,ax
        Scalar::Integer { bits: 32, .. } => code.extend_from_slice(&[0x89, 0xc0]), // mov eax,eax
        Scalar::Integer { .. } => {}
    }
    #[cfg(unix)]
    let buffer = {
        code.extend_from_slice(&[0xc9, 0xc3]); // leave; ret
        JitBuffer::new(&code)
    };
    #[cfg(windows)]
    let buffer = super::win64::finish(code, frame_bytes, &unwind);
    buffer.ok_or_else(|| EgclError::FfiError("cannot allocate foreign call adapter".into()))
}

fn signature_too_large() -> EgclError {
    EgclError::FfiError("foreign signature exceeds adapter displacement limits".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn answer() -> u64 {
        42
    }

    #[test]
    fn active_adapter_survives_cache_eviction() {
        let ty = AlienType::Int {
            bits: 64,
            signed: false,
        };
        let adapter = CallAdapter::get(&ty, &[], None).unwrap();
        // Simulate eviction while an invocation owns the adapter. This is
        // deterministic even when other tests are accessing the shared cache.
        CACHE.get().unwrap().lock().unwrap().clear();
        assert_eq!(unsafe { adapter.invoke(answer as *const (), &[]) }, 42);
    }
}
