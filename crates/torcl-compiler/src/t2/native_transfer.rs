//! SysV machine adapters for the native transfer ABI. These adapters are not
//! installed by the legacy T2 pipeline; call-site capture and landing metadata
//! must be supplied by the new ABI's installer.

use torcl_rt::asm::{Asm, Cc};
use torcl_rt::native_transfer::{NativeExit, NativeOutcome};

/// Version-two helper: a request pointer plus an explicit outcome destination.
/// The helper must initialize both outcome fields, return normally (including
/// on Lisp errors), and root request/payload references across allocation. It
/// must not jump across its Rust frame. The request layout belongs to the helper.
/// The request itself must occupy stable, nonmoving storage that remains alive
/// until either the normal return or the cold route finishes consuming it.
pub type NativeHelperV2 = unsafe extern "C" fn(*mut u8, *mut NativeOutcome);

/// Emit a callable adapter with SysV signature `(request) -> primary_value`.
/// Only the Rust-helper return is tested. A generated caller calls this adapter
/// normally and needs no successful-return transfer test of its own.
///
/// On Transfer or Deopt, tail-jump to `cold_entry` with `(request, value, exit)`
/// in RDI/RSI/RDX, after removing the adapter's temporary stack frame. The
/// caller's frame and return address remain live for capture. The cold entry
/// must be generated code/assembly using that convention and must not return to
/// the successful continuation. It must publish roots before allocating and
/// capture all outstanding cleanup/frame obligations before discarding frames.
/// No helper, allocation or safepoint occurs between reading the outcome and
/// entering that cold route. The outcome stack slot is not itself a GC root.
///
/// Before executing these bytes, the installer must retain both targets, check
/// platform transfer support, and supply the call-site maps and root protocol.
pub fn emit_helper_veneer(helper: NativeHelperV2, cold_entry: *const u8) -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<NativeOutcome>() == 16);
        assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
        assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
        assert!(NativeExit::Returned as u64 == 0);
    }
    let mut a = Asm::new();
    let exceptional = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // endbr64
    // 16 bytes for NativeOutcome and 8 for request; align RSP before Rust CALL.
    a.extend_from_slice(&[0x48, 0x83, 0xec, 24]); // sub rsp,24
    a.extend_from_slice(&[0x48, 0x89, 0x7c, 0x24, 16]); // mov [rsp+16],rdi
    a.extend_from_slice(&[0x48, 0x89, 0xe6]); // mov rsi,rsp (out)
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax,helper
    a.extend_from_slice(&(helper as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]); // call rax; Rust has returned before branching
    a.extend_from_slice(&[0x48, 0x8b, 0x04, 0x24]); // mov rax,[rsp] (value)
    a.extend_from_slice(&[0x48, 0x8b, 0x54, 0x24, 8]); // mov rdx,[rsp+8] (exit)
    a.extend_from_slice(&[0x48, 0x8b, 0x7c, 0x24, 16]); // mov rdi,[rsp+16]
    a.extend_from_slice(&[0x48, 0x83, 0xc4, 24]); // add rsp,24
    a.extend_from_slice(&[0x48, 0x85, 0xd2]); // test rdx,rdx (never test Lisp value)
    a.jcc(Cc::Ne, exceptional);
    a.extend_from_slice(&[0xc3]); // ret: successful primary in rax
    a.bind(exceptional);
    a.extend_from_slice(&[0x48, 0x89, 0xc6]); // mov rsi,rax
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax,cold_entry
    a.extend_from_slice(&(cold_entry as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xe0]); // jmp rax
    a.finish().expect("local helper veneer label")
}
