// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `ClassMeta.shared_slots` must survive a core image (bliss-dfm5f).
//!
//! Those are the names a class declares with `:allocation :class`, and they
//! exist for one job: CLHS 7.5.3 says an inherited INSTANCE slot of the same
//! name is then not part of this class's instance layout.
//! `finalize_class_layout` implements that by accumulating each class's
//! `shared_slots` into a `shadowed` set as it walks the precedence list.
//!
//! The serializer wrote five of `ClassMeta`'s fields and not that one, and the
//! restore hardcoded `Vec::new()`, so a restored class laid out an extra
//! instance slot for a name that is class-allocated.
//!
//! Tested here rather than through the CLI because it is not observable from
//! Lisp: `SLOT-VALUE` on a class-allocated slot consults the interpreter's own
//! `class_slot_values` map first, which IS serialized (by name, in the `CLSD`
//! block), so the shared value is found either way and the extra layout slot
//! is simply never read. Measured before writing this: a save/restore of such
//! a class answered identically for `SLOT-VALUE`, `SLOT-EXISTS-P`,
//! `CLASS-SLOTS` allocation, cross-instance sharing and `SLOT-MAKUNBOUND`.
//! The defect is a wrong layout and a wasted slot word, not a wrong answer —
//! which is exactly why it needs a test at this level to stay fixed.

use egcl_rt::value::EgclVal;
use egcl_stdlib::clos::*;

fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    bootstrap_clos().unwrap();
    guard
}

fn sym(name: &str) -> EgclVal {
    EgclVal::from_symbol_index(egcl_rt::symbols::intern(name))
}

/// `base` has an instance slot X; `derived` declares X `:allocation :class`,
/// so X must NOT be in the instance layout of `derived` or of anything that
/// inherits from it — before a save, and after.
///
/// The decisive half is the class defined AFTER the restore. A class that
/// already had a wrapper when the core was written does not re-finalize on
/// load: the wrapper record carries `layout.order` verbatim, so its layout
/// survives the image whatever happened to `shared_slots`. Measured — that
/// case answers identically with the field serialized or not, which is why an
/// earlier version of this test passed against the unfixed code and proved
/// nothing. A class finalized for the first time after the restore has to
/// rebuild its layout from the restored metadata, and that is where the lost
/// field shows up: X lands in the layout as an ordinary inherited instance
/// slot. Reverted the fix and measured `x=true` here against `x=false` with
/// it.
///
/// Ordinary Lisp reaches this by loading a core and subclassing one of its
/// classes.
#[test]
fn a_class_allocated_slot_still_shadows_an_inherited_one_after_a_restore() {
    let _guard = test_guard();

    let base = sym("SS-BASE");
    let derived = sym("SS-DERIVED");
    let x = sym("SS-X");
    let other = sym("SS-OTHER");

    define_class(base, base, &[], &[x, other]).unwrap();
    define_class_with_shared_slots(derived, derived, &[base], &[], &[x]).unwrap();

    // Before: X is shadowed by the class-allocated declaration, OTHER is not.
    let instance = allocate_instance(derived).unwrap();
    assert!(
        !slot_present_p(instance, x),
        "X is :allocation :class on SS-DERIVED, so it must not be in the instance layout"
    );
    assert!(
        slot_present_p(instance, other),
        "OTHER is an ordinary inherited instance slot and must be in the layout"
    );

    // Round-trip the whole CLOS state. The identity remap stands in for the
    // heap's old->new map; no value here is a heap pointer.
    let bytes = serialize_clos_state();
    let consumed = restore_clos_state(&bytes, &|raw| raw).unwrap();
    assert_eq!(consumed, bytes.len(), "the whole block should be consumed");

    // The restored class keeps its layout through its wrapper record.
    let restored = allocate_instance(derived).unwrap();
    assert!(!slot_present_p(restored, x), "SS-DERIVED's own layout changed");
    assert!(slot_present_p(restored, other), "SS-DERIVED lost OTHER");

    // The decisive case: a subclass finalized for the first time now, from the
    // restored metadata alone.
    let sub = sym("SS-SUB");
    define_class(sub, sub, &[derived], &[]).unwrap();
    let fresh = allocate_instance(sub).unwrap();
    assert!(
        !slot_present_p(fresh, x),
        "a class subclassed after a restore laid out X as an instance slot: its \
         superclass's :allocation :class shadowing was lost with ClassMeta.shared_slots"
    );
    assert!(
        slot_present_p(fresh, other),
        "OTHER should still be inherited as an instance slot"
    );
}

/// The version gate is exact. A core from any other build is refused earlier
/// anyway, on the image's runtime-source hash, so there is no older CLST
/// version to stay compatible with — but a corrupt or truncated block must
/// still be reported rather than parsed.
#[test]
fn a_wrong_version_or_a_truncated_block_is_refused() {
    let _guard = test_guard();
    let bytes = serialize_clos_state();

    assert_eq!(&bytes[..4], b"CLST");
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        5,
        "4 added shared_slots; 5 adds ClosState's standard_class and structure_class"
    );

    let mut wrong = bytes.clone();
    wrong[4..8].copy_from_slice(&4u32.to_le_bytes());
    let error = restore_clos_state(&wrong, &|raw| raw).unwrap_err();
    assert!(
        format!("{error}").contains("unsupported version 4"),
        "expected a version complaint, got: {error}"
    );

    // Past the marker and version, then out of data. Under 8 bytes trips the
    // marker check instead, which is a different message.
    let error = restore_clos_state(&bytes[..12], &|raw| raw).unwrap_err();
    assert!(
        format!("{error}").contains("truncated"),
        "expected a truncation complaint, got: {error}"
    );
    let error = restore_clos_state(&bytes[..6], &|raw| raw).unwrap_err();
    assert!(
        format!("{error}").contains("bad marker"),
        "a block too short to hold a marker and version is reported as such: {error}"
    );
}
