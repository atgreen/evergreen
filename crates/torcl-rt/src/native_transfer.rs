//! Machine boundary for native-only transfers. A segment may discard generated
//! frames only after runtime helpers have returned normally. Lisp cleanup, root
//! publication and TorclStack retirement are the caller's separate obligations.

use crate::execution_local::ExecutionLocal;
use crate::stack::{Frame, TorclStack};
#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
))]
use crate::value::NIL;
use crate::value::TorclVal;
use std::cell::Cell;

#[cfg(all(target_arch = "aarch64", unix))]
mod aarch64;
#[cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]
mod ppc64le;
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
    pub value: TorclVal,
    pub exit: NativeExit,
}

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
    previous: *mut NativeSegment,
    owner: SegmentOwner,
    carrier: crate::thread::NativeThreadId,
    stack_sp: *const u8,
    stack_fp: *const Frame,
    _pinned: std::marker::PhantomPinned,
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

/// Revalidate the platform hardening contract when a suspended fiber resumes
/// on a different carrier. The common case is a single comparison with no
/// syscall or platform query. A failed revalidation is deliberately reported
/// to the native poll caller so it can leave through the normal bytecode
/// fallback; generated code must never continue under an unknown contract.
pub fn revalidate_current_segment() -> bool {
    let current = crate::thread::current_thread_id();
    let segment = current_segment();
    if segment.is_null() {
        return true;
    }
    // SAFETY: ACTIVE contains a pinned segment owned by this execution and is
    // only read on that execution's carrier at a poll boundary.
    let segment = unsafe { &mut *segment };
    if segment.carrier == current {
        return true;
    }
    if !is_supported() {
        return false;
    }
    segment.carrier = current;
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentUnavailable;

/// Whether this execution can use the implemented machine transition. Unknown
/// hardening state is a refusal, not permission to reset a protected stack.
pub fn is_supported() -> bool {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        let mut features = 0u64;
        // Linux x86-64 arch_prctl(ARCH_SHSTK_STATUS, &features). Deliberately
        // accept only a successful query reporting no enabled shadow-stack bits.
        // Older kernels and seccomp-denied queries retain the legacy ABI.
        let result =
            unsafe { crate::syscall::syscall2(158, 0x5005, &mut features as *mut u64 as usize) };
        result == 0 && features == 0
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
    #[cfg(not(any(
        all(target_arch = "x86_64", any(target_os = "linux", windows)),
        all(
            target_arch = "powerpc64",
            target_endian = "little",
            target_os = "linux"
        ),
        all(target_arch = "aarch64", unix),
    )))]
    {
        false
    }
}

#[cfg(any(
    all(target_arch = "x86_64", any(target_os = "linux", windows)),
    all(
        target_arch = "powerpc64",
        target_endian = "little",
        target_os = "linux"
    ),
    all(target_arch = "aarch64", unix),
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
))]
pub unsafe fn invoke_native_segment(
    entry: *const u8,
    slots: *mut u64,
    stack: &TorclStack,
) -> Result<NativeOutcome, SegmentUnavailable> {
    if !is_supported() {
        return Err(SegmentUnavailable);
    }
    let owner = match crate::thread::current_fiber_id() {
        Some(id) => SegmentOwner::Fiber(id),
        None => SegmentOwner::Thread(crate::thread::current_thread_id()),
    };
    let carrier = crate::thread::current_thread_id();
    let previous = current_segment();
    let mut segment = std::pin::pin!(NativeSegment {
        saved_sp: 0,
        landing_pc: 0,
        previous,
        owner,
        carrier,
        stack_sp: stack.sp(),
        stack_fp: stack.fp(),
        _pinned: std::marker::PhantomPinned,
    });
    // SAFETY: this pinned stack local outlives the assembly entry and guard.
    let anchor = unsafe { segment.as_mut().get_unchecked_mut() as *mut NativeSegment };
    ACTIVE.with(|active| active.set(anchor));
    let _restore = ActiveSegment(previous);
    let mut outcome = NativeOutcome {
        value: NIL,
        exit: NativeExit::Returned,
    };
    unsafe { enter_platform(entry, slots, stack, anchor, &mut outcome) };
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
)))]
pub unsafe fn invoke_native_segment(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: &TorclStack,
) -> Result<NativeOutcome, SegmentUnavailable> {
    Err(SegmentUnavailable)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[unsafe(naked)]
unsafe extern "C" fn enter_sysv(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: *const TorclStack,
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
        "mov rsp, [rdi + {sp}]", "jmp qword ptr [rdi + {landing}]",
        sp = const std::mem::offset_of!(NativeSegment, saved_sp),
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
                previous: std::ptr::null_mut(),
                owner: SegmentOwner::Thread(crate::thread::NativeThreadId(0)),
                carrier: crate::thread::NativeThreadId(0),
                stack_sp: std::ptr::null(),
                stack_fp: std::ptr::null(),
                _pinned: std::marker::PhantomPinned,
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
            assert_eq!(outcome.value, TorclVal::from_fixnum(42));
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
            previous: std::ptr::null_mut(),
            owner: SegmentOwner::Thread(current),
            carrier: crate::thread::NativeThreadId(current.0.wrapping_add(1)),
            stack_sp: std::ptr::null(),
            stack_fp: std::ptr::null(),
            _pinned: std::marker::PhantomPinned,
        });
        let pointer = unsafe { segment.as_mut().get_unchecked_mut() as *mut NativeSegment };
        let previous = ACTIVE.with(|active| active.replace(pointer));
        assert!(revalidate_current_segment());
        assert_eq!(unsafe { (*pointer).carrier }, current);
        ACTIVE.with(|active| active.set(previous));
    }
}
