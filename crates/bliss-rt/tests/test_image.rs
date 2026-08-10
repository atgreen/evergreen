use bliss_rt::image::*;

#[test]
fn arch_and_os_repr_values() {
    assert_eq!(Arch::X86_64 as u32, 1);
    assert_eq!(Arch::Aarch64 as u32, 2);
    assert_eq!(Os::Linux as u32, 1);
    assert_eq!(Os::MacOS as u32, 2);
    assert_eq!(Os::FreeBSD as u32, 3);
}

#[test]
fn image_magic_encodes_blissimg() {
    assert_eq!(IMAGE_MAGIC, 0x424C4953_53494D47);
    let bytes = IMAGE_MAGIC.to_be_bytes();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(s, "BLISSIMG");
}

#[test]
fn platform_tag_unique_per_combination() {
    let mut tags = std::collections::HashSet::new();
    for &arch in &[Arch::X86_64, Arch::Aarch64] {
        for &os in &[Os::Linux, Os::MacOS, Os::FreeBSD] {
            assert!(tags.insert(platform_tag(arch, os)));
        }
    }
}

#[test]
fn platform_tag_deterministic() {
    assert_eq!(platform_tag(Arch::X86_64, Os::Linux), platform_tag(Arch::X86_64, Os::Linux));
}

#[test]
fn current_platform_tag_nonzero_and_consistent() {
    let tag = current_platform_tag();
    assert_ne!(tag, 0);
    assert_eq!(tag, current_platform_tag());
}

#[test]
fn image_flags_are_distinct_bits() {
    assert_eq!(image_flags::COMPRESSED, 1);
    assert_eq!(image_flags::CODE_SIGNED, 2);
    assert_eq!(image_flags::READ_ONLY_SAFE, 4);
}

#[test]
fn section_type_repr_values() {
    assert_eq!(SectionType::Heap as u32, 1);
    assert_eq!(SectionType::Symbols as u32, 2);
    assert_eq!(SectionType::Packages as u32, 3);
    assert_eq!(SectionType::Code as u32, 4);
    assert_eq!(SectionType::Reloc as u32, 5);
    assert_eq!(SectionType::GcMeta as u32, 6);
    assert_eq!(SectionType::Settings as u32, 7);
}

#[test]
fn image_compression_equality() {
    assert_ne!(ImageCompression::None, ImageCompression::Zstd);
    assert_eq!(ImageCompression::None, ImageCompression::None);
}

#[test]
fn image_header_is_128_bytes() {
    assert_eq!(std::mem::size_of::<ImageHeader>(), 128);
}

#[test]
fn image_header_fields_roundtrip() {
    let h = ImageHeader {
        magic: IMAGE_MAGIC, format_version: 1,
        flags: image_flags::COMPRESSED | image_flags::READ_ONLY_SAFE,
        platform_tag: platform_tag(Arch::X86_64, Os::Linux),
        original_base: 0x7F00_0000_0000, heap_size: 1 << 20,
        entry_continuation: 0, section_count: 3,
        gc_generation: 5, gc_metadata_offset: 4096,
        save_timestamp: 1700000000, reserved: [0u8; 24],
        header_sha256: [0u8; 32],
    };
    assert_eq!(h.magic, IMAGE_MAGIC);
    assert_eq!(h.section_count, 3);
    assert_eq!(h.gc_generation, 5);
}

#[test]
fn section_entry_fields() {
    let e = SectionEntry {
        section_type: SectionType::Code as u32,
        flags: 0, file_offset: 128,
        size: 65536, uncompressed_size: 131072,
    };
    assert_eq!(e.section_type, 4);
    assert_eq!(e.uncompressed_size, 131072);
}

#[test]
fn save_image_options_fields() {
    let opts = SaveImageOptions {
        executable: true,
        compression: ImageCompression::Zstd,
        purify: false,
    };
    assert!(opts.executable);
    assert_eq!(opts.compression, ImageCompression::Zstd);
}

#[test]
fn load_image_nonexistent_fails() {
    assert!(load_image("/tmp/nonexistent_bliss_image.bimg").is_err());
}

#[test]
fn validate_image_header_nonexistent_fails() {
    assert!(validate_image_header("/tmp/no_such_image.bimg").is_err());
}

#[test]
fn validate_image_header_bad_magic_fails() {
    use std::io::Write;
    let path = "/tmp/bliss_bad_magic_test.bimg";
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&[0u8; 128]).unwrap();
    drop(f);
    assert!(validate_image_header(path).is_err());
    std::fs::remove_file(path).ok();
}

#[test]
fn save_image_returns_result() {
    let path = "/tmp/bliss_test_save_image.bimg";
    let opts = SaveImageOptions {
        executable: false,
        compression: ImageCompression::None,
        purify: false,
    };
    // save_image should either succeed or return an error (not panic).
    let result = save_image(path, &opts);
    // Clean up if it somehow succeeded
    if result.is_ok() {
        std::fs::remove_file(path).ok();
    }
    // Assert the result is Ok (save should succeed with valid path and options)
    assert!(result.is_ok(), "save_image with valid path should succeed");
}

#[test]
fn save_image_with_compression() {
    let path = "/tmp/bliss_test_save_image_zstd.bimg";
    let opts = SaveImageOptions {
        executable: true,
        compression: ImageCompression::Zstd,
        purify: true,
    };
    let result = save_image(path, &opts);
    if result.is_ok() {
        std::fs::remove_file(path).ok();
    }
    assert!(result.is_ok(), "save_image with Zstd compression should succeed");
}

#[test]
fn find_appended_image_returns_result() {
    let result = find_appended_image();
    // For a non-appended test binary, find_appended_image should return None/Err
    // indicating no appended image was found. The key assertion: it completes
    // and returns a meaningful result.
    assert!(result.is_none(),
        "find_appended_image on a test binary should return None (no appended image)");
}
