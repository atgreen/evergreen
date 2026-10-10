// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check(program: &str, expected: &str) {
    check_with_admission(program, expected, false);
}

fn check_with_admission(program: &str, expected: &str, check_tier: bool) {
    for (tier, native) in [("interp", "0"), ("t0", "0"), ("t2", "0"), ("t2", "1")] {
        if tier == "t2" && !cfg!(all(target_arch = "x86_64", unix)) {
            continue;
        }
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", native)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run global symbol designator regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.lines().any(|line| line == expected),
            "tier={tier}, native={native}: {}\n{stdout}\n{stderr}",
            output.status
        );
        if check_tier && tier == "t2" {
            let installed = 2;
            assert!(
                stdout
                    .lines()
                    .any(|line| line == format!("TIER {installed}")),
                "expected actual admission tier {installed}: {stdout}"
            );
        }
    }
}

#[test]
fn symbol_funcall_and_apply_keep_global_definition_scope_under_flet() {
    check(
        r#"
      (setf (symbol-value 'designator-value) :global)
      (defun designator-global () (values designator-value :global-mv))
      (let ((designator-value :captured))
        (defun designator-captured () (values designator-value :captured-mv)))
      (let ((designator-value :caller))
        (flet ((designator-global () :local)
               (designator-captured () :local))
          (format t "SCOPE ~S~%"
            (list (multiple-value-list (funcall 'designator-global))
                  (multiple-value-list (apply 'designator-global nil))
                  (multiple-value-list (funcall 'designator-captured))
                  (multiple-value-list (apply 'designator-captured nil))))))
    "#,
        "SCOPE ((:GLOBAL :GLOBAL-MV) (:GLOBAL :GLOBAL-MV) (:CAPTURED :CAPTURED-MV) (:CAPTURED :CAPTURED-MV))",
    );
}

#[test]
fn symbol_designator_forwarding_preserves_builtins_and_setf_writers() {
    check(
        r#"
      (defun (setf designator-place) (value cell) (setf (car cell) value))
      (let ((cell (list 0)))
        (format t "WRITER ~S~%"
          (list (funcall 'car '(17))
                (funcall #'(setf designator-place) 23 cell)
                (apply #'(setf designator-place) (list 29 cell))
                (car cell))))
    "#,
        "WRITER (17 23 29 29)",
    );
}

#[test]
fn source_free_symbol_designators_keep_registered_dispatch_under_flet() {
    let dir = std::env::temp_dir().join(format!("egcl-symbol-designator-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("global.lisp");
    let artifact = dir.join("global.bfasl");
    // Each subprocess recreates the source, compiles it, and removes it before
    // loading. The saved function must dispatch its registered bytecode even
    // while a same-named lexical function is visible to the caller.
    let program = format!(
        r#"
      (with-open-file (out {:?} :direction :output :if-exists :supersede)
        (write-line "(defun designator-saved (x) (values (+ x 7) :saved-mv))" out))
      (compile-file {:?} {:?})
      (delete-file {:?})
      (load {:?})
      (dotimes (i 40) (funcall 'designator-saved i))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'designator-saved))
      (flet ((designator-saved (x) (values x :local-mv)))
        (format t "SAVED ~S~%"
          (list (multiple-value-list (funcall 'designator-saved 10))
                (multiple-value-list (apply 'designator-saved '(20))))))
    "#,
        source.to_str().unwrap(),
        source.to_str().unwrap(),
        artifact.to_str().unwrap(),
        source.to_str().unwrap(),
        artifact.to_str().unwrap()
    );
    check_with_admission(&program, "SAVED ((17 :SAVED-MV) (27 :SAVED-MV))", true);
    std::fs::remove_dir_all(dir).unwrap();
}
