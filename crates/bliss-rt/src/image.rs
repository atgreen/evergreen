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
    unimplemented!("platform_tag")
}

/// Return the platform tag for the current platform.
pub fn current_platform_tag() -> u64 {
    unimplemented!("current_platform_tag")
}

// ── Image header ───────────────────────────────────────────────────

/// Image file magic number: "BLISSIMG" in ASCII.
pub const IMAGE_MAGIC: u64 = 0x424C4953_53494D47;

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
    unimplemented!("save_image")
}

/// Load an image file and restore the heap.
/// Called during runtime startup.
pub fn load_image(path: &str) -> Result<BlissVal, BlissError> {
    unimplemented!("load_image")
}

/// Check if the running binary has an appended image.
pub fn find_appended_image() -> Option<String> {
    unimplemented!("find_appended_image")
}

/// Validate an image header without loading the full image.
pub fn validate_image_header(path: &str) -> Result<ImageHeader, BlissError> {
    unimplemented!("validate_image_header")
}
