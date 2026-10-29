// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defun pathname-prin1-render (p) (prin1-to-string p))
(defun pathname-stream-render (p)
  (with-output-to-string (s) (write p :stream s)))
(defun pathname-components (p)
  (list (pathname-host p) (pathname-device p) (pathname-directory p)
        (pathname-name p) (pathname-type p) (pathname-version p)))
(defun pathname-renderings (p)
  (list (pathname-prin1-render p) (pathname-stream-render p)
        (write-to-string p) (format nil "~S" p)
        (with-output-to-string (s) (prin1 p s))))
(let ((normal (make-pathname :directory '(:absolute "tmp") :name "readable" :type "txt")))
  (dotimes (i 80) (pathname-prin1-render normal) (pathname-stream-render normal))
  (when *pathname-required-tier*
    (assert (= (egcl-ext:function-tier 'pathname-prin1-render) *pathname-required-tier*))
    (assert (= (egcl-ext:function-tier 'pathname-stream-render) *pathname-required-tier*))))
(dolist (pretty '(nil t))
  (dolist (read-eval '(nil t))
    (let ((*print-readably* t) (*print-pretty* pretty) (*read-eval* read-eval)
          (*print-case* :downcase) (*print-base* 16) (*print-radix* t)
          (*default-pathname-defaults* #P"/unrelated/default.xyz"))
      (dolist (p (list (make-pathname :name nil :type "txt" :defaults #P"")
                      (make-pathname :name nil :type :wild :defaults #P"")))
        (dolist (renderer (list #'pathname-prin1-render #'pathname-stream-render
                               #'write-to-string
                               (lambda (x) (format nil "~S" x))
                               (lambda (x) (with-output-to-string (s) (prin1 x s)))
                               (lambda (x) (write-to-string x :escape nil :readably t))
                               (lambda (x) (write-to-string (list x x) :circle t))))
          (let ((seen nil))
            (assert
              (handler-case
                  (handler-bind ((print-not-readable
                                   (lambda (c)
                                     (assert (eq (print-not-readable-object c) p))
                                     (setq seen t))))
                    (funcall renderer p) nil)
                (print-not-readable (c)
                  (assert (eq (print-not-readable-object c) p)) t)))
            (assert seen)))
        ;; Failed printing leaves no parked error for later successful calls.
        (assert (= (read-from-string (prin1-to-string 42)) 42)))
      (dolist (p (list #P"/tmp/name.txt" #P".hidden" #P"/tmp/" #P"plain"))
        (dolist (text (pathname-renderings p))
          (let ((copy (read-from-string text)))
            (assert (equal (pathname-components copy) (pathname-components p)))))))))
(let ((*print-readably* nil) (*print-pretty* nil))
  (dolist (p (list (make-pathname :name nil :type "txt" :defaults #P"")
                  (make-pathname :name nil :type :wild :defaults #P"")))
    (let ((ns (namestring p)))
      (assert (equal (princ-to-string p) ns))
      (assert (equal (format nil "~A" p) ns))
      (assert (equal (write-to-string p) (concatenate 'string "#P" (prin1-to-string ns)))))))
(defparameter *custom-pathname-text*
  "#.(COMMON-LISP:MAKE-PATHNAME :NAME COMMON-LISP:NIL :TYPE \"txt\" :DEFAULTS #P\"\")")
;; Explicit pretty and PRINT-OBJECT methods retain their normal precedence.
(let ((*print-readably* t) (*print-pretty* t) (*read-eval* t)
      (*print-pprint-dispatch* (copy-pprint-dispatch nil)))
  (set-pprint-dispatch 'pathname
    (lambda (s p) (declare (ignore p)) (write-string *custom-pathname-text* s)))
  (let ((p (make-pathname :name nil :type "txt" :defaults #P"")))
    (assert (equal (prin1-to-string p) *custom-pathname-text*))))
(defmethod print-object ((p pathname) s)
  (write-string *custom-pathname-text* s)
  p)
(let ((*print-readably* t) (*print-pretty* nil) (*read-eval* t)
      (p (make-pathname :name nil :type "txt" :defaults #P"")))
  (dolist (text (pathname-renderings p))
    (assert (equal text *custom-pathname-text*))
    (assert (equal (pathname-components (read-from-string text)) (pathname-components p)))))
(write-line "PATHNAME-READABLY-OK")
"##;

fn invoke(
    program: &str,
    tier: &str,
    stress: bool,
    image: Option<&std::path::Path>,
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"));
    child.arg("--no-init");
    if let Some(image) = image {
        child.arg("--image").arg(image);
    }
    child.args(["--eval", program]);
    child
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_LAZY", "0")
        .env("EGCL_T2_THREADS", "1");
    for key in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
    ] {
        child.env_remove(key);
    }
    if stress {
        child
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    child.output().unwrap()
}

fn run_fixture(tier: &str, stress: bool, image: Option<&std::path::Path>) {
    let output = invoke(
        &format!(
            "(defparameter *pathname-required-tier* {}){PROGRAM}",
            if tier == "t2" { "2" } else { "nil" }
        ),
        tier,
        stress,
        image,
    );
    assert!(
        output.status.success(),
        "{tier}/{stress}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.trim() == "PATHNAME-READABLY-OK")
            .count(),
        1
    );
}

#[test]
fn pathname_readable_printing_preserves_components_or_signals_exact_object() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        run_fixture(tier, stress, None);
    }
}

#[test]
fn pathname_print_not_readable_bridge_is_registered_after_fresh_image_startup() {
    let dir = std::env::temp_dir().join(format!(
        "egcl-pathname-readable-image-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let image = dir.join("startup.core");
    // Save pristine startup, before PROGRAM installs its pathname printer.
    let output = invoke(
        &format!("(egcl-ext:save-lisp-and-die {image:?})"),
        "interp",
        false,
        None,
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        run_fixture(tier, stress, Some(&image));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
