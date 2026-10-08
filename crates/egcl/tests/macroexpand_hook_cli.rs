// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn run(form: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", form])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("HOOK-PASS"), "{stdout}\n{stderr}");
}

#[test]
fn macroexpand_hook_has_a_callable_default() {
    run(r#"(progn
      (assert (boundp '*macroexpand-hook*))
      (assert (= 42 (funcall *macroexpand-hook*
                            (lambda (form env) (declare (ignore env)) (second form))
                            '(test 42) nil)))
      (write-line "HOOK-PASS"))"#);
}

#[test]
fn rebound_hook_receives_callable_expander_and_original_form() {
    run(r#"(progn
      (defvar *macroexpand-hook* #'funcall)
      (defvar *hook-calls* 0)
      (defmacro hook-target (x) (list '+ x 1))
      (let ((saved *macroexpand-hook*))
        (let ((*macroexpand-hook*
                (lambda (expander form env)
                  (assert (functionp expander))
                  (when (equal form '(hook-target 41)) (incf *hook-calls*))
                  (funcall expander form env))))
          (multiple-value-bind (result expanded-p) (macroexpand-1 '(hook-target 41))
            (assert expanded-p)
            (assert (equal result '(+ 41 1)))))
        (assert (eq saved *macroexpand-hook*)))
      (assert (= *hook-calls* 1))
      (write-line "HOOK-PASS"))"#);
}

#[test]
fn symbol_macro_hook_sees_symbol_and_can_invoke_its_expander() {
    run(r#"(progn
      (defvar *macroexpand-hook* #'funcall)
      (defvar *symbol-hook-calls* 0)
      (defmacro expand-in-context (form &environment env)
        (list 'quote (macroexpand-1 form env)))
      (let ((*macroexpand-hook*
              (lambda (expander form env)
                (assert (functionp expander))
                (when (eq form 'hook-symbol) (incf *symbol-hook-calls*))
                (funcall expander form env))))
        (assert (= 73 (eval '(symbol-macrolet ((hook-symbol 73))
                              (expand-in-context hook-symbol))))))
      (assert (> *symbol-hook-calls* 0))
      (write-line "HOOK-PASS"))"#);
}

#[test]
fn hook_can_replace_expansion_and_restores_after_nonlocal_exit() {
    run(r#"(progn
      (defmacro hook-target () 1)
      (let ((*macroexpand-hook* (lambda (expander form env)
                                 (if (equal form '(hook-target)) 97
                                     (funcall expander form env)))))
        (assert (= 97 (macroexpand-1 '(hook-target)))))
      (let ((saved *macroexpand-hook*))
        (assert (eq :escaped
                    (catch 'hook-exit
                      (funcall
                        (lambda ()
                          (let ((*macroexpand-hook*
                                  (lambda (expander form env)
                                    (if (equal form '(hook-target))
                                        (throw 'hook-exit :escaped)
                                        (funcall expander form env)))))
                            (macroexpand-1 '(hook-target))))))))
        (assert (eq saved *macroexpand-hook*)))
      (assert (= 1 (macroexpand-1 '(hook-target))))
      (write-line "HOOK-PASS"))"#);
}

#[test]
fn hook_preserves_macrolet_and_symbol_macrolet_environments() {
    run(r#"(progn
      (defvar *context-hook-calls* 0)
      (defmacro hook-expand (form &environment environment)
        (macroexpand form environment))
      (let ((*macroexpand-hook*
              (lambda (expander form env)
                (when (equal form '(local-hook-macro 4))
                  (incf *context-hook-calls*))
                (funcall expander form env))))
        (assert (= 35
          (eval '(symbol-macrolet ((hook-symbol 31))
                   (macrolet ((local-hook-macro (x) (list '+ 'hook-symbol x)))
                     (hook-expand (local-hook-macro 4))))))))
      (assert (> *context-hook-calls* 0))
      (write-line "HOOK-PASS"))"#);
}

#[test]
fn identity_hook_preserves_expander_lexical_macros_and_handlers() {
    run(r#"(progn
      (defvar *handler-hook-calls* 0)
      (defmacro hook-signal () (signal 'simple-condition) 42)
      (let ((*macroexpand-hook* (lambda (f form env) (funcall f form env))))
        (assert (= 42 (eval '(macrolet ((outer (x) x))
                              (macrolet ((inner () (outer 42))) (inner))))))
        (handler-bind ((simple-condition
                        (lambda (c) (declare (ignore c)) (incf *handler-hook-calls*))))
          (assert (= 42 (eval '(hook-signal))))))
      (assert (> *handler-hook-calls* 0))
      (write-line "HOOK-PASS"))"#);
}

#[test]
fn compile_file_invokes_hook_and_fasl_loads_without_source() {
    let directory = std::env::temp_dir().join(format!("egcl-hook-compile-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("hook.lisp");
    let fasl = directory.join("hook.bfasl");
    std::fs::write(&source, "(defun hook-compiled-answer () (hook-target 41))").unwrap();
    run(&format!(r#"(progn
      (defvar *compile-hook-calls* 0)
      (defmacro hook-target (x) (list '+ x 1))
      (let ((*macroexpand-hook*
              (lambda (expander form env)
                (when (equal form '(hook-target 41)) (incf *compile-hook-calls*))
                (funcall expander form env))))
        (multiple-value-bind (path warnings failure)
            (compile-file {source:?} :output-file {fasl:?})
          (declare (ignore warnings))
          (assert path)
          (assert (not failure))))
      (assert (> *compile-hook-calls* 0))
      (write-line "HOOK-PASS"))"#));
    std::fs::remove_file(&source).unwrap();
    run(&format!(r#"(progn (load {fasl:?})
      (assert (= 42 (hook-compiled-answer))) (write-line "HOOK-PASS"))"#));
    std::fs::remove_file(&fasl).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
