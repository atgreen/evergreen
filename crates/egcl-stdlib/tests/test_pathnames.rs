// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tests for `egcl_stdlib::pathnames` — pathname operations, logical pathnames,
//! component accessors, wildcard matching, and filesystem operations.
//!
//! These are red-phase tests: all implementations are currently `unimplemented!()`,
//! so every test is expected to fail (panic) until the real code is written.

use egcl_rt::error::EgclError;
use egcl_rt::value::{NIL, EgclVal};
use egcl_stdlib::pathnames::*;
use egcl_stdlib::streams;

// ── Helper ────────────────────────────────────────────────────────────

/// Build and register a real heap string for pathname operations.
fn make_string_val(s: &str) -> EgclVal {
    let val = streams::make_lisp_string(s);
    register_string(val, s);
    val
}

/// Make a keyword-style EgclVal (symbol-index tag 101).
///
/// This remains a test-only symbol sentinel; unlike heap objects, symbol values
/// are registry indices and are never dereferenced as pointers.
fn make_keyword_val(s: &str) -> EgclVal {
    let mut h: u64 = 0x517cc1b727220a95;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    EgclVal::from_raw((h & !0b111) | 0b101)
}

/// Generate a unique temporary path using the test name and a counter-like suffix.
fn temp_path(label: &str) -> String {
    format!("/tmp/egcl_test_{}_{}", label, std::process::id())
}

// ══════════════════════════════════════════════════════════════════════
// 1. parse_namestring
// ══════════════════════════════════════════════════════════════════════

#[test]
fn parse_namestring_simple_path() {
    let input = make_string_val("/usr/local/bin/egcl");
    let (pathname, position) = parse_namestring(input, None, None).unwrap();
    assert_eq!(position, "/usr/local/bin/egcl".len());
    assert_ne!(pathname, NIL);
}

#[test]
fn parse_namestring_with_host() {
    let input = make_string_val("/tmp/test.lisp");
    let host = make_string_val("localhost");
    let (pathname, _) = parse_namestring(input, Some(host), None).unwrap();
    // Verify host was stored — the real implementation must return the host
    // that was provided, not an arbitrary value.
    let got_host = pathname_host(pathname);
    assert_ne!(
        got_host, NIL,
        "host should be set when provided to parse_namestring"
    );
    assert_eq!(got_host, host);
}

#[test]
fn parse_namestring_with_default_pathname() {
    let input = make_string_val("foo.lisp");
    let default_pn = make_string_val("/home/user/src/");
    let (pathname, position) = parse_namestring(input, None, Some(default_pn)).unwrap();
    assert_eq!(position, "foo.lisp".len());
    assert_ne!(pathname, NIL);
}

#[test]
fn parse_namestring_returns_pathname_and_position() {
    let input = make_string_val("bar.txt");
    let result = parse_namestring(input, None, None);
    assert!(result.is_ok());
    let (pn, pos) = result.unwrap();
    assert_ne!(pn, NIL);
    assert_eq!(pos, "bar.txt".len());
}

// ══════════════════════════════════════════════════════════════════════
// 2. make_pathname
// ══════════════════════════════════════════════════════════════════════

#[test]
fn make_pathname_all_components() {
    let host = make_string_val("myhost");
    let device = make_string_val("sda1");
    let directory = make_string_val("/usr/local/lib");
    let name = make_string_val("core");
    let type_field = make_string_val("lisp");
    let version = make_keyword_val("NEWEST");
    let pn = make_pathname(host, device, directory, name, type_field, version).unwrap();
    assert_ne!(pn, NIL);
    // Each accessor must return the exact component that was passed in.
    // Interned test strings preserve identity, so bit-equality is appropriate.
    assert_eq!(pathname_host(pn), host);
    assert_eq!(pathname_device(pn), device);
    assert_eq!(pathname_directory(pn), directory);
    assert_eq!(pathname_name(pn), name);
    assert_eq!(pathname_type(pn), type_field);
    assert_eq!(pathname_version(pn), version);
}

#[test]
fn make_pathname_with_nil_components() {
    let pn = make_pathname(NIL, NIL, NIL, make_string_val("test"), NIL, NIL).unwrap();
    assert_ne!(pn, NIL);
    assert_eq!(pathname_host(pn), NIL);
    assert_eq!(pathname_name(pn), make_string_val("test"));
    assert_eq!(pathname_type(pn), NIL);
}

// ══════════════════════════════════════════════════════════════════════
// 3. merge_pathnames
// ══════════════════════════════════════════════════════════════════════

#[test]
fn merge_pathnames_fills_missing_components() {
    let name_only = make_pathname(NIL, NIL, NIL, make_string_val("foo"), NIL, NIL).unwrap();
    let default_pn = make_pathname(
        make_string_val("defaulthost"),
        make_string_val("dev0"),
        make_string_val("/home/user/"),
        make_string_val("default"),
        make_string_val("txt"),
        make_keyword_val("NEWEST"),
    )
    .unwrap();
    let merged = merge_pathnames(name_only, default_pn, make_keyword_val("NEWEST")).unwrap();
    // name came from primary
    assert_eq!(pathname_name(merged), make_string_val("foo"));
    // host, device, type came from default (since primary had NIL)
    assert_eq!(pathname_host(merged), make_string_val("defaulthost"));
    assert_eq!(pathname_device(merged), make_string_val("dev0"));
    assert_eq!(pathname_type(merged), make_string_val("txt"));
}

#[test]
fn merge_pathnames_preserves_existing_components() {
    let full = make_pathname(
        make_string_val("h"),
        make_string_val("d"),
        make_string_val("/a/"),
        make_string_val("n"),
        make_string_val("t"),
        make_keyword_val("NEWEST"),
    )
    .unwrap();
    let default_pn = make_pathname(
        make_string_val("X"),
        make_string_val("Y"),
        make_string_val("/Z/"),
        make_string_val("W"),
        make_string_val("Q"),
        make_keyword_val("NEWEST"),
    )
    .unwrap();
    let merged = merge_pathnames(full, default_pn, make_keyword_val("NEWEST")).unwrap();
    assert_eq!(pathname_host(merged), make_string_val("h"));
    assert_eq!(pathname_name(merged), make_string_val("n"));
}

// ══════════════════════════════════════════════════════════════════════
// 4. namestring
// ══════════════════════════════════════════════════════════════════════

#[test]
fn namestring_roundtrip() {
    let input = make_string_val("/usr/local/lib/core.lisp");
    let (pn, _) = parse_namestring(input, None, None).unwrap();
    let ns = namestring(pn).unwrap();
    let (pn2, _) = parse_namestring(ns, None, None).unwrap();
    assert_eq!(namestring(pn).unwrap(), namestring(pn2).unwrap());
}

#[test]
fn namestring_of_root_path() {
    let input = make_string_val("/");
    let (pn, _) = parse_namestring(input, None, None).unwrap();
    let rendered = namestring(pn).unwrap();
    assert_ne!(rendered, NIL);
    assert!(
        rendered.is_string(),
        "a namestring must be a real heap string"
    );
}

#[test]
fn synthesized_namestring_has_a_readable_string_header() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_string_val("fresh-name"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    let rendered = namestring(pn).unwrap();
    assert!(rendered.is_string());
    assert_eq!(rendered.as_string(), "fresh-name.lisp");
}

// ══════════════════════════════════════════════════════════════════════
// 5. Component accessors
// ══════════════════════════════════════════════════════════════════════

#[test]
fn pathname_host_returns_correct_component() {
    let host = make_string_val("testhost");
    let pn = make_pathname(host, NIL, NIL, NIL, NIL, NIL).unwrap();
    assert_eq!(pathname_host(pn), host);
}

#[test]
fn pathname_device_returns_correct_component() {
    let device = make_string_val("sda1");
    let pn = make_pathname(NIL, device, NIL, NIL, NIL, NIL).unwrap();
    assert_eq!(pathname_device(pn), device);
}

#[test]
fn pathname_directory_returns_correct_component() {
    let dir = make_string_val("/home/user/");
    let pn = make_pathname(NIL, NIL, dir, NIL, NIL, NIL).unwrap();
    assert_eq!(pathname_directory(pn), dir);
}

#[test]
fn pathname_name_returns_correct_component() {
    let name = make_string_val("myfile");
    let pn = make_pathname(NIL, NIL, NIL, name, NIL, NIL).unwrap();
    assert_eq!(pathname_name(pn), name);
}

#[test]
fn pathname_type_returns_correct_component() {
    let typ = make_string_val("lisp");
    let pn = make_pathname(NIL, NIL, NIL, NIL, typ, NIL).unwrap();
    assert_eq!(pathname_type(pn), typ);
}

#[test]
fn pathname_version_returns_correct_component() {
    let ver = make_keyword_val("NEWEST");
    let pn = make_pathname(NIL, NIL, NIL, NIL, NIL, ver).unwrap();
    assert_eq!(pathname_version(pn), ver);
}

// ══════════════════════════════════════════════════════════════════════
// 6. pathname_match_p — wildcard matching
// ══════════════════════════════════════════════════════════════════════

#[test]
fn pathname_match_p_exact_match() {
    let pn = make_pathname(
        NIL,
        NIL,
        make_string_val("/usr/local/"),
        make_string_val("test"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    let wildcard = make_pathname(
        NIL,
        NIL,
        make_string_val("/usr/local/"),
        make_string_val("test"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    assert!(pathname_match_p(pn, wildcard).unwrap());
}

#[test]
fn pathname_match_p_wild_name() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_string_val("anything"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    let wildcard = make_pathname(
        NIL,
        NIL,
        NIL,
        make_keyword_val("WILD"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    assert!(pathname_match_p(pn, wildcard).unwrap());
}

#[test]
fn pathname_match_p_wild_type() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_string_val("foo"),
        make_string_val("txt"),
        NIL,
    )
    .unwrap();
    let wildcard = make_pathname(
        NIL,
        NIL,
        NIL,
        make_string_val("foo"),
        make_keyword_val("WILD"),
        NIL,
    )
    .unwrap();
    assert!(pathname_match_p(pn, wildcard).unwrap());
}

#[test]
fn pathname_match_p_no_match() {
    let pn = make_pathname(NIL, NIL, NIL, make_string_val("alpha"), NIL, NIL).unwrap();
    let wc = make_pathname(NIL, NIL, NIL, make_string_val("beta"), NIL, NIL).unwrap();
    assert!(!pathname_match_p(pn, wc).unwrap());
}

// ══════════════════════════════════════════════════════════════════════
// 7. wild_pathname_p — wildcard detection
// ══════════════════════════════════════════════════════════════════════

#[test]
fn wild_pathname_p_no_wildcards() {
    let pn = make_pathname(
        NIL,
        NIL,
        make_string_val("/home/"),
        make_string_val("file"),
        make_string_val("txt"),
        NIL,
    )
    .unwrap();
    assert!(!wild_pathname_p(pn, None));
}

#[test]
fn wild_pathname_p_wild_name_field_none() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_keyword_val("WILD"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    assert!(wild_pathname_p(pn, None));
}

#[test]
fn wild_pathname_p_specific_field_name() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_keyword_val("WILD"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    assert!(wild_pathname_p(pn, Some(make_keyword_val("NAME"))));
}

#[test]
fn wild_pathname_p_specific_field_type_not_wild() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_keyword_val("WILD"),
        make_string_val("lisp"),
        NIL,
    )
    .unwrap();
    assert!(!wild_pathname_p(pn, Some(make_keyword_val("TYPE"))));
}

#[test]
fn wild_pathname_p_wild_in_type() {
    let pn = make_pathname(
        NIL,
        NIL,
        NIL,
        make_string_val("foo"),
        make_keyword_val("WILD"),
        NIL,
    )
    .unwrap();
    assert!(wild_pathname_p(pn, Some(make_keyword_val("TYPE"))));
}

// ══════════════════════════════════════════════════════════════════════
// 8. Logical pathname translation
// ══════════════════════════════════════════════════════════════════════

#[test]
fn set_and_get_logical_pathname_translations_roundtrip() {
    let tv = make_string_val("((\"SYS:SRC;**;*.*.*\" \"/opt/egcl/src/**/*.*\"))");
    set_logical_pathname_translations("SYS", tv).unwrap();
    assert_eq!(logical_pathname_translations("SYS").unwrap(), tv);
}

#[test]
fn translate_logical_pathname_after_setting_translations() {
    let tv = make_string_val("((\"SYS:SRC;**;*.*.*\" \"/opt/egcl/src/**/*.*\"))");
    set_logical_pathname_translations("MYSYS", tv).unwrap();
    let logical_pn = make_string_val("MYSYS:SRC;COMPILER;IR.LISP");
    let (pn, _) = parse_namestring(logical_pn, None, None).unwrap();
    let physical = translate_logical_pathname(pn).unwrap();
    assert_ne!(physical, NIL);
    assert_eq!(pathname_host(physical), NIL); // physical POSIX → no host
}

// ══════════════════════════════════════════════════════════════════════
// 9. Filesystem operations
// ══════════════════════════════════════════════════════════════════════

#[test]
fn probe_file_existing_file() {
    let path = make_string_val(file!());
    let result = probe_file(path).unwrap();
    assert!(result.is_some());
    assert_ne!(result.unwrap(), NIL);
}

#[test]
fn probe_file_nonexistent() {
    let path = make_string_val("/nonexistent/path/to/file.xyz");
    assert!(probe_file(path).unwrap().is_none());
}

#[test]
fn truename_resolves_pathname() {
    let path = make_string_val(file!());
    let resolved = truename(path).unwrap();
    assert_ne!(resolved, NIL);
}

#[test]
fn directory_traverses_exact_wildcard_levels() {
    let root = temp_path("directory_wild_levels");
    let files = [
        "distinfo.txt",
        "one/distinfo.txt",
        "two/distinfo.txt",
        "one/child/distinfo.txt",
        "one/child/deep/distinfo.txt",
    ];
    for file in files {
        let path = std::path::Path::new(&root).join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture").unwrap();
    }
    for (suffix, expected) in [
        (
            "*/distinfo.txt",
            vec!["one/distinfo.txt", "two/distinfo.txt"],
        ),
        ("*/*/distinfo.txt", vec!["one/child/distinfo.txt"]),
        ("*/child/distinfo.txt", vec!["one/child/distinfo.txt"]),
        (
            "*/child/*/distinfo.txt",
            vec!["one/child/deep/distinfo.txt"],
        ),
        ("*/", vec!["one/", "two/"]),
        ("*/*/", vec!["one/child/"]),
        ("*/missing/distinfo.txt", vec![]),
        ("*/distinfo.txt/*.lisp", vec![]),
        ("**/distinfo.txt", files.to_vec()),
        ("**/**/distinfo.txt", files.to_vec()),
    ] {
        let mut expected: Vec<_> = expected
            .iter()
            .map(|path| format!("{root}/{path}"))
            .collect();
        expected.sort();
        for parsed in [false, true] {
            let designator = make_string_val(&format!("{root}/{suffix}"));
            let pattern = if parsed {
                parse_namestring(designator, None, None).unwrap().0
            } else {
                designator
            };
            let entries = directory(pattern).unwrap();
            let actual: Vec<_> = entries
                .iter()
                .map(|entry| namestring(*entry).unwrap().as_string().to_owned())
                .collect();
            assert_eq!(actual, expected, "{suffix}, parsed={parsed}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn probe_file_accepts_open_and_closed_file_streams() {
    use egcl_rt::value::T;
    use egcl_stdlib::streams::{close, open, ExternalFormat, StreamDirection};
    let file = temp_path("probe_file_stream");
    std::fs::write(&file, "fixture").unwrap();
    for direction in [
        StreamDirection::Input,
        StreamDirection::Output,
        StreamDirection::Io,
        StreamDirection::Probe,
    ] {
        egcl_rt::rooted!(
            stream = open(
                make_string_val(&file),
                direction,
                NIL,
                T,
                T,
                ExternalFormat::Utf8,
            )
            .unwrap()
        );
        let expected = std::fs::canonicalize(&file).unwrap();
        let probed = probe_file(*stream).unwrap().unwrap();
        assert_eq!(
            namestring(probed).unwrap().as_string(),
            expected.to_string_lossy()
        );
        close(*stream, false).unwrap();
        let probed = probe_file(*stream).unwrap().unwrap();
        assert_eq!(
            namestring(probed).unwrap().as_string(),
            expected.to_string_lossy()
        );
    }
    egcl_rt::rooted!(
        stream = open(
            make_string_val(&file),
            StreamDirection::Input,
            NIL,
            T,
            T,
            ExternalFormat::Utf8,
        )
        .unwrap()
    );
    close(*stream, false).unwrap();
    std::fs::remove_file(&file).unwrap();
    assert!(probe_file(*stream).unwrap().is_none());
}

#[test]
#[cfg(unix)]
fn directory_does_not_enter_unmatched_literal_subtrees() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::path::PathBuf::from(temp_path("directory_literal_pruning"));
    let matching = root.join("one/child/distinfo.txt");
    let unrelated = root.join("one/unrelated");
    std::fs::create_dir_all(matching.parent().unwrap()).unwrap();
    std::fs::create_dir(&unrelated).unwrap();
    std::fs::write(&matching, "fixture").unwrap();
    std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o000)).unwrap();
    let permission_denied = std::fs::read_dir(&unrelated).is_err();
    let result = directory(make_string_val(&format!(
        "{}/*/child/distinfo.txt",
        root.display()
    )));
    std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    if !permission_denied {
        eprintln!("permission regression requires an unprivileged user; skipping assertion");
        return;
    }
    let entries = result.expect("an unrelated unreadable directory must not affect matches");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        namestring(entries[0]).unwrap().as_string(),
        matching.to_string_lossy()
    );
}

#[test]
fn directory_wildcards_under_missing_roots_return_no_matches() {
    let root = temp_path("directory_missing_root");
    assert!(!std::path::Path::new(&root).exists());
    for suffix in ["*.lisp", "*.cl", "*/*.lisp", "**/*.lisp"] {
        let pattern = format!("{root}/{suffix}");
        let entries = directory(make_string_val(&pattern))
            .unwrap_or_else(|error| panic!("{pattern}: {error:?}"));
        assert!(entries.is_empty(), "{pattern} matched nonexistent files");
    }
}

#[test]
fn directory_does_not_hide_other_filesystem_errors() {
    assert!(matches!(
        directory(make_string_val("/tmp/invalid\0path/*.lisp")),
        Err(EgclError::FileError(_))
    ));
}

#[test]
fn directory_lists_contents() {
    // /tmp should always have entries on a Unix system
    let entries = directory(make_string_val("/tmp/*")).unwrap();
    assert!(
        !entries.is_empty(),
        "directory() on /tmp/* should return at least one entry"
    );
    // Each entry should be a non-NIL pathname
    for entry in &entries {
        assert_ne!(*entry, NIL, "directory entry should not be NIL");
    }
}

#[test]
fn ensure_directories_exist_includes_directory_only_leaf() {
    for (label, parsed) in [("directory_string", false), ("directory_pathname", true)] {
        let root = temp_path(label);
        let leaf = format!("{root}/dists/example/");
        let designator = make_string_val(&leaf);
        let path = if parsed {
            parse_namestring(designator, None, None).unwrap().0
        } else {
            designator
        };
        let (returned, created) = ensure_directories_exist(path).unwrap();
        assert_eq!(returned, path);
        assert!(created);
        assert!(
            std::path::Path::new(&leaf).is_dir(),
            "missing directory-only leaf: {leaf}"
        );
        assert!(!ensure_directories_exist(path).unwrap().1);
        std::fs::write(format!("{leaf}distinfo.txt"), "test").unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn ensure_directories_exist_creates_dirs() {
    let dir_path = temp_path("ensure_dirs");
    let full_path = format!("{}/a/b/c/file.txt", dir_path);

    // Clean up before test to ensure idempotency
    let _ = std::fs::remove_dir_all(&dir_path);

    let path = make_string_val(&full_path);
    let (returned_pn, created) = ensure_directories_exist(path).unwrap();
    assert_ne!(returned_pn, NIL);
    assert!(created, "directories should have been freshly created");

    // Verify the directory structure actually exists on disk
    let parent = std::path::Path::new(&full_path).parent().unwrap();
    assert!(
        parent.is_dir(),
        "parent directory should exist after ensure_directories_exist"
    );

    // Clean up after test
    let _ = std::fs::remove_dir_all(&dir_path);
}

#[test]
fn delete_file_removes_existing_file() {
    let file_path = temp_path("delete_file.tmp");

    // Actually create the file first
    std::fs::write(&file_path, b"test content").expect("failed to create temp file for test");
    assert!(
        std::path::Path::new(&file_path).exists(),
        "temp file should exist before delete_file"
    );

    let path = make_string_val(&file_path);
    delete_file(path).expect("delete_file should succeed on existing file");

    // Verify the file is actually gone
    assert!(
        !std::path::Path::new(&file_path).exists(),
        "file should no longer exist after delete_file"
    );
}

#[test]
fn rename_file_returns_three_values() {
    let old_path = temp_path("rename_old.tmp");
    let new_path = temp_path("rename_new.tmp");

    // Create source file and ensure destination doesn't exist
    std::fs::write(&old_path, b"rename test content").expect("failed to create source file");
    let _ = std::fs::remove_file(&new_path);

    let old = make_string_val(&old_path);
    let new_name = make_string_val(&new_path);
    let (defaulted_new, old_truename, new_truename) =
        rename_file(old, new_name).expect("rename_file should succeed");

    assert_ne!(defaulted_new, NIL, "defaulted-new-name should not be NIL");
    assert_ne!(old_truename, NIL, "old-truename should not be NIL");
    assert_ne!(new_truename, NIL, "new-truename should not be NIL");

    // Verify old file is gone and new file exists
    assert!(
        !std::path::Path::new(&old_path).exists(),
        "old file should not exist after rename"
    );
    assert!(
        std::path::Path::new(&new_path).exists(),
        "new file should exist after rename"
    );

    // Clean up
    let _ = std::fs::remove_file(&new_path);
}

// ══════════════════════════════════════════════════════════════════════
// 10. Error conditions
// ══════════════════════════════════════════════════════════════════════

#[test]
fn parse_namestring_malformed_input() {
    let bad_input = EgclVal(0xDEAD_BEEF_DEAD_BEEF);
    assert!(parse_namestring(bad_input, None, None).is_err());
}

#[test]
fn delete_file_nonexistent_errors() {
    let result = delete_file(make_string_val("/nonexistent/path/file_to_delete.tmp"));
    assert!(matches!(result, Err(EgclError::FileError(_))));
}

#[test]
fn rename_file_nonexistent_source_errors() {
    let result = rename_file(
        make_string_val("/nonexistent/source_file.tmp"),
        make_string_val("/tmp/destination.tmp"),
    );
    assert!(matches!(result, Err(EgclError::FileError(_))));
}

#[test]
fn translate_logical_pathname_no_translations_set() {
    let logical_pn = make_string_val("UNDEFINED-HOST:FILE.LISP");
    let (pn, _) = parse_namestring(logical_pn, None, None).unwrap();
    assert!(translate_logical_pathname(pn).is_err());
}

#[test]
fn open_with_nonexistent_directory_errors() {
    // CL `OPEN` with :direction :output on a path whose parent directory
    // does not exist should signal a FILE-ERROR.
    let path = make_string_val("/nonexistent_dir_egcl_test/subdir/file.lisp");
    let result = streams::open(
        path,
        streams::StreamDirection::Output,
        NIL, // default element-type
        NIL, // if-exists
        NIL, // if-does-not-exist
        streams::ExternalFormat::Utf8,
    );
    assert!(
        matches!(result, Err(EgclError::FileError(_))),
        "open on path with non-existent directory should return FileError"
    );
}

#[test]
fn truename_nonexistent_file_errors() {
    let result = truename(make_string_val("/nonexistent/truename_test.xyz"));
    assert!(matches!(result, Err(EgclError::FileError(_))));
}

#[test]
fn ensure_directories_exist_permission_error() {
    let result = ensure_directories_exist(make_string_val("/proc/egcl_impossible_dir/file.txt"));
    assert!(matches!(result, Err(EgclError::FileError(_))));
}

// ══════════════════════════════════════════════════════════════════════
// 11. Additional edge cases
// ══════════════════════════════════════════════════════════════════════

#[test]
fn logical_pathname_translations_unknown_host() {
    let result = logical_pathname_translations("NEVER_DEFINED_HOST_XYZ");
    assert!(result.is_err());
}

#[test]
fn parse_namestring_empty_string() {
    let input = make_string_val("");
    let (pn, pos) = parse_namestring(input, None, None).unwrap();
    assert_eq!(pos, 0);
    assert_eq!(pathname_name(pn), NIL);
    assert_eq!(pathname_type(pn), NIL);
}

#[test]
fn parse_namestring_trailing_slash() {
    let input = make_string_val("/tmp/");
    let (pn, pos) = parse_namestring(input, None, None).unwrap();
    assert_eq!(pos, "/tmp/".len());
    assert_eq!(pathname_name(pn), NIL);
}

#[test]
fn parse_namestring_dot_file() {
    let input = make_string_val(".gitignore");
    let (pn, _) = parse_namestring(input, None, None).unwrap();
    assert_eq!(pathname_name(pn), make_string_val(".gitignore"));
    assert_eq!(pathname_type(pn), NIL);
}

#[test]
fn parse_namestring_multiple_extensions() {
    let input = make_string_val("foo.tar.gz");
    let (pn, _) = parse_namestring(input, None, None).unwrap();
    assert_eq!(pathname_name(pn), make_string_val("foo.tar"));
    assert_eq!(pathname_type(pn), make_string_val("gz"));
}

#[test]
fn pathname_match_p_wild_name_matches_nil() {
    let pn = make_pathname(NIL, NIL, NIL, NIL, NIL, NIL).unwrap();
    let wildcard = make_pathname(NIL, NIL, NIL, make_keyword_val("WILD"), NIL, NIL).unwrap();
    assert!(pathname_match_p(pn, wildcard).unwrap());
}

#[test]
fn wild_pathname_p_host_never_wild_posix() {
    let pn = make_pathname(NIL, NIL, NIL, make_keyword_val("WILD"), NIL, NIL).unwrap();
    let host_field = make_keyword_val("HOST");
    assert!(!wild_pathname_p(pn, Some(host_field)));
}

#[test]
fn merge_pathnames_version_from_default_version_arg() {
    let primary = make_pathname(NIL, NIL, NIL, make_string_val("foo"), NIL, NIL).unwrap();
    let default_pn = make_pathname(NIL, NIL, NIL, NIL, NIL, NIL).unwrap();
    let newest = make_keyword_val("NEWEST");
    let merged = merge_pathnames(primary, default_pn, newest).unwrap();
    assert_eq!(pathname_version(merged), newest);
}
