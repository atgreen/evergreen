// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const DEFINITIONS: &str = r#"
(defmacro literal-restart-hidden () '(incf captured))
(defun literal-restart-maker (captured)
 (list (lambda () (incf captured))
       (lambda () captured)
       (lambda ()
        (restart-case
          (let ((restart (find-restart 'literal-done)))
           (assert restart)
           (assert (eq restart (find-restart 'literal-done)))
           (invoke-restart restart))
         (literal-done () :report "Retry assertion." :literal)))))
(defun captured-restart-controls ()
 (let ((captured 41))
  (flet ((touch () (incf captured)))
   (list
    (restart-case (invoke-restart 'hidden)
     (hidden () (literal-restart-hidden)))
    (restart-case (invoke-restart 'defaulted)
     (defaulted (&optional (value (incf captured))) value))
    (restart-case (invoke-restart 'option)
     (option () :report (progn (incf captured) (lambda (stream) (declare (ignore stream)) captured)) :option))
    captured))))
(defun control-restart ()
 (block outside
  (restart-case (invoke-restart 'escape)
   (escape () (return-from outside :escaped)))))
(defun special-restart ()
 (let ((*literal-restart-special* :dynamic))
  (declare (special *literal-restart-special*))
  (restart-case (invoke-restart 'read-special)
   (read-special () *literal-restart-special*))))
(defun failing-restart (captured)
 (flet ((touch () (incf captured)))
  (restart-case (invoke-restart 'bad)
   (bad () (incf captured) (error "expected restart error")))))
"#;

const CHECKS: &str = r#"
(let ((callbacks (literal-restart-maker 0)))
 (dotimes (i 30)
  (assert (= (funcall (car callbacks)) (1+ i)))
  (assert (= (funcall (cadr callbacks)) (1+ i)))
  (assert (eq (funcall (caddr callbacks)) :literal))
  (assert (null (find-restart 'literal-done)))))
(assert (equal (captured-restart-controls) '(42 43 :option 44)))
(assert (eq (control-restart) :escaped))
(assert (eq (special-restart) :dynamic))
(assert (handler-case (progn (failing-restart 0) nil) (error () t)))
(assert (null (find-restart 'bad)))
(assert (eq (funcall (caddr (literal-restart-maker 7))) :literal))
(format t "~&LITERAL-RESTART-FRAMES-PASS~%")
"#;

fn run(program: &str, tier: Option<&str>, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_HEAP_ENV",
    ] {
        command.env_remove(key);
    }
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "tier={tier:?} stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn literal_restart_frames_preserve_invocation_identity_captures_and_cleanup() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let output = run(&format!("(progn {DEFINITIONS} {CHECKS})"), tier, false);
        assert!(output.contains("LITERAL-RESTART-FRAMES-PASS"), "{output}");
    }
}

#[test]
fn literal_restart_frames_preserve_captures_under_moving_gc_stress() {
    let output = run(&format!("(progn {DEFINITIONS} {CHECKS})"), None, true);
    assert!(output.contains("LITERAL-RESTART-FRAMES-PASS"), "{output}");
}

#[test]
fn literal_restart_frames_survive_source_free_bytecode_reload() {
    let directory = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("literal-restart-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("definitions.lisp");
    let fasl = directory.join("definitions.bfasl");
    std::fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(compile-file {:?} :output-file {:?})",
            source.to_string_lossy(),
            fasl.to_string_lossy()
        ),
        None,
        false,
    );
    assert!(fasl.is_file());
    std::fs::remove_file(&source).unwrap();
    let output = run(
        &format!("(progn (load {:?}) {CHECKS})", fasl.to_string_lossy()),
        None,
        false,
    );
    assert!(output.contains("LITERAL-RESTART-FRAMES-PASS"), "{output}");
    std::fs::remove_dir_all(directory).unwrap();
}
