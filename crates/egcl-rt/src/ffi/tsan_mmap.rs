// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Keep ELF mapping lifetimes visible to TSan without changing the ordinary
//! loader backend. Raw syscalls leave access history attached to reused pages.

use elf_loader::{
    Result,
    error::MmapError,
    memory::{HostRegion, MappedRegion, VmAddr},
    os::{DefaultMmap, MadviseAdvice, MapFlags, Mmap, ProtFlags},
};

#[derive(Clone, Copy)]
pub(super) struct TSanMmap;

impl Mmap for TSanMmap {
    type Region = HostRegion;

    unsafe fn create_space(
        &self,
        addr: Option<VmAddr>,
        len: usize,
        prot: ProtFlags,
        _populate_later: bool,
    ) -> Result<MappedRegion<HostRegion>> {
        // SAFETY: the loader supplies the reservation's address and length.
        let ptr = unsafe {
            libc::mmap(
                addr.map_or(std::ptr::null_mut(), VmAddr::as_mut_ptr),
                len,
                prot.bits(),
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(MmapError::MmapAnonymousFailed { code: errno() }.into());
        }
        // Store this mapper in the owning region so drop also uses interception.
        Ok(MappedRegion::local(ptr, len, *self))
    }

    unsafe fn alias_space(&self, addr: VmAddr, len: usize) -> Result<MappedRegion<HostRegion>> {
        Ok(MappedRegion::local_alias(addr.as_mut_ptr(), len, *self))
    }

    unsafe fn map_file_at(
        &self,
        addr: VmAddr,
        len: usize,
        prot: ProtFlags,
        flags: MapFlags,
        offset: usize,
        fd: isize,
    ) -> Result<()> {
        // SAFETY: the loader owns the destination range and validates the file.
        let ptr = unsafe {
            libc::mmap(
                addr.as_mut_ptr(),
                len,
                prot.bits(),
                flags.bits(),
                fd as i32,
                offset as _,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(MmapError::MmapFailed { code: errno() }.into());
        }
        Ok(())
    }

    unsafe fn map_zero_at(
        &self,
        addr: VmAddr,
        len: usize,
        prot: ProtFlags,
        flags: MapFlags,
    ) -> Result<()> {
        // SAFETY: this replaces only a subrange of the loader's reservation.
        let ptr = unsafe {
            libc::mmap(
                addr.as_mut_ptr(),
                len,
                prot.bits(),
                flags.bits() | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(MmapError::MmapAnonymousFailed { code: errno() }.into());
        }
        Ok(())
    }

    unsafe fn munmap(&self, addr: VmAddr, len: usize) -> Result<()> {
        // SAFETY: the owning region releases its reservation exactly once.
        if unsafe { libc::munmap(addr.as_mut_ptr(), len) } != 0 {
            return Err(MmapError::MunmapFailed { code: errno() }.into());
        }
        Ok(())
    }

    unsafe fn madvise(&self, addr: VmAddr, len: usize, behavior: MadviseAdvice) -> Result<()> {
        // SAFETY: unchanged backend operation on the loader-owned range.
        unsafe { DefaultMmap.madvise(addr, len, behavior) }
    }

    unsafe fn mprotect(&self, addr: VmAddr, len: usize, prot: ProtFlags) -> Result<()> {
        // SAFETY: unchanged backend operation on the loader-owned range.
        unsafe { DefaultMmap.mprotect(addr, len, prot) }
    }
}

fn errno() -> u32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO) as u32
}
