//! Image persistence — save and restore heap state to `.bimg` files.
//!
//! See §7.2–§7.3 of the spec.

use crate::error::BlissError;
use crate::value::BlissVal;

// ── Platform tags ──────────────────────────────────────────────────

/// CPU architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Arch {
    X86_64 = 1,
    Aarch64 = 2,
}

/// Operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Os {
    Linux = 1,
    MacOS = 2,
    FreeBSD = 3,
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
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let arch = Arch::X86_64; // fallback

    #[cfg(target_os = "linux")]
    let os = Os::Linux;
    #[cfg(target_os = "macos")]
    let os = Os::MacOS;
    #[cfg(target_os = "freebsd")]
    let os = Os::FreeBSD;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
    let os = Os::Linux; // fallback

    platform_tag(arch, os)
}

// ── Image header ───────────────────────────────────────────────────

/// Image file magic number: "BLISSIMG" in ASCII.
pub const IMAGE_MAGIC: u64 = 0x424C4953_53494D47;

/// Current image format version.
const FORMAT_VERSION: u32 = 1;

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
    pub entry_continuation: u64, // BlissVal
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
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    // Round constants
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
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
    // SAFETY: We checked the length; T is repr(C)+Copy so any bit pattern is valid for our types.
    Some(unsafe { std::ptr::read(data.as_ptr() as *const T) })
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

/// Save the current heap state to an image file.
/// Triggers a full GC, stops all threads, serialises, then resumes.
///
/// Per R7.20 the write is atomic: we write to a temp file then rename.
pub fn save_image(path: &str, options: &SaveImageOptions) -> Result<(), BlissError> {
    use std::io::Write;

    let mut flags = if options.compression == ImageCompression::Zstd {
        image_flags::COMPRESSED
    } else {
        0
    };

    if options.purify {
        flags |= image_flags::READ_ONLY_SAFE;
    }

    let save_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Serialize the heap.  In a full runtime this would walk all live
    // objects after a full GC.  At this stage we serialize the root
    // continuation value (NIL by default) and any values reachable from
    // the global heap.  The heap section is a flat byte buffer whose
    // first 8 bytes encode the entry continuation.
    let entry_val = crate::value::NIL;
    let mut heap_data: Vec<u8> = Vec::new();
    // Write the entry continuation as the first 8 bytes of the heap section
    heap_data.extend_from_slice(&entry_val.to_raw().to_ne_bytes());

    // Compute section directory layout.
    // Header is at offset 0; section directory follows immediately;
    // then the heap data section starts after the directory.
    let section_dir_offset = HEADER_SIZE;
    let section_count: u32 = 1; // Only the heap section for now
    let data_offset = section_dir_offset + (section_count as usize) * SECTION_ENTRY_SIZE;

    let heap_section = SectionEntry {
        section_type: SectionType::Heap as u32,
        flags: 0,
        file_offset: data_offset as u64,
        size: heap_data.len() as u64,
        uncompressed_size: heap_data.len() as u64,
    };

    // Build the header (with zeroed checksum — we fill it after serialising).
    let mut header = ImageHeader {
        magic: IMAGE_MAGIC,
        format_version: FORMAT_VERSION,
        flags,
        platform_tag: current_platform_tag(),
        original_base: 0,
        heap_size: heap_data.len() as u64,
        entry_continuation: entry_val.to_raw(),
        section_count,
        gc_generation: 0,
        gc_metadata_offset: 0,
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
    let section_bytes = struct_to_bytes(&heap_section);

    // Atomic write: write to a temp file then rename (R7.20).
    let tmp_path = format!("{}.tmp", path);
    let write_result = (|| -> Result<(), BlissError> {
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| BlissError::FileError(format!("cannot create temp image file: {}", e)))?;
        file.write_all(&header_bytes)
            .map_err(|e| BlissError::FileError(format!("cannot write image header: {}", e)))?;
        file.write_all(&section_bytes)
            .map_err(|e| BlissError::FileError(format!("cannot write section directory: {}", e)))?;
        file.write_all(&heap_data)
            .map_err(|e| BlissError::FileError(format!("cannot write heap data: {}", e)))?;
        file.flush()
            .map_err(|e| BlissError::FileError(format!("cannot flush image file: {}", e)))?;
        Ok(())
    })();

    if let Err(e) = write_result {
        // Clean up temp file on error
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // Rename temp → final path (atomic on POSIX filesystems).
    std::fs::rename(&tmp_path, path)
        .map_err(|e| BlissError::FileError(format!("cannot rename image file: {}", e)))?;

    // Set executable permission if requested.
    if options.executable {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(path)
                .map_err(|e| BlissError::FileError(format!("cannot read file metadata: {}", e)))?;
            let mut perms = metadata.permissions();
            let mode = perms.mode();
            perms.set_mode(mode | 0o111);
            std::fs::set_permissions(path, perms)
                .map_err(|e| {
                    BlissError::FileError(format!("cannot set executable permission: {}", e))
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
pub fn load_image(path: &str) -> Result<BlissVal, BlissError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| BlissError::InvalidImage(format!("cannot open image: {}", e)))?;

    // Read the entire file contents.
    let mut file_data = Vec::new();
    file.read_to_end(&mut file_data)
        .map_err(|e| BlissError::InvalidImage(format!("cannot read image file: {}", e)))?;

    if file_data.len() < HEADER_SIZE {
        return Err(BlissError::InvalidImage(
            "image file too small to contain header".into(),
        ));
    }

    // Parse and validate the header.
    let header: ImageHeader = bytes_to_struct(&file_data)
        .ok_or_else(|| BlissError::InvalidImage("image file too small".into()))?;

    if header.magic != IMAGE_MAGIC {
        return Err(BlissError::InvalidImage(format!(
            "bad magic: expected {:#x}, got {:#x}",
            IMAGE_MAGIC, header.magic
        )));
    }

    if header.format_version > FORMAT_VERSION {
        return Err(BlissError::InvalidImage(format!(
            "unsupported format version: image has {}, runtime supports up to {}",
            header.format_version, FORMAT_VERSION
        )));
    }

    let expected_platform = current_platform_tag();
    if header.platform_tag != expected_platform {
        return Err(BlissError::InvalidImage(format!(
            "platform mismatch: image tag {:#x}, current platform tag {:#x}",
            header.platform_tag, expected_platform
        )));
    }

    // Verify header SHA-256 checksum.
    let expected_checksum = compute_header_checksum(&file_data[..HEADER_SIZE]);
    if header.header_sha256 != expected_checksum {
        return Err(BlissError::InvalidImage(
            "header checksum mismatch — image may be corrupt".into(),
        ));
    }

    // Parse the section directory.
    let section_dir_start = HEADER_SIZE;
    let section_dir_end =
        section_dir_start + (header.section_count as usize) * SECTION_ENTRY_SIZE;
    if file_data.len() < section_dir_end {
        return Err(BlissError::InvalidImage(
            "image file too small for section directory".into(),
        ));
    }

    // Find and restore the heap section.
    let mut heap_restored = false;
    for i in 0..header.section_count as usize {
        let entry_offset = section_dir_start + i * SECTION_ENTRY_SIZE;
        let entry: SectionEntry =
            bytes_to_struct(&file_data[entry_offset..]).ok_or_else(|| {
                BlissError::InvalidImage(format!("cannot parse section entry {}", i))
            })?;

        if entry.section_type == SectionType::Heap as u32 {
            let data_start = entry.file_offset as usize;
            let data_end = data_start + entry.size as usize;
            if file_data.len() < data_end {
                return Err(BlissError::InvalidImage(
                    "image file too small for heap section data".into(),
                ));
            }

            let heap_bytes = &file_data[data_start..data_end];

            // The heap section's first 8 bytes encode the entry continuation
            // as a native-endian u64.  When the heap has additional objects
            // they follow after that initial word — in a full runtime they
            // would be copied into the managed heap and pointers relocated.
            if heap_bytes.len() >= 8 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&heap_bytes[..8]);
                let raw = u64::from_ne_bytes(buf);
                // Verify the value matches the header's entry_continuation
                // for integrity.
                if raw != header.entry_continuation {
                    return Err(BlissError::InvalidImage(
                        "heap entry continuation does not match header".into(),
                    ));
                }
            }

            heap_restored = true;
        }
        // Future: handle SectionType::Symbols, Packages, Code, Reloc, etc.
    }

    if !heap_restored {
        return Err(BlissError::InvalidImage(
            "image contains no heap section".into(),
        ));
    }

    Ok(BlissVal::from_raw(header.entry_continuation))
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
pub fn validate_image_header(path: &str) -> Result<ImageHeader, BlissError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| BlissError::InvalidImage(format!("cannot open image: {}", e)))?;

    let mut buf = vec![0u8; HEADER_SIZE];
    file.read_exact(&mut buf)
        .map_err(|e| BlissError::InvalidImage(format!("cannot read image header: {}", e)))?;

    let header: ImageHeader =
        bytes_to_struct(&buf).ok_or_else(|| BlissError::InvalidImage("header too small".into()))?;

    if header.magic != IMAGE_MAGIC {
        return Err(BlissError::InvalidImage(format!(
            "bad magic: expected {:#x}, got {:#x}",
            IMAGE_MAGIC, header.magic
        )));
    }

    if header.format_version > FORMAT_VERSION {
        return Err(BlissError::InvalidImage(format!(
            "unsupported format version: image has {}, runtime supports up to {}",
            header.format_version, FORMAT_VERSION
        )));
    }

    let expected_platform = current_platform_tag();
    if header.platform_tag != expected_platform {
        return Err(BlissError::InvalidImage(format!(
            "platform mismatch: image tag {:#x}, current platform tag {:#x}",
            header.platform_tag, expected_platform
        )));
    }

    // Verify header checksum.
    let expected_checksum = compute_header_checksum(&buf);
    if header.header_sha256 != expected_checksum {
        return Err(BlissError::InvalidImage(
            "header checksum mismatch — image may be corrupt".into(),
        ));
    }

    Ok(header)
}
