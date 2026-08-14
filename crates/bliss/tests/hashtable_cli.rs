//! End-to-end hash-table protocol tests through the full CLI (bliss-jtc.8),
//! including the boot.lisp WITH-HASH-TABLE-ITERATOR macro. Each program is a
//! single top-level form whose value `--eval` echoes.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn bliss-cli");
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
