// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
const DEFINITIONS: &str = r#"
(deftype fnv-index () '(integer 0 #.most-positive-fixnum))
(deftype fnv-word () '(unsigned-byte 64))
(declaim (notinline string-aref-probe portable-fnv64))
(defun string-aref-probe (array index) (aref array index))
(defun string-svref-probe (array index) (svref array index))
(defun portable-fnv64 (string start end)
 (macrolet ((guts ()
             `(loop for i of-type fnv-index from start below end do
               (set-result (logxor result (char-code (aref string i))))
               (set-result (* result 16777619))))
            (set-result (form) `(setf result (ldb (byte 64 0) ,form))))
  (let ((result 2166136261))
   (declare (type fnv-word result))
   (guts) (logand result most-positive-fixnum))))
"#;
const CHECKS: &str = r#"
(let ((base (copy-seq "A Z")) (wide (copy-seq "Aλ😀"))
      (vector (vector nil t)) (octets (make-array 2 :element-type '(unsigned-byte 8) :initial-element 255)))
 (setf (char base 1) (code-char 0))
 (dotimes (i 100)
  (assert (char= (string-aref-probe base 1) (code-char 0)))
  (assert (char= (string-aref-probe wide 2) (code-char #x1f600)))
  (assert (null (string-aref-probe vector 0)))
  (assert (= (string-aref-probe octets 1) 255)))
 (setf (char base 2) #\X (char wide 1) #\β)
 (assert (char= (string-aref-probe base 2) #\X))
 (assert (char= (string-aref-probe wide 1) #\β))
 (dolist (string (list base wide))
  (assert (handler-case (progn (string-svref-probe string 0) nil) (type-error () t)))))
(let ((input "SB-C::IR1-CONVERT-FUNCTOID")
      (expected (logand 8540489340559491437 most-positive-fixnum)))
 ;; Cold arithmetic may discover legitimate numeric guards. The regression
 ;; concerns repeated rejection of the legal string layout after warming.
 (dotimes (i 32) (assert (= (portable-fnv64 input 0 (length input)) expected)))
 ; NATIVE-STEADY-CHECK
 (dotimes (i 1000) (assert (= (portable-fnv64 input 0 (length input)) expected))))
(write-line "STRING-AREF-NATIVE-OK")
"#;
// Only the x86-64 emitter gains this layout. Other targets still run all
// semantic checks without claiming a native no-deoptimization contract.
const STEADY_NATIVE_CHECK: &str = r#"
 #+egcl
 (let ((before (eval '(egcl-ext:deopt-count))))
  (dotimes (i 100) (portable-fnv64 input 0 (length input)))
  (assert (= before (eval '(egcl-ext:deopt-count)))))
"#;
static NEXT: AtomicU64 = AtomicU64::new(0);
fn directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "egcl-string-aref-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn run(program: &str, forced: bool, stress: bool) -> String {
    let dir = directory();
    let script = dir.join("run.lisp");
    let program = program.replace(
        "; NATIVE-STEADY-CHECK",
        if cfg!(target_arch = "x86_64") {
            STEADY_NATIVE_CHECK
        } else {
            ""
        },
    );
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load", script.to_str().unwrap()])
        .env("EGCL_FORCE_TIER", if forced { "t2" } else { "" })
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .output()
        .unwrap();
    fs::remove_dir_all(dir).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "forced={forced} stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}
fn checks(program: &str) {
    let normal = run(program, false, false);
    assert!(normal.contains("STRING-AREF-NATIVE-OK"));
    assert_eq!(normal, run(program, true, false));
    assert_eq!(normal, run(program, true, true));
}
#[test]
fn string_aref_and_hash_keep_exact_results_and_steady_native_execution() {
    checks(&format!("{DEFINITIONS}{CHECKS}"));
}
#[test]
fn fresh_bytecode_string_aref_and_hash_preserve_native_layouts() {
    let dir = directory();
    let source = dir.join("fnv.lisp");
    let artifact = dir.join("fnv.fasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure) (compile-file {:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
            source.to_str().unwrap(),
            artifact.to_str().unwrap()
        ),
        false,
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        assert!(bytes.windows(4).any(|b| b == b"BBU\0"));
    }
    fs::remove_file(source).unwrap();
    checks(&format!("(load {:?}){CHECKS}", artifact.to_str().unwrap()));
    fs::remove_dir_all(dir).unwrap();
}
