// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Shaking drops redundant string caches without dropping pathname semantics.
use egcl_stdlib::{pathnames::*, streams::make_lisp_string};

#[test]
fn logical_translations_survive_shake_cache_cleanup() {
    egcl_rt::gc::ensure_heap_initialized();
    let text = "((\"**;*.*.*\" \"/tmp/**/*.*\"))";
    egcl_rt::rooted!(translations = egcl_rt::gc::alloc_character_string(text));
    register_string(*translations, text);
    set_logical_pathname_translations("SHAKE", *translations).unwrap();
    egcl_rt::rooted!(source = make_lisp_string("SHAKE:DIR;FILE.LISP"));
    let (logical, _) = parse_namestring(*source, None, None).unwrap();
    let expected = namestring(translate_logical_pathname(logical).unwrap())
        .unwrap()
        .as_string();
    clear_shake_string_caches().unwrap();
    assert!(registered_string(*translations).is_none());
    let physical = translate_logical_pathname(logical).unwrap();
    assert_eq!(namestring(physical).unwrap().as_string(), expected);
}
