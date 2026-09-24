//! End-to-end hash-table protocol tests through the full CLI (bliss-jtc.8),
//! including the boot.lisp WITH-HASH-TABLE-ITERATOR macro. Each program is a
//! single top-level form whose value `--eval` echoes.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_torcl");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn torcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn with_hash_table_iterator_sums_values() {
    let out = eval(
        "(let ((h (make-hash-table)) (total 0)) \
           (setf (gethash :a h) 10) (setf (gethash :b h) 20) \
           (with-hash-table-iterator (nx h) \
             (loop (multiple-value-bind (more k v) (nx) k \
                     (if more (setq total (+ total v)) (return total))))))",
    );
    assert_eq!(out, "30");
}

#[test]
fn maphash_dispatches_an_interpreted_lambda() {
    let out = eval(
        "(let ((h (make-hash-table)) (s 0)) \
           (setf (gethash 1 h) 5) (setf (gethash 2 h) 7) \
           (maphash (lambda (k v) k (setq s (+ s v))) h) s)",
    );
    assert_eq!(out, "12");
}

#[test]
fn sxhash_is_equal_consistent() {
    assert_eq!(
        eval("(eql (sxhash (list 1 2 3)) (sxhash (list 1 2 3)))"),
        "T"
    );
}

/// bliss-jtc.22 / bliss-cpje: an EQUAL hash table keyed by a movable object
/// (a CLOS instance, hashed by address) must still find its live keys after the
/// moving GC relocates them. Before the rehash-on-move fix, gethash probed the
/// bucket for the key's NEW address while the entry sat in its OLD-address
/// bucket, so lookups silently missed — which broke ASDF's visited-actions and
/// blocked real-library self-host loads. GC stress relocates the keys between
/// insertion and lookup; all 40 must still be found.
#[test]
fn equal_hash_instance_keys_survive_moving_gc() {
    let out = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            "(progn (defclass k () ()) \
               (let ((h (make-hash-table :test 'equal)) (keys nil)) \
                 (dotimes (i 40) \
                   (let ((key (cons i (make-instance 'k)))) \
                     (push key keys) (setf (gethash key h) i))) \
                 (dotimes (i 60) (make-instance 'k)) \
                 (let ((hits 0)) \
                   (dolist (key keys) (when (nth-value 1 (gethash key h)) (incf hits))) \
                   hits)))",
        ])
        .env("TORCL_GC_STRESS", "1")
        .output()
        .expect("spawn torcl");
    assert!(
        out.status.success(),
        "program failed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "40",
        "all instance keys must be found after GC relocation (bliss-jtc.22)"
    );
}
