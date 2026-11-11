// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CLOSE :ABORT T on a file stream deletes a file OPEN created and leaves any
//! file it superseded in place (CLHS CLOSE; GitHub #1003).

use std::path::{Path, PathBuf};

use egcl_rt::value::{EgclVal, NIL};
use egcl_stdlib::pathnames::register_string;
use egcl_stdlib::streams::{
    ExternalFormat, IF_DOES_NOT_EXIST_CREATE_VAL, IF_EXISTS_APPEND_VAL, IF_EXISTS_RENAME_VAL,
    IF_EXISTS_SUPERSEDE_VAL, StreamDirection, close, make_lisp_string, make_lisp_string_fresh,
    open, stream_write_string,
};

fn scratch_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("close-abort-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn lisp_path(path: &Path) -> EgclVal {
    let text = path.to_str().unwrap();
    let val = make_lisp_string_fresh(text);
    register_string(val, text);
    val
}

fn write_new(path: &Path, direction: StreamDirection, if_exists: EgclVal, abort: bool) {
    let stream = open(
        lisp_path(path),
        direction,
        NIL,
        if_exists,
        IF_DOES_NOT_EXIST_CREATE_VAL,
        ExternalFormat::Utf8,
    )
    .unwrap();
    stream_write_string(stream, make_lisp_string("new\n"), 0, None).unwrap();
    close(stream, abort).unwrap();
}

/// Everything in DIR, so a leftover backup shows up as an extra name.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn aborting_a_new_file_deletes_it() {
    let dir = scratch_dir("new");
    let path = dir.join("f");
    write_new(
        &path,
        StreamDirection::Output,
        IF_EXISTS_SUPERSEDE_VAL,
        true,
    );
    assert!(names(&dir).is_empty());
    write_new(&path, StreamDirection::Io, IF_EXISTS_SUPERSEDE_VAL, true);
    assert!(names(&dir).is_empty());
}

#[test]
fn aborting_a_supersede_keeps_the_old_file() {
    let dir = scratch_dir("supersede");
    let path = dir.join("f");
    for direction in [StreamDirection::Output, StreamDirection::Io] {
        std::fs::write(&path, "old\n").unwrap();
        write_new(&path, direction, IF_EXISTS_SUPERSEDE_VAL, true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
        assert_eq!(names(&dir), ["f"]);

        write_new(&path, direction, IF_EXISTS_SUPERSEDE_VAL, false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(names(&dir), ["f"]);
    }
}

#[test]
fn rename_keeps_the_old_file_after_a_normal_close() {
    let dir = scratch_dir("rename");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    write_new(&path, StreamDirection::Output, IF_EXISTS_RENAME_VAL, true);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
    assert_eq!(names(&dir), ["f"]);

    write_new(&path, StreamDirection::Output, IF_EXISTS_RENAME_VAL, false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    assert_eq!(std::fs::read_to_string(dir.join("f.bak")).unwrap(), "old\n");
}

#[test]
fn aborting_an_append_keeps_the_file() {
    let dir = scratch_dir("append");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    write_new(&path, StreamDirection::Output, IF_EXISTS_APPEND_VAL, true);
    assert!(path.exists());
    assert!(std::fs::read_to_string(&path).unwrap().starts_with("old\n"));
}

#[cfg(unix)]
#[test]
fn superseding_keeps_the_mode_and_writes_through_a_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("mode");
    let target = dir.join("target");
    std::fs::write(&target, "old\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).unwrap();
    write_new(
        &target,
        StreamDirection::Output,
        IF_EXISTS_SUPERSEDE_VAL,
        false,
    );
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o750);

    let link = dir.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    write_new(
        &link,
        StreamDirection::Output,
        IF_EXISTS_SUPERSEDE_VAL,
        false,
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
}
