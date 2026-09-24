//! Symbol + package image serialization round-trip (bliss-jtc.6 Stage F).
//!
//! `symbols::restore` resets the process-global symbol registry, so these tests
//! take a process-global lock (they must not run concurrently with each other or
//! any interning) and live in their own binary/process.

use std::sync::{Mutex, OnceLock};

fn lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

#[test]
fn symbol_identity_survives_serialize_restore() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let names = ["RT-IMG-ONE", "RT-IMG-TWO", "RT-IMG-THREE"];
    let before: Vec<u32> = names.iter().map(|n| torcl_rt::symbols::intern(n)).collect();

    let blob = torcl_rt::symbols::serialize();
    torcl_rt::symbols::restore(&blob).expect("restore symbols");

    // Every saved name maps back to the same index, and that index still names it.
    for (name, &idx) in names.iter().zip(&before) {
        assert_eq!(
            torcl_rt::symbols::find_index(name),
            Some(idx),
            "name {name} must restore to its original index"
        );
        assert_eq!(
            torcl_rt::symbols::symbol_name(idx).as_deref(),
            Some(*name),
            "index {idx} must still name {name} after restore"
        );
    }
}

#[test]
fn package_names_and_nicknames_survive_serialize_restore() {
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    torcl_rt::packages::find_or_create("RT-IMG-PKG");
    torcl_rt::packages::add_nickname("RT-IMG-PKG", "RIP");

    let blob = torcl_rt::packages::serialize();
    // Restore is additive; recreating over the live registry must be idempotent.
    torcl_rt::packages::restore(&blob).expect("restore packages");

    assert!(torcl_rt::packages::exists("RT-IMG-PKG"));
    assert!(torcl_rt::packages::exists("RIP"));
    assert_eq!(
        torcl_rt::packages::find("RIP"),
        torcl_rt::packages::find("RT-IMG-PKG"),
        "nickname must resolve to the same package after restore"
    );
    // Standard packages are still present.
    assert!(torcl_rt::packages::exists("COMMON-LISP"));
    assert!(torcl_rt::packages::exists("CL"));
}
