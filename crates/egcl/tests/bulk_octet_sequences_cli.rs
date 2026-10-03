// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! READ-SEQUENCE and WRITE-SEQUENCE on byte streams transfer octets in bulk.
//!
//! The bulk paths exist because the per-element ones were pathological: reading
//! a 6.76 MB file took 1.125 s (a dispatch per octet plus a `Vec<EgclVal>` eight
//! times the size of the data) and writing 6.8 MB took 3.968 s (the stream lock
//! AND a heap allocation per octet). Together they were two thirds of a
//! self-hosted APK build. These tests pin the behaviour the fast paths must
//! preserve, since each one falls back to the generic path and the two must not
//! disagree.
#![cfg(unix)]

use std::process::Command;

fn run(source: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn read_sequence_transfers_octets_and_reports_short_reads() {
    let dir = std::env::temp_dir().join(format!("egcl-bulk-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("data.bin");
    std::fs::write(&path, b"ABCDE").unwrap();
    let source = format!(
        r#"
      (let ((v (make-array 10 :initial-element 99)))
        (with-open-file (s {path:?} :element-type '(unsigned-byte 8))
          ;; A short read returns what it transferred and leaves the tail alone.
          (assert (= 5 (read-sequence v s))))
        (assert (equalp v #(65 66 67 68 69 99 99 99 99 99))))
      (let ((v (make-array 8 :initial-element 0)))
        (with-open-file (s {path:?} :element-type '(unsigned-byte 8))
          (read-sequence v s :start 2 :end 5))
        (assert (equalp v #(0 0 65 66 67 0 0 0))))
      ;; READ-BYTE first leaves octets in the stream's own buffer; the bulk path
      ;; has to drain those before reading from the fd, or it skips them.
      (with-open-file (s {path:?} :element-type '(unsigned-byte 8))
        (assert (= 65 (read-byte s)))
        (let ((v (make-array 4 :initial-element 0)))
          (read-sequence v s)
          (assert (equalp v #(66 67 68 69)))))
      ;; FILE-POSITION invalidates that buffer; the bulk path must see the seek.
      (with-open-file (s {path:?} :element-type '(unsigned-byte 8))
        (read-byte s)
        (file-position s 3)
        (let ((v (make-array 2 :initial-element 0)))
          (read-sequence v s)
          (assert (equalp v #(68 69)))))
      ;; A fill-pointer vector is not a simple vector: the generic store runs.
      (let ((v (make-array 10 :fill-pointer 5 :initial-element 0)))
        (with-open-file (s {path:?} :element-type '(unsigned-byte 8))
          (read-sequence v s))
        (assert (equalp v #(65 66 67 68 69))))
      (format t "BULK-READ-OK~%")
    "#
    );
    let out = run(&source);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(out.contains("BULK-READ-OK"), "{out}");
}

#[test]
fn write_sequence_preserves_order_across_the_buffer_boundary() {
    let dir = std::env::temp_dir().join(format!("egcl-bulk-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let small = dir.join("small.bin");
    let ranged = dir.join("ranged.bin");
    let ordered = dir.join("ordered.bin");
    let from_list = dir.join("list.bin");
    let source = format!(
        r#"
      (with-open-file (s {small:?} :direction :output :element-type '(unsigned-byte 8)
                                   :if-exists :supersede)
        (write-sequence #(1 2 3 4 5) s))
      (with-open-file (s {ranged:?} :direction :output :element-type '(unsigned-byte 8)
                                    :if-exists :supersede)
        (write-sequence #(1 2 3 4 5) s :start 1 :end 4))
      ;; A transfer larger than the stream's write buffer goes straight to the
      ;; fd. If the buffered byte before it is not flushed first, that write
      ;; overtakes it and the file comes out in the wrong order.
      (with-open-file (s {ordered:?} :direction :output :element-type '(unsigned-byte 8)
                                     :if-exists :supersede)
        (write-byte 255 s)
        (write-sequence (make-array 20000 :initial-element 7) s)
        (write-byte 254 s))
      ;; A list is not a simple vector: the generic path runs.
      (with-open-file (s {from_list:?} :direction :output :element-type '(unsigned-byte 8)
                                       :if-exists :supersede)
        (write-sequence (list 9 8 7) s))
      ;; An element outside (unsigned-byte 8) is still a TYPE-ERROR, not a
      ;; silently truncated byte -- the bulk path declines and the generic path
      ;; signals.
      (assert (handler-case
                  (with-open-file (s {small:?} :direction :output
                                               :element-type '(unsigned-byte 8)
                                               :if-exists :supersede)
                    (write-sequence #(1 2 999 4) s)
                    nil)
                (error () t)))
      (format t "BULK-WRITE-OK~%")
    "#
    );
    let out = run(&source);
    assert!(out.contains("BULK-WRITE-OK"), "{out}");
    assert_eq!(std::fs::read(&ranged).unwrap(), b"\x02\x03\x04");
    assert_eq!(std::fs::read(&from_list).unwrap(), b"\x09\x08\x07");
    let bytes = std::fs::read(&ordered).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(bytes.len(), 20002);
    assert_eq!(bytes[0], 255, "the buffered byte did not stay first");
    assert_eq!(bytes[20001], 254, "the trailing byte did not stay last");
    assert!(bytes[1..20001].iter().all(|&b| b == 7));
}

#[test]
fn a_megabyte_round_trips_through_both_bulk_paths() {
    let dir = std::env::temp_dir().join(format!("egcl-bulk-trip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.bin");
    let source = format!(
        r#"
      (let ((out (make-array 1000000)))
        (dotimes (i 1000000) (setf (aref out i) (mod i 251)))
        (with-open-file (s {path:?} :direction :output :element-type '(unsigned-byte 8)
                                    :if-exists :supersede)
          (write-sequence out s))
        (let ((back (make-array 1000000 :initial-element 0)))
          (with-open-file (s {path:?} :element-type '(unsigned-byte 8))
            (assert (= 1000000 (read-sequence back s))))
          ;; EQUALP over a million elements, not a digest: a bulk path that
          ;; transposed or dropped a chunk has to fail here.
          (assert (equalp out back))))
      (format t "BULK-TRIP-OK~%")
    "#
    );
    let out = run(&source);
    let size = std::fs::metadata(&path).unwrap().len();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(size, 1000000);
    assert!(out.contains("BULK-TRIP-OK"), "{out}");
}
