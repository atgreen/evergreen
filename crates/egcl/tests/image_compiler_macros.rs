// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn checked(command: &mut Command) {
    let output = command.output().expect("run compiler-macro image probe");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn compiler_macro_functions_and_registrations_survive_image_restore() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "egcl-compiler-macros-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("macro.lisp");
    let fasl = dir.join("macro.fasl");
    let core = dir.join("core.bimg");
    std::fs::write(
        &source,
        r#"
      (defun compiled-target (x) (list :runtime x))
      (define-compiler-macro compiled-target (x) (list 'quote (list :compiled x)))
    "#,
    )
    .unwrap();
    let save = format!(
        r#"
      (load (compile-file {source:?} :output-file {fasl:?}))
      (defun source-target (x) (list :runtime x))
      (define-compiler-macro source-target (x) (list 'quote (list :old x)))
      (defparameter *old-expander* (compiler-macro-function 'source-target))
      (define-compiler-macro source-target (x) (list 'quote (list :new x)))
      (defparameter *uninterned-target* (gensym "TARGET"))
      (setf (symbol-function *uninterned-target*) (lambda (x) (list :runtime x)))
      (setf (compiler-macro-function *uninterned-target*)
            (lambda (form environment)
              (declare (ignore environment))
              (list 'quote (list :uninterned (second form)))))
      (save-lisp-and-die {core:?})
    "#,
        source = source.to_str().unwrap(),
        fasl = fasl.to_str().unwrap(),
        core = core.to_str().unwrap()
    );
    checked(Command::new("timeout").args([
        "240",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
        "--eval",
        &save,
    ]));
    checked(Command::new("timeout").args([
        "240", env!("CARGO_BIN_EXE_egcl"), "--image", core.to_str().unwrap(), "--no-init", "--eval",
        r#"
          (assert (equal '(quote (:old 7)) (funcall *old-expander* '(source-target 7) nil)))
          (assert (equal '(quote (:new 7)) (funcall (compiler-macro-function 'source-target) '(source-target 7) nil)))
          (assert (equal '(quote (:compiled 7)) (funcall (compiler-macro-function 'compiled-target) '(compiled-target 7) nil)))
          (assert (equal '(:new 7) (funcall (compile nil '(lambda () (source-target 7))))))
          (assert (equal '(:compiled 7) (funcall (compile nil '(lambda () (compiled-target 7))))))
          (assert (equal '(:uninterned 7) (funcall (compile nil (list 'lambda nil (list *uninterned-target* 7))))))
        "#,
    ]));
    std::fs::remove_dir_all(dir).unwrap();
}
