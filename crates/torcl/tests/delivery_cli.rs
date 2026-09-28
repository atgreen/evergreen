//! Application delivery starts from a saved world, never by replaying source.
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("torcl-delivery-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn run(exe: &str, args: &[&str]) -> Output {
    Command::new("timeout")
        .args(["--kill-after=5", "180", exe, "--no-init"])
        .args(args)
        .output()
        .unwrap()
}
fn ok(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
const BIN: &str = env!("CARGO_BIN_EXE_torcl");

#[test]
fn delivery_prunes_unreachable_functions_and_keeps_data_and_explicit_roots() {
    let f = Fixture::new();
    let image = f.path("input.core");
    let spec = f.path("app.delivery");
    let exe = f.path("app");
    let save = format!(
        r#"
      (defpackage :delivery-test (:use :cl))
      (in-package :delivery-test)
      (defun helper () 42)
      (defun callback () :callback)
      (defun data-function () :data)
      (defparameter *dispatch* (vector 'data-function))
      (defparameter *path* #P"delivery/test.lisp")
      (defun table-function () :table)
      (defparameter *table* (make-hash-table))
      (setf (gethash :entry *table*) 'table-function)
      (setf (gethash :cycle *table*) *table*)
      (defun dead-a () (dead-b))
      (defun dead-b () (dead-a))
      (defun main ()
        (assert (= 42 (helper)))
        (assert (eq :callback (funcall (intern "CALLBACK" :delivery-test))))
        (assert (eq :data (funcall (aref *dispatch* 0))))
        (assert (string= "test" (pathname-name *path*)))
        (assert (eq :table (funcall (gethash :entry *table*))))
        (assert (not (fboundp (intern "DEAD-A" :delivery-test))))
        (format t "DELIVERED-OK~%"))
      (save-lisp-and-die {image:?} :toplevel (lambda () (error "Input entry must not run")))
    "#
    );
    ok(run(BIN, &["--eval", &save]));
    let original = fs::read(&image).unwrap();
    fs::write(&spec, "version = 1\nentry = DELIVERY-TEST::MAIN\nprune-package = DELIVERY-TEST\nkeep = DELIVERY-TEST::CALLBACK\ndynamic = explicit\n").unwrap();
    let report = ok(run(
        BIN,
        &[
            "--image",
            &image,
            "--deliver",
            &spec,
            "--output",
            &exe,
            "--dry-run",
        ],
    ));
    assert!(report.contains("remove DELIVERY-TEST::DEAD-A"), "{report}");
    assert!(!std::path::Path::new(&exe).exists());
    let report = ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    assert!(report.contains("remove DELIVERY-TEST::DEAD-B"), "{report}");
    assert!(ok(run(&exe, &[])).contains("DELIVERED-OK"));
    assert_eq!(original, fs::read(&image).unwrap());
    assert!(fs::metadata(format!("{exe}.manifest")).unwrap().len() > 0);
    ok(run(
        BIN,
        &[
            "--image",
            &image,
            "--eval",
            "(assert (fboundp 'delivery-test::dead-a))",
        ],
    ));
}

#[test]
fn resaving_an_executable_replaces_the_previous_embedded_image() {
    let f = Fixture::new();
    let first = f.path("first");
    let second = f.path("second");
    ok(run(
        BIN,
        &[
            "--eval",
            &format!("(save-lisp-and-die {first:?} :executable t)"),
        ],
    ));
    ok(run(
        &first,
        &[
            "--eval",
            &format!("(save-lisp-and-die {second:?} :executable t)"),
        ],
    ));
    fn prefix(path: &str) -> usize {
        let bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[bytes.len() - 16..bytes.len() - 8], b"TORCLEXE");
        let len = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) as usize;
        bytes.len() - 16 - len
    }
    assert_eq!(
        prefix(&first),
        prefix(&second),
        "a previous image must not become part of the runtime prefix"
    );
    ok(run(&second, &["--eval", "(assert (= 3 (+ 1 2)))"]));
}

#[test]
fn delivery_of_source_free_code_keeps_closures_methods_and_unwind_paths() {
    let f = Fixture::new();
    let source = f.path("compiled.lisp");
    let fasl = f.path("compiled.bfasl");
    let image = f.path("compiled.core");
    let spec = f.path("compiled.delivery");
    let exe = f.path("compiled-app");
    fs::write(
        &source,
        r#"
      (defpackage :delivery-code (:use :cl))
      (in-package :delivery-code)
      (defun helper (n) (+ n 1))
      (defun cleanup () (format t "CLEANUP-OK~%"))
      (defun unused () (make-string 20000))
      (defun closure-maker (n) (lambda () (helper n)))
      (defmethod compute ((n integer)) (helper n))
      (defun main ()
        (assert (= 42 (funcall *saved-closure*)))
        (assert (= 42 (compute 41)))
        (assert (= 42 (handler-case
          (unwind-protect (error "exercise cold path") (cleanup))
          (error () (helper 41)))))
        (let ((sum 0))
          (dotimes (i 1000001) (incf sum (helper 1)))
          (assert (= sum 2000002)))
        ;; Exercise a different numeric type after fixnum warmup, and a
        ;; restart body whose dependencies live in nested bytecode.
        (assert (= 2.5 (helper 1.5)))
        (assert (= 42 (restart-case (invoke-restart 'answer)
          (answer () (helper 41)))))
        (assert (not (fboundp (intern "UNUSED" :delivery-code))))
        (format t "COMPILED-DELIVERY-OK~%"))
    "#,
    )
    .unwrap();
    ok(run(
        BIN,
        &[
            "--eval",
            &format!("(compile-file {source:?} :output-file {fasl:?})"),
        ],
    ));
    fs::remove_file(&source).unwrap();
    ok(run(
        BIN,
        &[
            "--eval",
            &format!(
                "(load {fasl:?}) (defparameter delivery-code::*saved-closure* (delivery-code::closure-maker 41)) (save-lisp-and-die {image:?})"
            ),
        ],
    ));
    fs::remove_file(&fasl).unwrap();
    fs::write(&spec, "version = 1\nentry = DELIVERY-CODE::MAIN\nprune-package = DELIVERY-CODE\ndynamic = explicit\n").unwrap();
    let report = ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    assert!(report.contains("remove DELIVERY-CODE::UNUSED"), "{report}");
    let output = ok(run(&exe, &[]));
    assert!(output.contains("CLEANUP-OK"), "{output}");
    assert!(output.contains("COMPILED-DELIVERY-OK"), "{output}");
}

#[test]
fn delivery_defaults_to_preserving_dynamic_targets_and_rejects_bad_specs() {
    let f = Fixture::new();
    let image = f.path("input.core");
    let spec = f.path("policy.delivery");
    let exe = f.path("app");
    ok(run(
        BIN,
        &[
            "--eval",
            &format!(
                r#"
      (defpackage :delivery-policy (:use :cl))
      (defun delivery-policy::target () 19)
      (defun delivery-policy::main ()
        (assert (= 19 (funcall (intern "TARGET" :delivery-policy))))
        (format t "DYNAMIC-OK~%"))
      (save-lisp-and-die {image:?})"#
            ),
        ],
    ));
    fs::write(
        &spec,
        "version = 1\nentry = DELIVERY-POLICY::MAIN\nprune-package = DELIVERY-POLICY\n",
    )
    .unwrap();
    let report = ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    assert!(report.contains("dynamic = preserve"));
    assert!(!report.contains("remove DELIVERY-POLICY::TARGET"));
    assert!(ok(run(&exe, &[])).contains("DYNAMIC-OK"));
    let previous = fs::read(&exe).unwrap();
    for bad in [
        "version = 2\nentry = DELIVERY-POLICY::MAIN\n",
        "version = 1\nentry = DELIVERY-POLICY::MISSING\n",
        "version = 1\nentry = DELIVERY-POLICY::MAIN\nprune-package = COMMON-LISP\n",
        "version = 1\nentry = DELIVERY-POLICY::MAIN\ndynamci = explicit\n",
    ] {
        fs::write(&spec, bad).unwrap();
        let result = run(
            BIN,
            &["--image", &image, "--deliver", &spec, "--output", &exe],
        );
        assert!(!result.status.success());
        assert_eq!(previous, fs::read(&exe).unwrap());
    }
    fs::write(&spec, "version = 1\nentry = DELIVERY-POLICY::MAIN\n").unwrap();
    let original = fs::read(&image).unwrap();
    assert!(
        !run(
            BIN,
            &["--image", &image, "--deliver", &spec, "--output", &image]
        )
        .status
        .success()
    );
    assert_eq!(original, fs::read(&image).unwrap());
    // Explicit delivery input wins over the image embedded in the driver.
    assert!(
        ok(run(
            &exe,
            &[
                "--image",
                &image,
                "--deliver",
                &spec,
                "--output",
                &f.path("other"),
                "--dry-run"
            ]
        ))
        .contains("entry = DELIVERY-POLICY::MAIN")
    );
}

#[test]
fn failed_publication_preserves_the_existing_manifest() {
    let f = Fixture::new();
    let image = f.path("input.core");
    let spec = f.path("app.delivery");
    let exe = f.path("directory");
    fs::create_dir(&exe).unwrap();
    let manifest = format!("{exe}.manifest");
    fs::write(&manifest, "previous manifest").unwrap();
    ok(run(
        BIN,
        &[
            "--eval",
            &format!("(defun main () 42) (save-lisp-and-die {image:?})"),
        ],
    ));
    fs::write(&spec, "version = 1\nentry = CL-USER::MAIN\n").unwrap();
    assert!(
        !run(
            BIN,
            &["--image", &image, "--deliver", &spec, "--output", &exe]
        )
        .status
        .success()
    );
    assert_eq!(fs::read_to_string(manifest).unwrap(), "previous manifest");
}

#[test]
fn delivery_reduces_the_embedded_core_when_unused_code_has_large_constants() {
    let f = Fixture::new();
    let source = f.path("large.lisp");
    let image = f.path("large.core");
    let spec = f.path("large.delivery");
    let exe = f.path("small-app");
    fs::write(
        &source,
        format!(
            r#"
      (defpackage :delivery-size (:use :cl))
      (in-package :delivery-size)
      (defun unused () "{}")
      (defun main () (format t "SMALL-OK~%"))
      (assert (> (length (unused)) 100000))
      (save-lisp-and-die {image:?})
    "#,
            "unreachable payload ".repeat(10000)
        ),
    )
    .unwrap();
    ok(run(BIN, &["--load", &source]));
    fs::remove_file(source).unwrap();
    fs::write(&spec, "version = 1\nentry = DELIVERY-SIZE::MAIN\nprune-package = DELIVERY-SIZE\ndynamic = explicit\n").unwrap();
    ok(run(
        BIN,
        &["--image", &image, "--deliver", &spec, "--output", &exe],
    ));
    let bytes = fs::read(&exe).unwrap();
    let core_size = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
    let input_size = fs::metadata(&image).unwrap().len();
    let payload = b"unreachable payload unreachable payload";
    assert!(
        fs::read(&image)
            .unwrap()
            .windows(payload.len())
            .any(|w| w == payload)
    );
    assert!(
        !bytes.windows(payload.len()).any(|w| w == payload),
        "unused constant must be absent, including from the restored pinned heap (offset {:?}, core starts {}): {}",
        bytes.windows(payload.len()).position(|w| w == payload),
        bytes.len() as u64 - 16 - core_size,
        fs::read_to_string(format!("{exe}.manifest")).unwrap()
    );
    let wide_payload: Vec<u8> = payload
        .iter()
        .flat_map(|&b| u32::from(b).to_ne_bytes())
        .collect();
    assert!(
        !bytes.windows(wide_payload.len()).any(|w| w == wide_payload),
        "unused wide-character constant must be absent from the restored pinned heap"
    );
    assert!(
        core_size < input_size,
        "delivered core {core_size} must be smaller than input {input_size}"
    );
    assert!(ok(run(&exe, &[])).contains("SMALL-OK"));
}
