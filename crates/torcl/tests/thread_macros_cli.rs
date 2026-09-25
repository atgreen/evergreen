//! Global macros remain visible and GC-safe across native workers.
use std::process::Command;

fn check(program: &str) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "60",
            env!("CARGO_BIN_EXE_torcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .env("TORCL_GC_POISON", "1")
        .output()
        .expect("run bounded native macro regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("SHARED-MACROS-OK"), "{stdout}\n{stderr}");
}

#[test]
fn main_macro_survives_collection_on_a_native_worker() {
    check(
        r#"
        (defmacro retained-macro (value) `(list ,value 42))
        (defun collect-on-worker () (torcl::%prune-closures) 17)
        (assert (= 17 (torcl-thread:join-thread
                       (torcl-thread:make-thread 'collect-on-worker))))
        (assert (equal '(list 19 42) (macroexpand-1 '(retained-macro 19))))
        (assert (equal '(19 42) (eval '(retained-macro 19))))
        (format t "SHARED-MACROS-OK~%")
        "#,
    );
}

#[test]
fn global_macro_is_visible_to_worker_expansion() {
    check(
        r#"
        (defmacro shared-answer (value) `(list ,value 42))
        (assert (equal '(19 42)
          (torcl-thread:join-thread
            (torcl-thread:make-thread
              (lambda () (eval '(shared-answer 19)))))))
        (format t "SHARED-MACROS-OK~%")
        "#,
    );
}

#[test]
fn worker_macro_retains_captured_values_after_worker_exit() {
    check(
        r#"
        (torcl-thread:join-thread
          (torcl-thread:make-thread
            (lambda ()
              (let ((captured (list 19 23)))
                (defmacro worker-payload () `(quote ,captured))))))
        (torcl::%prune-closures)
        (assert (equal '(19 23) (eval '(worker-payload))))
        (format t "SHARED-MACROS-OK~%")
        "#,
    );
}

#[test]
fn worker_redefinition_invalidates_main_compiler_macro_environment() {
    check(
        r#"
        (defmacro shared-version () :old)
        (assert (eq :old (shared-version)))
        (torcl-thread:join-thread
          (torcl-thread:make-thread
            (lambda () (eval '(defmacro shared-version () :new)))))
        (assert (eq :new (shared-version)))
        (torcl-thread:join-thread
          (torcl-thread:make-thread (lambda () (fmakunbound 'shared-version))))
        (assert (null (macro-function 'shared-version)))
        (format t "SHARED-MACROS-OK~%")
        "#,
    );
}

#[test]
fn loaded_bytecode_macro_can_expand_on_worker_after_collection() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("torcl-thread-macro-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("macro.lisp");
    let fasl = directory.join("macro.bfasl");
    std::fs::write(&source, "(defmacro shared-fasl-macro (x) `(list ,x 42))").unwrap();
    check(&format!(
        r#"
        (multiple-value-bind (path warnings failure)
            (compile-file "{}" :output-file "{}")
          (declare (ignore warnings))
          (assert path)
          (assert (null failure)))
        (fmakunbound 'shared-fasl-macro)
        (delete-file "{}")
        (load "{}")
        (assert (equal '(19 42)
          (torcl-thread:join-thread
            (torcl-thread:make-thread
              (lambda ()
                (torcl::%prune-closures)
                (eval '(shared-fasl-macro 19)))))))
        (assert (equal '(23 42) (eval '(shared-fasl-macro 23))))
        (format t "SHARED-MACROS-OK~%")
        "#,
        source.display(),
        fasl.display(),
        source.display(),
        fasl.display()
    ));
    std::fs::remove_dir_all(directory).unwrap();
}
