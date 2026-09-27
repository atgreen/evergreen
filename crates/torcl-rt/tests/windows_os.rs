#![cfg(windows)]
use torcl_rt::syscall as os;

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
