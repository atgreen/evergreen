//! Real hardware faults must return through generated recovery code, then the
//! segment landing, without skipping any live Rust helper or its destructor.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::sync::atomic::{AtomicUsize, Ordering};
use egcl_rt::EgclVal;
use egcl_rt::native_transfer::{self, NativeExit, NativeSegment};
use egcl_rt::runtime::{
    check_sigsegv_null_guard, check_sigsegv_stack_guard, current_sigsegv_null_guard_recovery_ip,
    current_sigsegv_stack_guard_recovery_ip, set_sigsegv_recovery_ips,
};

static DROPS: AtomicUsize = AtomicUsize::new(0);

struct DropProbe;
impl Drop for DropProbe {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

// The caller owns these recovery targets, just as run_native owns the existing
// JIT epilogue targets. The segment adapter must not replace or lose that state.
struct RecoveryTargets(usize, usize);
impl RecoveryTargets {
    fn arm() -> Self {
        let previous = Self(
            current_sigsegv_null_guard_recovery_ip(),
            current_sigsegv_stack_guard_recovery_ip(),
        );
        let recovery = fault_recovery as *const () as usize;
        set_sigsegv_recovery_ips(recovery, recovery);
        previous
    }
}
impl Drop for RecoveryTargets {
    fn drop(&mut self) {
        set_sigsegv_recovery_ips(self.0, self.1);
    }
}

extern "C" fn recovered_fault(anchor: *mut NativeSegment) -> u64 {
    // Recovery is eligible only in generated code, never in this Rust helper.
    set_sigsegv_recovery_ips(0, 0);
    let _drop = DropProbe;
    if anchor.is_null() || native_transfer::current_segment() != anchor {
        return 0;
    }
    let kind = match (check_sigsegv_null_guard(), check_sigsegv_stack_guard()) {
        (true, false) => 1,
        (false, true) => 2,
        _ => 0,
    };
    EgclVal::from_fixnum(kind).0
}

#[unsafe(naked)]
unsafe extern "C" fn fault_entry() -> u64 {
    core::arch::naked_asm!(
        "endbr64",
        "push rdx",       // anchor, and alignment for the recovery helper's call
        "mov rax, [rdi]", // address supplied in the first slot
        "mov rax, [rax]", // real null-page or EgclStack guard fault
        "ud2",            // reading either address must fault
    )
}

#[unsafe(naked)]
unsafe extern "C" fn fault_recovery() -> ! {
    core::arch::naked_asm!(
        "endbr64",
        "mov rdi, [rsp]",
        "call {helper}", // helper and DropProbe return normally
        "pop rdi", // segment anchor
        "mov rsi, rax", "mov edx, 1",
        "jmp {leave}",
        helper = sym recovered_fault,
        leave = sym native_transfer::leave_native_segment,
    )
}

#[test]
fn actual_null_and_stack_guard_faults_leave_only_the_native_segment() {
    if !native_transfer::is_supported() {
        eprintln!("native segment fault probes unavailable on this hardened host");
        return;
    }
    egcl_rt::install_signal_handlers().unwrap();
    let stack = egcl_rt::current_stack();
    let watermark = (stack.sp(), stack.fp());
    let guard = stack.guard_base().unwrap() as u64;
    let previous = (
        current_sigsegv_null_guard_recovery_ip(),
        current_sigsegv_stack_guard_recovery_ip(),
    );
    assert!(!check_sigsegv_null_guard());
    assert!(!check_sigsegv_stack_guard());
    for (index, mut address) in [0, guard, guard, 0].into_iter().enumerate() {
        let targets = RecoveryTargets::arm();
        let outcome = unsafe {
            native_transfer::invoke_native_segment(fault_entry as *const u8, &mut address, stack)
        }
        .unwrap();
        assert_eq!(outcome.exit, NativeExit::Transfer);
        assert_eq!(
            outcome.value,
            EgclVal::from_fixnum(if address == 0 { 1 } else { 2 })
        );
        assert!(native_transfer::current_segment().is_null());
        assert_eq!((stack.sp(), stack.fp()), watermark);
        assert_eq!(DROPS.load(Ordering::SeqCst), index + 1);
        assert!(!check_sigsegv_null_guard());
        assert!(!check_sigsegv_stack_guard());
        drop(targets);
        assert_eq!(
            (
                current_sigsegv_null_guard_recovery_ip(),
                current_sigsegv_stack_guard_recovery_ip(),
            ),
            previous,
        );
    }
}
