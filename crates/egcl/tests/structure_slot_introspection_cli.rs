// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::Path, process::Command};

const SOURCE: &str = r#"
(defun raw-mop (name &rest arguments)
  (apply (symbol-function (find-symbol name #+egcl "EGCL-MOP" #+sbcl "SB-MOP")) arguments))
(defun raw-slot-names (slots)
  (mapcar (lambda (slot) (symbol-name (raw-mop "SLOT-DEFINITION-NAME" slot))) slots))
(defstruct raw-slot-parent (x 11))
(defvar *raw-old-slots* (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-parent)))
(assert (eq (car *raw-old-slots*) (car (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-parent)))))
(defstruct raw-slot-parent (x 22))
(defvar *raw-new-slots* (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-parent)))
(assert (= (raw-mop "SLOT-DEFINITION-INITFORM" (car *raw-new-slots*)) 22))
(assert (eq (car *raw-new-slots*) (car (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-parent)))))
(assert (equal (raw-slot-names *raw-new-slots*) '("X")))
(assert (equal (raw-slot-names (raw-mop "CLASS-DIRECT-SLOTS" (find-class 'raw-slot-parent))) '("X")))
(assert (raw-mop "CLASS-FINALIZED-P" (find-class 'raw-slot-parent)))
(assert (= (raw-slot-parent-x (make-raw-slot-parent)) 22))
(defstruct (raw-slot-child (:include raw-slot-parent)) (y 33))
(defvar *raw-child-slots* (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-child)))
(assert (equal (sort (raw-slot-names *raw-child-slots*) #'string<) '("X" "Y")))
(assert (= (raw-slot-child-x (make-raw-slot-child)) 22))
(assert (= (raw-slot-child-y (make-raw-slot-child)) 33))
;; EGCL invalidates inherited metadata when the raw parent is redefined.
(defstruct raw-slot-parent (x 44))
(let ((slots (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-child))))
  (assert (equal (sort (raw-slot-names slots) #'string<) '("X" "Y")))
  (let ((slot (find "X" slots :key (lambda (slot) (symbol-name (raw-mop "SLOT-DEFINITION-NAME" slot))) :test #'string=)))
    (assert (= (raw-mop "SLOT-DEFINITION-INITFORM" slot)
               (raw-slot-child-x (make-raw-slot-child))))))
(defstruct (raw-slot-child (:include raw-slot-parent)) (y 55))
(defvar *raw-saved-slots* (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-child)))
(defvar *raw-saved-direct* (raw-mop "CLASS-DIRECT-SLOTS" (find-class 'raw-slot-child)))
(defvar *raw-saved-instance* (make-raw-slot-child :x 67 :y 89))
;; The standard/custom metaclass protocol must still run and retain its cache.
(defvar *raw-standard-hooks* 0)
(defclass raw-custom-meta (standard-class) ())
(defmethod #+egcl egcl-mop:validate-superclass #+sbcl sb-mop:validate-superclass
    ((class raw-custom-meta) (super standard-class)) t)
(defmethod #+egcl egcl-mop:compute-slots #+sbcl sb-mop:compute-slots :around
    ((class raw-custom-meta))
  (incf *raw-standard-hooks*) (call-next-method))
(defclass raw-custom-class () ((z :initform 77)) (:metaclass raw-custom-meta))
(unless (raw-mop "CLASS-FINALIZED-P" (find-class 'raw-custom-class))
  (raw-mop "FINALIZE-INHERITANCE" (find-class 'raw-custom-class)))
(defun verify-raw-slot-introspection ()
  (assert (equal (sort (raw-slot-names (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-child))) #'string<)
                 '("X" "Y")))
  (assert (eq (car *raw-saved-slots*) (car (raw-mop "CLASS-SLOTS" (find-class 'raw-slot-child)))))
  (assert (eq (car *raw-saved-direct*) (car (raw-mop "CLASS-DIRECT-SLOTS" (find-class 'raw-slot-child)))))
  (assert (= (raw-slot-child-x *raw-saved-instance*) 67))
  (assert (= (raw-slot-child-y *raw-saved-instance*) 89))
  (assert (= (raw-slot-child-x (make-raw-slot-child)) 44))
  (assert (= (raw-slot-child-y (make-raw-slot-child)) 55))
  (let ((value (make-raw-slot-child)))
    (setf (raw-slot-child-x value) 101 (raw-slot-child-y value) 202)
    (assert (= (raw-slot-child-x value) 101)) (assert (= (raw-slot-child-y value) 202)))
  (dolist (name '(cons integer t))
    (let ((class (find-class name)))
      (assert (null (raw-mop "CLASS-SLOTS" class)))
      (assert (null (raw-mop "CLASS-SLOTS" class)))
      (assert (null (raw-mop "CLASS-DIRECT-SLOTS" class)))
      (assert (raw-mop "CLASS-FINALIZED-P" class))))
  (assert (equal (raw-slot-names (raw-mop "CLASS-SLOTS" (find-class 'raw-custom-class))) '("Z")))
  (assert (plusp *raw-standard-hooks*))
  (let ((count *raw-standard-hooks*))
    (raw-mop "CLASS-SLOTS" (find-class 'raw-custom-class))
    (assert (= *raw-standard-hooks* count)))
  (assert (= (slot-value (make-instance 'raw-custom-class) 'z) 77))
  (write-line "RAW-SLOT-INTROSPECTION-OK"))
"#;

fn run(program: &str, stress: bool, image: Option<&Path>, marker: &str) -> String {
    let program = format!("(progn {program})");
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command
        .args(["--eval", &program])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command.output().expect("run structure slot introspection");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout={stdout} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains(marker), "stdout={stdout}");
    stdout
}

#[test]
fn redefined_structure_slots_preserve_metadata_layout_and_protocol_hooks() {
    let program = format!("{SOURCE} (verify-raw-slot-introspection)");
    assert_eq!(
        run(&program, false, None, "RAW-SLOT-INTROSPECTION-OK"),
        run(&program, true, None, "RAW-SLOT-INTROSPECTION-OK")
    );
}

#[test]
fn redefined_structure_load_forms_compile_and_load_without_source() {
    let directory = std::env::temp_dir().join(format!("egcl-raw-slot-mlf-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("slots.lisp");
    let artifact = directory.join("slots.bfasl");
    fs::write(
        &source,
        r#"
      (eval-when (:compile-toplevel :load-toplevel :execute)
        (defstruct raw-load-record (payload 42))
        (defmethod make-load-form ((object raw-load-record) &optional environment)
          (make-load-form-saving-slots object :environment environment)))
      (defun raw-load-literal () '#.(make-raw-load-record :payload '(7 9)))
    "#,
    )
    .unwrap();
    run(
        &format!(
            "(defstruct raw-load-record (payload 42)) (multiple-value-bind (path warnings failure) (compile-file {source:?} :output-file {artifact:?}) (declare (ignore warnings)) (assert path) (assert (not failure)) (write-line \"RAW-MLF-COMPILED\"))"
        ),
        false,
        None,
        "RAW-MLF-COMPILED",
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..count {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(
            bytecode,
            "structure load forms must compile to portable bytecode"
        );
    }
    fs::remove_file(source).unwrap();
    let program = format!(
        r#"(load {artifact:?}) (load {artifact:?})
      (assert (equal (raw-load-record-payload (raw-load-literal)) '(7 9)))
      (assert (= (raw-load-record-payload (make-raw-load-record)) 42))
      (make-load-form-saving-slots (make-raw-load-record))
      (write-line "RAW-MLF-LOADED")"#
    );
    assert_eq!(
        run(&program, false, None, "RAW-MLF-LOADED"),
        run(&program, true, None, "RAW-MLF-LOADED")
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn raw_slot_cache_identity_survives_saved_image_restoration() {
    let directory =
        std::env::temp_dir().join(format!("egcl-raw-slot-image-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let image = directory.join("slots.core");
    let save = format!(
        r#"{SOURCE}
      (verify-raw-slot-introspection)
      (write-line "RAW-IMAGE-READY")
      #+egcl (egcl-ext:save-lisp-and-die {image:?})
      #-egcl (write-line "RAW-IMAGE-EGCL-ONLY")"#
    );
    let stdout = run(&save, false, None, "RAW-IMAGE-READY");
    if !stdout.contains("RAW-IMAGE-EGCL-ONLY") {
        assert!(image.exists());
        run(
            "(verify-raw-slot-introspection)",
            true,
            Some(&image),
            "RAW-SLOT-INTROSPECTION-OK",
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
