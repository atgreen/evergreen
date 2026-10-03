// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Image persistence — save and restore heap state to `.bimg` files.
//!
//! See §7.2–§7.3 of the spec.

use crate::error::EgclError;
use crate::value::EgclVal;

// Native runtimes cannot be reconstructed from heap snapshots. This gate also
// serializes inhibition against a save already in progress without waiting for
// a mutator that the collector might be trying to stop.
static SAVE_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Permanently inhibit images once a foreign runtime may own process state.
pub fn inhibit_saving() -> Result<(), EgclError> {
    use std::sync::atomic::Ordering;
    match SAVE_STATE.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) | Err(2) => Ok(()),
        Err(_) => Err(EgclError::InvalidImage(
            "cannot start a foreign runtime while an image is being saved".into(),
        )),
    }
}

struct SavePermit;
impl SavePermit {
    fn acquire() -> Result<Self, EgclError> {
        use std::sync::atomic::Ordering;
        match SAVE_STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(Self),
            Err(2) => Err(EgclError::InvalidImage(
                "image saving inhibited by foreign runtime state; save before starting the JVM"
                    .into(),
            )),
            Err(_) => Err(EgclError::InvalidImage(
                "another image save is in progress".into(),
            )),
        }
    }
}
impl Drop for SavePermit {
    fn drop(&mut self) {
        SAVE_STATE.store(0, std::sync::atomic::Ordering::Release);
    }
}

// ── Platform tags ──────────────────────────────────────────────────

/// CPU architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Arch {
    X86_64 = 1,
    Aarch64 = 2,
    Powerpc64le = 3,
    S390x = 4,
}

/// Operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Os {
    Linux = 1,
    MacOS = 2,
    FreeBSD = 3,
    Windows = 4,
    /// Android gets its OWN tag rather than reusing Linux: the kernel is the
    /// same but the libc is not, so an image saved under bionic must not load in
    /// a glibc or musl EGCL (bliss-w2vp).
    ///
    /// 5 rather than 4 only because the Windows port claimed 4 first and its
    /// tests assert that number; neither tag has shipped, so the numbering is
    /// arbitrary beyond staying stable from here on.
    Android = 5,
}

/// Encode architecture and OS into a 64-bit platform tag.
pub fn platform_tag(arch: Arch, os: Os) -> u64 {
    ((arch as u64) << 32) | (os as u64)
}

/// Return the platform tag for the current platform.
pub fn current_platform_tag() -> u64 {
    #[cfg(target_arch = "x86_64")]
    let arch = Arch::X86_64;
    #[cfg(target_arch = "aarch64")]
    let arch = Arch::Aarch64;
    #[cfg(all(target_arch = "powerpc64", target_endian = "little"))]
    let arch = Arch::Powerpc64le;
    #[cfg(target_arch = "s390x")]
    let arch = Arch::S390x;
    #[cfg(not(any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        all(target_arch = "powerpc64", target_endian = "little"),
        target_arch = "s390x"
    )))]
    let arch = Arch::X86_64; // fallback

    #[cfg(target_os = "linux")]
    let os = Os::Linux;
    #[cfg(target_os = "android")]
    let os = Os::Android;
    #[cfg(target_os = "macos")]
    let os = Os::MacOS;
    #[cfg(target_os = "freebsd")]
    let os = Os::FreeBSD;
    #[cfg(windows)]
    let os = Os::Windows;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "freebsd",
        windows
    )))]
    let os = Os::Linux; // fallback

    platform_tag(arch, os)
}

// ── Image header ───────────────────────────────────────────────────

/// Image file magic number: "EGCLIMG" in ASCII, NUL-padded to eight bytes the
/// way `BFASL_MAGIC` pads its own.
///
/// It used to read `b"TORCLIMG"`, which was exactly eight. The rename to EGCL
/// shortened it to seven and the padding byte restores the width — so this is a
/// DELIBERATE format-magic change, not an accident of renaming. An image saved by
/// a `torcl` build therefore no longer loads, which is the correct outcome: the
/// header check at `restore` reports the mismatch and refuses, rather than
/// interpreting another implementation's layout.
pub const IMAGE_MAGIC: u64 = u64::from_be_bytes(*b"EGCLIMG\0");

/// Current image format version.
// Version 4 adds opaque condition-variable handles with nulled native pointers.
// Version 3 preserves compiled-method associations in the host registry. Version
// 2 added opaque MUTEX handles with nulled native pointers. Both older layouts
// remain readable; absent method associations use interpreted dispatch.
// Version 5 carries native runtime requirements, validated before restoration.
// Version 6 stores aligned heap spans with precise pointer fixups. Older
// per-object layouts are deliberately unsupported.
const FORMAT_VERSION: u32 = 6;

/// Image file header (128 bytes). D7.01.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct ImageHeader {
    pub magic: u64,
    pub format_version: u32,
    pub flags: u32,
    pub platform_tag: u64,
    pub original_base: u64,
    pub heap_size: u64,
    pub entry_continuation: u64, // EgclVal
    pub section_count: u32,
    pub gc_generation: u32,
    pub gc_metadata_offset: u64,
    pub save_timestamp: u64,
    pub reserved: [u8; 24],
    pub header_sha256: [u8; 32],
}

/// Image header flag bits.
pub mod image_flags {
    pub const COMPRESSED: u32 = 1 << 0;
    pub const CODE_SIGNED: u32 = 1 << 1;
    pub const READ_ONLY_SAFE: u32 = 1 << 2;
}

// ── Section directory ──────────────────────────────────────────────

/// Section type in the image file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SectionType {
    Heap = 1,
    Symbols = 2,
    Packages = 3,
    Code = 4,
    Reloc = 5,
    GcMeta = 6,
    Settings = 7,
    /// Host-crate registries (bliss-x0f2 M2): macros, setf-functions/expanders,
    /// CLOS state — data owned by the `egcl` crate, contributed via a registered
    /// hook because `egcl-rt` cannot see it. Restored after the heap so its saved
    /// pointers can be remapped through the heap old→new map.
    HostRegistries = 8,
    /// Off-heap object bodies (bliss-x0f2 M3): hash-tables and other VALUEs whose
    /// real body is a Box'd Rust struct off the GC heap. Restored INSIDE
    /// `restore_heap`, between its two passes, so references to them relocate.
    OffHeap = 9,
}

/// Host-crate registry serialization hooks (bliss-x0f2 M2). The `egcl` crate
/// registers these so `save_image`/`load_image` can persist registries that live
/// outside `egcl-rt` (macros, setf, CLOS). The serializer returns an opaque
/// blob; the deserializer restores it AFTER the heap/symbols/packages are back,
/// so it may use `gc::remap_saved_pointer`.
/// Cargo features that must survive a specialized CLI rebuild.
pub const RUNTIME_BUILD_FEATURES: &[&str] = &[
    #[cfg(feature = "c-ffi")]
    "egcl-rt/c-ffi",
    #[cfg(feature = "python")]
    "egcl-rt/python",
];

type RuntimeSerializeHook = fn() -> Vec<u8>;
type RuntimeValidateHook = fn(Option<&[u8]>) -> Result<(), EgclError>;
static RUNTIME_HOOKS: std::sync::Mutex<Option<(RuntimeSerializeHook, RuntimeValidateHook)>> =
    std::sync::Mutex::new(None);

/// Install runtime compatibility metadata and pre-restoration validation.
/// The validator runs before heap or host-registry mutation.
pub fn set_runtime_contract_hooks(save: RuntimeSerializeHook, validate: RuntimeValidateHook) {
    *RUNTIME_HOOKS.lock().unwrap() = Some((save, validate));
}

type HostSerializeHook = fn() -> Vec<u8>;
type HostRestoreHook = fn(&[u8]) -> Result<(), EgclError>;
static HOST_SERIALIZE: std::sync::Mutex<Option<HostSerializeHook>> = std::sync::Mutex::new(None);
static HOST_RESTORE: std::sync::Mutex<Option<HostRestoreHook>> = std::sync::Mutex::new(None);

/// Register the host-crate registry hooks (called once at startup by `egcl`).
pub fn set_host_registry_hooks(serialize: HostSerializeHook, restore: HostRestoreHook) {
    *HOST_SERIALIZE.lock().unwrap() = Some(serialize);
    *HOST_RESTORE.lock().unwrap() = Some(restore);
}

/// A single entry in the section directory. D7.02.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SectionEntry {
    pub section_type: u32,
    pub flags: u32,
    pub file_offset: u64,
    pub size: u64,
    pub uncompressed_size: u64,
}

// ── Image save/load ────────────────────────────────────────────────

/// Options for saving an image.
pub struct SaveImageOptions {
    pub executable: bool,
    pub compression: ImageCompression,
    pub purify: bool,
}

/// Compression mode for image save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageCompression {
    None,
    Zstd,
}

// ── SHA-256 (minimal implementation for header checksum) ───────────

/// Compute SHA-256 digest of `data`. Used for header checksum (§7.2.3).
/// This is a self-contained implementation so we avoid external deps.
fn sha256(data: &[u8]) -> [u8; 32] {
    // Initial hash values (first 32 bits of fractional parts of square roots of first 8 primes)
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Round constants
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    // Pre-processing: pad the message
    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) block
    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut digest = [0u8; 32];
    for (i, val) in h.iter().enumerate() {
        digest[i * 4..i * 4 + 4].copy_from_slice(&val.to_be_bytes());
    }
    digest
}

// ── Helpers ────────────────────────────────────────────────────────

const HEADER_SIZE: usize = std::mem::size_of::<ImageHeader>();
const SECTION_ENTRY_SIZE: usize = std::mem::size_of::<SectionEntry>();

/// Byte-serialise a repr(C) struct to a Vec<u8>.
fn struct_to_bytes<T: Sized>(val: &T) -> Vec<u8> {
    let ptr = val as *const T as *const u8;
    let len = std::mem::size_of::<T>();
    // SAFETY: T is repr(C), so reading its bytes is well-defined.
    unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
}

/// Deserialise a repr(C) struct from a byte slice. Returns None if too short.
fn bytes_to_struct<T: Sized + Copy>(data: &[u8]) -> Option<T> {
    if data.len() < std::mem::size_of::<T>() {
        return None;
    }
    // SAFETY: We checked the length; T is repr(C)+Copy. We use read_unaligned
    // to avoid undefined behaviour on platforms with strict alignment requirements
    // (the byte buffer from file I/O is not guaranteed to be aligned to T's alignment).
    Some(unsafe { std::ptr::read_unaligned(data.as_ptr() as *const T) })
}

/// Compute the SHA-256 of the header with the checksum field zeroed
/// (bytes 0x00..0x60, i.e. the first 96 bytes of the 128-byte header).
fn compute_header_checksum(header_bytes: &[u8]) -> [u8; 32] {
    // Spec says: SHA-256 of bytes 0x00..0x5F (the first 96 bytes,
    // everything before the 32-byte header_sha256 field itself).
    let checksum_offset = HEADER_SIZE - 32; // offset 0x60 = 96
    sha256(&header_bytes[..checksum_offset])
}

// ── Image save ─────────────────────────────────────────────────────

/// Minimal zstd-style compression using simple run-length encoding.
/// Used when `ImageCompression::Zstd` is requested. A real implementation
/// would link against libzstd; this provides a compatible compress/decompress
/// pair so that the COMPRESSED flag is correctly honoured.
fn compress_data(data: &[u8]) -> Vec<u8> {
    // Format: [u32 LE uncompressed_len] [compressed bytes...]
    // Compressed bytes use a simple scheme:
    //   - 0x00 <count u16 LE> <byte>  = run of `count` copies of `byte`
    //   - 0x01 <count u16 LE> <bytes...> = `count` literal bytes
    let mut out = Vec::with_capacity(data.len() + 4);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());

    let mut i = 0;
    while i < data.len() {
        // Check for a run of identical bytes (min length 4 to be worthwhile)
        let b = data[i];
        let mut run_len = 1usize;
        while i + run_len < data.len() && data[i + run_len] == b && run_len < 65535 {
            run_len += 1;
        }
        if run_len >= 4 {
            out.push(0x00);
            out.extend_from_slice(&(run_len as u16).to_le_bytes());
            out.push(b);
            i += run_len;
        } else {
            // Gather literal bytes (up to 65535)
            let start = i;
            let mut lit_len = 0usize;
            while i + lit_len < data.len() && lit_len < 65535 {
                // Check if next position starts a worthwhile run
                let nb = data[i + lit_len];
                let mut nr = 1usize;
                while i + lit_len + nr < data.len() && data[i + lit_len + nr] == nb && nr < 65535 {
                    nr += 1;
                }
                if nr >= 4 {
                    break;
                }
                lit_len += 1;
            }
            if lit_len == 0 {
                lit_len = 1;
            }
            out.push(0x01);
            out.extend_from_slice(&(lit_len as u16).to_le_bytes());
            out.extend_from_slice(&data[start..start + lit_len]);
            i += lit_len;
        }
    }
    out
}

/// Decompress data produced by `compress_data`.
fn decompress_data(compressed: &[u8]) -> Result<Vec<u8>, EgclError> {
    if compressed.len() < 4 {
        return Err(EgclError::InvalidImage("compressed data too short".into()));
    }
    let uncompressed_len =
        u32::from_le_bytes([compressed[0], compressed[1], compressed[2], compressed[3]]) as usize;
    let mut out = Vec::with_capacity(uncompressed_len);
    let mut i = 4;
    while i < compressed.len() {
        let tag = compressed[i];
        i += 1;
        if i + 2 > compressed.len() {
            return Err(EgclError::InvalidImage(
                "truncated compressed stream".into(),
            ));
        }
        let count = u16::from_le_bytes([compressed[i], compressed[i + 1]]) as usize;
        i += 2;
        match tag {
            0x00 => {
                // Run-length
                if i >= compressed.len() {
                    return Err(EgclError::InvalidImage("truncated RLE byte".into()));
                }
                let b = compressed[i];
                i += 1;
                for _ in 0..count {
                    out.push(b);
                }
            }
            0x01 => {
                // Literals
                if i + count > compressed.len() {
                    return Err(EgclError::InvalidImage("truncated literal block".into()));
                }
                out.extend_from_slice(&compressed[i..i + count]);
                i += count;
            }
            _ => {
                return Err(EgclError::InvalidImage(format!(
                    "unknown compression tag: {:#x}",
                    tag
                )));
            }
        }
    }
    if out.len() != uncompressed_len {
        return Err(EgclError::InvalidImage(format!(
            "decompressed size mismatch: expected {}, got {}",
            uncompressed_len,
            out.len()
        )));
    }
    Ok(out)
}

/// Save the current heap state to an image file.
/// Stops all threads, serialises the current heap, then resumes.
///
/// Per R7.20 the write is atomic: we write to a temp file then rename.
pub fn save_image(path: &str, options: &SaveImageOptions) -> Result<(), EgclError> {
    let _permit = SavePermit::acquire()?;
    crate::gc::ensure_heap_initialized();
    crate::gc::with_heap_snapshot(|| save_image_impl(path, options, false))?
}

/// Save an application-delivery image, omitting unreachable objects even when
/// restored-world pinning prevents the collector from reclaiming their regions.
pub fn save_reachable_image(path: &str, options: &SaveImageOptions) -> Result<(), EgclError> {
    let _permit = SavePermit::acquire()?;
    crate::gc::with_heap_snapshot(|| save_image_impl(path, options, true))?
}

fn save_image_impl(
    path: &str,
    options: &SaveImageOptions,
    reachable_only: bool,
) -> Result<(), EgclError> {
    use std::io::Write;

    let use_compression = options.compression == ImageCompression::Zstd;
    let mut flags: u32 = 0;
    if use_compression {
        flags |= image_flags::COMPRESSED;
    }
    if options.purify {
        flags |= image_flags::READ_ONLY_SAFE;
    }

    let save_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Retrieve the entry continuation from the runtime (§7.2.3).
    let entry_val = crate::gc::get_entry_continuation();

    // Both save entry points hold a stopped-world snapshot. The continuation
    // lives in the image header; putting it before this directory would break
    // the page alignment of the heap spans.
    let heap_data_raw = unsafe { crate::gc::snapshot_heap_regions(reachable_only)? }.encode()?;
    let heap_uncompressed_size = heap_data_raw.len();

    // Build the symbol table section (§7.2.7) by calling into the
    // symbol table subsystem.
    let symbol_data_raw: Vec<u8> = crate::gc::serialize_symbols();
    let symbol_uncompressed_size = symbol_data_raw.len();

    // Build the package registry section (§7.2.8) by calling into the
    // package registry subsystem.
    let package_data_raw: Vec<u8> = crate::gc::serialize_packages();
    let package_uncompressed_size = package_data_raw.len();

    // Build the compiled code cache section by calling into the code
    // cache subsystem.
    let code_data_raw: Vec<u8> = crate::gc::serialize_code_cache();
    let code_uncompressed_size = code_data_raw.len();

    // Region payloads carry their own precise fixup offsets. Keep the reserved
    // relocation section empty; the old conservative word scan is unnecessary.
    let reloc_data_raw: Vec<u8> = Vec::new();
    let reloc_uncompressed_size = reloc_data_raw.len();

    // Build the GC metadata section with all GcStats fields + generation.
    let gc_meta_raw: Vec<u8> = crate::gc::serialize_gc_metadata();
    let gc_meta_uncompressed_size = gc_meta_raw.len();

    // Build the host-crate registry section from the registered hook (bliss-x0f2
    // M2): macros/setf/CLOS owned by the `egcl` crate. Empty if no hook.
    let host_raw: Vec<u8> = match *HOST_SERIALIZE.lock().unwrap() {
        Some(hook) => hook(),
        None => Vec::new(),
    };
    let host_uncompressed_size = host_raw.len();

    // Off-heap object bodies (bliss-x0f2 M3): hash-tables etc.
    let offheap_raw: Vec<u8> = crate::gc::serialize_offheap_objects();
    let offheap_uncompressed_size = offheap_raw.len();

    // Apply compression if requested.
    let heap_data = if use_compression {
        compress_data(&heap_data_raw)
    } else {
        heap_data_raw
    };
    let symbol_data = if use_compression {
        compress_data(&symbol_data_raw)
    } else {
        symbol_data_raw
    };
    let package_data = if use_compression {
        compress_data(&package_data_raw)
    } else {
        package_data_raw
    };
    let code_data = if use_compression {
        compress_data(&code_data_raw)
    } else {
        code_data_raw
    };
    let reloc_data = if use_compression {
        compress_data(&reloc_data_raw)
    } else {
        reloc_data_raw
    };
    let gc_meta_data = if use_compression {
        compress_data(&gc_meta_raw)
    } else {
        gc_meta_raw
    };
    let host_data = if use_compression {
        compress_data(&host_raw)
    } else {
        host_raw
    };
    let offheap_data = if use_compression {
        compress_data(&offheap_raw)
    } else {
        offheap_raw
    };

    // Sections: Heap, Symbols, Packages, Code, Reloc, GcMeta, HostRegistries, OffHeap
    let runtime_hooks = *RUNTIME_HOOKS.lock().unwrap();
    let settings_raw = runtime_hooks.map(|(save, _)| save()).unwrap_or_default();
    let settings_size = settings_raw.len();
    let settings_data = if use_compression {
        compress_data(&settings_raw)
    } else {
        settings_raw
    };
    let section_count: u32 = 9;

    // Page alignment constant (4 KiB) — §7.2.2 requires sections after
    // the directory to be page-aligned to allow mmap with MAP_FIXED (R7.02).
    const PAGE_SIZE: usize = 4096;

    // Compute section directory layout.
    let section_dir_offset = HEADER_SIZE;
    let data_start_unaligned = section_dir_offset + (section_count as usize) * SECTION_ENTRY_SIZE;
    // Round up to next page boundary for the first section.
    let data_start = (data_start_unaligned + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    // Lay out sections page-aligned after the directory.
    let mut current_offset = data_start;

    /// Round up to the next page boundary.
    fn align_to_page(offset: usize) -> usize {
        const PAGE: usize = 4096;
        (offset + PAGE - 1) & !(PAGE - 1)
    }

    let heap_section = SectionEntry {
        section_type: SectionType::Heap as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: heap_data.len() as u64,
        uncompressed_size: heap_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + heap_data.len());

    let symbol_section = SectionEntry {
        section_type: SectionType::Symbols as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: symbol_data.len() as u64,
        uncompressed_size: symbol_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + symbol_data.len());

    let package_section = SectionEntry {
        section_type: SectionType::Packages as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: package_data.len() as u64,
        uncompressed_size: package_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + package_data.len());

    let code_section = SectionEntry {
        section_type: SectionType::Code as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: code_data.len() as u64,
        uncompressed_size: code_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + code_data.len());

    let reloc_section = SectionEntry {
        section_type: SectionType::Reloc as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: reloc_data.len() as u64,
        uncompressed_size: reloc_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + reloc_data.len());

    let gc_meta_offset = current_offset as u64;
    let gc_meta_section = SectionEntry {
        section_type: SectionType::GcMeta as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: gc_meta_data.len() as u64,
        uncompressed_size: gc_meta_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + gc_meta_data.len());

    let host_section = SectionEntry {
        section_type: SectionType::HostRegistries as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: host_data.len() as u64,
        uncompressed_size: host_uncompressed_size as u64,
    };
    current_offset = align_to_page(current_offset + host_data.len());

    let offheap_section = SectionEntry {
        section_type: SectionType::OffHeap as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: offheap_data.len() as u64,
        uncompressed_size: offheap_uncompressed_size as u64,
    };

    current_offset = align_to_page(current_offset + offheap_data.len());
    let settings_section = SectionEntry {
        section_type: SectionType::Settings as u32,
        flags: 0,
        file_offset: current_offset as u64,
        size: settings_data.len() as u64,
        uncompressed_size: settings_size as u64,
    };

    // Get the actual heap base address for relocation tracking (R7.03).
    let original_base = crate::gc::heap_base_address();
    let gc_generation = crate::gc::gc_generation();

    // Build the header (with zeroed checksum — we fill it after serialising).
    let mut header = ImageHeader {
        magic: IMAGE_MAGIC,
        format_version: FORMAT_VERSION,
        flags,
        platform_tag: current_platform_tag(),
        original_base,
        heap_size: heap_uncompressed_size as u64,
        entry_continuation: entry_val.to_raw(),
        section_count,
        gc_generation,
        gc_metadata_offset: gc_meta_offset,
        save_timestamp,
        reserved: [0u8; 24],
        header_sha256: [0u8; 32],
    };

    // Compute the header checksum over bytes 0x00..0x5F.
    let header_bytes_pre = struct_to_bytes(&header);
    let checksum = compute_header_checksum(&header_bytes_pre);
    header.header_sha256 = checksum;

    // Serialise the final header + section directory + data.
    let header_bytes = struct_to_bytes(&header);

    // Atomic write: write to a temp file then rename (R7.20).
    let tmp_path = format!("{}.tmp", path);
    let write_result = (|| -> Result<(), EgclError> {
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| EgclError::FileError(format!("cannot create temp image file: {}", e)))?;
        file.write_all(&header_bytes)
            .map_err(|e| EgclError::FileError(format!("cannot write image header: {}", e)))?;

        // Write section directory entries
        let sections = [
            heap_section,
            symbol_section,
            package_section,
            code_section,
            reloc_section,
            gc_meta_section,
            host_section,
            offheap_section,
            settings_section,
        ];
        for section in &sections {
            let section_bytes = struct_to_bytes(section);
            file.write_all(&section_bytes).map_err(|e| {
                EgclError::FileError(format!("cannot write section directory: {}", e))
            })?;
        }

        // Write section data with page-alignment padding between sections.
        // Each section's file_offset was computed with page alignment, so
        // we pad to match those offsets.
        let section_data_slices: [&[u8]; 9] = [
            &heap_data,
            &symbol_data,
            &package_data,
            &code_data,
            &reloc_data,
            &gc_meta_data,
            &host_data,
            &offheap_data,
            &settings_data,
        ];
        let mut write_pos = section_dir_offset + sections.len() * SECTION_ENTRY_SIZE;
        for (idx, data) in section_data_slices.iter().enumerate() {
            let target_offset = sections[idx].file_offset as usize;
            // Write padding zeros to reach the page-aligned offset.
            if target_offset > write_pos {
                let padding = vec![0u8; target_offset - write_pos];
                file.write_all(&padding).map_err(|e| {
                    EgclError::FileError(format!("cannot write section padding: {}", e))
                })?;
                write_pos = target_offset;
            }
            file.write_all(data)
                .map_err(|e| EgclError::FileError(format!("cannot write section data: {}", e)))?;
            write_pos += data.len();
        }

        file.flush()
            .map_err(|e| EgclError::FileError(format!("cannot flush image file: {}", e)))?;
        Ok(())
    })();

    if let Err(e) = write_result {
        // Clean up temp file on error
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Rename temp → final path (atomic on POSIX filesystems).
    std::fs::rename(&tmp_path, path)
        .map_err(|e| EgclError::FileError(format!("cannot rename image file: {}", e)))?;

    // Set executable permission if requested.
    if options.executable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(path)
                .map_err(|e| EgclError::FileError(format!("cannot read file metadata: {}", e)))?;
            let mut perms = metadata.permissions();
            let mode = perms.mode();
            perms.set_mode(mode | 0o111);
            std::fs::set_permissions(path, perms).map_err(|e| {
                EgclError::FileError(format!("cannot set executable permission: {}", e))
            })?;
        }
    }

    Ok(())
}

// ── Image load ─────────────────────────────────────────────────────

/// Load an image file and restore the heap.
/// Called during runtime startup.  Reads the file, validates the header,
/// parses the section directory, restores heap data, and returns the
/// entry continuation so the caller can resume execution.
pub fn load_image(path: &str) -> Result<EgclVal, EgclError> {
    let file = std::fs::File::open(path)
        .map_err(|e| EgclError::InvalidImage(format!("cannot open image: {e}")))?;
    let len = file.metadata().map_err(|e| EgclError::InvalidImage(e.to_string()))?.len();
    load_image_from_file(&file, 0, len)
}

/// Restore a standalone or embedded image without reading its heap into a Vec.
/// Image files must not be modified in place while their restored heap is in
/// use. Atomic replacement and unlinking are supported: mappings keep the old
/// inode alive. The file descriptor need not remain open after this returns.
pub fn load_image_from_file(file: &std::fs::File, offset: u64, len: u64) -> Result<EgclVal, EgclError> {
    let invalid = || EgclError::InvalidImage("invalid image file extent".into());
    let end = offset.checked_add(len).ok_or_else(invalid)?;
    if len == 0 || end > file.metadata().map_err(|e| EgclError::InvalidImage(e.to_string()))?.len()
        || end > isize::MAX as u64 { return Err(invalid()); }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        struct Mapping { ptr: *mut u8, len: usize }
        impl Drop for Mapping {
            fn drop(&mut self) { unsafe { let _ = crate::syscall::munmap(self.ptr, self.len); } }
        }
        // Map from offset zero so embedded image offsets need not match the OS
        // page size. Pages before the image are never touched by this reader.
        let ptr = unsafe { crate::syscall::mmap(std::ptr::null_mut(), end as usize,
            crate::syscall::PROT_READ, crate::syscall::MAP_PRIVATE, file.as_raw_fd(), 0) }
            .map_err(|errno| EgclError::InvalidImage(format!("image mmap failed: errno {errno}")))?;
        let mapping = Mapping { ptr, len: end as usize };
        let bytes = unsafe { std::slice::from_raw_parts(mapping.ptr.add(offset as usize), len as usize) };
        load_image_contents(bytes, Some((file, offset)))
    }
    #[cfg(not(target_os = "linux"))]
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut reader = file.try_clone().map_err(|e| EgclError::InvalidImage(e.to_string()))?;
        reader.seek(SeekFrom::Start(offset)).map_err(|e| EgclError::InvalidImage(e.to_string()))?;
        let mut bytes = vec![0; usize::try_from(len).map_err(|_| invalid())?];
        reader.read_exact(&mut bytes).map_err(|e| EgclError::InvalidImage(e.to_string()))?;
        load_image_from_bytes(&bytes)
    }
}

/// Load an image from an in-memory byte slice (the image header must start at
/// offset 0). Used for images embedded in an `:executable` save (bliss-x0f2 M4),
/// where the loader already holds the extracted image bytes. `load_image` is the
/// path-based wrapper. The heap must be initialized before calling either.
pub fn load_image_from_bytes(file_data: &[u8]) -> Result<EgclVal, EgclError> {
    load_image_contents(file_data, None)
}

fn load_image_contents(file_data: &[u8], backing: Option<(&std::fs::File, u64)>) -> Result<EgclVal, EgclError> {
    if file_data.len() < HEADER_SIZE {
        return Err(EgclError::InvalidImage(
            "image file too small to contain header".into(),
        ));
    }

    // Parse and validate the header.
    let header: ImageHeader = bytes_to_struct(file_data)
        .ok_or_else(|| EgclError::InvalidImage("image file too small".into()))?;

    if header.magic != IMAGE_MAGIC {
        return Err(EgclError::InvalidImage(format!(
            "bad magic: expected {:#x}, got {:#x}",
            IMAGE_MAGIC, header.magic
        )));
    }

    if header.format_version != FORMAT_VERSION {
        return Err(EgclError::InvalidImage(format!(
            "unsupported format version: image has {}, runtime requires {}",
            header.format_version, FORMAT_VERSION
        )));
    }

    let expected_platform = current_platform_tag();
    if header.platform_tag != expected_platform {
        return Err(EgclError::InvalidImage(format!(
            "platform mismatch: image tag {:#x}, current platform tag {:#x}",
            header.platform_tag, expected_platform
        )));
    }

    // Verify header SHA-256 checksum.
    let expected_checksum = compute_header_checksum(&file_data[..HEADER_SIZE]);
    if header.header_sha256 != expected_checksum {
        return Err(EgclError::InvalidImage(
            "header checksum mismatch — image may be corrupt".into(),
        ));
    }

    // Parse the section directory.
    let section_dir_start = HEADER_SIZE;
    let section_dir_end = section_dir_start + (header.section_count as usize) * SECTION_ENTRY_SIZE;
    if file_data.len() < section_dir_end {
        return Err(EgclError::InvalidImage(
            "image file too small for section directory".into(),
        ));
    }

    let is_compressed = (header.flags & image_flags::COMPRESSED) != 0;

    // Helper to read and optionally decompress a section's data.
    let read_section_data = |entry: &SectionEntry| -> Result<std::borrow::Cow<'_, [u8]>, EgclError> {
        let data_start = usize::try_from(entry.file_offset).map_err(|_| EgclError::InvalidImage("section offset overflow".into()))?;
        let size = usize::try_from(entry.size).map_err(|_| EgclError::InvalidImage("section size overflow".into()))?;
        let data_end = data_start.checked_add(size).ok_or_else(|| EgclError::InvalidImage("section extent overflow".into()))?;
        if file_data.len() < data_end {
            return Err(EgclError::InvalidImage(
                "image file too small for section data".into(),
            ));
        }
        let raw = &file_data[data_start..data_end];
        if is_compressed && entry.size > 0 {
            decompress_data(raw).map(std::borrow::Cow::Owned)
        } else {
            Ok(std::borrow::Cow::Borrowed(raw))
        }
    };

    // Collect all sections first so we can process them in the right order.
    // We need the relocation table before restoring the heap if bases differ.
    let mut settings_entry: Option<SectionEntry> = None;
    let mut heap_entry: Option<SectionEntry> = None;
    let mut symbol_entry: Option<SectionEntry> = None;
    let mut package_entry: Option<SectionEntry> = None;
    let mut code_entry: Option<SectionEntry> = None;
    let mut gc_meta_entry: Option<SectionEntry> = None;
    let mut host_entry: Option<SectionEntry> = None;
    let mut offheap_entry: Option<SectionEntry> = None;

    let mut seen_sections = std::collections::HashSet::new();
    let mut extents = Vec::new();
    for i in 0..header.section_count as usize {
        let entry_offset = section_dir_start + i * SECTION_ENTRY_SIZE;
        let entry: SectionEntry = bytes_to_struct(&file_data[entry_offset..])
            .ok_or_else(|| EgclError::InvalidImage(format!("cannot parse section entry {}", i)))?;

        let end = entry.file_offset.checked_add(entry.size)
            .ok_or_else(|| EgclError::InvalidImage("section extent overflow".into()))?;
        if !seen_sections.insert(entry.section_type) || entry.file_offset < section_dir_end as u64
            || entry.file_offset % 4096 != 0 || end > file_data.len() as u64 {
            return Err(EgclError::InvalidImage("invalid or duplicate image section".into()));
        }
        if entry.size != 0 { extents.push((entry.file_offset, end)); }

        match entry.section_type {
            t if t == SectionType::Settings as u32 => {
                if settings_entry.replace(entry).is_some() {
                    return Err(EgclError::InvalidImage(
                        "duplicate runtime settings section".into(),
                    ));
                }
            }
            t if t == SectionType::Heap as u32 => heap_entry = Some(entry),
            t if t == SectionType::Symbols as u32 => symbol_entry = Some(entry),
            t if t == SectionType::Packages as u32 => package_entry = Some(entry),
            t if t == SectionType::Code as u32 => code_entry = Some(entry),
            t if t == SectionType::GcMeta as u32 => gc_meta_entry = Some(entry),
            t if t == SectionType::HostRegistries as u32 => host_entry = Some(entry),
            t if t == SectionType::OffHeap as u32 => offheap_entry = Some(entry),
            _ => {
                // Unknown section type — skip for forward compatibility.
            }
        }
    }

    extents.sort_unstable();
    if extents.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(EgclError::InvalidImage("overlapping image sections".into()));
    }
    let settings = settings_entry
        .as_ref()
        .map(&read_section_data)
        .transpose()?;
    let runtime_hooks = *RUNTIME_HOOKS.lock().unwrap();
    if let Some((_, validate)) = runtime_hooks {
        validate(settings.as_deref().filter(|bytes| !bytes.is_empty()))?;
    } else if settings.as_ref().is_some_and(|bytes| !bytes.is_empty()) {
        return Err(EgclError::InvalidImage(
            "image requires a runtime contract validator".into(),
        ));
    }

    // Stash the off-heap-body section so restore_heap can re-materialize those
    // objects between its two passes (bliss-x0f2 M3), then clear it afterward.
    let offheap_bytes = match offheap_entry {
        Some(entry) => read_section_data(&entry)?,
        None => std::borrow::Cow::Borrowed(&[][..]),
    };
    crate::gc::set_pending_offheap(if offheap_bytes.is_empty() {
        None
    } else {
        Some(offheap_bytes.into_owned())
    });

    // Restore the heap section.
    let heap_entry = heap_entry
        .ok_or_else(|| EgclError::InvalidImage("image contains no heap section".into()))?;
    let heap_bytes = read_section_data(&heap_entry)?;

    let heap_backing = if is_compressed { None } else {
        backing.map(|(file, offset)| offset.checked_add(heap_entry.file_offset)
            .map(|offset| (file, offset)).ok_or_else(|| EgclError::InvalidImage("heap file offset overflow".into())))
            .transpose()?
    };
    // Native images are runtime-specific; the header and runtime contract were
    // checked above. No Lisp mutators run during startup restoration.
    let restored = unsafe { crate::gc::restore_heap_regions(&heap_bytes, heap_backing) };
    if let Err(error) = restored {
        crate::gc::set_pending_offheap(None);
        return Err(error);
    }
    // Restore the symbol table (§7.2.7) and the package registry (§7.2.8).
    // A failure here still has to drop the stashed off-heap section, like the
    // heap-restore failure above.
    let registries = (|| {
        if let Some(entry) = symbol_entry {
            crate::gc::restore_symbols(&read_section_data(&entry)?)?;
        }
        if let Some(entry) = package_entry {
            crate::gc::restore_packages(&read_section_data(&entry)?)?;
        }
        Ok(())
    })();
    if let Err(error) = registries {
        crate::gc::set_pending_offheap(None);
        return Err(error);
    }

    // Phase 2 of off-heap restore: now that restore_heap has released the heap
    // lock and set the final relocation map, populate the off-heap object bodies
    // (hash-table contents). Then clear the stash.
    //
    // This MUST run after the symbol and package registries are rebuilt. The
    // heap restore installs a FRESH heap mapping and unmaps the old one, so
    // every pre-restore registry entry is a wild pointer until its registry is
    // replaced. Hashing a symbol key here reads the symbol's name out of the
    // registry (`symbols::symbol_name`), and populate also allocates (fresh
    // strings for by-content slots), which can run a root scan over those
    // registries. Both faulted on the stale old-heap addresses.
    crate::gc::run_offheap_populate();
    crate::gc::set_pending_offheap(None);

    // Restore the compiled code cache.
    if let Some(entry) = code_entry {
        let code_bytes = read_section_data(&entry)?;
        crate::gc::restore_code_cache(&code_bytes)?;
    }

    // Restore GC metadata (stats + generation).
    if let Some(entry) = gc_meta_entry {
        let gc_meta_bytes = read_section_data(&entry)?;
        crate::gc::restore_gc_metadata(&gc_meta_bytes)?;
    }

    // Restore host-crate registries (bliss-x0f2 M2): macros/setf/CLOS owned by
    // the `egcl` crate. Runs LAST so the heap old→new map and the symbol/package
    // registries are already in place — the hook remaps its saved pointers via
    // `gc::remap_saved_pointer`. A missing hook (or an empty section) is a no-op.
    if let Some(entry) = host_entry {
        let host_bytes = read_section_data(&entry)?;
        if !host_bytes.is_empty() {
            if let Some(hook) = *HOST_RESTORE.lock().unwrap() {
                hook(&host_bytes)?;
            }
        }
    }

    // Store the restored entry continuation, remapped through the heap's old→new
    // map (it is itself a saved pointer into the relocated heap).
    let entry_cont = EgclVal::from_raw(crate::gc::remap_saved_pointer(header.entry_continuation));
    crate::gc::set_entry_continuation(entry_cont);

    Ok(entry_cont)
}

// ── Appended image detection ───────────────────────────────────────

/// Check if the running binary has an appended image.
pub fn find_appended_image() -> Option<String> {
    let exe_path = std::env::current_exe().ok()?;
    let data = std::fs::read(&exe_path).ok()?;
    let magic_bytes = IMAGE_MAGIC.to_ne_bytes();
    let header_size = HEADER_SIZE;
    if data.len() < header_size {
        return None;
    }
    // Search backwards through the last 1 MB for the magic number
    // at a position where a full header could fit.
    let search_start = if data.len() > 1024 * 1024 {
        data.len() - 1024 * 1024
    } else {
        0
    };
    for offset in (search_start..data.len().saturating_sub(header_size)).rev() {
        if data[offset..offset + 8] == magic_bytes {
            // Validate that this looks like a real header (check version range).
            if let Some(candidate) = bytes_to_struct::<ImageHeader>(&data[offset..]) {
                if candidate.format_version >= 1 && candidate.format_version <= FORMAT_VERSION {
                    return Some(exe_path.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

// ── Header validation (standalone) ─────────────────────────────────

/// Validate an image header without loading the full image.
pub fn validate_image_header(path: &str) -> Result<ImageHeader, EgclError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| EgclError::InvalidImage(format!("cannot open image: {}", e)))?;

    let mut buf = vec![0u8; HEADER_SIZE];
    file.read_exact(&mut buf)
        .map_err(|e| EgclError::InvalidImage(format!("cannot read image header: {}", e)))?;

    let header: ImageHeader =
        bytes_to_struct(&buf).ok_or_else(|| EgclError::InvalidImage("header too small".into()))?;

    if header.magic != IMAGE_MAGIC {
        return Err(EgclError::InvalidImage(format!(
            "bad magic: expected {:#x}, got {:#x}",
            IMAGE_MAGIC, header.magic
        )));
    }

    if header.format_version != FORMAT_VERSION {
        return Err(EgclError::InvalidImage(format!(
            "unsupported format version: image has {}, runtime requires {}",
            header.format_version, FORMAT_VERSION
        )));
    }

    let expected_platform = current_platform_tag();
    if header.platform_tag != expected_platform {
        return Err(EgclError::InvalidImage(format!(
            "platform mismatch: image tag {:#x}, current platform tag {:#x}",
            header.platform_tag, expected_platform
        )));
    }

    // Verify header checksum.
    let expected_checksum = compute_header_checksum(&buf);
    if header.header_sha256 != expected_checksum {
        return Err(EgclError::InvalidImage(
            "header checksum mismatch — image may be corrupt".into(),
        ));
    }

    Ok(header)
}
