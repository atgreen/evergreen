//! Lisp-facing foreign types and memory operations. Native byte access and
//! allocation ownership live in torcl-rt; public lambda lists live in boot.lisp.
use torcl_rt::ffi::{AlienType, marshal_to_c, memory::ForeignPointer, unmarshal_from_c};
use torcl_rt::object::{ObjectHeader, type_id};
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

fn vector_layout(vector: TorclVal, ty: &AlienType) -> Result<(usize, usize), TorclError> {
    let vector_kind = vector.is_heap_object()
        && unsafe {
            matches!(
                (*(vector.as_ptr() as *const ObjectHeader)).type_id(),
                type_id::SIMPLE_VECTOR | type_id::COMPLEX_ARRAY
            )
        };
    if !vector_kind
        || !matches!(
            ty,
            AlienType::Int { .. } | AlienType::Float | AlienType::Double
        )
    {
        return Err(TorclError::FfiError(
            "vector scope requires a numeric vector and scalar element type".into(),
        ));
    }
    let count = crate::sequences::array_total_size(vector)
        .ok_or_else(|| TorclError::FfiError("vector has no array storage".into()))?;
    let bytes = count
        .checked_mul(ty.size())
        .filter(|n| *n <= isize::MAX as usize)
        .ok_or_else(|| TorclError::FfiError("foreign vector size overflow".into()))?;
    Ok((count, bytes))
}

fn copy_vector(
    pointer: ForeignPointer,
    vector: TorclVal,
    ty: &AlienType,
    into_native: bool,
) -> Result<(), TorclError> {
    let (count, _) = vector_layout(vector, ty)?;
    torcl_rt::rooted!(vector = vector);
    for index in 0..count {
        let element = pointer.offset((index * ty.size()) as isize)?;
        if into_native {
            let value = crate::sequences::aref(*vector, index)?;
            let bits = marshal_to_c(value, ty)?;
            // SAFETY: owned bounds are checked; borrowed storage follows the
            // same explicit caller-owned validity contract as MEM-SET.
            unsafe {
                element.write_scalar(ty, bits)?;
            }
        } else {
            // Read native bits before allocating a boxed Lisp numeric value.
            let bits = unsafe { element.read_scalar(ty)? };
            torcl_rt::rooted!(value = unmarshal_from_c(bits, ty)?);
            crate::sequences::set_aref(*vector, index, *value)?;
        }
    }
    Ok(())
}

fn library_p(value: TorclVal) -> bool {
    value.is_heap_object()
        && unsafe {
            (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::FOREIGN_LIBRARY
        }
}

fn library_handle(value: TorclVal) -> Result<*mut (), TorclError> {
    if !library_p(value) {
        return Err(TorclError::TypeError {
            datum: value,
            expected: "TORCL-FFI:FOREIGN-LIBRARY".into(),
        });
    }
    // SAFETY: this leaf object's only word is the native registry token.
    Ok(unsafe { (value.as_ptr().add(8) as *const u64).read() } as usize as *mut ())
}

fn foreign_name(value: TorclVal) -> Result<String, TorclError> {
    let bytes =
        crate::sequences::string_content_bytes(value).ok_or_else(|| TorclError::TypeError {
            datum: value,
            expected: "STRING".into(),
        })?;
    String::from_utf8(bytes).map_err(|_| TorclError::FfiError("foreign name is not UTF-8".into()))
}

/// Explicit library lifetime, like CFFI: the caller must stop using retained
/// symbols before closing their provider. GC never implicitly unloads it.
pub fn library_call(args: &[TorclVal]) -> Result<TorclVal, TorclError> {
    let operation = args.first().copied().map(symbol_name).unwrap_or_default();
    match (
        operation.rsplit(':').next().unwrap_or_default(),
        &args[args.len().min(1)..],
    ) {
        ("P", [value]) => Ok(if library_p(*value) { T } else { NIL }),
        ("LOAD", [path]) => {
            let handle = torcl_rt::ffi::load_foreign_library(&foreign_name(*path)?)?;
            let Some(body) = torcl_rt::gc::alloc_typed(8, type_id::FOREIGN_LIBRARY) else {
                // SAFETY: this just-loaded reference has never escaped to C or Lisp.
                unsafe {
                    torcl_rt::ffi::close_foreign_library(handle)?;
                }
                return Err(TorclError::Oom);
            };
            // SAFETY: fresh leaf body with no intervening Lisp allocation.
            unsafe {
                (body as *mut u64).write(handle as usize as u64);
                Ok(TorclVal::from_heap_ptr(body.sub(8)))
            }
        }
        ("CLOSE", [library]) => {
            // SAFETY: the explicit close API requires callers to retire all
            // uses of symbols/callbacks from this provider before unloading.
            unsafe {
                torcl_rt::ffi::close_foreign_library(library_handle(*library)?)?;
            }
            Ok(NIL)
        }
        ("SYMBOL", [name, library]) => {
            let name = foreign_name(*name)?;
            // SAFETY: lookup returns a borrowed pointer. The same explicit
            // lifetime contract requires its provider to remain loaded in use.
            let pointer = unsafe {
                if library.is_nil() {
                    torcl_rt::ffi::foreign_symbol_global(&name)?
                } else {
                    torcl_rt::ffi::foreign_symbol(library_handle(*library)?, &name)?
                }
            };
            ForeignPointer::from_address(pointer as usize).into_lisp()
        }
        _ => Err(TorclError::ProgramError(format!(
            "invalid foreign library operation {operation:?}"
        ))),
    }
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
        ("VECTOR-SIZE", [vector, ty]) => {
            let (_, bytes) = vector_layout(*vector, &alien_type(*ty)?)?;
            unmarshal_from_c(
                bytes as u64,
                &AlienType::Int {
                    signed: false,
                    bits: usize::BITS as u8,
                },
            )
        }
        ("COPY-IN" | "COPY-OUT", [pointer, vector, ty]) => {
            copy_vector(
                ForeignPointer::from_lisp(*pointer)?,
                *vector,
                &alien_type(*ty)?,
                operation.rsplit(':').next() == Some("COPY-IN"),
            )?;
            Ok(NIL)
        }
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
