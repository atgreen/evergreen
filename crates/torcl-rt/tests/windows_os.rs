#![cfg(windows)]
use torcl_rt::syscall as os;

#[cfg(target_arch = "x86_64")]
#[test]
fn windows_jit_memory_is_executable_readonly_and_released() {
    use torcl_rt::jit::JitBuffer;
    use windows_sys::Win32::System::Memory::*;

    assert!(JitBuffer::new(&[]).is_none());
    // A Win64 leaf: mov eax, 42; ret. No stack or nonvolatile state changes.
    let code = JitBuffer::new(&[0xb8, 42, 0, 0, 0, 0xc3]).expect("executable memory");
    let address = code.as_ptr();
    unsafe {
        let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        assert_ne!(
            VirtualQuery(address.cast(), &mut info, size_of_val(&info)),
            0
        );
        assert_eq!(info.Protect, PAGE_EXECUTE_READ);
        let function: extern "C" fn() -> u32 = std::mem::transmute(address);
        assert_eq!(function(), 42);
        drop(code);
        assert_ne!(
            VirtualQuery(address.cast(), &mut info, size_of_val(&info)),
            0
        );
        assert_eq!(info.State, MEM_FREE);
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn windows_jit_frame_unwinds_at_each_instruction_boundary() {
    use torcl_rt::jit::JitBuffer;
    use windows_sys::Win32::System::Diagnostics::Debug::*;

    let bytes = [
        0xf3, 0x0f, 0x1e, 0xfa, // endbr64
        0x55, // push rbp
        0x48, 0x83, 0xec, 32, // sub rsp,32 (shadow space)
        0x48, 0x89, 0xe5, // mov rbp,rsp
        0xb8, 42, 0, 0, 0, // mov eax,42
        0x48, 0x83, 0xc4, 32,   // add rsp,32
        0x5d, // pop rbp
        0xc3, // ret
    ];
    // Version 1; 12-byte prologue; three unwind codes; RBP frame at RSP.
    // Reverse order: establish RBP, allocate 32 bytes, push RBP; then padding.
    let unwind = [1, 12, 3, 5, 12, 3, 9, 0x32, 5, 0x50, 0, 0];
    let code = unsafe { JitBuffer::new_with_windows_unwind(&bytes, &unwind) }.unwrap();
    let address = code.as_ptr() as u64;
    unsafe {
        let mut base = 0;
        let entry = RtlLookupFunctionEntry(address + 12, &mut base, std::ptr::null_mut());
        assert!(
            !entry.is_null(),
            "generated frames need a registered unwind table"
        );
        assert_eq!(base, address);
        assert_eq!((*entry).BeginAddress, 0);
        assert_eq!((*entry).EndAddress, bytes.len() as u32);
        let function: extern "C" fn() -> u32 = std::mem::transmute(code.as_ptr());
        assert_eq!(function(), 42);

        // Independently model the machine state before/after each prologue
        // and epilogue instruction. The OS must restore both RBP and RSP.
        let stack = [0u64, 0, 0, 0, 0x1234_5678, 0x9876_5432, 0];
        let bottom = stack.as_ptr() as u64;
        for (offset, rsp_slot, frame_established) in [
            (0, 5, false),
            (4, 5, false),
            (5, 4, false),
            (9, 0, false),
            (12, 0, true),
            (17, 0, true),
            (21, 4, true),
            (22, 5, false),
        ] {
            let mut context: CONTEXT = std::mem::zeroed();
            context.Rip = address + offset;
            context.Rsp = bottom + rsp_slot * 8;
            context.Rbp = if frame_established { bottom } else { stack[4] };
            let mut handler_data = std::ptr::null_mut();
            let mut establisher = 0;
            RtlVirtualUnwind(
                0,
                base,
                context.Rip,
                entry,
                &mut context,
                &mut handler_data,
                &mut establisher,
                std::ptr::null_mut(),
            );
            assert_eq!(context.Rip, stack[5], "RIP at offset {offset}");
            assert_eq!(context.Rbp, stack[4], "RBP at offset {offset}");
            assert_eq!(context.Rsp, bottom + 48, "RSP at offset {offset}");
        }
        drop(code);
        assert!(RtlLookupFunctionEntry(address + 12, &mut base, std::ptr::null_mut()).is_null());
    }
}

#[test]
fn windows_memory_and_thread_services() {
    let page = os::page_size();
    assert!(page.is_power_of_two());
    unsafe {
        let p = os::mmap(
            std::ptr::null_mut(),
            page,
            os::PROT_READ | os::PROT_WRITE,
            os::MAP_PRIVATE | os::MAP_ANONYMOUS,
            -1,
            0,
        )
        .unwrap();
        p.write(42);
        os::mprotect(p, page, os::PROT_READ).unwrap();
        assert_eq!(p.read(), 42);
        os::munmap(p, page).unwrap();
        assert!(os::mprotect(std::ptr::null_mut(), page, os::PROT_READ).is_err());
    }
    assert_eq!(os::getpid() as u32, std::process::id());
    assert!(os::gettid() > 0);
    assert!(os::thread_cpu_time_ns().is_ok());
    assert_eq!(torcl_rt::current_platform_tag(), (1 << 32) | 4);
}

#[test]
fn windows_dll_lifetime() {
    use torcl_rt::ffi::*;
    let library = load_foreign_library("kernel32.dll").unwrap();
    unsafe {
        assert!(
            !foreign_symbol(library, "GetCurrentProcessId")
                .unwrap()
                .is_null()
        );
        assert!(foreign_symbol(library, "torcl_nonexistent_export").is_err());
        close_foreign_library(library).unwrap();
        assert!(foreign_symbol(library, "GetCurrentProcessId").is_err());
        assert!(close_foreign_library(library).is_err());
    }
}

#[test]
fn windows_stack_budget_fits_current_thread() {
    let (mut low, mut high) = (0, 0);
    unsafe {
        windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high);
    }
    assert!(torcl_rt::eval_stack_budget() < high - low);
}
