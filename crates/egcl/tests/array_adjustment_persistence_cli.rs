use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("array-adjust-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(dir: &Path, program: &str, image: Option<&Path>, stress: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.current_dir(dir).arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        command.env_remove(key);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command
        .args(["--eval", program])
        .output()
        .expect("run array persistence fixture");
    assert!(
        output.status.success(),
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn adjustable_array_graph_loads_without_source_and_preserves_metadata_and_aliases() {
    let dir = Scratch::new();
    std::fs::write(dir.0.join("arrays.lisp"), r#"
(defparameter *adjusted-array-constants*
  #.(let* ((base (make-array 6 :initial-contents '(a b c d e f) :adjustable t :fill-pointer 4))
           (view (egcl-internal::%make-md-array-with-storage '(2 2)
                   (make-array 4 :displaced-to base :displaced-index-offset 1
                                 :adjustable t :fill-pointer 2)))
           (bytes (make-array '(2 2) :element-type '(unsigned-byte 8) :initial-element 255 :adjustable t))
           (floats (make-array '(2 2) :element-type 'double-float :initial-element -1.25d0 :adjustable t))
           (bits (make-array '(2 2) :element-type 'bit :initial-element 1 :adjustable t))
           (chars (make-array '(2 2) :element-type 'base-char :initial-element #\x :adjustable t)))
      (vector base view bytes floats bits chars base)))
"#).unwrap();
    let compiled = run(
        &dir.0,
        r#"(progn (compile-file "arrays.lisp" :output-file "arrays.bfasl") (format t "ARRAYS-COMPILED~%"))"#,
        None,
        false,
    );
    let compile_log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert!(
        !compile_log.to_lowercase().contains("source-only"),
        "{compile_log}"
    );
    let image = egcl_rt::bfasl::load(&std::fs::read(dir.0.join("arrays.bfasl")).unwrap()).unwrap();
    assert!(
        image
            .section(egcl_rt::bfasl::section::TOPLEVEL_FORMS)
            .is_none()
    );
    let unit = image
        .section(egcl_rt::bfasl::section::BYTECODE_UNIT)
        .unwrap();
    assert_eq!(u16::from_le_bytes(unit[4..6].try_into().unwrap()), 0x0302);
    assert_ne!(u32::from_le_bytes(unit[8..12].try_into().unwrap()) & 1, 0);
    std::fs::write(
        dir.0.join("portable.bfasl"),
        egcl_rt::bfasl::BfaslBuilder::new()
            .content_hash(image.content_hash)
            .section(egcl_rt::bfasl::section::BYTECODE_UNIT, unit.to_vec())
            .build(),
    )
    .unwrap();
    std::fs::remove_file(dir.0.join("arrays.lisp")).unwrap();
    std::fs::remove_file(dir.0.join("arrays.bfasl")).unwrap();
    let checks = r#"(progn
      (load "portable.bfasl")
      (let ((base (aref *adjusted-array-constants* 0)) (view (aref *adjusted-array-constants* 1)))
        (assert (eq base (aref *adjusted-array-constants* 6)))
        (assert (= (fill-pointer base) 4))
        (assert (equal (array-dimensions base) '(6)))
        (assert (adjustable-array-p view))
        (assert (eq (array-displacement view) base))
        (assert (= (nth-value 1 (array-displacement view)) 1))
        ;; The MD backing has FP=2 and capacity=4. Array access must use all
        ;; four row-major elements, including after portable graph restoration.
        (assert (eq (aref view 1 1) 'e))
        (setf (aref view 1 1) 'changed)
        (assert (eq (aref base 4) 'changed))
        (assert (eq base (adjust-array base 6 :initial-contents '(p q r s t u))))
        (assert (eq (aref view 0 0) 'q))
        (assert (eq view (adjust-array view '(2 3) :initial-element 'new)))
        (assert (null (array-displacement view)))
        (assert (equal (loop for i below 6 collect (row-major-aref view i)) '(q r new s t new))))
      (loop for index from 2 below 6
            for element-type in '((unsigned-byte 8) double-float bit base-char)
            do (let ((array (aref *adjusted-array-constants* index)))
                 (assert (adjustable-array-p array))
                 (assert (equal (array-element-type array) element-type))
                 (assert (typep array (type-of array)))
                 (assert (eq array (adjust-array array '(2 2))))))
      (format t "ADJUSTABLE-ARRAY-BFASL-OK~%"))"#;
    for stress in [false, true] {
        let output = run(&dir.0, checks, None, stress);
        assert!(String::from_utf8_lossy(&output.stdout).contains("ADJUSTABLE-ARRAY-BFASL-OK"));
        if stress {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT:")
            );
        }
    }
}

#[test]
fn core_image_preserves_adjustable_array_identity_in_globals_and_compiled_constants() {
    let dir = Scratch::new();
    let core = dir.0.join("arrays.core");
    let build = format!(
        r#"(progn
      (defparameter *array-image-base* (make-array '(1 3) :initial-contents '((a b c)) :adjustable t))
      (defparameter *array-image-view* (make-array 2 :displaced-to *array-image-base* :displaced-index-offset 1))
      (eval (list 'defun 'array-image-constant nil (list 'quote *array-image-base*)))
      (compile 'array-image-constant)
      (defparameter *array-image-floats* (make-array 4 :element-type 'double-float :initial-element 1.25d0))
      (defparameter *array-image-bits* (make-array 4 :element-type 'bit :initial-element 0))
      (eval (list 'defun 'array-image-float-constant nil (list 'quote *array-image-floats*)))
      (eval (list 'defun 'array-image-bit-constant nil (list 'quote *array-image-bits*)))
      (compile 'array-image-float-constant)
      (compile 'array-image-bit-constant)
      (assert (eq *array-image-floats* (array-image-float-constant)))
      (assert (eq *array-image-bits* (array-image-bit-constant)))
      (assert (eq *array-image-base* (array-image-constant)))
      (egcl-ext:save-lisp-and-die {core:?}))"#
    );
    run(&dir.0, &build, None, false);
    let checks = r#"(progn
      (assert (eq *array-image-floats* (array-image-float-constant)))
      (assert (eq *array-image-bits* (array-image-bit-constant)))
      (setf (aref *array-image-floats* 1) 42.5d0)
      (setf (aref *array-image-bits* 1) 1)
      (assert (= (aref (array-image-float-constant) 1) 42.5d0))
      (assert (= (aref (array-image-bit-constant) 1) 1))
      (assert (adjustable-array-p *array-image-base*))
      (assert (eq *array-image-base* (array-image-constant)))
      (assert (eq (array-displacement *array-image-view*) *array-image-base*))
      (assert (eq *array-image-base* (adjust-array *array-image-base* '(2 2) :initial-element 'new)))
      (assert (eq *array-image-base* (array-image-constant)))
      (assert (eq (aref *array-image-view* 0) 'b))
      (assert (eq (aref *array-image-view* 1) 'new))
      (format t "ADJUSTABLE-ARRAY-IMAGE-OK~%"))"#;
    for stress in [false, true] {
        let output = run(&dir.0, checks, Some(&core), stress);
        assert!(String::from_utf8_lossy(&output.stdout).contains("ADJUSTABLE-ARRAY-IMAGE-OK"));
    }
}
