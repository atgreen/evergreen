//! Lisp-facing foreign types and memory operations. Native byte access and
//! allocation ownership live in egcl-rt; public lambda lists live in boot.lisp.
use egcl_rt::ffi::{AlienType, marshal_to_c, memory::ForeignPointer, unmarshal_from_c};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{NIL, T};
use egcl_rt::{EgclError, EgclVal};

fn symbol_name(value: EgclVal) -> String {
    value
        .symbol_index()
        .and_then(egcl_rt::symbols::symbol_name)
        .unwrap_or_default()
}

pub fn alien_type(keyword: EgclVal) -> Result<AlienType, EgclError> {
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
            return Err(EgclError::FfiError(format!(
                "unknown foreign type {name:?}"
            )));
        }
    })
}

/// Walk only proper, acyclic lists; this operation never allocates Lisp data.
fn foreign_list(mut value: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let mut values = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while !value.is_nil() {
        if !value.is_cons() || !seen.insert(value.to_raw()) {
            return Err(EgclError::FfiError(
                "foreign descriptor/buffer list must be proper and acyclic".into(),
            ));
        }
        let cell = unsafe { &*(value.as_ptr() as *const egcl_rt::object::ConsCell) };
        values.push(cell.car);
        value = cell.cdr;
    }
    Ok(values)
}

fn native_type(value: EgclVal, depth: usize) -> Result<AlienType, EgclError> {
    if depth > 64 {
        return Err(EgclError::FfiError(
            "foreign aggregate nesting exceeds 64 levels".into(),
        ));
    }
    if !value.is_cons() {
        return alien_type(value);
    }
    let parts = foreign_list(value)?;
    let name = symbol_name(parts[0]);
    let kind = name.rsplit(':').next().unwrap_or_default();
    if !matches!(kind, "STRUCT" | "PACKED-STRUCT" | "UNION") {
        return Err(EgclError::FfiError(format!(
            "unknown native foreign descriptor {name:?}"
        )));
    }
    let fields = parts[1..]
        .iter()
        .map(|v| native_type(*v, depth + 1))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if kind == "UNION" {
        AlienType::Union { variants: fields }
    } else {
        AlienType::Struct {
            fields,
            packed: kind == "PACKED-STRUCT",
        }
    })
}

fn native_layout(ty: &AlienType) -> Result<(usize, usize), EgclError> {
    #[cfg(all(target_arch = "x86_64", any(unix, windows)))]
    {
        egcl_rt::ffi::native_layout(ty)
    }
    #[cfg(not(all(target_arch = "x86_64", any(unix, windows))))]
    {
        if matches!(ty, AlienType::Struct { .. } | AlienType::Union { .. }) {
            return Err(EgclError::FfiError(
                "aggregate layouts are not implemented for this target ABI".into(),
            ));
        }
        Ok((ty.size(), ty.alignment()))
    }
}

/// All arguments are addresses of native C objects, not Lisp values to marshal.
/// The caller supplies matching signatures and valid borrowed storage. Tracked
/// storage is copied under short registry locks; no locks survive a C callback.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub fn buffered_call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    if !(5..=6).contains(&args.len()) {
        return Err(EgclError::ProgramError("FOREIGN-CALL-BUFFERED requires pointer, result type, argument types, argument buffers, result buffer and optional fixed count".into()));
    }
    let function = ForeignPointer::from_lisp(args[0])?.call_address()?;
    let result_type = native_type(args[1], 0)?;
    let types = foreign_list(args[2])?
        .into_iter()
        .map(|v| native_type(v, 0))
        .collect::<Result<Vec<_>, _>>()?;
    let buffers = foreign_list(args[3])?;
    let fixed_count = args.get(5).copied().map(unsigned).transpose()?;
    if function == 0 || types.len() != buffers.len() || fixed_count.is_some_and(|n| n > types.len())
    {
        return Err(EgclError::FfiError(
            "invalid buffered foreign call arguments".into(),
        ));
    }
    let (result_size, _) = native_layout(&result_type)?;
    let output = ForeignPointer::from_lisp(args[4])?;
    if result_size != 0 {
        output.check_range(result_size)?;
    }
    // Validate every descriptor and pointer before reading any native storage.
    let inputs = types
        .iter()
        .zip(buffers)
        .map(|(ty, value)| {
            let (size, _) = native_layout(ty)?;
            if size == 0 {
                return Err(EgclError::FfiError("void is not an argument type".into()));
            }
            let pointer = ForeignPointer::from_lisp(value)?;
            pointer.check_range(size)?;
            Ok((pointer, size))
        })
        .collect::<Result<Vec<_>, EgclError>>()?;
    let storage = inputs
        .into_iter()
        .map(|(pointer, size)| unsafe { pointer.read_buffer(size) })
        .collect::<Result<Vec<_>, _>>()?;
    let addresses: Vec<_> = storage
        .iter()
        .map(|buffer| buffer.as_ptr().cast())
        .collect();
    let mut result = vec![std::mem::MaybeUninit::<u8>::uninit(); result_size];
    egcl_rt::rooted!(returned_pointer = args[4]);
    // SAFETY: staging owns stable native buffers. Signature/callee agreement,
    // borrowed storage and embedded pointer lifetimes are the explicit FFI contract.
    unsafe {
        egcl_rt::ffi::ffi_call_buffered(
            function as *const (),
            &result_type,
            &types,
            &addresses,
            result.as_mut_ptr().cast(),
            fixed_count,
        )?;
        if result_size != 0 {
            output.write_buffer(&result)?;
        }
    }
    Ok(if result_size == 0 {
        NIL
    } else {
        *returned_pointer
    })
}

#[cfg(not(all(target_arch = "x86_64", any(unix, windows))))]
pub fn buffered_call(_args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    Err(EgclError::FfiError(
        "buffered calls are not implemented for this target ABI".into(),
    ))
}

fn unsigned(value: EgclVal) -> Result<usize, EgclError> {
    Ok(marshal_to_c(
        value,
        &AlienType::Int {
            signed: false,
            bits: usize::BITS as u8,
        },
    )? as usize)
}

fn offset(value: EgclVal) -> Result<isize, EgclError> {
    Ok(marshal_to_c(
        value,
        &AlienType::Int {
            signed: true,
            bits: isize::BITS as u8,
        },
    )? as isize)
}

fn vector_layout(vector: EgclVal, ty: &AlienType) -> Result<(usize, usize), EgclError> {
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
        return Err(EgclError::FfiError(
            "vector scope requires a numeric vector and scalar element type".into(),
        ));
    }
    let count = crate::sequences::array_total_size(vector)
        .ok_or_else(|| EgclError::FfiError("vector has no array storage".into()))?;
    let bytes = count
        .checked_mul(ty.size())
        .filter(|n| *n <= isize::MAX as usize)
        .ok_or_else(|| EgclError::FfiError("foreign vector size overflow".into()))?;
    Ok((count, bytes))
}

fn copy_vector(
    pointer: ForeignPointer,
    vector: EgclVal,
    ty: &AlienType,
    into_native: bool,
) -> Result<(), EgclError> {
    let (count, _) = vector_layout(vector, ty)?;
    egcl_rt::rooted!(vector = vector);
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
            egcl_rt::rooted!(value = unmarshal_from_c(bits, ty)?);
            crate::sequences::set_aref(*vector, index, *value)?;
        }
    }
    Ok(())
}

fn library_p(value: EgclVal) -> bool {
    value.is_heap_object()
        && unsafe {
            (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::FOREIGN_LIBRARY
        }
}

fn library_handle(value: EgclVal) -> Result<*mut (), EgclError> {
    if !library_p(value) {
        return Err(EgclError::TypeError {
            datum: value,
            expected: "EGCL-FFI:FOREIGN-LIBRARY".into(),
        });
    }
    // SAFETY: this leaf object's only word is the native registry token.
    Ok(unsafe { (value.as_ptr().add(8) as *const u64).read() } as usize as *mut ())
}

fn foreign_name(value: EgclVal) -> Result<String, EgclError> {
    let bytes =
        crate::sequences::string_content_bytes(value).ok_or_else(|| EgclError::TypeError {
            datum: value,
            expected: "STRING".into(),
        })?;
    String::from_utf8(bytes).map_err(|_| EgclError::FfiError("foreign name is not UTF-8".into()))
}

/// Explicit callback ownership, independent of the lifetime of its Lisp wrapper.
/// The caller must retire all foreign pointer uses before FREE; the active check
/// additionally rejects a callback trying to release its own executable entry.
#[cfg(all(target_arch = "x86_64", any(unix, windows)))]
pub fn callback_call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    use std::sync::{Mutex, OnceLock};
    use egcl_rt::ffi::managed_callback::LispCallback;
    static CALLBACKS: OnceLock<Mutex<Vec<Option<LispCallback>>>> = OnceLock::new();
    let registry = CALLBACKS.get_or_init(|| Mutex::new(Vec::new()));
    let is_callback = |value: EgclVal| {
        value.is_heap_object()
            && unsafe {
                (*(value.as_ptr() as *const ObjectHeader)).type_id() == type_id::FOREIGN_CALLBACK
            }
    };
    let operation = args.first().copied().map(symbol_name).unwrap_or_default();
    let rest = &args[args.len().min(1)..];
    match (operation.rsplit(':').next().unwrap_or_default(), rest) {
        ("P", [value]) => Ok(if is_callback(*value) { T } else { NIL }),
        ("MAKE", [function, result, arguments]) => {
            let result = alien_type(*result)?;
            let mut arguments = *arguments;
            let mut types = Vec::new();
            let mut seen = std::collections::HashSet::new();
            while !arguments.is_nil() {
                if !arguments.is_cons() || !seen.insert(arguments.to_raw()) {
                    return Err(EgclError::FfiError(
                        "callback argument types must be a proper list".into(),
                    ));
                }
                // No Lisp allocation while translating this list to native types.
                let cell = unsafe { &*(arguments.as_ptr() as *const egcl_rt::object::ConsCell) };
                types.push(alien_type(cell.car)?);
                arguments = cell.cdr;
            }
            let callback = LispCallback::new(*function, result, types)?;
            // Callback already roots the function; never allocate under the registry lock.
            let body =
                egcl_rt::gc::alloc_typed(8, type_id::FOREIGN_CALLBACK).ok_or(EgclError::Oom)?;
            let mut callbacks = registry.lock().unwrap_or_else(|error| error.into_inner());
            callbacks.push(Some(callback));
            let token = callbacks.len(); // one-based, never reused; zero is invalid after restore
            unsafe {
                (body as *mut usize).write(token);
                Ok(EgclVal::from_heap_ptr(body.sub(8)))
            }
        }
        (action @ ("POINTER" | "FREE" | "ERROR"), [value]) => {
            if !is_callback(*value) {
                return Err(EgclError::TypeError {
                    datum: *value,
                    expected: "EGCL-FFI:FOREIGN-CALLBACK".into(),
                });
            }
            let token = unsafe { (value.as_ptr().add(8) as *const usize).read() };
            let mut callbacks = registry.lock().unwrap_or_else(|error| error.into_inner());
            let slot = token
                .checked_sub(1)
                .and_then(|index| callbacks.get_mut(index))
                .filter(|slot| slot.is_some())
                .ok_or_else(|| {
                    EgclError::FfiError(
                        "callback is freed or unavailable after image restore".into(),
                    )
                })?;
            let callback = slot.as_ref().unwrap();
            match action {
                "POINTER" => {
                    let address = callback.as_fn_ptr() as usize;
                    drop(callbacks);
                    ForeignPointer::from_address(address).into_lisp()
                }
                "FREE" => {
                    if callback.is_active() {
                        return Err(EgclError::FfiError(
                            "cannot free an active callback".into(),
                        ));
                    }
                    let released = slot.take();
                    drop(callbacks);
                    drop(released);
                    Ok(NIL)
                }
                _ => {
                    let error = callback.take_error();
                    drop(callbacks);
                    Ok(error.map_or(NIL, |message| {
                        crate::streams::make_lisp_string_fresh(&message)
                    }))
                }
            }
        }
        _ => Err(EgclError::ProgramError(format!(
            "invalid foreign callback operation {operation:?}"
        ))),
    }
}

#[cfg(not(all(target_arch = "x86_64", any(unix, windows))))]
pub fn callback_call(_args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    Err(EgclError::FfiError(
        "callbacks are not implemented for this target ABI".into(),
    ))
}

/// Explicit library lifetime, like CFFI: the caller must stop using retained
/// symbols before closing their provider. GC never implicitly unloads it.
pub fn library_call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let operation = args.first().copied().map(symbol_name).unwrap_or_default();
    match (
        operation.rsplit(':').next().unwrap_or_default(),
        &args[args.len().min(1)..],
    ) {
        ("JVM-RUNTIME-VERSION", []) => Ok(EgclVal::from_fixnum(
            if cfg!(all(
                target_arch = "x86_64",
                target_os = "linux",
                target_env = "gnu"
            )) && egcl_rt::runtime::supports_jvm_coexistence()
            {
                1
            } else {
                0
            },
        )),
        // Companion to JVM-RUNTIME-VERSION: a 0 there has four possible causes and
        // Lisp can see none of them, so the reason is reported from Rust (bliss-hllzi).
        ("JVM-RUNTIME-DIAGNOSTIC", []) => Ok(egcl_rt::gc::alloc_character_string(
            &egcl_rt::runtime::jvm_coexistence_diagnostic(),
        )),
        ("INHIBIT-IMAGE", []) => {
            egcl_rt::image::inhibit_saving()?;
            Ok(NIL)
        }
        ("P", [value]) => Ok(if library_p(*value) { T } else { NIL }),
        ("LOAD", [path]) => {
            let handle = egcl_rt::ffi::load_foreign_library(&foreign_name(*path)?)?;
            let Some(body) = egcl_rt::gc::alloc_typed(8, type_id::FOREIGN_LIBRARY) else {
                // SAFETY: this just-loaded reference has never escaped to C or Lisp.
                unsafe {
                    egcl_rt::ffi::close_foreign_library(handle)?;
                }
                return Err(EgclError::Oom);
            };
            // SAFETY: fresh leaf body with no intervening Lisp allocation.
            unsafe {
                (body as *mut u64).write(handle as usize as u64);
                Ok(EgclVal::from_heap_ptr(body.sub(8)))
            }
        }
        ("CLOSE", [library]) => {
            // SAFETY: the explicit close API requires callers to retire all
            // uses of symbols/callbacks from this provider before unloading.
            unsafe {
                egcl_rt::ffi::close_foreign_library(library_handle(*library)?)?;
            }
            Ok(NIL)
        }
        ("SYMBOL", [name, library]) => {
            let name = foreign_name(*name)?;
            // SAFETY: lookup returns a borrowed pointer. The same explicit
            // lifetime contract requires its provider to remain loaded in use.
            let pointer = unsafe {
                if library.is_nil() {
                    egcl_rt::ffi::foreign_symbol_global(&name)?
                } else {
                    egcl_rt::ffi::foreign_symbol(library_handle(*library)?, &name)?
                }
            };
            ForeignPointer::from_address(pointer as usize).into_lisp()
        }
        _ => Err(EgclError::ProgramError(format!(
            "invalid foreign library operation {operation:?}"
        ))),
    }
}

/// Called only after the evaluator's FFI sandbox gate. Borrowed pointers expose
/// an intentionally unsafe Lisp API; their validity is the foreign caller's
/// responsibility, just as with CFFI on other implementations.
pub fn memory_call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
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
        ("TYPE-SIZE", [ty]) => Ok(EgclVal::from_fixnum(
            native_layout(&native_type(*ty, 0)?)?.0 as i64,
        )),
        ("TYPE-ALIGNMENT", [ty]) => Ok(EgclVal::from_fixnum(
            native_layout(&native_type(*ty, 0)?)?.1 as i64,
        )),
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
        _ => Err(EgclError::ProgramError(format!(
            "invalid foreign memory operation {operation:?}"
        ))),
    }
}
