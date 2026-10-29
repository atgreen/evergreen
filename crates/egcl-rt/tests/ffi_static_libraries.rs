// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Runtime registration and logical handle lifetime, independent of dlopen.
use egcl_rt::ffi::{
    close_foreign_library, foreign_symbol, foreign_symbol_global, load_foreign_library,
    register_static_library,
};

extern "C" fn first() -> i32 {
    42
}
extern "C" fn other() -> i32 {
    9
}
static mut COUNTER: i32 = 7;

#[test]
fn linked_addresses_support_aliases_global_scope_and_nonreused_handles() {
    let entries = [
        ("egcl_static_test_first", first as *const ()),
        (
            "egcl_static_test_counter",
            std::ptr::addr_of_mut!(COUNTER).cast::<()>() as *const (),
        ),
    ];
    // SAFETY: these symbols are part of this executable for its whole lifetime.
    unsafe {
        register_static_library("egcl-static-test", &["egcl-static-alias"], &entries).unwrap();
        register_static_library("egcl-static-test", &["egcl-static-alias"], &entries).unwrap();
        assert_eq!(foreign_symbol_global(entries[0].0).unwrap(), entries[0].1);
        let handle = load_foreign_library("egcl-static-alias").unwrap();
        assert_eq!(foreign_symbol(handle, entries[0].0).unwrap(), entries[0].1);
        assert_eq!(foreign_symbol(handle, entries[1].0).unwrap(), entries[1].1);
        assert!(foreign_symbol(handle, "not_in_static_table").is_err());
        close_foreign_library(handle).unwrap();
        let fresh = load_foreign_library("egcl-static-test").unwrap();
        assert_ne!(fresh, handle);
        assert!(foreign_symbol(handle, entries[0].0).is_err());
        assert_eq!(foreign_symbol(fresh, entries[0].0).unwrap(), entries[0].1);
        close_foreign_library(fresh).unwrap();
        assert_eq!(foreign_symbol_global(entries[0].0).unwrap(), entries[0].1);
    }
}

#[test]
fn conflicting_registration_is_atomic() {
    let entries = [("egcl_static_atomic_first", first as *const ())];
    // SAFETY: both addresses are process-lifetime functions in this executable.
    unsafe {
        register_static_library("egcl-static-atomic", &[], &entries).unwrap();
        assert!(
            register_static_library(
                "egcl-static-conflict",
                &["egcl-static-new-alias"],
                &[(entries[0].0, other as *const ())]
            )
            .is_err()
        );
        assert!(load_foreign_library("egcl-static-new-alias").is_err());
        assert_eq!(foreign_symbol_global(entries[0].0).unwrap(), entries[0].1);
        assert!(
            register_static_library(
                "egcl-static-atomic",
                &[],
                &[("egcl_static_unpublished", other as *const ())]
            )
            .is_err()
        );
        assert!(foreign_symbol_global("egcl_static_unpublished").is_err());
        assert!(
            register_static_library(
                "egcl-static-null",
                &[],
                &[("egcl_static_null", std::ptr::null())]
            )
            .is_err()
        );
    }
}
