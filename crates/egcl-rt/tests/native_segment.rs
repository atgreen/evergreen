//! Physical segment exits must abandon generated frames only, never Rust frames.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use egcl_rt::native_transfer::{self, NativeExit, NativeSegment};

#[unsafe(naked)]
unsafe extern "C" fn normal_entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64",
        "mov rax, rsp",
        "and eax, 15",
        "cmp eax, 8",
        "jne 2f",
        "mov rax, [rdi]",
        // A generated body may use all the nonvolatile registers. The adapter
        // must restore its Rust caller's values even on the exceptional path.
        "mov rbp, 11",
        "mov rbx, 12",
        "mov r12, 13",
        "mov r13, 14",
        "mov r14, 15",
        "mov r15, 16",
        "ret",
        "2:",
        "ud2",
    )
}

static DROPS: AtomicUsize = AtomicUsize::new(0);
struct DropProbe;
impl Drop for DropProbe {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

extern "C" fn rust_helper() -> u64 {
    let _drop = DropProbe;
    let pointer = native_transfer::current_segment();
    if pointer.is_null() {
        return 0;
    }
    // The helper is still inside this live segment and cannot collect here.
    let anchor = unsafe { &*pointer };
    let stack = egcl_rt::current_stack();
    u64::from(
        anchor.stack_watermark() == (stack.sp(), stack.fp())
            && anchor.owner()
                == native_transfer::SegmentOwner::Thread(egcl_rt::current_thread_id()),
    )
}

#[unsafe(naked)]
unsafe extern "C" fn transfer_entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64",
        "push rdx", // anchor; also aligns the stack before entering Rust
        "call {helper}", // this Rust frame returns and drops normally
        "pop rdi",
        "mov rsi, rax", "mov edx, 1",
        "mov rbp, 21", "mov rbx, 22", "mov r12, 23",
        "mov r13, 24", "mov r14, 25", "mov r15, 26",
        // A tail jump from assembly, never from a live Rust helper frame.
        "jmp {leave}",
        helper = sym rust_helper,
        leave = sym native_transfer::leave_native_segment,
    )
}

extern "C" fn nested_rust_helper() -> u64 {
    let _drop = DropProbe;
    let outer = native_transfer::current_segment();
    let result = unsafe {
        native_transfer::invoke_native_segment(
            transfer_entry as *const u8,
            std::ptr::null_mut(),
            egcl_rt::current_stack(),
        )
    };
    u64::from(
        result.is_ok_and(|out| out.exit == NativeExit::Transfer && out.value.0 == 1)
            && native_transfer::current_segment() == outer
            && !outer.is_null(),
    )
}

#[unsafe(naked)]
unsafe extern "C" fn nested_entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64", "sub rsp, 8", "call {helper}", "add rsp, 8", "ret",
        helper = sym nested_rust_helper,
    )
}

#[test]
fn segment_returns_and_transfers_restore_the_enclosing_execution() {
    let stack = egcl_rt::current_stack();
    if !native_transfer::is_supported() {
        // Unsupported configurations must refuse before dereferencing an entry.
        assert!(
            unsafe {
                native_transfer::invoke_native_segment(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    stack,
                )
            }
            .is_err()
        );
        eprintln!("native segment execution probe unavailable on this hardened host");
        return;
    }
    let before = (stack.sp(), stack.fp());
    let mut value = 0x1230;
    let ordinary = unsafe {
        native_transfer::invoke_native_segment(normal_entry as *const u8, &mut value, stack)
    }
    .unwrap();
    assert_eq!(ordinary.exit, NativeExit::Returned);
    assert_eq!(ordinary.value.0, value);
    assert!(native_transfer::current_segment().is_null());
    DROPS.store(0, Ordering::SeqCst);
    let exceptional = unsafe {
        native_transfer::invoke_native_segment(transfer_entry as *const u8, &mut value, stack)
    }
    .unwrap();
    assert_eq!(exceptional.exit, NativeExit::Transfer);
    assert_eq!(exceptional.value.0, 1);
    assert_eq!(DROPS.load(Ordering::SeqCst), 1);
    assert!(native_transfer::current_segment().is_null());
    let nested = unsafe {
        native_transfer::invoke_native_segment(nested_entry as *const u8, &mut value, stack)
    }
    .unwrap();
    assert_eq!(nested.exit, NativeExit::Returned);
    assert_eq!(nested.value.0, 1);
    assert_eq!(DROPS.load(Ordering::SeqCst), 3);
    assert!(native_transfer::current_segment().is_null());
    assert_eq!((stack.sp(), stack.fp()), before);
}

#[test]
fn native_segment_records_have_stable_machine_layout() {
    assert_eq!(std::mem::offset_of!(NativeSegment, saved_sp), 0);
    assert_eq!(std::mem::offset_of!(NativeSegment, landing_pc), 8);
    assert_eq!(
        std::mem::offset_of!(native_transfer::NativeOutcome, value),
        0
    );
    assert_eq!(
        std::mem::offset_of!(native_transfer::NativeOutcome, exit),
        8
    );
}

extern "C" fn capture_segment_backtrace() -> u64 {
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    let crossed_boundary = trace.contains("segment_backtrace_boundary");
    if !crossed_boundary {
        eprintln!("backtrace stopped before the Rust segment caller:\n{trace}");
    }
    u64::from(crossed_boundary)
}

#[unsafe(naked)]
unsafe extern "C" fn backtrace_entry() -> u64 {
    core::arch::naked_asm!(
        ".cfi_startproc",
        "endbr64",
        "sub rsp, 8",
        ".cfi_def_cfa_offset 16",
        // Frame walking must use the adapter's save area, not accidentally
        // succeed because generated code left the caller's registers intact.
        "mov rbp, 1", "mov rbx, 2", "mov r12, 3",
        "mov r13, 4", "mov r14, 5", "mov r15, 6",
        "call {helper}",
        "add rsp, 8",
        ".cfi_def_cfa_offset 8",
        "ret",
        ".cfi_endproc",
        helper = sym capture_segment_backtrace,
    )
}

#[inline(never)]
fn segment_backtrace_boundary() -> u64 {
    let result = unsafe {
        native_transfer::invoke_native_segment(
            backtrace_entry as *const u8,
            std::ptr::null_mut(),
            egcl_rt::current_stack(),
        )
    }
    .unwrap();
    std::hint::black_box(result.value.0)
}

#[test]
fn backtrace_crosses_the_segment_adapter() {
    if !native_transfer::is_supported() {
        eprintln!("native segment backtrace probe unavailable on this hardened host");
        return;
    }
    assert_eq!(segment_backtrace_boundary(), 1);
}
