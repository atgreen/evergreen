// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
(let ((condition (make-condition 'type-error :datum 5 :expected-type 'string)))
  ;; Force uncached AMOP introspection before reading an already-allocated
  ;; condition. Finalization must preserve its physical slots and values.
  (let ((slots (egcl-mop:class-slots (class-of condition))))
    (assert (find "DATUM" slots :key (lambda (slot)
              (symbol-name (egcl-mop:slot-definition-name slot))) :test #'string=)))
  (assert (= (type-error-datum condition) 5))
  (assert (eq (type-error-expected-type condition) 'string)))
(assert (eq (handler-case (+ 1 'a) (type-error (c) (type-error-datum c))) 'a))
(assert (handler-case (symbol-value (gensym))
          (unbound-variable (c) (symbolp (cell-error-name c)))))
(assert (handler-case (funcall (gensym))
          (undefined-function (c) (symbolp (cell-error-name c)))))
(defclass condition-slot-probe () ((missing)))
(let ((object (make-instance 'condition-slot-probe)))
  (assert (handler-case (slot-value object 'missing)
            (unbound-slot (c)
              (and (eq (unbound-slot-instance c) object)
                   (eq (cell-error-name c) 'missing))))))
(let ((condition (make-condition 'division-by-zero :operation '/ :operands '(1 0))))
  (egcl-mop:class-slots (class-of condition))
  (assert (eq (arithmetic-error-operation condition) '/))
  (assert (equal (arithmetic-error-operands condition) '(1 0))))
(let ((condition (make-condition 'simple-error
                   :format-control "hi ~a" :format-arguments '(1))))
  (egcl-mop:class-slots (class-of condition))
  (assert (string= (simple-condition-format-control condition) "hi ~a"))
  (assert (equal (simple-condition-format-arguments condition) '(1))))
(define-condition inherited-condition-slot-probe (type-error) ((extra :initarg :extra)))
(let ((condition (make-condition 'inherited-condition-slot-probe
                   :datum 7 :expected-type 'string :extra 9)))
  (egcl-mop:class-slots (class-of condition))
  (assert (= (type-error-datum condition) 7))
  (assert (= (slot-value condition 'extra) 9)))
(write-line "CONDITION-SLOT-METADATA-OK")
"#;

#[test]
fn condition_readers_preserve_raw_class_slots_through_finalization() {
    for stress in [false, true] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
            .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}: {stdout}\n{stderr}"
        );
        assert!(stdout.contains("CONDITION-SLOT-METADATA-OK"), "{stdout}");
    }
}
