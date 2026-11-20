// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! CLOSE :ABORT T on a file stream deletes a file OPEN created and leaves any
//! file it superseded in place (CLHS CLOSE; GitHub #1003).

use std::path::{Path, PathBuf};

use egcl_rt::error::EgclError;
use egcl_rt::value::{EgclVal, NIL};
use egcl_stdlib::pathnames::register_string;
use egcl_stdlib::streams::{
    ExternalFormat, IF_DOES_NOT_EXIST_CREATE_VAL, IF_EXISTS_APPEND_VAL, IF_EXISTS_RENAME_VAL,
    IF_EXISTS_SUPERSEDE_VAL, StreamDirection, close, make_lisp_string, make_lisp_string_fresh,
    open, stream_finish_output, stream_write_string,
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
    egcl_rt::rooted!(
        stream = open(
            lisp_path(path),
            direction,
            NIL,
            if_exists,
            IF_DOES_NOT_EXIST_CREATE_VAL,
            ExternalFormat::Utf8,
        )
        .unwrap()
    );
    egcl_rt::rooted!(text = make_lisp_string("new\n"));
    stream_write_string(*stream, *text, 0, None).unwrap();
    close(*stream, abort).unwrap();
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

#[test]
fn rename_does_not_replace_an_existing_backup() {
    let dir = scratch_dir("backup-collision");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    std::fs::write(dir.join("f.bak"), "unrelated\n").unwrap();
    write_new(&path, StreamDirection::Output, IF_EXISTS_RENAME_VAL, false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("f.bak")).unwrap(),
        "unrelated\n"
    );
    let old_copies: Vec<_> = names(&dir)
        .into_iter()
        .filter(|name| {
            name != "f"
                && name != "f.bak"
                && std::fs::read_to_string(dir.join(name)).unwrap() == "old\n"
        })
        .collect();
    assert_eq!(old_copies.len(), 1);
}

#[cfg(unix)]
#[test]
fn rename_does_not_replace_a_dangling_backup_symlink() {
    let dir = scratch_dir("backup-symlink");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    std::os::unix::fs::symlink("missing", dir.join("f.bak")).unwrap();
    write_new(&path, StreamDirection::Output, IF_EXISTS_RENAME_VAL, true);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
    assert_eq!(
        std::fs::read_link(dir.join("f.bak")).unwrap(),
        Path::new("missing")
    );
    assert_eq!(names(&dir), ["f", "f.bak"]);
}

#[test]
fn failed_abort_preserves_backup_and_independent_replacement() {
    let dir = scratch_dir("failed-abort");
    let path = dir.join("f");
    for directory in [true, false] {
        std::fs::write(&path, "old\n").unwrap();
        egcl_rt::rooted!(
            stream = open(
                lisp_path(&path),
                StreamDirection::Output,
                NIL,
                IF_EXISTS_SUPERSEDE_VAL,
                IF_DOES_NOT_EXIST_CREATE_VAL,
                ExternalFormat::Utf8,
            )
            .unwrap()
        );
        // Keep the actual output alive under another name so inode reuse
        // cannot make the independent replacement look like the opened file.
        std::fs::rename(&path, dir.join("output")).unwrap();
        if directory {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, "independent\n").unwrap();
        }
        assert!(close(*stream, true).is_err());
        if directory {
            assert!(path.is_dir());
            std::fs::remove_dir(&path).unwrap();
        } else {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "independent\n");
            std::fs::remove_file(&path).unwrap();
        }
        let backups: Vec<_> = names(&dir)
            .into_iter()
            .filter(|name| name != "output")
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.join(&backups[0])).unwrap(),
            "old\n"
        );
        std::fs::remove_file(dir.join(&backups[0])).unwrap();
        std::fs::remove_file(dir.join("output")).unwrap();
    }
}

#[test]
fn superseding_a_directory_does_not_move_it_aside() {
    let dir = scratch_dir("directory");
    let path = dir.join("f");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep"), "old\n").unwrap();
    assert!(
        open(
            lisp_path(&path),
            StreamDirection::Output,
            NIL,
            IF_EXISTS_SUPERSEDE_VAL,
            IF_DOES_NOT_EXIST_CREATE_VAL,
            ExternalFormat::Utf8,
        )
        .is_err()
    );
    assert_eq!(names(&dir), ["f"]);
    assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "old\n");
}

#[cfg(unix)]
#[test]
fn rename_without_directory_write_permission_never_truncates() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("directory-permission");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = open(
        lisp_path(&path),
        StreamDirection::Output,
        NIL,
        IF_EXISTS_RENAME_VAL,
        IF_DOES_NOT_EXIST_CREATE_VAL,
        ExternalFormat::Utf8,
    );
    // Restore before assertions so a failed regression does not leave an
    // unwritable test directory. This runs as an unprivileged test user.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
    assert_eq!(names(&dir), ["f"]);
}

#[cfg(unix)]
#[test]
fn superseding_a_read_only_file_does_not_bypass_its_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("file-permission");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let result = open(
        lisp_path(&path),
        StreamDirection::Output,
        NIL,
        IF_EXISTS_SUPERSEDE_VAL,
        IF_DOES_NOT_EXIST_CREATE_VAL,
        ExternalFormat::Utf8,
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");
    assert_eq!(names(&dir), ["f"]);
}

#[cfg(unix)]
#[test]
fn failed_abort_deletion_keeps_the_only_backup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("abort-permission");
    let path = dir.join("f");
    std::fs::write(&path, "old\n").unwrap();
    egcl_rt::rooted!(
        stream = open(
            lisp_path(&path),
            StreamDirection::Output,
            NIL,
            IF_EXISTS_SUPERSEDE_VAL,
            IF_DOES_NOT_EXIST_CREATE_VAL,
            ExternalFormat::Utf8,
        )
        .unwrap()
    );
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = close(*stream, true);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert!(path.is_file());
    let backups: Vec<_> = names(&dir).into_iter().filter(|name| name != "f").collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.join(&backups[0])).unwrap(),
        "old\n"
    );
}

#[test]
fn close_uses_the_opening_directory_after_cwd_changes() {
    const CHILD: &str = "EGCL_TEST_ABORT_CWD_DIR";
    if let Some(dir) = std::env::var_os(CHILD) {
        let dir = PathBuf::from(dir);
        for abort in [true, false] {
            std::env::set_current_dir(&dir).unwrap();
            std::fs::write("f", "old\n").unwrap();
            egcl_rt::rooted!(
                stream = open(
                    lisp_path(Path::new("f")),
                    StreamDirection::Output,
                    NIL,
                    IF_EXISTS_SUPERSEDE_VAL,
                    IF_DOES_NOT_EXIST_CREATE_VAL,
                    ExternalFormat::Utf8,
                )
                .unwrap()
            );
            egcl_rt::rooted!(text = make_lisp_string("new\n"));
            stream_write_string(*stream, *text, 0, None).unwrap();
            std::env::set_current_dir(dir.join("changed")).unwrap();
            close(*stream, abort).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.join("f")).unwrap(),
                if abort { "old\n" } else { "new\n" }
            );
            assert_eq!(names(&dir), ["changed", "f"]);
            assert!(names(&dir.join("changed")).is_empty());
        }
        return;
    }
    // Change cwd only in a fresh process; sibling tests keep their cwd.
    let dir = scratch_dir("cwd");
    std::fs::create_dir(dir.join("changed")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "close_uses_the_opening_directory_after_cwd_changes",
            "--nocapture",
        ])
        .env(CHILD, &dir)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn supersede_private_mode_is_preserved_without_privilege_bits() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("private-mode");
    let path = dir.join("f");
    for (mode, abort) in [
        (0o600, false),
        (0o600, true),
        (0o4750, false),
        (0o2750, true),
    ] {
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let original_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        egcl_rt::rooted!(
            stream = open(
                lisp_path(&path),
                StreamDirection::Output,
                NIL,
                IF_EXISTS_SUPERSEDE_VAL,
                IF_DOES_NOT_EXIST_CREATE_VAL,
                ExternalFormat::Utf8,
            )
            .unwrap()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            mode & 0o777
        );
        egcl_rt::rooted!(text = make_lisp_string("new\n"));
        stream_write_string(*stream, *text, 0, None).unwrap();
        close(*stream, abort).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            if abort { original_mode } else { mode & 0o777 }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            if abort { "old\n" } else { "new\n" }
        );
        assert_eq!(names(&dir), ["f"]);
    }
}

#[cfg(unix)]
#[test]
fn superseding_a_fifo_keeps_a_single_write_through_open() {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    const CHILD: &str = "EGCL_TEST_ABORT_FIFO_DIR";
    if let Some(dir) = std::env::var_os(CHILD) {
        let dir = PathBuf::from(dir);
        let path = dir.join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            std::fs::File::open(reader_path)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        });
        write_new(
            &path,
            StreamDirection::Output,
            IF_EXISTS_SUPERSEDE_VAL,
            false,
        );
        assert_eq!(reader.join().unwrap(), b"new\n");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_fifo()
        );
        assert_eq!(names(&dir), ["fifo"]);
        return;
    }
    // Bound a FIFO regression in its own process, so a failed extra open
    // reports an assertion instead of hanging the full integration target.
    let dir = scratch_dir("fifo");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "superseding_a_fifo_keeps_a_single_write_through_open",
        ])
        .env(CHILD, &dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("FIFO output did not finish with its single reader");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn flushed_output(path: &Path, direction: StreamDirection, if_exists: EgclVal) -> EgclVal {
    egcl_rt::rooted!(
        stream = open(
            lisp_path(path),
            direction,
            NIL,
            if_exists,
            IF_DOES_NOT_EXIST_CREATE_VAL,
            ExternalFormat::Utf8,
        )
        .unwrap()
    );
    egcl_rt::rooted!(text = make_lisp_string("new\n"));
    stream_write_string(*stream, *text, 0, None).unwrap();
    stream_finish_output(*stream).unwrap();
    *stream
}

#[test]
fn aborting_with_a_substituted_backup_preserves_all_files() {
    let dir = scratch_dir("substituted-backup");
    let path = dir.join("f");
    let backup = dir.join("f.bak");
    let saved = dir.join("saved");
    for direction in [StreamDirection::Output, StreamDirection::Io] {
        std::fs::write(&path, "old\n").unwrap();
        egcl_rt::rooted!(stream = flushed_output(&path, direction, IF_EXISTS_RENAME_VAL));
        std::fs::rename(&backup, &saved).unwrap();
        std::fs::write(&backup, "independent\n").unwrap();
        assert!(matches!(close(*stream, true), Err(EgclError::FileError(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "independent\n");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "old\n");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&backup).unwrap();
        std::fs::remove_file(&saved).unwrap();
    }
}

#[test]
fn aborting_with_a_missing_backup_preserves_output() {
    let dir = scratch_dir("missing-backup");
    let path = dir.join("f");
    let backup = dir.join("f.bak");
    let saved = dir.join("saved");
    for direction in [StreamDirection::Output, StreamDirection::Io] {
        std::fs::write(&path, "old\n").unwrap();
        egcl_rt::rooted!(stream = flushed_output(&path, direction, IF_EXISTS_RENAME_VAL));
        std::fs::rename(&backup, &saved).unwrap();
        assert!(matches!(close(*stream, true), Err(EgclError::FileError(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert!(!backup.exists());
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "old\n");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&saved).unwrap();
    }
}

#[test]
fn normal_cleanup_preserves_a_substituted_private_backup() {
    let dir = scratch_dir("normal-backup-substitution");
    let path = dir.join("f");
    let saved = dir.join("saved");
    // Unix also permits unlink/recreate while the retained identity handle
    // keeps the old inode alive. Windows exercises rename/replacement.
    let moves: &[bool] = if cfg!(unix) { &[true, false] } else { &[true] };
    for &move_original in moves {
        std::fs::write(&path, "old\n").unwrap();
        egcl_rt::rooted!(
            stream = flushed_output(&path, StreamDirection::Output, IF_EXISTS_SUPERSEDE_VAL)
        );
        let backups: Vec<_> = names(&dir)
            .into_iter()
            .filter(|name| name.starts_with(".f.egcl-"))
            .collect();
        assert_eq!(backups.len(), 1);
        let backup = dir.join(&backups[0]);
        if move_original {
            std::fs::rename(&backup, &saved).unwrap();
        } else {
            std::fs::remove_file(&backup).unwrap();
        }
        std::fs::write(&backup, "independent\n").unwrap();
        close(*stream, false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "independent\n");
        if move_original {
            assert_eq!(std::fs::read_to_string(&saved).unwrap(), "old\n");
            std::fs::remove_file(&saved).unwrap();
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&backup).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn dangling_symlink_write_through_does_not_claim_link_ownership() {
    let dir = scratch_dir("dangling-write-through");
    let link = dir.join("link");
    let target = dir.join("target");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    write_new(
        &link,
        StreamDirection::Output,
        IF_EXISTS_SUPERSEDE_VAL,
        false,
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    std::fs::remove_file(&target).unwrap();
    egcl_rt::rooted!(stream = flushed_output(&link, StreamDirection::Io, IF_EXISTS_SUPERSEDE_VAL));
    close(*stream, true).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(names(&dir), ["link", "target"]);
}
