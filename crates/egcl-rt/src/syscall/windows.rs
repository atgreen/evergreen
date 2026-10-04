// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Windows implementations of the runtime's OS services. No Linux syscall
//! numbers or Unix signal/context layouts are used in a Windows process.
use windows_sys::Win32::{
    Foundation::*,
    System::{Memory::*, SystemInformation::*, Threading::*},
};

pub const PROT_NONE: i32 = 0;
pub const PROT_READ: i32 = 1;
pub const PROT_WRITE: i32 = 2;
pub const PROT_EXEC: i32 = 4;
pub const MAP_PRIVATE: i32 = 2;
pub const MAP_ANONYMOUS: i32 = 0x20;

pub fn page_size() -> usize {
    static PAGE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *PAGE.get_or_init(|| unsafe {
        let mut info: SYSTEM_INFO = std::mem::zeroed();
        GetSystemInfo(&mut info);
        info.dwPageSize as usize
    })
}

fn protection(prot: i32) -> Result<u32, i32> {
    match prot {
        PROT_NONE => Ok(PAGE_NOACCESS),
        PROT_READ => Ok(PAGE_READONLY),
        3 => Ok(PAGE_READWRITE),
        5 => Ok(PAGE_EXECUTE_READ),
        _ => Err(ERROR_INVALID_PARAMETER as i32),
    }
}

/// Allocate a private anonymous region; file mappings are not supported here.
/// # Safety
/// The caller must own and eventually release the returned mapping.
pub unsafe fn mmap(
    addr: *mut u8,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> Result<*mut u8, i32> {
    if flags != MAP_PRIVATE | MAP_ANONYMOUS || fd != -1 || offset != 0 || len == 0 {
        return Err(ERROR_INVALID_PARAMETER as i32);
    }
    let p = unsafe {
        VirtualAlloc(
            addr.cast(),
            len,
            MEM_RESERVE | MEM_COMMIT,
            protection(prot)?,
        )
    };
    if p.is_null() {
        Err(unsafe { GetLastError() } as i32)
    } else {
        Ok(p.cast())
    }
}

/// Release a complete region allocated by mmap.
/// # Safety
/// addr must be the allocation base and no references may remain live.
pub unsafe fn munmap(addr: *mut u8, _len: usize) -> Result<(), i32> {
    if unsafe { VirtualFree(addr.cast(), 0, MEM_RELEASE) } == 0 {
        Err(unsafe { GetLastError() } as i32)
    } else {
        Ok(())
    }
}

/// Change protection of committed pages.
/// # Safety
/// The caller must own the range and obey the new access permissions.
pub unsafe fn mprotect(addr: *mut u8, len: usize, prot: i32) -> Result<(), i32> {
    let mut old = 0;
    if unsafe { VirtualProtect(addr.cast(), len, protection(prot)?, &mut old) } == 0 {
        Err(unsafe { GetLastError() } as i32)
    } else {
        Ok(())
    }
}

pub fn gettid() -> i32 {
    unsafe { GetCurrentThreadId() as i32 }
}
pub fn cached_tid() -> i32 {
    gettid()
}
pub fn getpid() -> i32 {
    unsafe { GetCurrentProcessId() as i32 }
}
pub fn exit_group(code: i32) -> ! {
    std::process::exit(code)
}
pub fn abort() -> ! {
    std::process::abort()
}
pub fn sched_yield() {
    std::thread::yield_now();
}
pub fn dbg_write(bytes: &[u8]) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(bytes);
}

pub fn thread_cpu_time_ns() -> Result<u64, i32> {
    unsafe {
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        if GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        ) == 0
        {
            return Err(GetLastError() as i32);
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        Ok((ticks(kernel) + ticks(user)).saturating_mul(100))
    }
}

/// User and kernel CPU time consumed by all threads in this process.
pub fn process_cpu_time_ns() -> Result<u64, i32> {
    unsafe {
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        ) == 0
        {
            return Err(GetLastError() as i32);
        }
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        Ok(ticks(kernel).saturating_add(ticks(user)).saturating_mul(100))
    }
}

/// Bounds of the current OS thread's reserved stack.
pub fn thread_stack_limits() -> (usize, usize) {
    let (mut low, mut high) = (0, 0);
    unsafe {
        GetCurrentThreadStackLimits(&mut low, &mut high);
    }
    (low, high)
}
