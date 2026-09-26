//! Lisp-facing foreign types and memory operations. Native byte access and
//! allocation ownership live in torcl-rt; public lambda lists live in boot.lisp.
use torcl_rt::ffi::{AlienType, marshal_to_c, memory::ForeignPointer, unmarshal_from_c};
use torcl_rt::value::{NIL, T};
use torcl_rt::{TorclError, TorclVal};

fn symbol_name(value: TorclVal) -> String {
    value
        .symbol_index()
        .and_then(torcl_rt::symbols::symbol_name)
        .unwrap_or_default()
}

pub fn alien_type(keyword: TorclVal) -> Result<AlienType, TorclError> {
    let name = symbol_name(keyword);
    let int = |signed, bits| AlienType::Int { signed, bits };
    Ok(match name.rsplit(':').next().unwrap_or_default() {
        "VOID" => AlienType::Void,
        "CHAR" | "INT8" | "SIGNED-CHAR" => int(true, 8),
        "UCHAR" | "UINT8" | "UNSIGNED-CHAR" => int(false, 8),
        "SHORT" | "INT16" => int(true, 16),
        "USHORT" | "UINT16" | "UNSIGNED-SHORT" => int(false, 16),
        "INT" | "INT32" => int(true, 32),
        "UINT" | "UINT32" | "UNSIGNED-INT" => int(false, 32),
        "LONG" => int(true, (std::mem::size_of::<std::ffi::c_long>() * 8) as u8),
        "ULONG" | "UNSIGNED-LONG" => {
            int(false, (std::mem::size_of::<std::ffi::c_ulong>() * 8) as u8)
        }
        "LONG-LONG" | "INT64" => int(true, 64),
        "UNSIGNED-LONG-LONG" | "UINT64" => int(false, 64),
        "SIZE-T" => int(false, usize::BITS as u8),
        "FLOAT" => AlienType::Float,
        "DOUBLE" => AlienType::Double,
        "POINTER" | "STRING" => AlienType::Pointer(Box::new(AlienType::Void)),
        _ => {
            return Err(TorclError::FfiError(format!(
                "unknown foreign type {name:?}"
            )));
        }
    })
}

fn unsigned(value: TorclVal) -> Result<usize, TorclError> {
    Ok(marshal_to_c(
        value,
        &AlienType::Int {
            signed: false,
            bits: usize::BITS as u8,
        },
    )? as usize)
}

fn offset(value: TorclVal) -> Result<isize, TorclError> {
    Ok(marshal_to_c(
        value,
        &AlienType::Int {
            signed: true,
            bits: isize::BITS as u8,
        },
    )? as isize)
}

/// Called only after the evaluator's FFI sandbox gate. Borrowed pointers expose
/// an intentionally unsafe Lisp API; their validity is the foreign caller's
/// responsibility, just as with CFFI on other implementations.
pub fn memory_call(args: &[TorclVal]) -> Result<TorclVal, TorclError> {
    let operation = args.first().copied().map(symbol_name).unwrap_or_default();
    let truth = |yes| if yes { T } else { NIL };
    match (
        operation.rsplit(':').next().unwrap_or_default(),
        &args[args.len().min(1)..],
    ) {
        ("POINTERP", [value]) => Ok(truth(ForeignPointer::is_pointer(*value))),
        ("MAKE-POINTER", [address]) => {
            ForeignPointer::from_address(unsigned(*address)?).into_lisp()
        }
        ("POINTER-ADDRESS", [pointer]) => unmarshal_from_c(
            ForeignPointer::from_lisp(*pointer)?.address() as u64,
            &AlienType::Int {
                signed: false,
                bits: usize::BITS as u8,
            },
        ),
        ("POINTER-EQ", [a, b]) => Ok(truth(
            ForeignPointer::from_lisp(*a)?.address() == ForeignPointer::from_lisp(*b)?.address(),
        )),
        ("INC-POINTER", [pointer, bytes]) => ForeignPointer::from_lisp(*pointer)?
            .offset(offset(*bytes)?)?
            .into_lisp(),
        ("ALLOC", [size]) => {
            let pointer = ForeignPointer::allocate(unsigned(*size)?)?;
            match pointer.into_lisp() {
                Ok(value) => Ok(value),
                Err(error) => {
                    pointer.free()?;
                    Err(error)
                }
            }
        }
        ("FREE", [pointer]) => {
            ForeignPointer::from_lisp(*pointer)?.free()?;
            Ok(NIL)
        }
        ("TYPE-SIZE", [ty]) => Ok(TorclVal::from_fixnum(alien_type(*ty)?.size() as i64)),
        ("TYPE-ALIGNMENT", [ty]) => Ok(TorclVal::from_fixnum(alien_type(*ty)?.alignment() as i64)),
        ("REF", [pointer, ty, bytes]) => {
            let pointer = ForeignPointer::from_lisp(*pointer)?.offset(offset(*bytes)?)?;
            let ty = alien_type(*ty)?;
            // SAFETY: checked owned memory; the explicit borrowed-pointer API
            // contract requires foreign memory validity and synchronization.
            let bits = unsafe { pointer.read_scalar(&ty)? };
            unmarshal_from_c(bits, &ty)
        }
        ("SET", [pointer, ty, bytes, value]) => {
            let pointer = ForeignPointer::from_lisp(*pointer)?.offset(offset(*bytes)?)?;
            let ty = alien_type(*ty)?;
            let bits = if matches!(ty, AlienType::Pointer(_)) {
                ForeignPointer::from_lisp(*value)?.address() as u64
            } else {
                marshal_to_c(*value, &ty)?
            };
            // SAFETY: same explicit borrowed-pointer contract as REF.
            unsafe {
                pointer.write_scalar(&ty, bits)?;
            }
            Ok(*value)
        }
        _ => Err(TorclError::ProgramError(format!(
            "invalid foreign memory operation {operation:?}"
        ))),
    }
}
