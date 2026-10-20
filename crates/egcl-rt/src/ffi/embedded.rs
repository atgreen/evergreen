// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shared libraries that travel with the image (bliss-rhsy4).
//!
//! `embed` reads a shared object on the BUILD host and records its bytes, its
//! ELF `DT_SONAME` and `DT_NEEDED` entries, and a SHA-256. A save writes them
//! into their own image section, outside the Lisp heap, with each library's
//! bytes at a page-aligned offset. A restore does not copy them: an entry then
//! refers to the image file through a descriptor retained for the purpose, and
//! a later save copies the bytes forward.
//!
//! `load_foreign_library` consults this registry before the file system. An
//! entry answers to its SONAME, to the absolute path it was embedded from, and
//! to any extra names given at embed time. On glibc (the `c-ffi` backend) a
//! library is loaded without touching the disk: its bytes go into a sealed
//! memfd, the hash is checked after sealing, and the loader opens
//! `/proc/self/fd/N`. Embedded libraries it names in `DT_NEEDED` are loaded
//! first, so glibc finds them already loaded by SONAME instead of searching for
//! them (bliss-rhsy4.2).

use crate::digest::sha256;
use crate::error::EgclError;
use std::sync::{Arc, Mutex};

/// One embedded shared object.
#[derive(Clone)]
struct Entry {
    /// Names `load_foreign_library` matches exactly: the absolute source path,
    /// the SONAME, then any aliases. The first is the one reported to Lisp.
    names: Vec<String>,
    soname: Option<String>,
    needed: Vec<String>,
    sha256: [u8; 32],
    len: u64,
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
    /// In the image this process was restored from, at a file offset.
    Image {
        file: Arc<std::fs::File>,
        offset: u64,
    },
}

impl Source {
    fn read_all(&self, len: u64) -> Result<Vec<u8>, EgclError> {
        match self {
            Source::Bytes(bytes) => Ok(bytes.to_vec()),
            Source::Image { file, offset } => {
                let mut bytes = vec![0; usize::try_from(len).map_err(|_| too_large())?];
                read_exact_at(file, &mut bytes, *offset)?;
                Ok(bytes)
            }
        }
    }
}

#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> Result<(), EgclError> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(bytes, offset)
        .map_err(|e| EgclError::FfiError(format!("cannot read embedded library from image: {e}")))
}

#[cfg(not(unix))]
fn read_exact_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> Result<(), EgclError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(offset))
        .and_then(|_| file.read_exact(bytes))
        .map_err(|e| EgclError::FfiError(format!("cannot read embedded library from image: {e}")))
}

fn too_large() -> EgclError {
    EgclError::FfiError("embedded library is too large for this address space".into())
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// The registry key for a designator containing a separator: absolute, the
/// normalisation embedded files use. A bare name such as `libz.so.1` is kept
/// as given, because it is matched against SONAMEs.
fn normalize(name: &str) -> String {
    if !name.contains('/') {
        return name.to_owned();
    }
    let absolute = std::path::absolute(name)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| name.to_owned());
    absolute.trim_end_matches('/').to_owned()
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

/// Read the shared object at `path` now and carry it in the image. It answers
/// to its SONAME, its absolute path and every name in `aliases`. Embedding
/// under a name already in use replaces that entry. Returns the name it is
/// reported under: the SONAME when it has one.
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
    let mut names = Vec::new();
    for name in elf
        .soname
        .iter()
        .cloned()
        .chain([normalize(path)])
        .chain(aliases.iter().map(|a| normalize(a)))
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let entry = Entry {
        names,
        soname: elf.soname,
        needed: elf.needed,
        sha256: sha256(&bytes),
        len: bytes.len() as u64,
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

// ── Image section ───────────────────────────────────────────────────
//
// Section layout (all integers little-endian, offsets section-relative):
//
//   "EGCLFLB\0"  u32 version  u32 count
//   per entry:   u64 offset  u64 length  [u8; 32] sha256
//                u32 name count, names   u8 has-soname, soname
//                u32 needed count, needed names        (strings: u32 len, bytes)
//   payloads, each at a page-aligned offset after the manifest
//
// The section is always written uncompressed (`SECTION_STORED`): the payloads
// must be directly mappable and copyable from the file.

const SECTION_MAGIC: &[u8; 8] = b"EGCLFLB\0";
const SECTION_VERSION: u32 = 1;
const PAGE: u64 = 4096;

fn align_page(offset: u64) -> u64 {
    offset.div_ceil(PAGE) * PAGE
}

/// Serialise every embedded library as the image's foreign-library section.
/// Empty when nothing is embedded.
pub(crate) fn serialize_section() -> Result<Vec<u8>, EgclError> {
    let entries = REGISTRY.lock().unwrap().clone();
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let put_str = |out: &mut Vec<u8>, s: &str| {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    };
    // The manifest's length does not depend on the offsets it records, so
    // write it once with placeholders and patch them after layout.
    let mut out = Vec::new();
    out.extend_from_slice(SECTION_MAGIC);
    out.extend_from_slice(&SECTION_VERSION.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut offset_slots = Vec::new();
    for entry in &entries {
        offset_slots.push(out.len());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&entry.len.to_le_bytes());
        out.extend_from_slice(&entry.sha256);
        out.extend_from_slice(&(entry.names.len() as u32).to_le_bytes());
        for name in &entry.names {
            put_str(&mut out, name);
        }
        out.push(entry.soname.is_some() as u8);
        put_str(&mut out, entry.soname.as_deref().unwrap_or(""));
        out.extend_from_slice(&(entry.needed.len() as u32).to_le_bytes());
        for needed in &entry.needed {
            put_str(&mut out, needed);
        }
    }
    for (entry, slot) in entries.iter().zip(offset_slots) {
        let offset = align_page(out.len() as u64);
        out.resize(offset as usize, 0);
        out[slot..slot + 8].copy_from_slice(&offset.to_le_bytes());
        let bytes = entry.source.read_all(entry.len)?;
        if sha256(&bytes) != entry.sha256 {
            return Err(EgclError::FfiError(format!(
                "embedded library '{}' changed since it was embedded",
                entry.names[0]
            )));
        }
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// A parsed foreign-library section, installed by `install_section` once the
/// rest of the image has been restored.
pub(crate) struct PendingSection(Vec<Entry>);

/// Bounds-checked reads through a foreign-library section's manifest.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

fn section_error(what: &str) -> EgclError {
    EgclError::InvalidImage(format!("foreign-library section: {what}"))
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], EgclError> {
        let bytes = self
            .pos
            .checked_add(n)
            .and_then(|end| self.bytes.get(self.pos..end))
            .ok_or_else(|| section_error("truncated"))?;
        self.pos += n;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, EgclError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, EgclError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> Result<String, EgclError> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| section_error("name is not UTF-8"))
    }

    fn strings(&mut self) -> Result<Vec<String>, EgclError> {
        let count = self.u32()?;
        (0..count).map(|_| self.string()).collect()
    }
}

/// Validate a foreign-library section. `backing` is the image file and the
/// file offset of the section's first byte; without it (an image read into
/// memory) the payloads are copied out of `section`.
pub(crate) fn parse_section(
    section: &[u8],
    backing: Option<(&std::fs::File, u64)>,
) -> Result<PendingSection, EgclError> {
    if section.is_empty() {
        return Ok(PendingSection(Vec::new()));
    }
    let mut cursor = Cursor {
        bytes: section,
        pos: 0,
    };
    if cursor.take(8)? != SECTION_MAGIC {
        return Err(section_error("bad magic"));
    }
    if cursor.u32()? != SECTION_VERSION {
        return Err(section_error("unsupported version"));
    }
    let count = cursor.u32()?;
    let mut manifest = Vec::new();
    for _ in 0..count {
        let offset = cursor.u64()?;
        let len = cursor.u64()?;
        let digest: [u8; 32] = cursor.take(32)?.try_into().unwrap();
        let names = cursor.strings()?;
        let has_soname = cursor.take(1)?[0] != 0;
        let soname = cursor.string()?;
        let needed = cursor.strings()?;
        if names.is_empty() {
            return Err(section_error("entry has no names"));
        }
        manifest.push((
            offset,
            len,
            digest,
            names,
            has_soname.then_some(soname),
            needed,
        ));
    }
    let manifest_end = cursor.pos as u64;
    let file = backing
        .map(|(file, _)| file.try_clone().map(Arc::new))
        .transpose()
        .map_err(|e| EgclError::InvalidImage(format!("cannot retain image file: {e}")))?;
    let mut extents = Vec::new();
    let mut entries = Vec::new();
    for (offset, len, digest, names, soname, needed) in manifest {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| section_error("payload extent overflow"))?;
        if len == 0 || offset < manifest_end || offset % PAGE != 0 || end > section.len() as u64 {
            return Err(section_error("bad payload extent"));
        }
        extents.push((offset, end));
        let source = match (&file, backing) {
            (Some(file), Some((_, base))) => Source::Image {
                file: Arc::clone(file),
                offset: base
                    .checked_add(offset)
                    .ok_or_else(|| section_error("file offset overflow"))?,
            },
            _ => Source::Bytes(section[offset as usize..end as usize].into()),
        };
        entries.push(Entry {
            names,
            soname,
            needed,
            sha256: digest,
            len,
            source,
            #[cfg(egcl_dlopen_embedded)]
            memfd: None,
        });
    }
    extents.sort_unstable();
    if extents.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(section_error("overlapping payloads"));
    }
    Ok(PendingSection(entries))
}

/// Make a restored image's libraries the embedded set.
pub(crate) fn install_section(pending: PendingSection) {
    *REGISTRY.lock().unwrap() = pending.0;
}

// ── Loading through a sealed memfd (glibc, `c-ffi`) ─────────────────

#[cfg(egcl_dlopen_embedded)]
mod memfd_loader {
    use super::*;
    use crate::digest::Sha256;
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

    /// A sealed memfd holding exactly `entry`'s verified bytes.
    fn materialise(entry: &Entry) -> Result<OwnedFd, EgclError> {
        let label = entry.soname.clone().unwrap_or_else(|| {
            entry.names[0]
                .rsplit('/')
                .next()
                .unwrap_or("embedded")
                .to_owned()
        });
        let label = CString::new(label)
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
        fill(&memfd, entry)?;
        let seals = F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SEAL;
        // SAFETY: plain fcntl on a descriptor we own.
        if unsafe { libc::fcntl(memfd.as_raw_fd(), F_ADD_SEALS, seals) } != 0 {
            return Err(os_error("cannot seal an embedded library's memfd"));
        }
        // Verify only after sealing: from here the contents cannot change
        // between the check and the loader mapping them.
        if hash_fd(&memfd, entry.len)? != entry.sha256 {
            return Err(EgclError::FfiError(format!(
                "embedded library '{}' failed its SHA-256 check",
                entry.names[0]
            )));
        }
        Ok(memfd)
    }

    fn fill(memfd: &OwnedFd, entry: &Entry) -> Result<(), EgclError> {
        use std::io::Write;
        // SAFETY: the File borrows the descriptor; ManuallyDrop keeps it open.
        let mut out =
            std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(memfd.as_raw_fd()) });
        match &entry.source {
            Source::Bytes(bytes) => out.write_all(bytes).map_err(|e| {
                EgclError::FfiError(format!("cannot fill embedded library memfd: {e}"))
            }),
            Source::Image { file, offset } => {
                // The kernel copies image pages straight into the memfd; the
                // bytes never pass through a user-space buffer.
                let mut position = *offset as libc::off_t;
                let mut remaining = entry.len as usize;
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
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                        {
                            continue;
                        }
                        // A file system without sendfile support: copy instead.
                        let start = entry.len as usize - remaining;
                        let mut rest = vec![0; remaining];
                        read_exact_at(file, &mut rest, offset + start as u64)?;
                        return out.write_all(&rest).map_err(|e| {
                            EgclError::FfiError(format!("cannot fill embedded library memfd: {e}"))
                        });
                    }
                    if sent == 0 {
                        return Err(EgclError::FfiError(
                            "image ended inside an embedded library".into(),
                        ));
                    }
                    remaining -= sent as usize;
                }
                Ok(())
            }
        }
    }

    fn hash_fd(memfd: &OwnedFd, len: u64) -> Result<[u8; 32], EgclError> {
        let len = usize::try_from(len).map_err(|_| too_large())?;
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
            let mut digest = Sha256::new();
            digest.update(std::slice::from_raw_parts(map as *const u8, len));
            libc::munmap(map, len);
            Ok(digest.finish())
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
        // glibc keeps one object per SONAME. If a copy from elsewhere is
        // already loaded, DT_NEEDED would bind to it rather than to this one.
        if let Some(soname) = &entry.soname {
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
            for needed in &entry.needed {
                let dependency = find(&REGISTRY.lock().unwrap(), needed);
                if let Some(dependency) = dependency {
                    dependencies.push(open_entry(dependency, chain)?);
                }
            }
            let memfd = Arc::new(materialise(&entry)?);
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
    fn a_saved_section_restores_as_references_into_the_image_file() {
        let payloads: [&[u8]; 2] = [b"first library bytes", &[7u8; 5000]];
        let entries: Vec<Entry> = payloads
            .iter()
            .enumerate()
            .map(|(i, bytes)| Entry {
                names: vec![format!("libtest{i}.so.1"), format!("/opt/libtest{i}.so.1")],
                soname: Some(format!("libtest{i}.so.1")),
                needed: if i == 1 {
                    vec!["libtest0.so.1".into()]
                } else {
                    vec![]
                },
                sha256: sha256(bytes),
                len: bytes.len() as u64,
                source: Source::Bytes((*bytes).into()),
                #[cfg(egcl_dlopen_embedded)]
                memfd: None,
            })
            .collect();
        *REGISTRY.lock().unwrap() = entries;
        let section = serialize_section().unwrap();
        *REGISTRY.lock().unwrap() = Vec::new();

        // The section sits at a page-aligned offset inside a larger file.
        let path = std::env::temp_dir().join(format!("egcl-flb-{}", std::process::id()));
        let mut file_bytes = vec![0xAB; 8192];
        file_bytes.extend_from_slice(&section);
        std::fs::write(&path, &file_bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let PendingSection(restored) = parse_section(&section, Some((&file, 8192))).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(restored.len(), 2);
        for (entry, bytes) in restored.iter().zip(payloads) {
            let Source::Image { offset, .. } = &entry.source else {
                panic!("restored payload was copied instead of referenced");
            };
            assert_eq!(offset % PAGE, 0);
            assert_eq!(entry.source.read_all(entry.len).unwrap(), bytes);
        }
        assert_eq!(restored[1].needed, ["libtest0.so.1"]);
        assert_eq!(restored[1].names[1], "/opt/libtest1.so.1");

        // Without a backing file the payloads are copied out of the section.
        let PendingSection(copied) = parse_section(&section, None).unwrap();
        assert!(matches!(copied[0].source, Source::Bytes(_)));

        // A truncated or misaligned manifest rejects the image.
        assert!(parse_section(&section[..40], None).is_err());
        let mut bad = section.clone();
        bad[16] = 1; // first payload offset no longer page aligned
        assert!(parse_section(&bad, None).is_err());
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
