//! Per R8.05, a foreign pointer has a distinct, precisely traced Lisp wrapper.
use torcl_rt::ffi::memory::ForeignPointer;

#[test]
fn boxed_pointer_survives_moving_gc_and_serializes_without_native_addresses() {
    let body = torcl_rt::gc::alloc_typed(8, torcl_rt::object::type_id::FOREIGN_LIBRARY).unwrap();
    let library = unsafe {
        (body as *mut u64).write(0xfedcba9876543210);
        torcl_rt::TorclVal::from_heap_ptr(body.sub(8))
    };
    torcl_rt::rooted!(library = library);
    let body = torcl_rt::gc::alloc_typed(8, torcl_rt::object::type_id::FOREIGN_CALLBACK).unwrap();
    let callback = unsafe {
        (body as *mut u64).write(0x123456789abcdef0);
        torcl_rt::TorclVal::from_heap_ptr(body.sub(8))
    };
    torcl_rt::rooted!(callback = callback);
    let pointer = ForeignPointer::allocate(8).unwrap();
    torcl_rt::rooted!(value = pointer.into_lisp().unwrap());
    for _ in 0..20 {
        let _ = torcl_rt::gc::alloc_double_float(1.0);
    }
    torcl_rt::gc::full_gc().unwrap();
    assert_eq!(
        unsafe { (library.as_ptr().add(8) as *const u64).read() },
        0xfedcba9876543210
    );
    assert_eq!(ForeignPointer::from_lisp(*value).unwrap(), pointer);
    assert_eq!(
        unsafe { (callback.as_ptr().add(8) as *const u64).read() },
        0x123456789abcdef0
    );
    assert!(!ForeignPointer::is_pointer(torcl_rt::value::NIL));
    for address in [0, 1, usize::MAX] {
        let boxed = torcl_rt::ffi::unmarshal_from_c(
            address as u64,
            &torcl_rt::ffi::AlienType::Pointer(Box::new(torcl_rt::ffi::AlienType::Void)),
        )
        .unwrap();
        assert_eq!(ForeignPointer::from_lisp(boxed).unwrap().address(), address);
    }
    assert!(ForeignPointer::from_lisp(torcl_rt::TorclVal::from_fixnum(3)).is_err());
    let image = torcl_rt::gc::serialize_heap_objects();
    let mut records = image.as_slice();
    let mut found = false;
    let mut library_found = false;
    let mut callback_found = false;
    while !records.is_empty() {
        let kind = records[8];
        let size = u32::from_le_bytes(records[9..13].try_into().unwrap()) as usize;
        let body = &records[13..13 + size];
        if kind == torcl_rt::object::type_id::FOREIGN_POINTER {
            assert!(body.iter().all(|byte| *byte == 0));
            found = true;
        }
        if kind == torcl_rt::object::type_id::FOREIGN_LIBRARY {
            assert!(body.iter().all(|byte| *byte == 0));
            library_found = true;
        }
        if kind == torcl_rt::object::type_id::FOREIGN_CALLBACK {
            assert!(body.iter().all(|byte| *byte == 0));
            callback_found = true;
        }
        records = &records[13 + size..];
    }
    assert!(
        found,
        "foreign pointer must be preserved as a null handle in images"
    );
    assert!(
        library_found,
        "foreign library must restore with an invalid token"
    );
    assert!(
        callback_found,
        "foreign callback must restore with an invalid token"
    );
    pointer.free().unwrap();
    assert!(
        torcl_rt::ffi::marshal_to_c(
            *value,
            &torcl_rt::ffi::AlienType::Pointer(Box::new(torcl_rt::ffi::AlienType::Void))
        )
        .is_err(),
        "a freed owned pointer must not reach C through marshalling"
    );
}
