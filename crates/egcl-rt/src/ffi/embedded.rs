// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shared libraries that travel with a saved executable (bliss-rhsy4).
//!
//! `embed` reads a shared object on the BUILD host and records its bytes and
//! the names it answers to. A save stores each one as a `lib/` member of the
//! executable's zip archive (`crate::container`), with its names in the
//! archive's manifest. A restore does not copy them: an entry then refers to
//! its member through a descriptor retained for the purpose, and a later save
//! copies the bytes forward.
//!
//! The archive is an ordinary zip, so a user can replace, add or remove a
//! library with zip tools, as the LGPL requires they can. Nothing about a
//! library is therefore recorded at save time beyond its names: the SONAME and
//! `DT_NEEDED` entries are read from the bytes when it loads.
//!
//! `load_foreign_library` consults this registry before the file system. An
//! entry answers to its SONAME, to the absolute path it was embedded from, to
//! any extra names given at embed time, and to its member's file name. On glibc
//! (the `c-ffi` backend) a library is loaded without touching the disk: its
//! bytes go into a sealed memfd, the member's CRC-32 is checked after sealing,
//! and the loader opens `/proc/self/fd/N`. Embedded libraries it names in
//! `DT_NEEDED` are loaded first, so glibc finds them already loaded by SONAME
//! instead of searching for them (bliss-rhsy4.2).

use crate::container::{self, Archive, Manifest};
use crate::error::EgclError;
use std::sync::{Arc, Mutex};

/// One embedded shared object.
#[derive(Clone)]
struct Entry {
    /// Names `load_foreign_library` matches exactly. The first is the one
    /// reported to Lisp.
    names: Vec<String>,
    /// The archive member it is saved as: `lib/` and its SONAME or file name.
    member: String,
    source: Source,
    /// The sealed memfd this entry was loaded from, kept for the life of the
    /// process. Keeping it open also keeps its `/proc/self/fd/N` path unique:
    /// glibc recognises an already-loaded object by the path it was opened
    /// with, so a recycled descriptor number would alias another library.
    #[cfg(egcl_dlopen_embedded)]
    memfd: Option<Arc<std::os::fd::OwnedFd>>,
}

#[derive(Clone)]
enum Source {
    /// Read on this host and not yet saved.
    Bytes(Arc<[u8]>),
    /// A member of the archive this process was restored from.
    Archive {
        file: Arc<std::fs::File>,
        member: container::Member,
    },
}

impl Source {
    fn read_all(&self) -> Result<Vec<u8>, EgclError> {
        match self {
            Source::Bytes(bytes) => Ok(bytes.to_vec()),
            Source::Archive { file, member } => container::read_member(&**file, member),
        }
    }
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// The registry key for a designator containing a separator: absolute, the
/// normalisation embedded files use. A bare name such as `libz.so.1` is kept
/// as given, because it is matched against SONAMEs and file names.
fn normalize(name: &str) -> String {
    if !name.contains('/') {
        return name.to_owned();
    }
    let absolute = std::path::absolute(name)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_owned());
    absolute.trim_end_matches('/').to_owned()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

// ── ELF dynamic section ─────────────────────────────────────────────

/// What `embed` needs from an ELF shared object.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ElfDynamic {
    pub class: u8,
    pub data: u8,
    pub machine: u16,
    pub soname: Option<String>,
    pub needed: Vec<String>,
}

const ET_DYN: u16 = 3;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_SONAME: u64 = 14;

/// Read the identity and dynamic dependencies of an ELF shared object, of
/// either class and byte order. Everything is bounds-checked: the bytes come
/// from an arbitrary file.
pub(crate) fn parse_elf_dynamic(bytes: &[u8]) -> Result<ElfDynamic, String> {
    if bytes.len() < 52 || &bytes[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    let (class, data) = (bytes[4], bytes[5]);
    let wide = match class {
        1 => false,
        2 => true,
        _ => return Err(format!("unknown ELF class {class}")),
    };
    let big = match data {
        1 => false,
        2 => true,
        _ => return Err(format!("unknown ELF byte order {data}")),
    };
    let at = |offset: u64, width: usize| -> Result<u64, String> {
        let start = usize::try_from(offset).map_err(|_| "ELF offset overflow".to_owned())?;
        let field = start
            .checked_add(width)
            .and_then(|end| bytes.get(start..end))
            .ok_or_else(|| "truncated ELF file".to_owned())?;
        let mut value = 0u64;
        for i in 0..width {
            let byte = if big { field[i] } else { field[width - 1 - i] };
            value = value << 8 | byte as u64;
        }
        Ok(value)
    };
    let word = if wide { 8 } else { 4 };
    let e_type = at(16, 2)? as u16;
    let machine = at(18, 2)? as u16;
    if e_type != ET_DYN {
        return Err("not a shared object (ELF type is not ET_DYN)".into());
    }
    let (phoff, phentsize, phnum) = if wide {
        (at(32, 8)?, at(54, 2)?, at(56, 2)?)
    } else {
        (at(28, 4)?, at(42, 2)?, at(44, 2)?)
    };
    if phentsize < if wide { 56 } else { 32 } {
        return Err("bad ELF program header size".into());
    }

    // Program headers: the PT_LOAD segments translate the string table's
    // virtual address to a file offset; PT_DYNAMIC locates the entries.
    let mut loads = Vec::new();
    let mut dynamic = None;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        let p_type = at(ph, 4)? as u32;
        let (offset, vaddr, filesz) = if wide {
            (at(ph + 8, 8)?, at(ph + 16, 8)?, at(ph + 32, 8)?)
        } else {
            (at(ph + 4, 4)?, at(ph + 8, 4)?, at(ph + 16, 4)?)
        };
        match p_type {
            PT_LOAD => loads.push((vaddr, offset, filesz)),
            PT_DYNAMIC => dynamic = Some((offset, filesz)),
            _ => {}
        }
    }
    let Some((dyn_offset, dyn_size)) = dynamic else {
        // A static shared object with no dependencies and no SONAME.
        return Ok(ElfDynamic {
            class,
            data,
            machine,
            soname: None,
            needed: Vec::new(),
        });
    };

    let (mut strtab, mut strsz, mut soname, mut needed) = (None, None, None, Vec::new());
    let entry_size = 2 * word as u64;
    for i in 0..dyn_size / entry_size {
        let entry = dyn_offset + i * entry_size;
        let (tag, value) = (at(entry, word)?, at(entry + word as u64, word)?);
        match tag {
            DT_NULL => break,
            DT_NEEDED => needed.push(value),
            DT_STRTAB => strtab = Some(value),
            DT_STRSZ => strsz = Some(value),
            DT_SONAME => soname = Some(value),
            _ => {}
        }
    }
    if soname.is_none() && needed.is_empty() {
        return Ok(ElfDynamic {
            class,
            data,
            machine,
            soname: None,
            needed: Vec::new(),
        });
    }
    let strtab = strtab.ok_or("ELF dynamic section has no DT_STRTAB")?;
    let strsz = strsz.ok_or("ELF dynamic section has no DT_STRSZ")?;
    let strtab_offset = loads
        .iter()
        .find(|(vaddr, _, filesz)| strtab >= *vaddr && strtab - vaddr < *filesz)
        .map(|(vaddr, offset, _)| offset + (strtab - vaddr))
        .ok_or("DT_STRTAB is outside every loaded segment")?;
    let string = |index: u64| -> Result<String, String> {
        if index >= strsz {
            return Err("ELF string index outside DT_STRSZ".into());
        }
        let start = usize::try_from(strtab_offset + index).map_err(|_| "ELF offset overflow")?;
        let limit = usize::try_from(strtab_offset + strsz).map_err(|_| "ELF offset overflow")?;
        let table = bytes
            .get(start..limit.min(bytes.len()))
            .ok_or("truncated ELF string table")?;
        let end = table
            .iter()
            .position(|&b| b == 0)
            .ok_or("unterminated ELF string")?;
        String::from_utf8(table[..end].to_vec()).map_err(|_| "ELF string is not UTF-8".into())
    };
    Ok(ElfDynamic {
        class,
        data,
        machine,
        soname: soname.map(string).transpose()?,
        needed: needed.into_iter().map(string).collect::<Result<_, _>>()?,
    })
}

/// The class, byte order and machine of the running executable, which an
/// embedded library must share to be loadable by it.
fn host_elf_identity() -> Result<(u8, u8, u16), EgclError> {
    use std::io::Read;
    let mut header = [0u8; 20];
    std::fs::File::open("/proc/self/exe")
        .or_else(|_| std::env::current_exe().and_then(std::fs::File::open))
        .and_then(|mut exe| exe.read_exact(&mut header))
        .map_err(|e| {
            EgclError::FfiError(format!("cannot read this executable's ELF header: {e}"))
        })?;
    if &header[..4] != b"\x7fELF" {
        return Err(EgclError::FfiError(
            "embedded foreign libraries need an ELF host".into(),
        ));
    }
    let machine = if header[5] == 2 {
        u16::from_be_bytes([header[18], header[19]])
    } else {
        u16::from_le_bytes([header[18], header[19]])
    };
    Ok((header[4], header[5], machine))
}

// ── Registry ────────────────────────────────────────────────────────

fn push_unique(names: &mut Vec<String>, name: String) {
    if !names.contains(&name) {
        names.push(name);
    }
}

/// Read the shared object at `path` now and carry it in saved executables. It
/// answers to its SONAME, its absolute path and every name in `aliases`.
/// Embedding under a name already in use replaces that entry. Returns the name
/// it is reported under: the SONAME when it has one.
pub fn embed(path: &str, aliases: &[String]) -> Result<String, EgclError> {
    let bytes = std::fs::read(path)
        .map_err(|e| EgclError::FfiError(format!("cannot embed '{path}': {e}")))?;
    let elf = parse_elf_dynamic(&bytes)
        .map_err(|e| EgclError::FfiError(format!("cannot embed '{path}': {e}")))?;
    let host = host_elf_identity()?;
    if (elf.class, elf.data, elf.machine) != host {
        return Err(EgclError::FfiError(format!(
            "cannot embed '{path}': it is built for ELF machine {} (class {}, byte order {}), \
             but this executable is machine {} (class {}, byte order {})",
            elf.machine, elf.class, elf.data, host.2, host.0, host.1
        )));
    }
    let absolute = normalize(path);
    let mut names = Vec::new();
    for name in elf
        .soname
        .iter()
        .cloned()
        .chain([absolute.clone()])
        .chain(aliases.iter().map(|a| normalize(a)))
    {
        push_unique(&mut names, name);
    }
    let member = format!(
        "{}{}",
        container::LIBRARY_DIR,
        elf.soname
            .as_deref()
            .unwrap_or_else(|| file_name(&absolute))
    );
    let entry = Entry {
        names,
        member,
        source: Source::Bytes(bytes.into()),
        #[cfg(egcl_dlopen_embedded)]
        memfd: None,
    };
    let primary = entry.names[0].clone();
    let mut registry = REGISTRY.lock().unwrap();
    // A name may designate one library only: whatever this one is called by,
    // an older embedding answering to any of the same names is replaced.
    registry.retain(|old| !old.names.iter().any(|n| entry.names.contains(n)));
    registry.push(entry);
    Ok(primary)
}

/// The names embedded libraries are reported under, in embedding order.
pub fn embedded_names() -> Vec<String> {
    REGISTRY
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.names[0].clone())
        .collect()
}

/// True when `designator` names an embedded library.
pub fn is_embedded(designator: &str) -> bool {
    find(&REGISTRY.lock().unwrap(), designator).is_some()
}

fn find(registry: &[Entry], designator: &str) -> Option<usize> {
    let key = normalize(designator);
    registry.iter().position(|e| e.names.contains(&key))
}

// ── Archive members ─────────────────────────────────────────────────

/// One library as a save writes it.
pub(crate) struct SavedLibrary {
    pub member: String,
    pub names: Vec<String>,
    pub bytes: Vec<u8>,
}

/// Every embedded library's bytes, for `container::build`. A library restored
/// from an archive is read back from it, CRC-checked, so one damaged since is
/// not carried forward.
pub(crate) fn saved_libraries() -> Result<Vec<SavedLibrary>, EgclError> {
    let entries = REGISTRY.lock().unwrap().clone();
    let mut saved: Vec<SavedLibrary> = Vec::with_capacity(entries.len());
    for entry in entries {
        if saved.iter().any(|s| s.member == entry.member) {
            return Err(EgclError::FfiError(format!(
                "two embedded libraries would both be saved as '{}'; give one of them a \
                 distinct SONAME or file name",
                entry.member
            )));
        }
        saved.push(SavedLibrary {
            bytes: entry.source.read_all()?,
            member: entry.member,
            names: entry.names,
        });
    }
    Ok(saved)
}

/// Make the `lib/` members of a restored archive the embedded set. A member
/// answers to the names its manifest record gives, then to its file name; one
/// added to the archive by hand has only its file name.
pub fn install_archive(
    file: &std::fs::File,
    archive: &Archive,
    manifest: &Manifest,
) -> Result<(), EgclError> {
    let file = Arc::new(file.try_clone().map_err(|e| {
        EgclError::InvalidImage(format!(
            "cannot retain the executable for its libraries: {e}"
        ))
    })?);
    let entries = archive
        .libraries()
        .map(|member| {
            let mut names = manifest.names_for(&member.name).to_vec();
            push_unique(&mut names, file_name(&member.name).to_owned());
            Entry {
                names,
                member: member.name.clone(),
                source: Source::Archive {
                    file: Arc::clone(&file),
                    member: member.clone(),
                },
                #[cfg(egcl_dlopen_embedded)]
                memfd: None,
            }
        })
        .collect();
    *REGISTRY.lock().unwrap() = entries;
    Ok(())
}

// ── Loading through a sealed memfd (glibc, `c-ffi`) ─────────────────

#[cfg(egcl_dlopen_embedded)]
mod memfd_loader {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const MFD_CLOEXEC: libc::c_uint = 0x0001;
    const MFD_ALLOW_SEALING: libc::c_uint = 0x0002;
    /// Linux 6.3+: request an executable memfd explicitly, which
    /// `vm.memfd_noexec=1` otherwise denies by default.
    const MFD_EXEC: libc::c_uint = 0x0010;
    const F_ADD_SEALS: libc::c_int = 1033;
    const F_SEAL_SEAL: libc::c_int = 0x0001;
    const F_SEAL_SHRINK: libc::c_int = 0x0002;
    const F_SEAL_GROW: libc::c_int = 0x0004;
    const F_SEAL_WRITE: libc::c_int = 0x0008;

    /// Serialises embedded loads, so a library and its dependencies are
    /// materialised once even when two threads ask at the same time.
    static LOAD_LOCK: Mutex<()> = Mutex::new(());

    fn os_error(what: &str) -> EgclError {
        EgclError::FfiError(format!("{what}: {}", std::io::Error::last_os_error()))
    }

    fn fill_error(e: impl std::fmt::Display) -> EgclError {
        EgclError::FfiError(format!("cannot fill embedded library memfd: {e}"))
    }

    fn dlerror_text() -> String {
        // SAFETY: dlerror returns null or a NUL-terminated thread-local string.
        unsafe {
            let message = libc::dlerror();
            if message.is_null() {
                "unknown dlopen error".into()
            } else {
                std::ffi::CStr::from_ptr(message)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    /// A sealed memfd holding exactly `entry`'s verified bytes, and what its
    /// ELF dynamic section says.
    fn materialise(entry: &Entry) -> Result<(OwnedFd, ElfDynamic), EgclError> {
        let label = CString::new(file_name(&entry.member))
            .map_err(|_| EgclError::FfiError("library name contains NUL".into()))?;
        let create = |flags: libc::c_uint| unsafe {
            libc::syscall(libc::SYS_memfd_create, label.as_ptr(), flags) as libc::c_int
        };
        let mut fd = create(MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_EXEC);
        if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
            // Kernels before 6.3 reject the flag; their memfds are executable.
            fd = create(MFD_CLOEXEC | MFD_ALLOW_SEALING);
        }
        if fd < 0 {
            return Err(os_error("cannot create a memfd for an embedded library"));
        }
        // SAFETY: memfd_create returned a fresh descriptor this function owns.
        let memfd = unsafe { OwnedFd::from_raw_fd(fd) };
        let len = fill(&memfd, entry)?;
        let seals = F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SEAL;
        // SAFETY: plain fcntl on a descriptor we own.
        if unsafe { libc::fcntl(memfd.as_raw_fd(), F_ADD_SEALS, seals) } != 0 {
            return Err(os_error("cannot seal an embedded library's memfd"));
        }
        // Verify only after sealing: from here the contents cannot change
        // between the check and the loader mapping them.
        let elf = inspect(&memfd, len, entry)?;
        Ok((memfd, elf))
    }

    /// Copy `entry`'s bytes into `memfd`; returns how many.
    fn fill(memfd: &OwnedFd, entry: &Entry) -> Result<usize, EgclError> {
        use std::io::Write;
        // SAFETY: the File borrows the descriptor; ManuallyDrop keeps it open.
        let mut out =
            std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(memfd.as_raw_fd()) });
        let (file, member) = match &entry.source {
            Source::Archive { file, member } if member.method == container::STORED => {
                (file, member)
            }
            source => {
                // In memory, or deflated in the archive.
                let bytes = source.read_all()?;
                out.write_all(&bytes).map_err(fill_error)?;
                return Ok(bytes.len());
            }
        };
        // Stored: the kernel copies the executable's pages straight into the
        // memfd, never through a user-space buffer.
        let len = usize::try_from(member.size).map_err(|_| too_large())?;
        let mut position = member.data_offset as libc::off_t;
        let mut remaining = len;
        while remaining > 0 {
            // SAFETY: both descriptors are open; `position` is ours.
            let sent = unsafe {
                libc::sendfile(
                    memfd.as_raw_fd(),
                    file.as_raw_fd(),
                    &mut position,
                    remaining,
                )
            };
            if sent < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                // A file system without sendfile support: copy instead.
                let mut rest = vec![0; remaining];
                container::ReadAt::read_exact_at(&**file, &mut rest, position as u64)
                    .map_err(fill_error)?;
                out.write_all(&rest).map_err(fill_error)?;
                break;
            }
            if sent == 0 {
                return Err(fill_error("the executable ends inside an embedded library"));
            }
            remaining -= sent as usize;
        }
        Ok(len)
    }

    /// Check a sealed memfd against its archive member's CRC-32 and read its
    /// ELF dynamic section, through one read-only mapping.
    fn inspect(memfd: &OwnedFd, len: usize, entry: &Entry) -> Result<ElfDynamic, EgclError> {
        let not_loadable = |why: String| {
            EgclError::FfiError(format!(
                "embedded library '{}' is not a loadable shared object: {why}",
                entry.names[0]
            ))
        };
        if len == 0 {
            return Err(not_loadable("it is empty".into()));
        }
        // SAFETY: a fresh read-only private mapping of a sealed file whose size
        // is exactly `len`; it is unmapped before returning.
        unsafe {
            let map = libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                memfd.as_raw_fd(),
                0,
            );
            if map == libc::MAP_FAILED {
                return Err(os_error("cannot map an embedded library for verification"));
            }
            let bytes = std::slice::from_raw_parts(map as *const u8, len);
            let result = match &entry.source {
                Source::Archive { member, .. }
                    if len as u64 != member.size || container::crc32(bytes) != member.crc32 =>
                {
                    Err(EgclError::FfiError(format!(
                        "embedded library '{}' failed its CRC-32 check",
                        entry.names[0]
                    )))
                }
                _ => parse_elf_dynamic(bytes).map_err(not_loadable),
            };
            libc::munmap(map, len);
            result
        }
    }

    fn proc_path(memfd: &OwnedFd) -> CString {
        CString::new(format!("/proc/self/fd/{}", memfd.as_raw_fd())).unwrap()
    }

    /// Open `designator` from the embedded set: `None` when it is not
    /// embedded, else a fresh `dlopen` reference.
    pub fn open(designator: &str) -> Option<Result<*mut libc::c_void, EgclError>> {
        let index = find(&REGISTRY.lock().unwrap(), designator)?;
        let _serial = LOAD_LOCK.lock().unwrap();
        Some(open_entry(index, &mut Vec::new()))
    }

    fn open_entry(index: usize, chain: &mut Vec<usize>) -> Result<*mut libc::c_void, EgclError> {
        let entry = REGISTRY.lock().unwrap()[index].clone();
        if chain.contains(&index) {
            return Err(EgclError::FfiError(format!(
                "embedded library '{}' depends on itself through DT_NEEDED",
                entry.names[0]
            )));
        }
        let flags = libc::RTLD_NOW | libc::RTLD_GLOBAL;

        // Materialised before: the same path reaches the same object, loaded
        // or not, and needs no copy or check.
        if let Some(memfd) = &entry.memfd {
            return dlopen_path(&proc_path(memfd), flags, &entry);
        }
        let (memfd, elf) = materialise(&entry)?;
        // glibc keeps one object per SONAME. If a copy from elsewhere is
        // already loaded, DT_NEEDED would bind to it rather than to this one.
        if let Some(soname) = &elf.soname {
            let name = CString::new(soname.as_str()).unwrap();
            // SAFETY: RTLD_NOLOAD only probes; a hit is a reference we release.
            let existing =
                unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
            if !existing.is_null() {
                unsafe { libc::dlclose(existing) };
                return Err(EgclError::FfiError(format!(
                    "cannot load embedded '{soname}': a library with that SONAME is already \
                     loaded from outside the image, and the loader would bind to it instead"
                )));
            }
        }

        // Embedded dependencies first, so glibc finds them by SONAME. The
        // references are dropped once the dependent holds its own.
        chain.push(index);
        let mut dependencies = Vec::new();
        let result = (|| {
            for needed in &elf.needed {
                let dependency = find(&REGISTRY.lock().unwrap(), needed);
                if let Some(dependency) = dependency {
                    dependencies.push(open_entry(dependency, chain)?);
                }
            }
            let memfd = Arc::new(memfd);
            let handle = dlopen_path(&proc_path(&memfd), flags, &entry)?;
            if let Some(slot) = REGISTRY.lock().unwrap().get_mut(index) {
                slot.memfd = Some(memfd);
            }
            Ok(handle)
        })();
        chain.pop();
        for dependency in dependencies {
            // SAFETY: each is a reference this function took above.
            unsafe { libc::dlclose(dependency) };
        }
        result
    }

    fn dlopen_path(
        path: &CString,
        flags: libc::c_int,
        entry: &Entry,
    ) -> Result<*mut libc::c_void, EgclError> {
        // SAFETY: dlopen on a NUL-terminated path.
        let handle = unsafe { libc::dlopen(path.as_ptr(), flags) };
        if !handle.is_null() {
            return Ok(handle);
        }
        let message = dlerror_text();
        let hint =
            if message.contains("failed to map segment") || message.contains("Permission denied") {
                " (the kernel may forbid executable memfds: see vm.memfd_noexec)"
            } else {
                ""
            };
        Err(EgclError::FfiError(format!(
            "cannot load embedded library '{}': {message}{hint}",
            entry.names[0]
        )))
    }

    fn too_large() -> EgclError {
        EgclError::FfiError("embedded library is too large for this address space".into())
    }
}

#[cfg(egcl_dlopen_embedded)]
pub(crate) use memfd_loader::open;

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal little-endian ELF64 shared object: one PT_LOAD covering the
    /// file, a PT_DYNAMIC with DT_STRTAB/DT_STRSZ/DT_SONAME/DT_NEEDED.
    fn tiny_elf(big: bool) -> Vec<u8> {
        let mut f = vec![0u8; 0x200];
        let mut put = |at: usize, width: usize, value: u64| {
            for i in 0..width {
                let shift = 8 * if big { width - 1 - i } else { i };
                f[at + i] = (value >> shift) as u8;
            }
        };
        put(0, 4, if big { 0x7f454c46 } else { 0x464c457f });
        put(4, 1, 2);
        put(5, 1, if big { 2 } else { 1 });
        put(16, 2, ET_DYN as u64);
        put(18, 2, 62);
        put(32, 8, 0x40); // phoff
        put(54, 2, 56);
        put(56, 2, 2);
        // PT_LOAD: offset 0, vaddr 0x1000, filesz 0x200
        put(0x40, 4, PT_LOAD as u64);
        put(0x48, 8, 0);
        put(0x50, 8, 0x1000);
        put(0x60, 8, 0x200);
        // PT_DYNAMIC at 0x100, five entries
        put(0x78, 4, PT_DYNAMIC as u64);
        put(0x80, 8, 0x100);
        put(0x98, 8, 5 * 16);
        let strtab = b"\0libfoo.so.1\0libbar.so.2\0";
        let dynamic = [
            (DT_STRTAB, 0x1180),
            (DT_STRSZ, strtab.len() as u64),
            (DT_SONAME, 1),
            (DT_NEEDED, 13),
            (DT_NULL, 0),
        ];
        for (i, (tag, value)) in dynamic.iter().enumerate() {
            put(0x100 + i * 16, 8, *tag);
            put(0x108 + i * 16, 8, *value);
        }
        f[0x180..0x180 + strtab.len()].copy_from_slice(strtab);
        f
    }

    #[test]
    fn parses_soname_and_needed_in_both_byte_orders() {
        for big in [false, true] {
            let elf = parse_elf_dynamic(&tiny_elf(big)).unwrap();
            assert_eq!(elf.soname.as_deref(), Some("libfoo.so.1"));
            assert_eq!(elf.needed, ["libbar.so.2"]);
            assert_eq!(
                (elf.class, elf.data, elf.machine),
                (2, if big { 2 } else { 1 }, 62)
            );
        }
    }

    #[test]
    fn a_restored_archive_installs_its_libraries_as_references_into_the_file() {
        let manifest = Manifest {
            application: false,
            libraries: vec![(
                "lib/libtest0.so.1".into(),
                vec![
                    "libtest0.so.1".into(),
                    "/opt/libtest0.so.1".into(),
                    "t0".into(),
                ],
            )],
        };
        let mut writer = container::Writer::new(vec![0xAB; 8192]);
        writer
            .add(
                container::MANIFEST,
                manifest.render().unwrap().as_bytes(),
                1,
                0o644,
            )
            .unwrap();
        writer
            .add("lib/libtest0.so.1", b"first library bytes", 16384, 0o755)
            .unwrap();
        // Added by hand, with no manifest record.
        writer
            .add("lib/libextra.so", &[7u8; 5000], 16384, 0o755)
            .unwrap();
        let bytes = writer.finish().unwrap();
        let path = std::env::temp_dir().join(format!("egcl-flb-{}", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let archive = container::read(&file).unwrap().unwrap();
        install_archive(&file, &archive, &archive.manifest(&file).unwrap()).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(embedded_names(), ["libtest0.so.1", "libextra.so"]);
        assert!(is_embedded("/opt/libtest0.so.1") && is_embedded("t0"));
        let saved = saved_libraries().unwrap();
        *REGISTRY.lock().unwrap() = Vec::new();
        assert_eq!(saved[0].bytes, b"first library bytes");
        assert_eq!(saved[0].names, manifest.libraries[0].1);
        assert_eq!(saved[1].member, "lib/libextra.so");
        assert_eq!(saved[1].names, ["libextra.so"]);
        assert_eq!(saved[1].bytes, vec![7u8; 5000]);
    }

    #[test]
    fn rejects_malformed_elf() {
        assert!(parse_elf_dynamic(b"not an elf").is_err());
        let mut elf = tiny_elf(false);
        elf[16] = 2; // ET_EXEC
        assert!(parse_elf_dynamic(&elf).is_err());
        let mut elf = tiny_elf(false);
        elf.truncate(0x110); // dynamic section cut short
        assert!(parse_elf_dynamic(&elf).is_err());
        let mut elf = tiny_elf(false);
        elf[0x128] = 0xff; // SONAME index beyond DT_STRSZ
        assert!(parse_elf_dynamic(&elf).is_err());
    }
}
