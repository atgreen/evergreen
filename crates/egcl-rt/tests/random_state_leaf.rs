// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use egcl_rt::{Collector, HeapCollector, random_state};

#[test]
fn raw_random_state_bits_move_without_becoming_gc_edges() {
    egcl_rt::rooted!(other = egcl_rt::gc::alloc_double_float(42.0));
    let pointer_bits = other.to_raw();
    egcl_rt::rooted!(state = random_state::make(pointer_bits).unwrap());
    assert!(!state.is_cons());
    let before = state.to_raw();
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(state.to_raw(), before, "state must actually relocate");
    assert_ne!(
        other.to_raw(),
        pointer_bits,
        "pointer-shaped seed must stay raw"
    );
    assert_eq!(random_state::snapshot(*state), Some(pointer_bits));
    egcl_rt::rooted!(copy = random_state::make(pointer_bits).unwrap());
    assert_ne!(*copy, *state);
    for _ in 0..20 {
        assert_eq!(random_state::draw(*state), random_state::draw(*copy));
    }
    let unchanged = random_state::snapshot(*copy);
    random_state::draw(*state).unwrap();
    assert_eq!(random_state::snapshot(*copy), unchanged);
    // Core payloads preserve the exact state, and old cons-state images are
    // rejected before replacing this heap. Raw state words are not fixups.
    egcl_rt::rooted!(undrawn = random_state::make(0x123456789abcdef).unwrap());
    let saved = random_state::snapshot(*undrawn).unwrap();
    egcl_rt::gc::set_entry_continuation(*undrawn);
    let path = std::env::temp_dir().join(format!("egcl-random-state-{}.core", std::process::id()));
    let old_path = path.with_extension("old.core");
    egcl_rt::image::save_image(
        path.to_str().unwrap(),
        &egcl_rt::image::SaveImageOptions {
            executable: false,
            compression: egcl_rt::image::ImageCompression::None,
            purify: false,
        },
    )
    .unwrap();
    assert_eq!(random_state::snapshot(*undrawn), Some(saved));
    let next = random_state::draw(*undrawn).unwrap();
    let mut old_bytes = std::fs::read(&path).unwrap();
    assert_eq!(u32::from_ne_bytes(old_bytes[8..12].try_into().unwrap()), 16);
    old_bytes[8..12].copy_from_slice(&12_u32.to_ne_bytes());
    std::fs::write(&old_path, old_bytes).unwrap();
    let error = egcl_rt::image::load_image(old_path.to_str().unwrap()).unwrap_err();
    assert!(error.to_string().contains("unsupported format version"));
    assert_ne!(random_state::snapshot(*undrawn), Some(saved));
    drop(undrawn);
    drop(copy);
    drop(state);
    drop(other);
    egcl_rt::rooted!(restored = egcl_rt::image::load_image(path.to_str().unwrap()).unwrap());
    assert_eq!(random_state::snapshot(*restored), Some(saved));
    assert_eq!(random_state::draw(*restored), Some(next));
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(old_path).unwrap();
}

#[test]
fn random_state_predicates_reject_nonmapped_pseudoheap_handles() {
    for address in [0x1000usize, 0x7fff_ffff_f000usize] {
        let value = unsafe { egcl_rt::EgclVal::from_heap_ptr(address as *mut u8) };
        assert!(value.is_heap_object());
        assert!(!egcl_rt::random_state::is_random_state(value));
        assert_eq!(egcl_rt::random_state::snapshot(value), None);
        assert_eq!(egcl_rt::random_state::draw(value), None);
    }
}
