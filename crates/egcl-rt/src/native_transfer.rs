// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Machine boundary for native-only transfers. A segment may discard generated
//! frames only after runtime helpers have returned normally. Lisp cleanup, root
//! publication and EgclStack retirement are the caller's separate obligations.

use crate::execution_local::ExecutionLocal;
use crate::stack::{EgclStack, Frame};
use crate::value::EgclVal;
#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
    all(target_arch = "s390x", unix),
    all(target_arch = "riscv64", unix),
))]
use crate::value::NIL;
use std::cell::Cell;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod ordinary_x64;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub use ordinary_x64::{invoke_ordinary_native_segment, OrdinaryNativeOutcome};

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(crate) mod migration_tests;

#[cfg(all(
    any(test, feature = "native-transfer-test-hooks"),
    target_arch = "x86_64",
    target_os = "linux"
))]
pub mod test_hooks;

#[cfg(all(target_arch = "aarch64", unix))]
mod aarch64;
#[cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]
mod ppc64le;
#[cfg(all(target_arch = "riscv64", unix))]
mod riscv64;
#[cfg(all(target_arch = "s390x", unix))]
mod s390x;
#[cfg(all(target_arch = "x86_64", any(windows, all(test, target_os = "linux"))))]
mod win64;
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use self::enter_sysv as enter_platform;
#[cfg(all(target_arch = "aarch64", unix))]
use aarch64::enter as enter_platform;
#[cfg(all(target_arch = "aarch64", unix))]
pub use aarch64::leave_native_segment;
#[cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]
use ppc64le::enter as enter_platform;
#[cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]
pub use ppc64le::leave_native_segment;
#[cfg(all(target_arch = "riscv64", unix))]
use riscv64::enter as enter_platform;
#[cfg(all(target_arch = "riscv64", unix))]
pub use riscv64::leave_native_segment;
#[cfg(all(target_arch = "s390x", unix))]
use s390x::enter as enter_platform;
#[cfg(all(target_arch = "s390x", unix))]
pub use s390x::leave_native_segment;
#[cfg(all(target_arch = "x86_64", windows))]
use win64::enter as enter_platform;
#[cfg(all(target_arch = "x86_64", windows))]
pub use win64::leave_native_segment;

#[repr(u64)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeExit {
    Returned = 0,
    Transfer = 1,
    Deopt = 2,
}

/// Explicit out parameter at the Rust/assembly boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NativeOutcome {
    pub value: EgclVal,
    pub exit: NativeExit,
}

/// Borrowed successful-return tuple at a nonallocating C/native boundary.
///
/// On x86-64 Linux, native Lisp returns use CF=0 for exactly one RAX value,
/// or CF=1 with count in RDX and this payload pointer in RCX. The payload
/// includes the primary at index zero. Counts zero and one have a null payload.
/// The environment owns and precisely traces the tuple; this descriptor is
/// not a root and must not outlive a safepoint, yield, or tuple mutation.
/// Exceptional transfers continue to use NativeOutcome, independently.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NativeReturnValues {
    pub count: usize,
    pub payload: *const EgclVal,
}

impl NativeReturnValues {
    pub const SINGLE: Self = Self { count: 1, payload: std::ptr::null() };
}

/// Reserved operation in the T2 multiple-value helper: write a
/// NativeReturnValues descriptor to the destination without allocating.
pub const SNAPSHOT_NATIVE_RETURN_VALUES: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentOwner {
    Thread(crate::thread::NativeThreadId),
    Fiber(crate::thread::FiberId),
}

/// An active anchor has a stable address on its owning execution's host stack.
/// Machine fields are exposed for emitter offsets, not for Rust-side mutation.
/// The watermarks describe entry state; they do not authorize bulk restoration
/// before required cleanups or root retirement have completed.
#[repr(C)]
pub struct NativeSegment {
    pub saved_sp: usize,
    pub landing_pc: usize,
    entry_return_pc: usize,
    previous: *mut NativeSegment,
    owner: SegmentOwner,
    carrier: crate::thread::NativeThreadId,
    stack_sp: *const u8,
    stack_fp: *const Frame,
    _pinned: std::marker::PhantomPinned,
    /// Optional fixed recovery SP when outgoing arguments make saved_sp dynamic.
    /// saved_sp still identifies the precise caller boundary for native walks.
    /// Zero retains the original fixed-frame landing convention.
    landing_sp: usize,
}

impl NativeSegment {
    pub fn previous(&self) -> *mut Self {
        self.previous
    }
    pub fn owner(&self) -> SegmentOwner {
        self.owner
    }

    pub fn carrier(&self) -> crate::thread::NativeThreadId {
        self.carrier
    }
    pub fn stack_watermark(&self) -> (*const u8, *const Frame) {
        (self.stack_sp, self.stack_fp)
    }

    /// Return address the entry stub's CALL pushed for generated code. Physical
    /// walks end at this PC with `saved_sp`. Ordinary entry records this PC
    /// explicitly before capturing the MV tuple; legacy entry derives it from
    /// the two-byte instruction preceding its cold-transfer landing.
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    pub fn return_pc(&self) -> usize {
        if self.entry_return_pc != 0 {
            self.entry_return_pc
        } else {
            self.landing_pc - 2
        }
    }
}

// SAFETY: only the owning execution accesses its slot. An anchor stays pinned
// until its guard restores the enclosing pointer; the slot never owns an anchor.
static ACTIVE: ExecutionLocal<Cell<*mut NativeSegment>> =
    unsafe { ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };

/// Cold-path lookup. No successful generated-to-generated return uses this.
/// The returned pointer is valid only while that execution's segment is active;
/// it must not be dereferenced after return or from another execution.
pub fn current_segment() -> *mut NativeSegment {
    ACTIVE.with(Cell::get)
}

/// Revalidate all active anchors together. The scheduler calls this before
/// resuming a fiber, including one suspended inside an ordinary Rust helper.
/// Capability must not change while any segment is active or suspended.
pub fn revalidate_current_segment() -> bool {
    prepare_carrier_resume().is_ok()
}

/// An incompatible destination must not resume the fiber at all. Return its
/// last validated carrier so the scheduler can route the still-live stack back
/// without unwinding Rust frames or discarding precise roots.
pub(crate) fn prepare_carrier_resume() -> Result<(), crate::thread::NativeThreadId> {
    prepare_carrier_resume_with(|| {
        let supported = is_supported();
        #[cfg(all(
            any(test, feature = "native-transfer-test-hooks"),
            target_arch = "x86_64",
            target_os = "linux"
        ))]
        let supported = test_hooks::destination_supported(supported);
        supported
    })
}

fn prepare_carrier_resume_with(
    supported: impl FnOnce() -> bool,
) -> Result<(), crate::thread::NativeThreadId> {
    let current = crate::thread::current_thread_id();
    let segment = current_segment();
    if segment.is_null() {
        return Ok(());
    }
    let owner = crate::thread::current_fiber_id()
        .map(SegmentOwner::Fiber)
        .unwrap_or(SegmentOwner::Thread(current));
    // SAFETY: this execution owns the pinned chain. At mount the fiber is
    // exclusively claimed and cannot run until this validation finishes.
    let previous_carrier = unsafe { (*segment).carrier };
    let mut changed = false;
    let mut anchor = segment;
    while !anchor.is_null() {
        let saved = unsafe { &*anchor };
        assert_eq!(saved.owner, owner, "native segment belongs to another execution");
        changed |= saved.carrier != current;
        anchor = saved.previous;
    }
    if !changed {
        return Ok(());
    }
    if !supported() {
        return Err(previous_carrier);
    }
    // Commit only after the complete chain and destination are accepted.
    anchor = segment;
    while !anchor.is_null() {
        let saved = unsafe { &mut *anchor };
        saved.carrier = current;
        anchor = saved.previous;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentUnavailable;

/// Whether this execution can use the implemented machine transition. Unknown
/// hardening state is a refusal, not permission to reset a protected stack.
pub fn is_supported() -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        // Every interpreter-to-native call enters a segment, so this query ran
        // once per call; the syscall alone cost ~0.3 us, more than the plain
        // T0 call it was gating (GitHub #487). Shadow-stack state belongs to
        // the OS thread and is fixed before Lisp code runs on it (the loader
        // enables it at startup and new threads inherit it), so the answer is
        // cached per carrier thread. Fibers that migrate are revalidated on
        // their new carrier, which has its own cache.
        thread_local! {
            static SUPPORTED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
        }
        SUPPORTED.with(|cached| {
            cached.get().unwrap_or_else(|| {
                let supported = query_shadow_stack_disabled();
                cached.set(Some(supported));
                supported
            })
        })
    }
    #[cfg(all(target_arch = "x86_64", windows))]
    {
        win64::is_supported()
    }
    #[cfg(all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ))]
    {
        ppc64le::is_supported()
    }
    #[cfg(all(target_arch = "aarch64", unix))]
    {
        aarch64::is_supported()
    }
    #[cfg(all(target_arch = "s390x", unix))]
    {
        s390x::is_supported()
    }
    #[cfg(all(target_arch = "riscv64", unix))]
    {
        riscv64::is_supported()
    }
    #[cfg(not(any(
        all(target_arch = "x86_64", any(target_os = "linux", windows)),
        all(
            target_arch = "powerpc64",
            target_endian = "little",
            target_os = "linux"
        ),
        all(target_arch = "aarch64", unix),
        all(target_arch = "s390x", unix),
        all(target_arch = "riscv64", unix),
    )))]
    {
        false
    }
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
thread_local! {
    static SHADOW_STACK_QUERIES: Cell<usize> = const { Cell::new(0) };
}

/// Each carrier thread asks the kernel once; every later native entry on it
/// reuses the answer (GitHub #487: the query was a syscall per call).
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
#[test]
fn shadow_stack_status_is_queried_once_per_thread() {
    std::thread::spawn(|| {
        let first = is_supported();
        for _ in 0..1000 {
            assert_eq!(is_supported(), first);
        }
        assert_eq!(SHADOW_STACK_QUERIES.with(Cell::get), 1);
    })
    .join()
    .unwrap();
}

/// Uncached x86-64 Linux hardening query behind [`is_supported`].
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn query_shadow_stack_disabled() -> bool {
    #[cfg(test)]
    SHADOW_STACK_QUERIES.with(|count| count.set(count.get() + 1));
    let mut features = 0u64;
    // Linux x86-64 arch_prctl(ARCH_SHSTK_STATUS, &features). Deliberately
    // accept only a successful query reporting no enabled shadow-stack bits.
    // Older kernels and seccomp-denied queries retain the legacy ABI.
    let result =
        unsafe { crate::syscall::syscall2(158, 0x5005, &mut features as *mut u64 as usize) };
    // -EINVAL means the kernel was built without user shadow-stack support
    // (CONFIG_X86_USER_SHADOW_STACK), so no shadow stack can be enabled for
    // this process: the legacy ABI applies. Any other failure (e.g. a
    // seccomp -EPERM) leaves the hardening state unknown and stays a refusal.
    (result == 0 && features == 0) || result == -22
}

#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
    all(target_arch = "s390x", unix),
    all(target_arch = "riscv64", unix),
))]
struct ActiveSegment(*mut NativeSegment);
#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
    all(target_arch = "s390x", unix),
    all(target_arch = "riscv64", unix),
))]
impl Drop for ActiveSegment {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(self.0));
    }
}

/// Enter generated code with `(slots, stack, anchor)` in the host argument
/// registers. A normal entry returns its primary value; a selected transfer
/// jumps to `leave_native_segment` after helper frames have returned.
///
/// # Safety
/// `entry` must point to live generated/assembly code obeying that contract;
/// `slots` must cover every slot it accesses. All heap references must be rooted
/// before allocation/yield. No live Rust/foreign frame may be skipped. Before
/// leaving, generated frames' roots and Lisp cleanup must be handled by the
/// transfer preparation protocol. Hardening cannot be enabled within a segment.
/// The returned primary must be rooted before the caller next allocates.
#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
    all(target_arch = "s390x", unix),
    all(target_arch = "riscv64", unix),
))]
pub unsafe fn invoke_native_segment(
    entry: *const u8,
    slots: *mut u64,
    stack: &EgclStack,
) -> Result<NativeOutcome, SegmentUnavailable> {
    unsafe { with_native_segment(stack, |anchor, out| enter_platform(entry, slots, stack, anchor, out)) }
}

#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(target_arch = "powerpc64", target_endian = "little", target_os = "linux"),
    all(target_arch = "aarch64", unix),
    all(target_arch = "s390x", unix),
    all(target_arch = "riscv64", unix),
))]
unsafe fn with_native_segment(
    stack: &EgclStack,
    enter: impl FnOnce(*mut NativeSegment, *mut NativeOutcome),
) -> Result<NativeOutcome, SegmentUnavailable> {
    if !is_supported() {
        return Err(SegmentUnavailable);
    }
    let carrier = crate::thread::current_thread_id();
    let owner = match crate::thread::current_fiber_id() {
        Some(id) => SegmentOwner::Fiber(id),
        None => SegmentOwner::Thread(carrier),
    };
    let previous = current_segment();
    let mut segment = std::pin::pin!(NativeSegment {
        saved_sp: 0,
        landing_pc: 0,
        entry_return_pc: 0,
        previous,
        owner,
        carrier,
        stack_sp: stack.sp(),
        stack_fp: stack.fp(),
        _pinned: std::marker::PhantomPinned,
        landing_sp: 0,
    });
    // SAFETY: this pinned stack local outlives the assembly entry and guard.
    let anchor = unsafe { segment.as_mut().get_unchecked_mut() as *mut NativeSegment };
    ACTIVE.with(|active| active.set(anchor));
    let _restore = ActiveSegment(previous);
    let mut outcome = NativeOutcome {
        value: NIL,
        exit: NativeExit::Returned,
    };
    enter(anchor, &mut outcome);
    Ok(outcome)
}

/// Unsupported targets retain their existing native ABI.
///
/// # Safety
/// Same entry/slot contract as the supported implementation; no code is entered.
#[cfg(not(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
    all(target_arch = "s390x", unix),
    all(target_arch = "riscv64", unix),
)))]
pub unsafe fn invoke_native_segment(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: &EgclStack,
) -> Result<NativeOutcome, SegmentUnavailable> {
    Err(SegmentUnavailable)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(naked)]
unsafe extern "C" fn enter_sysv(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: *const EgclStack,
    _anchor: *mut NativeSegment,
    _out: *mut NativeOutcome,
) {
    core::arch::naked_asm!(
        ".cfi_startproc",
        "endbr64",
        "push rbp", ".cfi_def_cfa_offset 16", ".cfi_offset rbp, -16",
        "push rbx", ".cfi_def_cfa_offset 24", ".cfi_offset rbx, -24",
        "push r12", ".cfi_def_cfa_offset 32", ".cfi_offset r12, -32",
        "push r13", ".cfi_def_cfa_offset 40", ".cfi_offset r13, -40",
        "push r14", ".cfi_def_cfa_offset 48", ".cfi_offset r14, -48",
        "push r15", ".cfi_def_cfa_offset 56", ".cfi_offset r15, -56",
        // RSP is now 8 mod 16. Three words align it for the generated entry.
        "sub rsp, 24", ".cfi_def_cfa_offset 80",
        "mov [rsp], rcx", "mov [rsp + 8], r8",
        "stmxcsr [rsp + 16]", "fnstcw [rsp + 20]",
        "mov [rcx + {sp}], rsp",
        "lea rax, [rip + 2f]", "mov [rcx + {landing}], rax",
        "mov r11, rdi", "mov rdi, rsi", "mov rsi, rdx", "mov rdx, rcx",
        "call r11",
        "xor edx, edx", // NativeExit::Returned
        "2:", "endbr64",
        // Both paths arrive here with RAX=value, RDX=exit, RSP=anchor save area.
        "mov rcx, [rsp + 8]",
        "mov [rcx + {value}], rax", "mov [rcx + {exit}], rdx",
        "mov rcx, [rsp]",
        "mov qword ptr [rcx + {sp}], 0", "mov qword ptr [rcx + {landing}], 0",
        "ldmxcsr [rsp + 16]", "fldcw [rsp + 20]", "cld",
        "add rsp, 24", ".cfi_def_cfa_offset 56",
        "pop r15", ".cfi_def_cfa_offset 48", ".cfi_restore r15",
        "pop r14", ".cfi_def_cfa_offset 40", ".cfi_restore r14",
        "pop r13", ".cfi_def_cfa_offset 32", ".cfi_restore r13",
        "pop r12", ".cfi_def_cfa_offset 24", ".cfi_restore r12",
        "pop rbx", ".cfi_def_cfa_offset 16", ".cfi_restore rbx",
        "pop rbp", ".cfi_def_cfa_offset 8", ".cfi_restore rbp",
        "ret", ".cfi_endproc",
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
        landing = const std::mem::offset_of!(NativeSegment, landing_pc),
        value = const std::mem::offset_of!(NativeOutcome, value),
        exit = const std::mem::offset_of!(NativeOutcome, exit),
    )
}

/// Cold assembly exit, called only after transfer preparation and Rust helper
/// return. This function never unwinds a Rust frame.
///
/// # Safety
/// `anchor` must be this execution's active segment. Every frame between here
/// and its landing site must be generated code or assembly eligible for discard.
/// Roots and cleanup obligations of discarded frames must already be settled.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(naked)]
pub unsafe extern "C" fn leave_native_segment(
    _anchor: *mut NativeSegment,
    _value: u64,
    _exit: NativeExit,
) -> ! {
    core::arch::naked_asm!(
        "endbr64", "mov rax, rsi",
        "mov rsp, [rdi + {sp}]", "mov r10, [rdi + {landing_sp}]",
        "test r10, r10", "cmovnz rsp, r10", "jmp qword ptr [rdi + {landing}]",
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
        landing_sp = const std::mem::offset_of!(NativeSegment, landing_sp),
        landing = const std::mem::offset_of!(NativeSegment, landing_pc),
    )
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests {
    use super::*;

    #[unsafe(naked)]
    unsafe extern "C" fn clobber_entry() -> u64 {
        core::arch::naked_asm!(
            "endbr64", "mov rax, [rdi]",
            "mov rbp, 1", "mov rbx, 2", "mov r12, 3",
            "mov r13, 4", "mov r14, 5", "mov r15, 6",
            "sub rsp, 8", "mov dword ptr [rsp], 0x3f80", "ldmxcsr [rsp]",
            "mov word ptr [rsp], 0x077f", "fldcw [rsp]", "add rsp, 8",
            "test eax, eax", "jz 2f",
            "mov rdi, rdx", "mov edx, eax", "mov esi, 336", "jmp {leave}",
            "2:", "mov eax, 336", "ret",
            leave = sym leave_native_segment,
        )
    }

    #[unsafe(naked)]
    unsafe extern "C" fn register_probe(
        _anchor: *mut NativeSegment,
        _out: *mut NativeOutcome,
        _exit: u64,
    ) -> u64 {
        core::arch::naked_asm!(
            "endbr64",
            "push rbp", "push rbx", "push r12", "push r13", "push r14", "push r15",
            "sub rsp, 40", "mov [rsp], rdx",
            "stmxcsr [rsp + 8]", "fnstcw [rsp + 12]",
            "mov rcx, rdi", "mov r8, rsi", "lea rdi, [rip + {entry}]",
            "mov rsi, rsp", "xor edx, edx",
            "mov rbp, 101", "mov rbx, 102", "mov r12, 103",
            "mov r13, 104", "mov r14, 105", "mov r15, 106",
            "call {enter}",
            "mov eax, 1",
            "cmp rbp, 101", "jne 2f", "cmp rbx, 102", "jne 2f",
            "cmp r12, 103", "jne 2f", "cmp r13, 104", "jne 2f",
            "cmp r14, 105", "jne 2f", "cmp r15, 106", "jne 2f",
            "stmxcsr [rsp + 16]", "mov ecx, [rsp + 16]", "cmp ecx, [rsp + 8]", "jne 2f",
            "fnstcw [rsp + 16]", "mov cx, [rsp + 16]", "cmp cx, [rsp + 12]", "jne 2f",
            "xor eax, eax",
            "2:", "ldmxcsr [rsp + 8]", "fldcw [rsp + 12]",
            "add rsp, 40",
            "pop r15", "pop r14", "pop r13", "pop r12", "pop rbx", "pop rbp", "ret",
            entry = sym clobber_entry,
            enter = sym enter_sysv,
        )
    }

    #[test]
    fn sysv_registers_and_fp_control_survive_all_segment_exit_kinds() {
        if !is_supported() {
            eprintln!("native segment register probe unavailable on this hardened host");
            return;
        }
        for exit in [
            NativeExit::Returned,
            NativeExit::Transfer,
            NativeExit::Deopt,
        ] {
            let mut anchor = NativeSegment {
                saved_sp: 0,
                landing_pc: 0,
                entry_return_pc: 0,
                previous: std::ptr::null_mut(),
                owner: SegmentOwner::Thread(crate::thread::NativeThreadId(0)),
                carrier: crate::thread::NativeThreadId(0),
                stack_sp: std::ptr::null(),
                stack_fp: std::ptr::null(),
                _pinned: std::marker::PhantomPinned,
                landing_sp: 0,
            };
            let mut outcome = NativeOutcome {
                value: NIL,
                exit: NativeExit::Returned,
            };
            // No Rust frame exists between register_probe, generated entry and
            // landing. The anchor remains at this address until the probe returns.
            assert_eq!(
                unsafe { register_probe(&mut anchor, &mut outcome, exit as u64) },
                0
            );
            assert_eq!(outcome.exit, exit);
            assert_eq!(outcome.value, EgclVal::from_fixnum(42));
            assert_eq!((anchor.saved_sp, anchor.landing_pc), (0, 0));
        }
    }

    #[test]
    fn carrier_change_revalidates_before_resuming_a_segment() {
        if !is_supported() {
            eprintln!("native segment hardening probe unavailable on this host");
            return;
        }
        let current = crate::thread::current_thread_id();
        let mut segment = std::pin::pin!(NativeSegment {
            saved_sp: 0,
            landing_pc: 0,
            entry_return_pc: 0,
            previous: std::ptr::null_mut(),
            owner: SegmentOwner::Thread(current),
            carrier: crate::thread::NativeThreadId(current.0.wrapping_add(1)),
            stack_sp: std::ptr::null(),
            stack_fp: std::ptr::null(),
            _pinned: std::marker::PhantomPinned,
            landing_sp: 0,
        });
        let pointer = unsafe { segment.as_mut().get_unchecked_mut() as *mut NativeSegment };
        let previous = ACTIVE.with(|active| active.replace(pointer));
        assert!(revalidate_current_segment());
        assert_eq!(unsafe { (*pointer).carrier }, current);
        ACTIVE.with(|active| active.set(previous));
    }
}
