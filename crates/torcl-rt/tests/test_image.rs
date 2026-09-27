use std::sync::{Mutex, OnceLock};
use torcl_rt::gc;
use torcl_rt::image::*;

/// Serialize every test that touches the process-global heap (init_heap,
/// record_object, restore_heap, save_image). The heap is a single
/// OnceLock<Mutex<Option<HeapState>>>; concurrent mutation across the cargo
/// test-harness threads races and can SIGSEGV. Matches spec_gc_pinning.rs.
fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// A temp image path nobody else can be using. The paths here used to be fixed
/// strings, which is fine for ONE cargo process and wrong for a machine running
/// two checkouts: the roundtrip tests SAVE the heap to the path and LOAD it back,
/// so a second session saving its own heap to the same name made the first load
/// the wrong image and fail its contents assertions. The lock() above cannot help
/// — it serializes tests inside one process, and this is a race BETWEEN processes
/// (bliss-kqp7). crates/torcl-stdlib/tests/test_pathnames.rs already keys its
/// temp paths this way.
///
/// Used for the must-NOT-exist paths too: another session's leftover at a fixed
/// name would break those assertions just as surely.
fn image_path(label: &str) -> String {
    format!(
        "{}/torcl-test-{}-{}.bimg",
        std::env::temp_dir().display(),
        label,
        std::process::id()
    )
}

#[test]
fn arch_and_os_repr_values() {
    assert_eq!(Arch::X86_64 as u32, 1);
    assert_eq!(Arch::Aarch64 as u32, 2);
    assert_eq!(Os::Linux as u32, 1);
    assert_eq!(Os::MacOS as u32, 2);
    assert_eq!(Os::FreeBSD as u32, 3);
}

#[test]
fn image_magic_encodes_torclimg() {
    assert_eq!(IMAGE_MAGIC, 0x544F5243_4C494D47);
    let bytes = IMAGE_MAGIC.to_be_bytes();
    let s = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(s, "TORCLIMG");
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
    assert_eq!(
        platform_tag(Arch::X86_64, Os::Linux),
        platform_tag(Arch::X86_64, Os::Linux)
    );
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
        magic: IMAGE_MAGIC,
        format_version: 1,
        flags: image_flags::COMPRESSED | image_flags::READ_ONLY_SAFE,
        platform_tag: platform_tag(Arch::X86_64, Os::Linux),
        original_base: 0x7F00_0000_0000,
        heap_size: 1 << 20,
        entry_continuation: 0,
        section_count: 3,
        gc_generation: 5,
        gc_metadata_offset: 4096,
        save_timestamp: 1700000000,
        reserved: [0u8; 24],
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
        flags: 0,
        file_offset: 128,
        size: 65536,
        uncompressed_size: 131072,
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
    assert!(load_image(&image_path("nonexistent")).is_err());
}

#[test]
fn validate_image_header_nonexistent_fails() {
    assert!(validate_image_header(&image_path("no-such")).is_err());
}

#[test]
fn validate_image_header_bad_magic_fails() {
    use std::io::Write;
    let path = image_path("bad-magic");
    let path = path.as_str();
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&[0u8; 128]).unwrap();
    drop(f);
    assert!(validate_image_header(path).is_err());
    std::fs::remove_file(path).ok();
}

#[test]
fn save_image_returns_result() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = image_path("save-image");
    let path = path.as_str();
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
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = image_path("save-image-zstd");
    let path = path.as_str();
    let opts = SaveImageOptions {
        executable: true,
        compression: ImageCompression::Zstd,
        purify: true,
    };
    let result = save_image(path, &opts);
    if result.is_ok() {
        std::fs::remove_file(path).ok();
    }
    assert!(
        result.is_ok(),
        "save_image with Zstd compression should succeed"
    );
}

#[test]
fn find_appended_image_returns_result() {
    let result = find_appended_image();
    // For a non-appended test binary, find_appended_image should return None/Err
    // indicating no appended image was found. The key assertion: it completes
    // and returns a meaningful result.
    assert!(
        result.is_none(),
        "find_appended_image on a test binary should return None (no appended image)"
    );
}

#[test]
fn save_load_roundtrip_preserves_heap_objects() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    // Initialize the heap so objects can be recorded.
    let config = gc::GcConfig {
        heap_size: 64 * 1024 * 1024,
        heap_max: 256 * 1024 * 1024,
        nursery_size: 16 * 1024 * 1024,
        tlab_size: 8192,
        region_size: 1024 * 1024,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 1024,
        old_occupancy_trigger: 0.45,
    };
    // init_heap may fail if already initialized by another test; that's fine.
    let _ = gc::init_heap(&config);

    // Record some objects into the heap.
    let obj1_data = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let obj2_data = vec![0xAA, 0xBB, 0xCC, 0xDD];
    gc::record_object(1, obj1_data.clone());
    gc::record_object(2, obj2_data.clone());

    // Set an entry continuation.
    let entry_val = torcl_rt::value::TorclVal::from_fixnum(42);
    gc::set_entry_continuation(entry_val);

    // Save the image.
    let path = image_path("roundtrip");
    let path = path.as_str();
    let opts = SaveImageOptions {
        executable: false,
        compression: ImageCompression::None,
        purify: false,
    };
    save_image(path, &opts).expect("save_image should succeed");

    // Clear the heap to prove load restores data.
    gc::restore_heap(&[]).expect("clearing heap should succeed");

    // Verify heap is empty.
    let mut count_before = 0usize;
    gc::walk_heap(|_, _, _| {
        count_before += 1;
        true
    })
    .unwrap();
    assert_eq!(count_before, 0, "heap should be empty after clear");

    // Load the image back.
    let restored_entry = load_image(path).expect("load_image should succeed");

    // Verify the entry continuation was restored.
    assert_eq!(
        restored_entry, entry_val,
        "entry continuation should survive round-trip"
    );

    // Walk the restored heap and verify objects are present.
    let mut restored_objects: Vec<(u8, Vec<u8>)> = Vec::new();
    gc::walk_heap(|ptr, type_id, size| {
        let data = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
        restored_objects.push((type_id, data));
        true
    })
    .unwrap();

    assert!(
        restored_objects.len() >= 2,
        "should have at least 2 restored objects, got {}",
        restored_objects.len()
    );

    // Find our objects among the restored set (there may be others from prior tests
    // since heap state is global).
    let found_obj1 = restored_objects
        .iter()
        .any(|(tid, data)| *tid == 1 && data == &obj1_data);
    let found_obj2 = restored_objects
        .iter()
        .any(|(tid, data)| *tid == 2 && data == &obj2_data);
    assert!(found_obj1, "object 1 should survive save/load round-trip");
    assert!(found_obj2, "object 2 should survive save/load round-trip");

    // Clean up.
    std::fs::remove_file(path).ok();
}

#[test]
fn save_load_roundtrip_with_compression() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let config = gc::GcConfig {
        heap_size: 64 * 1024 * 1024,
        heap_max: 256 * 1024 * 1024,
        nursery_size: 16 * 1024 * 1024,
        tlab_size: 8192,
        region_size: 1024 * 1024,
        promotion_threshold: 15,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 1024,
        old_occupancy_trigger: 0.45,
    };
    let _ = gc::init_heap(&config);

    // Record an object with repeated bytes (compresses well).
    let obj_data = vec![0xFFu8; 100];
    gc::record_object(5, obj_data.clone());

    let entry_val = torcl_rt::value::TorclVal::from_fixnum(99);
    gc::set_entry_continuation(entry_val);

    let path = image_path("roundtrip-compressed");
    let path = path.as_str();
    let opts = SaveImageOptions {
        executable: false,
        compression: ImageCompression::Zstd,
        purify: false,
    };
    save_image(path, &opts).expect("save_image with compression should succeed");

    gc::restore_heap(&[]).expect("clearing heap should succeed");

    let restored_entry = load_image(path).expect("load_image should succeed");
    assert_eq!(restored_entry, entry_val);

    let mut found = false;
    gc::walk_heap(|ptr, type_id, size| {
        if type_id == 5 {
            let data = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
            if data == obj_data {
                found = true;
            }
        }
        true
    })
    .unwrap();
    assert!(found, "compressed object should survive round-trip");

    std::fs::remove_file(path).ok();
}
