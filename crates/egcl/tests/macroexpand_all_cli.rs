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
    assert!(stdout.contains("EXPAND-ALL-PASS"), "{stdout}\n{stderr}");
}

#[test]
fn optional_bindings_shadow_symbol_macros_in_compiler_expansion() {
    run(r#"(progn
      (assert (= 1 (macrolet ((probe () (symbol-macrolet ((x 99))
                      ((lambda (&optional (x 1)) x))))) (probe))))
      (assert (= 1 (macrolet ((probe () (symbol-macrolet ((x 99))
                      (flet ((f (&optional (x 1)) x)) (f))))) (probe))))
      (assert (= 1 (macrolet ((probe () (symbol-macrolet ((x 99))
                      (labels ((f (&optional (x 1)) x)) (f))))) (probe))))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn recursive_expansion_preserves_quotes_and_expands_local_macros() {
    run(r#"(progn
      (assert (eq :external (nth-value 1 (find-symbol "MACROEXPAND-ALL" "EGCL-EXT"))))
      (defmacro expanded-number () 41)
      (assert (equal (egcl-ext:macroexpand-all '(list '(expanded-number) (expanded-number)))
                     '(list '(expanded-number) 41)))
      (assert (equal (egcl-ext:macroexpand-all
                       '(macrolet ((m (x) (list '+ x 1)))
                          (symbol-macrolet ((x 8)) (m x)))) '(+ 8 1)))
      (assert (= 3 (eval (egcl-ext:macroexpand-all '(loop repeat 3 count t)))))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn lambda_initializers_have_sequential_scope_and_supplied_p_bindings() {
    run(r#"(progn
      (defmacro initial-value () 7)
      (let ((expanded
              (egcl-ext:macroexpand-all
                '(symbol-macrolet ((x 11) (xp 99) (z 33))
                   (lambda (&optional (x (initial-value) xp)
                            &key ((:other y) x yp) &aux (z y))
                     (list x xp y yp z))))))
        (assert (equal expanded
                 '(lambda (&optional (x 7 xp) &key ((:other y) x yp) &aux (z y))
                    (list x xp y yp z))))
        (assert (equal (funcall (eval expanded)) '(7 nil 7 nil 7))))
      (assert (equal (egcl-ext:macroexpand-all
                       '(symbol-macrolet ((x 11)) (lambda (&optional (x x)) x)))
                     '(lambda (&optional (x 11)) x)))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn lexical_environment_and_global_symbol_macros_are_distinct() {
    run(r#"(progn
      (define-symbol-macro global-expansion 42)
      (assert (= 42 (egcl-ext:macroexpand-all 'global-expansion)))
      (defmacro expand-here (form &environment env)
        (list 'quote (egcl-ext:macroexpand-all form env)))
      (assert (equal (macrolet ((m () 12))
                       (symbol-macrolet ((x 23)) (expand-here (list (m) x))))
                     '(list 12 23)))
      (assert (eq 'local-only
                  (symbol-macrolet ((local-only 77))
                    (egcl-ext:macroexpand-all 'local-only nil))))
      (assert (handler-case (progn (egcl-ext:macroexpand-all 'x 123) nil)
                (program-error () t)))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn local_functions_shadow_global_compiler_macros() {
    run(r#"(progn
      (defun cm-target () 9)
      (define-compiler-macro cm-target () 99)
      (assert (equal '(cm-target) (egcl-ext:macroexpand-all '(cm-target))))
      (assert (= 1 (eval (egcl-ext:macroexpand-all
                          '(flet ((cm-target () 1)) (cm-target))))))
      (assert (= 2 (eval (egcl-ext:macroexpand-all
                          '(labels ((cm-target () 2)) (cm-target))))))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn load_time_value_uses_global_environment_and_errors_propagate() {
    run(r#"(progn
      (defmacro ltv-macro () 41)
      (define-symbol-macro ltv-symbol 17)
      (assert (equal
                (egcl-ext:macroexpand-all
                  '(macrolet ((ltv-macro () 99))
                     (symbol-macrolet ((ltv-symbol 88))
                       (load-time-value (list (ltv-macro) ltv-symbol) t))))
                '(load-time-value (list 41 17) t)))
      (defmacro expansion-error () (error "expansion failure"))
      (assert (handler-case (progn (egcl-ext:macroexpand-all '(list (expansion-error))) nil)
                (error () t)))
      (assert (handler-case
                (progn (egcl-ext:macroexpand-all '(egcl-ext:atomic-incf x)) nil)
                (program-error () t)))
      (defmacro generated-atomic () '(egcl-ext:atomic-incf x))
      (assert (handler-case
                (progn (egcl-ext:macroexpand-all '(generated-atomic)) nil)
                (program-error () t)))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn inline_methods_expand_initializers_before_discarding_macrolet() {
    run(r#"(progn
      (eval (egcl-ext:macroexpand-all
              '(macrolet ((initial-method-value () 7))
                 (defgeneric expanded-generic (x &optional y)
                   (:method ((x t) &optional (y (initial-method-value))) y)))))
      (assert (= 7 (expanded-generic :argument)))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn lambda_binding_declarations_are_visible_in_later_initializers() {
    run(r#"(progn
      (defmacro seen-special (&environment env)
        (list 'quote (egcl-ext:declaration-specifier 'special env)))
      (assert (equal (egcl-ext:macroexpand-all
                       '(lambda (x &optional (y (seen-special)))
                          (declare (special x)) y))
                     '(lambda (x &optional (y '(special x)))
                        (declare (special x)) y)))
      (assert (equal (egcl-ext:macroexpand-all
                       '(symbol-macrolet ((x 11))
                          (lambda (&optional (x x)) (declare (special x)) x)))
                     '(lambda (&optional (x 11)) (declare (special x)) x)))
      (write-line "EXPAND-ALL-PASS"))"#);
}

#[test]
fn unsupported_macro_definers_signal_instead_of_losing_default_scope() {
    run(r#"(progn
      (assert (handler-case
                (progn (egcl-ext:macroexpand-all
                         '(macrolet ((m () 7))
                            (defmacro generated (&optional (x (m))) x))) nil)
                (program-error () t)))
      (assert (handler-case
                (progn (egcl-ext:macroexpand-all
                         '(define-compiler-macro generated (&optional (x 7)) x)) nil)
                (program-error () t)))
      (write-line "EXPAND-ALL-PASS"))"#);
}
