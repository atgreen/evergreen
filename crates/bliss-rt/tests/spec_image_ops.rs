use bliss_rt::gc::{
    GcConfig, heap_base_address, init_heap, record_object, set_entry_continuation, walk_heap,
};
use bliss_rt::image::{
    ImageCompression, SaveImageOptions, load_image, save_image, validate_image_header,
};
use bliss_rt::value::BlissVal;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 1024 * 1024,
        heap_max: 4 * 1024 * 1024,
        nursery_size: 256 * 1024,
        tlab_size: 256,
        region_size: 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

fn init_test_heap() {
    init_heap(&gc_config()).expect("init_heap");
}

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_path(name: &str) -> PathBuf {
    let dir = PathBuf::from("target/spec-image-tests");
    fs::create_dir_all(&dir).expect("create temp dir");
    dir.join(name)
}

fn image_opts() -> SaveImageOptions {
    SaveImageOptions {
        executable: false,
        compression: ImageCompression::None,
        purify: true,
    }
}

fn walk_objects_with_data() -> Vec<(usize, u8, Vec<u8>)> {
    let mut out = Vec::new();
    walk_heap(|ptr, type_id, size| {
        let data = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
        out.push((ptr as usize, type_id, data));
        true
    })
    .expect("walk_heap");
    out
}

#[test]
fn spec_image_round_trip_restores_heap_and_entry_state() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R7.01, R7.04, and R10.22, the runtime must save and restore a full
    // image, preserving heap content and the entry continuation.
    init_test_heap();
    let path = temp_path("round-trip.bimg");
    let _ = fs::remove_file(&path);

    record_object(0x61, vec![1, 2, 3, 4]);
    record_object(0x62, vec![5, 6, 7, 8, 9]);
    let entry = BlissVal::from_fixnum(1234);
    set_entry_continuation(entry);

    save_image(path.to_str().unwrap(), &image_opts()).expect("save_image");
    let header = validate_image_header(path.to_str().unwrap()).expect("validate header");
    assert_eq!(header.entry_continuation, entry.to_raw());
    // Per-object record is [old_body u64][type u8][size u32][data] = 8+1+4+size
    // (bliss-x0f2 M2.0 added the old-body address); plus the 8-byte entry word.
    assert_eq!(
        header.heap_size,
        8 + (8 + 1 + 4 + 4) as u64 + (8 + 1 + 4 + 5) as u64
    );

    init_test_heap();
    let restored = load_image(path.to_str().unwrap()).expect("load_image");
    assert_eq!(restored, entry);

    let restored_objects = walk_objects_with_data();
    assert!(
        restored_objects
            .iter()
            .any(|(_, t, data)| *t == 0x61 && data == &vec![1, 2, 3, 4])
    );
    assert!(
        restored_objects
            .iter()
            .any(|(_, t, data)| *t == 0x62 && data == &vec![5, 6, 7, 8, 9])
    );
}

#[test]
fn spec_image_round_trip_rebuilds_symbol_registry_cross_process() {
    // bliss-x0f2 M2: a fresh process (re-inited heap) has an empty symbol
    // registry. After an image load, restore_symbols rebuilds the registry to
    // index the RESTORED symbol objects (addresses remapped via the per-object
    // old→new map), so the symbol resolves by name and its value cell survives.
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_test_heap();
    let idx = bliss_rt::symbols::intern("COREDUMP-RT-SYM");
    bliss_rt::symbols::set_symbol_value(idx, BlissVal::from_fixnum(42));
    set_entry_continuation(BlissVal::from_fixnum(1));

    let path = temp_path("symbols.bimg");
    let _ = fs::remove_file(&path);
    save_image(path.to_str().unwrap(), &image_opts()).expect("save_image");

    init_test_heap();
    load_image(path.to_str().unwrap()).expect("load_image");

    assert_eq!(
        bliss_rt::symbols::find_index("COREDUMP-RT-SYM"),
        Some(idx),
        "symbol must resolve by name after cross-process image load"
    );
    assert_eq!(
        bliss_rt::symbols::symbol_name(idx).as_deref(),
        Some("COREDUMP-RT-SYM"),
        "symbol name (relocated name string) must be readable after load"
    );
    assert_eq!(
        bliss_rt::symbols::symbol_value(idx),
        Some(BlissVal::from_fixnum(42)),
        "symbol value cell (in the relocated object) must survive the load"
    );
}

#[test]
fn spec_image_loader_relocates_tagged_lisp_pointers_when_base_changes() {
    // bliss-x0f2: real Lisp slots hold TAGGED values (cons=|001, heap-object=
    // |010), not raw body pointers. The relocation scan must catch these or a
    // Lisp graph breaks across a base change. Store a tagged cons ref (points at
    // the body) and a tagged heap-object ref (points at the header) to a target,
    // then verify both relocate.
    use bliss_rt::value::{TAG_CONS, TAG_HEAP_OBJECT};
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    init_test_heap();
    let path = temp_path("reloc-tagged.bimg");
    let _ = fs::remove_file(&path);

    record_object(0x81, vec![0xBB; 8]);
    let target_body = walk_objects_with_data()
        .into_iter()
        .find(|(_, type_id, _)| *type_id == 0x81)
        .map(|(ptr, _, _)| ptr as u64)
        .expect("target object");
    let header_size = 8u64; // OBJECT_HEADER_SIZE
    let cons_ref = target_body | TAG_CONS; // cons points at the body
    let heapobj_ref = (target_body - header_size) | TAG_HEAP_OBJECT; // points at header

    let mut holder = vec![0u8; 16];
    holder[..8].copy_from_slice(&cons_ref.to_le_bytes());
    holder[8..].copy_from_slice(&heapobj_ref.to_le_bytes());
    record_object(0x82, holder);
    set_entry_continuation(BlissVal::from_fixnum(1));

    save_image(path.to_str().unwrap(), &image_opts()).expect("save_image");
    init_test_heap();
    load_image(path.to_str().unwrap()).expect("load_image");

    let restored = walk_objects_with_data();
    let new_body = restored
        .iter()
        .find(|(_, t, _)| *t == 0x81)
        .map(|(ptr, _, _)| *ptr as u64)
        .expect("restored target");
    let holder_data = restored
        .iter()
        .find(|(_, t, _)| *t == 0x82)
        .map(|(_, _, data)| data.clone())
        .expect("restored holder");
    let restored_cons = u64::from_le_bytes(holder_data[..8].try_into().unwrap());
    let restored_heapobj = u64::from_le_bytes(holder_data[8..16].try_into().unwrap());
    assert_eq!(
        restored_cons,
        new_body | TAG_CONS,
        "tagged cons ref must relocate to the new body, preserving its tag"
    );
    assert_eq!(
        restored_heapobj,
        (new_body - header_size) | TAG_HEAP_OBJECT,
        "tagged heap-object ref must relocate to the new header, preserving its tag"
    );
}

#[test]
fn spec_image_loader_relocates_heap_pointers_when_base_changes() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R7.03, a loaded image must relocate pointer fields when the heap
    // maps at a different base address than the saved image.
    init_test_heap();
    let path = temp_path("relocation.bimg");
    let _ = fs::remove_file(&path);

    record_object(0x71, vec![0xAA; 8]);
    let target_ptr = walk_objects_with_data()
        .into_iter()
        .find(|(_, type_id, _)| *type_id == 0x71)
        .map(|(ptr, _, _)| ptr)
        .expect("target object");

    let mut holder = vec![0u8; 16];
    holder[..8].copy_from_slice(&(target_ptr as u64).to_le_bytes());
    holder[8..].copy_from_slice(&0xDEADBEEFu64.to_le_bytes());
    record_object(0x72, holder);
    let entry = BlissVal::from_fixnum(77);
    set_entry_continuation(entry);

    let saved_base = heap_base_address();
    save_image(path.to_str().unwrap(), &image_opts()).expect("save_image");

    init_test_heap();
    let loaded_entry = load_image(path.to_str().unwrap()).expect("load_image");
    assert_eq!(loaded_entry, entry);
    let restored_base = heap_base_address();
    assert_ne!(saved_base, 0);
    assert_ne!(restored_base, 0);

    let restored = walk_objects_with_data();
    let new_target = restored
        .iter()
        .find(|(_, type_id, _)| *type_id == 0x71)
        .map(|(ptr, _, _)| *ptr as u64)
        .expect("restored target");
    let holder_field = restored
        .iter()
        .find(|(_, type_id, _)| *type_id == 0x72)
        .map(|(_, _, data)| u64::from_le_bytes(data[..8].try_into().unwrap()))
        .expect("restored holder");
    assert_eq!(holder_field, new_target);
}

#[test]
fn spec_image_header_validation_rejects_corrupt_or_incompatible_images() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R7.04 and R7.15, the loader must reject bad magic, mismatched
    // platform metadata, checksum mismatches, and unsupported versions
    // before using heap data.
    init_test_heap();
    let path = temp_path("header.bimg");
    let _ = fs::remove_file(&path);
    save_image(path.to_str().unwrap(), &image_opts()).expect("save_image");

    let mut bytes = fs::read(&path).expect("read image");
    bytes[0] ^= 0xFF;
    let bad_magic = temp_path("header-bad-magic.bimg");
    fs::write(&bad_magic, &bytes).expect("write mutated image");
    assert!(validate_image_header(bad_magic.to_str().unwrap()).is_err());

    let mut bytes = fs::read(&path).expect("read image");
    bytes[0x10] ^= 0x01;
    let bad_platform = temp_path("header-bad-platform.bimg");
    fs::write(&bad_platform, &bytes).expect("write mutated image");
    assert!(load_image(bad_platform.to_str().unwrap()).is_err());

    let mut bytes = fs::read(&path).expect("read image");
    let heap_offset = 4096usize;
    bytes[heap_offset] ^= 0xFF;
    let bad_checksum = temp_path("header-bad-checksum.bimg");
    fs::write(&bad_checksum, &bytes).expect("write mutated image");
    assert!(load_image(bad_checksum.to_str().unwrap()).is_err());

    let mut bytes = fs::read(&path).expect("read image");
    bytes[0x08..0x0C].copy_from_slice(&u32::MAX.to_le_bytes());
    let bad_version = temp_path("header-bad-version.bimg");
    fs::write(&bad_version, &bytes).expect("write mutated image");
    let err = load_image(bad_version.to_str().unwrap()).expect_err("unsupported version");
    let msg = format!("{err}");
    assert!(
        msg.contains("unsupported format version") || msg.contains("checksum mismatch"),
        "unexpected error: {msg}"
    );
}

#[test]
fn spec_image_save_is_atomic_and_preserves_previous_file_on_failure() {
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // Per R7.20, image save must either complete or leave the previous file
    // untouched. Forcing temp-file creation to fail should preserve the target.
    init_test_heap();
    let path = temp_path("atomic-save.bimg");
    let tmp_dir = temp_path("atomic-save.bimg.tmp");
    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir_all(&tmp_dir);

    fs::write(&path, b"previous-image").expect("seed target");
    fs::create_dir_all(&tmp_dir).expect("block tmp file creation");

    let err = save_image(path.to_str().unwrap(), &image_opts()).expect_err("save should fail");
    assert!(format!("{err}").contains("temp image file"));
    assert_eq!(
        fs::read(&path).expect("read preserved target"),
        b"previous-image"
    );
}
