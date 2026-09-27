//! Generated C calls using caller-owned native argument and result storage.
use super::{
    AlienType,
    abi::{self, Layout},
    call::Scalar,
};
#[cfg(unix)]
use super::{
    abi::{Class, Placement},
    call::Location,
};
use crate::{TorclError, jit::JitBuffer};
use std::{
    collections::HashMap,
    mem::MaybeUninit,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Eq, Hash, PartialEq)]
struct Signature {
    result: Layout,
    arguments: Vec<Layout>,
    variadic: bool,
}
// Win64 indirect aggregate arguments require caller-owned, 16-byte-aligned
// copies. MaybeUninit preserves native padding without interpreting it.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct StorageBlock([MaybeUninit<u64>; 2]);

fn storage(bytes: u32) -> Vec<StorageBlock> {
    vec![StorageBlock([MaybeUninit::new(0); 2]); (bytes as usize).div_ceil(16)]
}

struct Adapter {
    code: JitBuffer,
}
static CACHE: OnceLock<Mutex<HashMap<Signature, Arc<Adapter>>>> = OnceLock::new();

/// Call C with scalar or aggregate native buffers. Each argument address points
/// to its declared C representation; result storage receives exactly its type's
/// size. Trailing variadic scalars undergo the usual C default promotions.
///
/// # Safety
/// Target/signature must agree. Each input must be readable for its type's size,
/// and the result writable for its size (null is allowed only for void). Native
/// pointer fields must remain valid across C and callbacks. Buffers must not
/// refer to movable Lisp storage. No libffi or external compiler is used here.
pub unsafe fn ffi_call_buffered(
    target: *const (),
    result: &AlienType,
    types: &[AlienType],
    arguments: &[*const u8],
    output: *mut u8,
    fixed_count: Option<usize>,
) -> Result<(), TorclError> {
    if target.is_null()
        || types.len() != arguments.len()
        || fixed_count.is_some_and(|n| n > types.len())
    {
        return Err(TorclError::FfiError(
            "invalid buffered foreign call arguments".into(),
        ));
    }
    let result = Layout::new(result)?;
    if result.size != 0 && output.is_null() {
        return Err(TorclError::FfiError("null foreign result buffer".into()));
    }
    let originals = types
        .iter()
        .map(Layout::new)
        .collect::<Result<Vec<_>, _>>()?;
    let mut normalized = originals.clone();
    for (index, layout) in normalized.iter_mut().enumerate() {
        if layout.size == 0 || arguments[index].is_null() {
            return Err(TorclError::FfiError(
                "void or null foreign argument buffer".into(),
            ));
        }
        if fixed_count.is_some_and(|count| index >= count) {
            *layout = match layout.scalar {
                Some(Scalar::Float) => Layout::scalar(Scalar::Double),
                Some(Scalar::Integer { bits: 8 | 16, .. }) => Layout::scalar(Scalar::Integer {
                    bits: 32,
                    signed: true,
                }),
                _ => layout.clone(),
            };
        }
    }
    let signature = Signature {
        result,
        arguments: normalized,
        variadic: fixed_count.is_some(),
    };
    let offsets = offsets(&signature.arguments)?;
    let byte_count = offsets.last().copied().unwrap_or(0);
    // MaybeUninit preserves potentially uninitialized C aggregate padding.
    // Only scalar values (which have no padding here) are interpreted in Rust.
    let mut slot_storage = storage(byte_count);
    // SAFETY: repr(C) gives each block two contiguous u64 slots with no padding;
    // its stronger alignment is also valid for this view of the allocation.
    let slots = unsafe {
        std::slice::from_raw_parts_mut(
            slot_storage.as_mut_ptr().cast::<MaybeUninit<u64>>(),
            byte_count as usize / 8,
        )
    };
    for (index, original) in originals.iter().enumerate() {
        let start = offsets[index] as usize / 8;
        unsafe {
            std::ptr::copy_nonoverlapping(
                arguments[index],
                slots.as_mut_ptr().add(start).cast(),
                original.size as usize,
            );
        }
        if let Some(scalar) = original.scalar {
            let bits = unsafe { slots[start].assume_init() };
            let bits = match scalar {
                Scalar::Integer {
                    bits: 8,
                    signed: true,
                } => bits as i8 as i32 as u32 as u64,
                Scalar::Integer {
                    bits: 16,
                    signed: true,
                } => bits as i16 as i32 as u32 as u64,
                Scalar::Float if signature.arguments[index].scalar == Some(Scalar::Double) => {
                    (f32::from_bits(bits as u32) as f64).to_bits()
                }
                _ => bits,
            };
            slots[start] = MaybeUninit::new(bits);
        }
    }
    let mut result_slots = storage(signature.result.slot_bytes()?);
    let adapter = {
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        if let Some(adapter) = cache.get(&signature) {
            Arc::clone(adapter)
        } else {
            let code = emit(&signature, &offsets)?;
            #[cfg(unix)]
            let code = JitBuffer::new(&code);
            let code = code.ok_or_else(|| {
                TorclError::FfiError("cannot allocate buffered foreign adapter".into())
            })?;
            let adapter = Arc::new(Adapter { code });
            if cache.len() >= 256 {
                let victim = cache.keys().next().unwrap().clone();
                cache.remove(&victim);
            }
            cache.insert(signature.clone(), Arc::clone(&adapter));
            adapter
        }
    };
    let errors = super::managed_callback::ForeignCallErrors::enter();
    let state = crate::safepoint::ForeignStateScope::native();
    let entry: unsafe extern "C" fn(*const (), *const MaybeUninit<u64>, *mut MaybeUninit<u64>) =
        unsafe { std::mem::transmute(adapter.code.as_ptr()) };
    unsafe {
        entry(target, slots.as_ptr(), result_slots.as_mut_ptr().cast());
    }
    drop(state);
    errors.finish()?;
    if signature.result.size != 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(
                result_slots.as_ptr().cast(),
                output,
                signature.result.size as usize,
            );
        }
    }
    Ok(())
}

fn offsets(arguments: &[Layout]) -> Result<Vec<u32>, TorclError> {
    let mut offsets = vec![0u32];
    for argument in arguments {
        let alignment = if cfg!(windows) { 16 } else { 8 };
        offsets.push(
            offsets
                .last()
                .unwrap()
                .checked_add(abi::align_up(argument.size, alignment)?)
                .filter(|n| *n <= i32::MAX as u32)
                .ok_or_else(abi::too_large)?,
        );
    }
    Ok(offsets)
}

#[cfg(unix)]
fn emit(signature: &Signature, offsets: &[u32]) -> Result<Vec<u8>, TorclError> {
    let memory_result = signature.result.classes.is_none();
    let plan = abi::assign(&signature.arguments, memory_result)?;
    let frame = abi::align_up(
        plan.stack_bytes.checked_add(8).ok_or_else(abi::too_large)?,
        16,
    )?;
    let mut code = vec![
        0xf3, 0x0f, 0x1e, 0xfa, 0x55, 0x48, 0x89, 0xe5, 0x49, 0x89, 0xfb, 0x49, 0x89, 0xf2, 0x48,
        0x81, 0xec,
    ];
    code.extend_from_slice(&frame.to_le_bytes());
    code.extend_from_slice(&[0x48, 0x89, 0x55, 0xf8]); // mov [rbp-8],rdx (result slots)
    // Copy spilled objects before assigning registers: REP MOVSQ uses the C
    // argument registers rdi/rsi/rcx, but preserves r10 (slots) and r11 (target).
    for (index, placement) in plan.placements.iter().enumerate() {
        if let Placement::Stack(at) = placement {
            code.extend_from_slice(&[0x49, 0x8d, 0xb2]);
            code.extend_from_slice(&offsets[index].to_le_bytes());
            code.extend_from_slice(&[0x48, 0x8d, 0xbc, 0x24]);
            code.extend_from_slice(&at.to_le_bytes());
            code.push(0xb9);
            code.extend_from_slice(&(signature.arguments[index].slot_bytes()? / 8).to_le_bytes());
            code.extend_from_slice(&[0xf3, 0x48, 0xa5]);
        }
    }
    if memory_result {
        code.extend_from_slice(&[0x48, 0x8b, 0x7d, 0xf8]);
    }
    for (index, placement) in plan.placements.iter().enumerate() {
        if let Placement::Registers(pieces) = placement {
            for (offset, location) in pieces {
                code.extend_from_slice(&[0x49, 0x8b, 0x82]);
                code.extend_from_slice(&(offsets[index] + offset).to_le_bytes());
                match location {
                    Location::Integer(register) => code.extend_from_slice(&[
                        0x48 | (register >> 3),
                        0x89,
                        0xc0 | (register & 7),
                    ]),
                    Location::Sse(register) => {
                        code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0 | (register << 3)])
                    }
                    Location::Stack(_) => unreachable!(),
                }
            }
        }
    }
    if signature.variadic {
        code.push(0xb8);
        code.extend_from_slice(&u32::from(plan.sse).to_le_bytes());
    }
    code.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11
    code.extend_from_slice(&[0x4c, 0x8b, 0x55, 0xf8]); // mov r10,[rbp-8]
    if let Some(classes) = &signature.result.classes {
        let (mut integer, mut sse) = (0, 0u8);
        for (index, class) in classes.iter().enumerate() {
            match class {
                Class::None => continue,
                Class::Integer => {
                    let register = [0u8, 2][integer];
                    integer += 1;
                    code.extend_from_slice(&[0x49, 0x89, 0x82 | (register << 3)]);
                }
                Class::Sse => {
                    code.extend_from_slice(&[0x66, 0x41, 0x0f, 0xd6, 0x82 | (sse << 3)]);
                    sse += 1;
                }
            }
            code.extend_from_slice(&(index as u32 * 8).to_le_bytes());
        }
    }
    code.extend_from_slice(&[0xc9, 0xc3]);
    Ok(code)
}

/// Win64 passes 1/2/4/8-byte aggregates as integers; all other aggregates
/// travel by address. A hidden result pointer consumes argument position zero.
#[cfg(windows)]
fn indirect(layout: &Layout) -> bool {
    layout.scalar.is_none() && !matches!(layout.size, 1 | 2 | 4 | 8)
}

#[cfg(windows)]
fn emit(signature: &Signature, offsets: &[u32]) -> Result<Option<JitBuffer>, TorclError> {
    use super::win64;
    let hidden = indirect(&signature.result);
    let positions = signature
        .arguments
        .len()
        .checked_add(usize::from(hidden))
        .ok_or_else(abi::too_large)?;
    let saved_result = u32::try_from(positions.max(4))
        .ok()
        .and_then(|n| n.checked_mul(8))
        .ok_or_else(abi::too_large)?;
    let frame = abi::align_up(saved_result.checked_add(8).ok_or_else(abi::too_large)?, 16)?;
    let (mut code, unwind) = win64::prologue(frame)?;
    code.extend_from_slice(&[
        0x49, 0x89, 0xcb, // mov r11,rcx (target)
        0x49, 0x89, 0xd2, // mov r10,rdx (staging slots)
        0x4c, 0x89, 0x84, 0x24, // mov [rsp+saved_result],r8
    ]);
    code.extend_from_slice(&saved_result.to_le_bytes());
    if hidden {
        code.extend_from_slice(&[0x4c, 0x89, 0xc1]); // mov rcx,r8 (hidden result)
    }
    for (index, layout) in signature.arguments.iter().enumerate() {
        let position = index + usize::from(hidden);
        // Staging already owns an aligned copy; the C callee can modify it
        // without changing the caller's original by-value argument object.
        code.extend_from_slice(&[0x49, if indirect(layout) { 0x8d } else { 0x8b }, 0x82]);
        code.extend_from_slice(&offsets[index].to_le_bytes());
        if position >= 4 {
            code.extend_from_slice(&[0x48, 0x89, 0x84, 0x24]);
            code.extend_from_slice(&((position as u32) * 8).to_le_bytes());
        } else {
            let floating = matches!(layout.scalar, Some(Scalar::Float | Scalar::Double));
            if floating {
                code.extend_from_slice(&[0x66, 0x48, 0x0f, 0x6e, 0xc0 | ((position as u8) << 3)]);
            }
            if !floating || signature.variadic {
                let register = win64::ARGUMENT_REGISTERS[position];
                code.extend_from_slice(&[0x48 | (register >> 3), 0x89, 0xc0 | (register & 7)]);
            }
        }
    }
    code.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11
    if !hidden && signature.result.size != 0 {
        code.extend_from_slice(&[0x4c, 0x8b, 0x94, 0x24]); // mov r10,[rsp+saved_result]
        code.extend_from_slice(&saved_result.to_le_bytes());
        if matches!(
            signature.result.scalar,
            Some(Scalar::Float | Scalar::Double)
        ) {
            code.extend_from_slice(&[0x66, 0x41, 0x0f, 0xd6, 0x02]); // movq [r10],xmm0
        } else {
            code.extend_from_slice(&[0x49, 0x89, 0x02]); // mov [r10],rax
        }
    }
    Ok(win64::finish(code, frame, &unwind))
}
