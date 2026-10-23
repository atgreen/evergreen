// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! bliss-4due: the pathname string registries are weak. A registered string
//! that survives a collection is found again at its NEW address (the forward
//! map follows the move), and one that dies is dropped from both maps -- by
//! the minor collector while it is young and by the major collector once it
//! has been promoted.

use egcl_stdlib::pathnames::{
    is_registered_string, register_string, registered_string, string_registry_counts,
};

#[test]
fn registered_strings_follow_moves_and_die_with_their_string() {
    egcl_rt::gc::ensure_heap_initialized();
    // Settle anything earlier setup left in the nursery.
    egcl_rt::full_gc().unwrap();
    let baseline = string_registry_counts();

    let text = "bliss-4due weak registry probe";
    egcl_rt::rooted!(kept = egcl_rt::gc::alloc_character_string(text));
    let unrooted = *kept;
    register_string(*kept, text);
    assert_eq!(registered_string(*kept).as_deref(), Some(text));
    assert_eq!(string_registry_counts(), (baseline.0 + 1, baseline.1 + 1));

    // A young string is evacuated by the next collection. The probe is only
    // meaningful if it actually moved, so check that first.
    egcl_rt::full_gc().unwrap();
    assert_ne!(*kept, unrooted, "the registered string did not move");
    assert_eq!(registered_string(*kept).as_deref(), Some(text));
    assert!(is_registered_string(*kept));
    assert!(
        !is_registered_string(unrooted),
        "a stale pre-move key survived"
    );
    assert_eq!(string_registry_counts(), (baseline.0 + 1, baseline.1 + 1));

    // A young string that dies is dropped by the minor collector.
    let doomed_text = "bliss-4due doomed young string";
    let doomed = egcl_rt::gc::alloc_character_string(doomed_text);
    register_string(doomed, doomed_text);
    assert_eq!(string_registry_counts(), (baseline.0 + 2, baseline.1 + 2));
    egcl_rt::full_gc().unwrap();
    assert_eq!(string_registry_counts(), (baseline.0 + 1, baseline.1 + 1));

    // The promoted survivor, once unreferenced, is dropped by the major
    // collector.
    drop(kept);
    egcl_rt::full_gc().unwrap();
    egcl_rt::full_gc().unwrap();
    assert_eq!(string_registry_counts(), baseline);
}
