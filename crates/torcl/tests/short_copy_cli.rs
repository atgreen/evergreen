//! The CLI's short-copy primitive must access exactly its supplied byte ranges.
#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "musl"))]

#[path = "../src/short_copy.rs"]
mod short_copy;

unsafe extern "C" {
    #[link_name = "__wrap_memcpy"]
    fn copy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8;
    #[link_name = "memcpy"]
    fn libc_copy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8;
}

// This integration executable is not linker-wrapped. Supply its fallback
// here, leaving the CLI (including its unit-test harness) bound to the
// linker's __real_memcpy rather than a cfg(test)-dependent symbol.
#[unsafe(no_mangle)]
unsafe extern "C" fn __real_memcpy(dst: *mut u8, src: *const u8, len: usize) -> *mut u8 {
    unsafe { libc_copy(dst, src, len) }
}

fn pattern(index: usize) -> u8 {
    (index.wrapping_mul(37).wrapping_add(index / 7)) as u8
}

fn check_copy(len: usize, source_offset: usize, destination_offset: usize) {
    let source: Vec<_> = (0..len + 64).map(pattern).collect();
    let mut destination = vec![0xa5; len + 64];
    let dst = unsafe { destination.as_mut_ptr().add(destination_offset) };
    let src = unsafe { source.as_ptr().add(source_offset) };
    assert_eq!(unsafe { copy(dst, src, len) }, dst);
    // Volatile byte reads keep the oracle independent of memcpy and SIMD.
    for i in 0..destination.len() {
        let expected = if (destination_offset..destination_offset + len).contains(&i) {
            pattern(source_offset + i - destination_offset)
        } else {
            0xa5
        };
        let actual = unsafe { destination.as_ptr().add(i).read_volatile() };
        assert_eq!(
            actual, expected,
            "len={len}, src={source_offset}, dst={destination_offset}, byte={i}"
        );
    }
    for i in 0..source.len() {
        assert_eq!(
            unsafe { source.as_ptr().add(i).read_volatile() },
            pattern(i)
        );
    }
}

#[test]
fn short_copy_preserves_bytes_for_every_length_and_alignment() {
    for len in 0..=128 {
        for source_offset in 0..32 {
            for destination_offset in 0..32 {
                check_copy(len, source_offset, destination_offset);
            }
        }
    }
}

#[test]
fn short_copy_preserves_large_fallback_and_concurrent_calls() {
    let threads: Vec<_> = (0..4)
        .map(|thread| {
            std::thread::spawn(move || {
                for len in [
                    129, 255, 256, 257, 511, 512, 513, 4095, 4096, 4097, 65535, 65536, 65537,
                ] {
                    for source_offset in [0, 1, 7, 15, 31] {
                        check_copy(len, source_offset, thread * 7);
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn short_copy_snapshots_overlapping_small_ranges() {
    // musl's memmove may tail-call memcpy for a forward-safe overlap. The
    // short path loads its complete input before storing, so it handles either
    // overlap direction without relying on memcpy's non-overlap contract.
    for len in 0..=64 {
        for destination_offset in 0..=128 {
            let mut bytes: Vec<_> = (0..192).map(pattern).collect();
            let base = bytes.as_mut_ptr();
            unsafe { copy(base.add(destination_offset), base.add(64), len) };
            for i in 0..bytes.len() {
                let expected = if (destination_offset..destination_offset + len).contains(&i) {
                    pattern(64 + i - destination_offset)
                } else {
                    pattern(i)
                };
                assert_eq!(unsafe { base.add(i).read_volatile() }, expected);
            }
        }
    }
}

unsafe extern "C" {
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn mprotect(address: *mut std::ffi::c_void, length: usize, prot: i32) -> i32;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
    fn getpagesize() -> i32;
}

struct GuardedPage {
    mapping: *mut u8,
    page: *mut u8,
    size: usize,
}

impl GuardedPage {
    fn new() -> Self {
        let size = unsafe { getpagesize() } as usize;
        let mapping = unsafe { mmap(std::ptr::null_mut(), size * 3, 0, 0x22, -1, 0) };
        assert_ne!(mapping as isize, -1, "mmap");
        let mapping = mapping.cast::<u8>();
        let page = unsafe { mapping.add(size) };
        assert_eq!(unsafe { mprotect(page.cast(), size, 3) }, 0);
        Self {
            mapping,
            page,
            size,
        }
    }
}

impl Drop for GuardedPage {
    fn drop(&mut self) {
        assert_eq!(unsafe { munmap(self.mapping.cast(), self.size * 3) }, 0);
    }
}

#[test]
fn short_copy_does_not_touch_guard_pages_or_zero_length_pointers() {
    let source = GuardedPage::new();
    let destination = GuardedPage::new();
    for i in 0..source.size {
        unsafe { source.page.add(i).write_volatile(pattern(i)) };
    }
    assert_eq!(unsafe { mprotect(source.page.cast(), source.size, 1) }, 0);
    assert_eq!(
        unsafe { copy(std::ptr::null_mut(), std::ptr::null(), 0) },
        std::ptr::null_mut()
    );
    assert_eq!(
        unsafe { copy(destination.mapping, source.mapping, 0) },
        destination.mapping
    );
    for len in (0..=128).chain([255, 256, 257, source.size - 1, source.size]) {
        for source_offset in [0, source.size - len] {
            for destination_offset in [0, destination.size - len] {
                for i in 0..destination.size {
                    unsafe { destination.page.add(i).write_volatile(0xa5) };
                }
                let dst = unsafe { destination.page.add(destination_offset) };
                let src = unsafe { source.page.add(source_offset) };
                assert_eq!(unsafe { copy(dst, src, len) }, dst);
                for i in 0..destination.size {
                    let expected = if (destination_offset..destination_offset + len).contains(&i) {
                        pattern(source_offset + i - destination_offset)
                    } else {
                        0xa5
                    };
                    assert_eq!(
                        unsafe { destination.page.add(i).read_volatile() },
                        expected,
                        "len={len}, byte={i}"
                    );
                }
            }
        }
    }
}

core::arch::global_asm!(
    r#"
    .text
    .globl torcl_test_copy_abi
    .type torcl_test_copy_abi,@function
torcl_test_copy_abi:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    sub rsp, 8
    mov ebx, 0x11223344
    mov ebp, 0x22334455
    mov r12d, 0x33445566
    mov r13d, 0x44556677
    mov r14d, 0x55667711
    mov r15d, 0x66771122
    call __wrap_memcpy
    xor eax, eax
    cmp rbx, 0x11223344
    je 2f
    or eax, 1
2:  cmp rbp, 0x22334455
    je 3f
    or eax, 2
3:  cmp r12, 0x33445566
    je 4f
    or eax, 4
4:  cmp r13, 0x44556677
    je 5f
    or eax, 8
5:  cmp r14, 0x55667711
    je 6f
    or eax, 16
6:  cmp r15, 0x66771122
    je 7f
    or eax, 32
7:  pushfq
    pop rcx
    test ecx, 0x400
    jz 8f
    or eax, 64
    cld
8:  add rsp, 8
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret
    .size torcl_test_copy_abi, .-torcl_test_copy_abi
"#
);

unsafe extern "C" {
    fn torcl_test_copy_abi(dst: *mut u8, src: *const u8, len: usize) -> u64;
}

#[test]
fn short_copy_preserves_sysv_registers_and_direction_flag() {
    let source: Vec<_> = (0..4096).map(pattern).collect();
    let mut destination = vec![0; source.len()];
    for len in (0..=129).chain([4096]) {
        assert_eq!(
            unsafe { torcl_test_copy_abi(destination.as_mut_ptr(), source.as_ptr(), len) },
            0,
            "ABI violation for length {len}"
        );
    }
}
