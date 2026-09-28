#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use torcl_compiler::t2::native_transfer::emit_helper_veneer;
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer::{self, NativeExit, NativeOutcome, NativeSegment};
use torcl_rt::stack::TorclStack;
use torcl_rt::value::{NIL, TorclVal};

#[repr(C)]
struct Request {
    anchor: *mut NativeSegment,
    drops: usize,
    continued: usize,
    value: TorclVal,
    exit: NativeExit,
}

struct Finished<'a>(&'a mut usize);
impl Drop for Finished<'_> {
    fn drop(&mut self) {
        *self.0 += 1;
    }
}

unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &mut *request.cast::<Request>() };
    let _finished = Finished(&mut request.drops);
    request.anchor = native_transfer::current_segment();
    unsafe {
        out.write(NativeOutcome {
            value: request.value,
            exit: request.exit,
        })
    };
}

// The fixture has no heap roots or Lisp cleanup to retire. A real cold route
// must capture those obligations before it can leave a segment.
#[unsafe(naked)]
unsafe extern "C" fn cold_route(_request: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!(
        "endbr64",
        "mov rdi, [rdi]",
        "jmp {leave}",
        leave = sym native_transfer::leave_native_segment,
    );
}

#[test]
fn successful_helper_returns_all_primary_values_after_rust_cleanup() {
    let veneer = JitBuffer::new(&emit_helper_veneer(helper, cold_route as *const u8)).unwrap();
    let call: unsafe extern "C" fn(*mut u8) -> TorclVal =
        unsafe { std::mem::transmute(veneer.as_ptr()) };
    for value in [TorclVal::from_fixnum(0), NIL, TorclVal::from_fixnum(42)] {
        let mut request = Request {
            anchor: std::ptr::null_mut(),
            drops: 0,
            continued: 0,
            value,
            exit: NativeExit::Returned,
        };
        assert_eq!(
            unsafe { call((&mut request as *mut Request).cast()) },
            value
        );
        assert_eq!(request.drops, 1);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn generated_caller_reaches_segment_landing_only_after_helper_returns() {
    assert!(
        native_transfer::is_supported(),
        "native segment execution gate unavailable"
    );
    let veneer = JitBuffer::new(&emit_helper_veneer(helper, cold_route as *const u8)).unwrap();
    // Entry saves the request, calls the veneer, then records normal completion.
    // There is deliberately no transfer test after the generated-to-generated call.
    let mut caller = vec![0xf3, 0x0f, 0x1e, 0xfa, 0x53, 0x48, 0x89, 0xfb, 0x48, 0xb8];
    caller.extend_from_slice(&(veneer.as_ptr() as u64).to_le_bytes());
    caller.extend_from_slice(&[0xff, 0xd0, 0x48, 0xff, 0x43, 16, 0x5b, 0xc3]);
    let caller = JitBuffer::new(&caller).unwrap();
    let stack = TorclStack::new(64 * 1024);
    for exit in [
        NativeExit::Returned,
        NativeExit::Transfer,
        NativeExit::Deopt,
    ] {
        let mut request = Request {
            anchor: std::ptr::null_mut(),
            drops: 0,
            continued: 0,
            value: TorclVal::from_fixnum(42),
            exit,
        };
        let outcome = unsafe {
            native_transfer::invoke_native_segment(
                caller.as_ptr(),
                (&mut request as *mut Request).cast(),
                &stack,
            )
        }
        .unwrap();
        assert_eq!(outcome.exit, exit);
        assert_eq!(outcome.value, request.value);
        assert_eq!(request.drops, 1, "Rust destructor cannot be skipped");
        assert_eq!(request.continued, usize::from(exit == NativeExit::Returned));
        assert!(native_transfer::current_segment().is_null());
    }
    eprintln!("executed generated helper success/transfer/deopt segment routes");
}
