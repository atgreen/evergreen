#![cfg(windows)]
use egcl_rt::syscall as os;

#[cfg(target_arch = "x86_64")]
#[test]
fn windows_jit_memory_is_executable_readonly_and_released() {
    use egcl_rt::jit::JitBuffer;
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
    use egcl_rt::jit::JitBuffer;
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

#[cfg(target_arch = "x86_64")]
#[test]
fn windows_jit_multiple_entries_have_independent_unwind_ranges() {
    use egcl_rt::jit::{JitBuffer, WindowsUnwindInfo};
    use windows_sys::Win32::System::Diagnostics::Debug::*;

    // Two independently callable entries, separated by three unregistered bytes.
    // The first saves RBP; the second has only a 40-byte stack allocation.
    let bytes = [
        0x55, 0x48, 0x83, 0xec, 32, 0xb8, 42, 0, 0, 0, 0x48, 0x83, 0xc4, 32, 0x5d, 0xc3, 0xcc,
        0xcc, 0xcc, 0x48, 0x83, 0xec, 40, 0xb8, 43, 0, 0, 0, 0x48, 0x83, 0xc4, 40, 0xc3,
    ];
    let entries = [
        WindowsUnwindInfo {
            begin: 0,
            end: 16,
            unwind_info: &[1, 5, 2, 0, 5, 0x32, 1, 0x50],
        },
        WindowsUnwindInfo {
            begin: 19,
            end: 33,
            unwind_info: &[1, 4, 1, 0, 4, 0x42, 0, 0],
        },
    ];
    let code = unsafe { JitBuffer::new_with_windows_unwind_ranges(&bytes, &entries) }.unwrap();
    let address = code.as_ptr() as u64;
    unsafe {
        let mut base = 0;
        let mut metadata = Vec::new();
        for (range, expected) in entries.iter().zip([42, 43]) {
            let function: extern "C" fn() -> u32 =
                std::mem::transmute(code.as_ptr().add(range.begin as usize));
            assert_eq!(function(), expected);
            for offset in range.begin..range.end {
                let entry = RtlLookupFunctionEntry(
                    address + u64::from(offset),
                    &mut base,
                    std::ptr::null_mut(),
                );
                assert!(!entry.is_null(), "missing range at {offset}");
                assert_eq!(base, address);
                assert_eq!((*entry).BeginAddress, range.begin);
                assert_eq!((*entry).EndAddress, range.end);
                assert_eq!((*entry).Anonymous.UnwindInfoAddress % 4, 0);
                assert_eq!(
                    std::slice::from_raw_parts(
                        (base + u64::from((*entry).Anonymous.UnwindInfoAddress)) as *const u8,
                        range.unwind_info.len()
                    ),
                    range.unwind_info,
                );
            }
            let entry = RtlLookupFunctionEntry(
                address + u64::from(range.begin),
                &mut base,
                std::ptr::null_mut(),
            );
            metadata.push((*entry).Anonymous.UnwindInfoAddress);
        }
        assert_ne!(metadata[0], metadata[1]);
        for offset in [16, 17, 18, 33] {
            assert!(
                RtlLookupFunctionEntry(address + offset, &mut base, std::ptr::null_mut()).is_null()
            );
        }

        let stack = [0u64, 0, 0, 0, 0x1234_5678, 0x9876_5432, 0];
        let bottom = stack.as_ptr() as u64;
        // Actual instruction boundaries, including both epilogues. At entry
        // and after allocation the same stack contents model both functions.
        for (offset, rsp_slot) in [
            (0, 5),
            (1, 4),
            (5, 0),
            (10, 0),
            (14, 4),
            (15, 5),
            (19, 5),
            (23, 0),
            (28, 0),
            (32, 5),
        ] {
            let mut context: CONTEXT = std::mem::zeroed();
            context.Rip = address + offset;
            context.Rsp = bottom + rsp_slot * 8;
            context.Rbp = stack[4];
            let entry = RtlLookupFunctionEntry(context.Rip, &mut base, std::ptr::null_mut());
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
            assert_eq!(context.Rip, stack[5], "RIP at {offset}");
            assert_eq!(context.Rsp, bottom + 48, "RSP at {offset}");
            assert_eq!(context.Rbp, stack[4], "RBP at {offset}");
        }
        drop(code);
        for offset in [5, 23] {
            assert!(
                RtlLookupFunctionEntry(address + offset, &mut base, std::ptr::null_mut()).is_null()
            );
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn windows_jit_rejects_invalid_unwind_ranges() {
    use egcl_rt::jit::{JitBuffer, WindowsUnwindInfo};
    let leaf = WindowsUnwindInfo {
        begin: 0,
        end: 1,
        unwind_info: &[1, 0, 0, 0],
    };
    let code = [0xc3; 8];
    for ranges in [
        vec![],
        vec![WindowsUnwindInfo { end: 0, ..leaf }],
        vec![WindowsUnwindInfo { begin: 2, ..leaf }],
        vec![WindowsUnwindInfo { end: 9, ..leaf }],
        vec![WindowsUnwindInfo {
            unwind_info: &[1, 0, 0],
            ..leaf
        }],
        vec![leaf, leaf],
        vec![
            WindowsUnwindInfo {
                begin: 4,
                end: 5,
                ..leaf
            },
            leaf,
        ],
        vec![
            WindowsUnwindInfo { end: 5, ..leaf },
            WindowsUnwindInfo {
                begin: 4,
                end: 6,
                ..leaf
            },
        ],
    ] {
        assert!(unsafe { JitBuffer::new_with_windows_unwind_ranges(&code, &ranges) }.is_none());
    }
    assert!(unsafe { JitBuffer::new_with_windows_unwind_ranges(&[], &[leaf]) }.is_none());
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
    assert_eq!(egcl_rt::current_platform_tag(), (1 << 32) | 4);
}

#[test]
fn windows_dll_lifetime() {
    use egcl_rt::ffi::*;
    let library = load_foreign_library("kernel32.dll").unwrap();
    unsafe {
        assert!(
            !foreign_symbol(library, "GetCurrentProcessId")
                .unwrap()
                .is_null()
        );
        assert!(foreign_symbol(library, "egcl_nonexistent_export").is_err());
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
    assert!(egcl_rt::eval_stack_budget() < high - low);
}
