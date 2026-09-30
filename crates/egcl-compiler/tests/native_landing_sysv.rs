#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::mem::offset_of;
use egcl_compiler::t2::native_transfer::{
    SysvNativeLanding, SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
    emit_native_landing_stub,
};
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{self, NativeExit, NativeOutcome};
use egcl_rt::{Collector, HeapCollector, EgclStack, EgclVal};

#[repr(C)]
struct Request {
    landing: SysvNativeLanding,
    helper_drops: usize,
    prepare_drops: usize,
    landings: usize,
    registers: [u64; 6],
    spill: u64,
    primary: u64,
    observed_sp: usize,
    return_pc: usize,
    call_adjust: usize,
    shadow: *mut EgclVal,
    original: u64,
}

struct Finished<'a>(&'a mut usize);
impl Drop for Finished<'_> {
    fn drop(&mut self) {
        *self.0 += 1;
    }
}

unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &mut *request.cast::<Request>() };
    let _finished = Finished(&mut request.helper_drops);
    HeapCollector::new().minor_gc().unwrap();
    unsafe {
        out.write(NativeOutcome {
            value: request.shadow.read(),
            exit: NativeExit::Transfer,
        });
    }
}

unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let capture = unsafe { &mut *capture };
    let request = unsafe { &mut *capture.request.cast::<Request>() };
    assert_eq!(
        request.helper_drops, 1,
        "helper Rust frame returned normally"
    );
    let _finished = Finished(&mut request.prepare_drops);
    assert_eq!(capture.return_pc as usize, request.return_pc);
    assert_eq!(
        capture.preserved,
        [request.original, 102, 103, 104, 105, 106]
    );
    egcl_rt::rooted!(payload = capture.value);
    HeapCollector::new().minor_gc().unwrap();
    let relocated = unsafe { request.shadow.read().to_raw() };
    assert_ne!(
        relocated, request.original,
        "the native register really became stale"
    );
    capture.value = *payload;
    request.landing.stack_pointer =
        unsafe { capture.caller_sp.add(request.call_adjust / 8).cast_mut() };
    assert_eq!(unsafe { request.landing.stack_pointer.read() }, 555);
    unsafe { request.landing.stack_pointer.write(relocated) };
    capture.preserved[0] = relocated;
    for value in capture.preserved.iter_mut().skip(1) {
        *value += 200;
    }
    // The fixture's first field is the stable landing packet. Production
    // dispatch must select this only after checking its frame/target maps.
    capture.request = std::ptr::from_mut(&mut request.landing).cast();
}

unsafe extern "C" fn observe(request: *mut Request) -> EgclVal {
    let request = unsafe { &mut *request };
    assert_eq!((request.helper_drops, request.prepare_drops), (1, 1));
    let relocated = unsafe { request.shadow.read() };
    assert_eq!(
        request.registers,
        [relocated.to_raw(), 302, 303, 304, 305, 306]
    );
    assert_eq!(request.spill, relocated.to_raw());
    assert_eq!(request.primary, relocated.to_raw());
    assert_eq!(EgclVal(request.registers[0]).as_double_float(), 42.5);
    assert_eq!(EgclVal(request.spill).as_double_float(), 42.5);
    assert_eq!(request.observed_sp, request.landing.stack_pointer as usize);
    assert!(!native_transfer::current_segment().is_null());
    request.landings += 1;
    EgclVal::from_fixnum(123)
}

fn immediate(code: &mut Vec<u8>, register: u8, value: u64) {
    code.extend_from_slice(&[0x48 | (register >> 3), 0xb8 | (register & 7)]);
    code.extend_from_slice(&value.to_le_bytes());
}

fn store_to_request(code: &mut Vec<u8>, register: u8, offset: usize) {
    code.extend_from_slice(&[
        0x48 | ((register >> 3) << 2),
        0x89,
        0x87 | ((register & 7) << 3),
    ]);
    code.extend_from_slice(&(offset as i32).to_le_bytes());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn cold_preparation_lands_in_the_live_native_frame_after_rust_returns() {
    assert!(native_transfer::is_supported());
    let landing = JitBuffer::new(&emit_native_landing_stub()).unwrap();
    let capture = JitBuffer::new(&emit_capture_stub(prepare, landing.as_ptr())).unwrap();
    let veneer = JitBuffer::new(&emit_helper_veneer(helper, capture.as_ptr())).unwrap();
    for adjustment in [0u8, 16, 32, 64] {
        let body = egcl_rt::alloc_typed(8, egcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
        unsafe {
            body.cast::<f64>().write(42.5);
        }
        egcl_rt::rooted!(payload = unsafe { EgclVal::from_heap_ptr(body.sub(8)) });
        let original = payload.to_raw();
        let mut code = vec![0xf3, 0x0f, 0x1e, 0xfa];
        // Keep the owning Rust/segment nonvolatiles on this still-live frame.
        code.extend_from_slice(&[0x53, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]);
        code.extend_from_slice(&[0x48, 0x83, 0xec, 24]);
        immediate(&mut code, 0, 555);
        code.extend_from_slice(&[0x48, 0x89, 0x04, 0x24]);
        for (register, value) in [3, 5, 12, 13, 14, 15]
            .into_iter()
            .zip([original, 102, 103, 104, 105, 106])
        {
            immediate(&mut code, register, value);
        }
        code.extend_from_slice(&[0x48, 0x83, 0xec, adjustment]);
        immediate(&mut code, 0, veneer.as_ptr() as u64);
        code.extend_from_slice(&[0xff, 0xd0]);
        let return_offset = code.len();
        code.extend_from_slice(&[0x0f, 0x0b]); // never take the normal return
        let landing_offset = code.len();
        code.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]);
        store_to_request(&mut code, 0, offset_of!(Request, primary));
        for (index, register) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
            store_to_request(
                &mut code,
                register,
                offset_of!(Request, registers) + index * 8,
            );
        }
        code.extend_from_slice(&[0x48, 0x8b, 0x04, 0x24]);
        store_to_request(&mut code, 0, offset_of!(Request, spill));
        code.extend_from_slice(&[0x48, 0x89, 0xe0]);
        store_to_request(&mut code, 0, offset_of!(Request, observed_sp));
        code.extend_from_slice(&[0x0f, 0x28, 0x04, 0x24]); // MOVAPS proves aligned body SP
        immediate(&mut code, 0, observe as *const () as u64);
        code.extend_from_slice(&[0xff, 0xd0]);
        code.extend_from_slice(&[
            0x48, 0x83, 0xc4, 24, 0x41, 0x5f, 0x41, 0x5e, 0x41, 0x5d, 0x41, 0x5c, 0x5d, 0x5b, 0xc3,
        ]);
        let code = JitBuffer::new(&code).unwrap();
        let mut request = Request {
            landing: SysvNativeLanding {
                stack_pointer: std::ptr::null_mut(),
                entry: unsafe { code.as_ptr().add(landing_offset) },
            },
            helper_drops: 0,
            prepare_drops: 0,
            landings: 0,
            registers: [0; 6],
            spill: 0,
            primary: 0,
            observed_sp: 0,
            return_pc: code.as_ptr() as usize + return_offset,
            call_adjust: usize::from(adjustment),
            shadow: std::ptr::from_mut(&mut *payload),
            original,
        };
        let stack = EgclStack::new(64 * 1024);
        let watermarks = (stack.sp(), stack.fp());
        let outcome = unsafe {
            native_transfer::invoke_native_segment(
                code.as_ptr(),
                std::ptr::from_mut(&mut request).cast(),
                &stack,
            )
        }
        .unwrap();
        assert_eq!(outcome.exit, NativeExit::Returned);
        assert_eq!(outcome.value, EgclVal::from_fixnum(123));
        assert_eq!(request.landings, 1);
        assert_eq!((stack.sp(), stack.fp()), watermarks);
        assert!(native_transfer::current_segment().is_null());
    }
}
