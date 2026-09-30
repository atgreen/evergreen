#![cfg(all(target_arch = "x86_64", windows))]

use egcl_rt::native_transfer::{self, NativeExit};

#[unsafe(naked)]
unsafe extern "C" fn entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64",
        "mov rax, rsp", "and eax, 15", "cmp eax, 8", "jne 3f",
        // The generated callee owns all four caller-provided home slots.
        "mov qword ptr [rsp + 8], 11", "mov qword ptr [rsp + 16], 12",
        "mov qword ptr [rsp + 24], 13", "mov qword ptr [rsp + 32], 14",
        "mov rax, [rcx]", // exit kind in slot 0
        "test eax, eax", "jz 2f",
        "mov rcx, r8", "mov r8, rax", "mov edx, 336", "jmp {leave}",
        "2:", "mov eax, 336", "ret",
        "3:", "ud2",
        leave = sym native_transfer::leave_native_segment,
    )
}

#[test]
fn unavailable_win64_segment_refuses_before_dereferencing_entry() {
    if !native_transfer::is_supported() {
        eprintln!("Win64 segment execution unavailable under this mitigation policy");
        assert!(
            unsafe {
                native_transfer::invoke_native_segment(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    egcl_rt::current_stack(),
                )
            }
            .is_err()
        );
    }
    // The execution gate below separately requires a supported host.
}

#[test]
#[ignore = "requires native Windows with verified mitigation state; run explicitly with --ignored"]
fn win64_segment_has_shadow_space_and_explicit_exit_kinds() {
    assert!(
        native_transfer::is_supported(),
        "mitigation state unavailable or incompatible"
    );
    for exit in [
        NativeExit::Returned,
        NativeExit::Transfer,
        NativeExit::Deopt,
    ] {
        let mut mode = exit as u64;
        let result = unsafe {
            native_transfer::invoke_native_segment(
                entry as *const u8,
                &mut mode,
                egcl_rt::current_stack(),
            )
        }
        .unwrap();
        assert_eq!(result.exit, exit);
        assert_eq!(result.value, egcl_rt::EgclVal::from_fixnum(42));
        assert!(native_transfer::current_segment().is_null());
    }
}
