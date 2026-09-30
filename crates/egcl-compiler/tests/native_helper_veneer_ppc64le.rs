#![cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]

use std::sync::atomic::{AtomicU64, Ordering};
use egcl_compiler::t2::native_transfer_ppc64le::emit_helper_veneer;
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
use egcl_rt::value::{EgclVal, NIL};

#[repr(C)]
struct Request {
    value: EgclVal,
    exit: NativeExit,
}

unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &*request.cast::<Request>() };
    unsafe {
        out.write(NativeOutcome {
            value: request.value,
            exit: request.exit,
        });
    }
}

static COLD_VALUE: AtomicU64 = AtomicU64::new(0);
static COLD_EXIT: AtomicU64 = AtomicU64::new(0);

extern "C" fn cold_route(_request: *mut u8, value: u64, exit: u64) -> u64 {
    COLD_VALUE.store(value, Ordering::SeqCst);
    COLD_EXIT.store(exit, Ordering::SeqCst);
    0xfeed_face
}

#[test]
fn elfv2_helper_veneer_returns_and_tails_to_transfer_route() {
    let code = emit_helper_veneer(helper, cold_route as *const u8);
    assert!(!code.is_empty(), "veneer emitter returned no code");
    let veneer = JitBuffer::new(&code).expect("executable ELFv2 veneer");
    let call: unsafe extern "C" fn(*mut u8) -> u64 =
        unsafe { std::mem::transmute(veneer.as_ptr()) };

    let mut normal = Request {
        value: NIL,
        exit: NativeExit::Returned,
    };
    assert_eq!(unsafe { call((&mut normal as *mut Request).cast()) }, NIL.0);

    let mut transfer = Request {
        value: EgclVal::from_fixnum(41),
        exit: NativeExit::Transfer,
    };
    assert_eq!(
        unsafe { call((&mut transfer as *mut Request).cast()) },
        0xfeed_face
    );
    assert_eq!(COLD_VALUE.load(Ordering::SeqCst), transfer.value.0);
    assert_eq!(
        COLD_EXIT.load(Ordering::SeqCst),
        NativeExit::Transfer as u64
    );
}
