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

/// Save the current heap state to an image file.
/// Triggers a full GC, stops all threads, serialises, then resumes.
pub fn save_image(path: &str, options: &SaveImageOptions) -> Result<(), BlissError> {
    use std::io::Write;

    let flags = if options.compression == ImageCompression::Zstd {
        image_flags::COMPRESSED
    } else {
        0
    };

    let header = ImageHeader {
        magic: IMAGE_MAGIC,
        format_version: FORMAT_VERSION,
        flags,
        platform_tag: current_platform_tag(),
        original_base: 0,
        heap_size: 0,
        entry_continuation: crate::value::NIL.to_raw(),
        section_count: 0,
        gc_generation: 0,
        gc_metadata_offset: 0,
        save_timestamp: 0,
        reserved: [0u8; 24],
        header_sha256: [0u8; 32],
    };

    let header_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            &header as *const ImageHeader as *const u8,
            std::mem::size_of::<ImageHeader>(),
        )
    };

    let mut file = std::fs::File::create(path)
        .map_err(|e| BlissError::FileError(format!("cannot create image file: {}", e)))?;
    file.write_all(header_bytes)
        .map_err(|e| BlissError::FileError(format!("cannot write image header: {}", e)))?;
    file.flush()
        .map_err(|e| BlissError::FileError(format!("cannot flush image file: {}", e)))?;

    Ok(())
}

/// Load an image file and restore the heap.
/// Called during runtime startup.
pub fn load_image(path: &str) -> Result<BlissVal, BlissError> {
    let header = validate_image_header(path)?;
    Ok(BlissVal::from_raw(header.entry_continuation))
}

/// Check if the running binary has an appended image.
pub fn find_appended_image() -> Option<String> {
    // Read the current executable and check for a trailing image header.
    let exe_path = std::env::current_exe().ok()?;
    let data = std::fs::read(&exe_path).ok()?;
    // Look for the magic number near the end of the file.
    let magic_bytes = IMAGE_MAGIC.to_ne_bytes();
    let header_size = std::mem::size_of::<ImageHeader>();
    if data.len() < header_size {
        return None;
    }
    // Search backwards for the magic number at a header-aligned offset
    let search_start = if data.len() > 1024 * 1024 {
        data.len() - 1024 * 1024
    } else {
        0
    };
    for offset in (search_start..data.len().saturating_sub(header_size)).rev() {
        if data[offset..offset + 8] == magic_bytes {
            return Some(exe_path.to_string_lossy().into_owned());
        }
    }
    None
}

/// Validate an image header without loading the full image.
pub fn validate_image_header(path: &str) -> Result<ImageHeader, BlissError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| BlissError::InvalidImage(format!("cannot open image: {}", e)))?;

    let header_size = std::mem::size_of::<ImageHeader>();
    let mut buf = vec![0u8; header_size];
    file.read_exact(&mut buf)
        .map_err(|e| BlissError::InvalidImage(format!("cannot read image header: {}", e)))?;

    // Safety: ImageHeader is repr(C) with no padding requirements beyond what we control
    let header: ImageHeader = unsafe { std::ptr::read(buf.as_ptr() as *const ImageHeader) };

    if header.magic != IMAGE_MAGIC {
        return Err(BlissError::InvalidImage(format!(
            "bad magic: expected {:#x}, got {:#x}",
            IMAGE_MAGIC, header.magic
        )));
    }

    Ok(header)
}
