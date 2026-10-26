// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

#[test]
fn packed_u8_native_producers_recover_errors_and_preserve_svref_contract() {
    let program = r#"
      (declaim (notinline u8-native-read u8-native-store u8-native-svstore))
      (defun u8-native-read (array index)
        (let ((result (aref array index)))
          (if result result nil)))
      (defun u8-native-store (array index byte)
        (let ((result (setf (aref array index) byte)))
          (if (eq result byte) result :wrong-result)))
      (defun u8-native-svstore (array index value)
        (let ((result (setf (svref array index) value)))
          (if (eq result value) result :wrong-result)))
      (defun u8-native-type-error-p (thunk)
        (handler-case (progn (funcall thunk) nil)
          (type-error () t)))
      (defvar *u8-native-argument-effects* 0)
      (let ((small (make-array 8 :element-type '(unsigned-byte 8) :initial-element 17))
            (large (make-array 600000 :element-type '(unsigned-byte 8) :initial-element 17))
            (general (make-array 8 :initial-element 17))
            (doubles (make-array 2 :element-type 'double-float :initial-element 1.25d0))
            (token (vector :heap-reference :native-barrier)))
        (dotimes (i 150)
          (assert (= (u8-native-read small 0) 17))
          (assert (= (u8-native-read general 0) 17))
          (assert (= (u8-native-store small 0 17) 17))
          (assert (= (u8-native-store general 0 17) 17))
          (assert (= (u8-native-svstore general 0 17) 17)))
        ;; Compilation is asynchronous. Require actual installation rather
        ;; than assuming FORCE_TIER or a fixed warmup count establishes it.
        (loop repeat 50
              until (and (= 2 (egcl-ext:function-tier 'u8-native-read))
                         (= 2 (egcl-ext:function-tier 'u8-native-store))
                         (= 2 (egcl-ext:function-tier 'u8-native-svstore)))
              do (dotimes (i 20)
                   (u8-native-read small 0)
                   (u8-native-store small 0 17)
                   (u8-native-svstore general 0 17))
                 (sleep 0.01))
        (assert (= 2 (egcl-ext:function-tier 'u8-native-read)))
        (assert (= 2 (egcl-ext:function-tier 'u8-native-store)))
        (assert (= 2 (egcl-ext:function-tier 'u8-native-svstore)))
        ;; The same installed entries accept both layouts and both header
        ;; classes without falling through deoptimization on valid arguments.
        (let ((before (egcl-ext:deopt-count)))
          (dolist (array (list small large general))
            (dolist (index (list 0 (1- (length array))))
              (dolist (byte '(0 1 127 128 255))
                (assert (= (u8-native-store array index byte) byte))
                (assert (= (u8-native-read array index) byte)))))
          (assert (eq (u8-native-svstore general 1 token) token))
          (assert (eq (u8-native-read general 1) token))
          (assert (eq (svref general 1) token))
          (assert (= before (egcl-ext:deopt-count))))
        ;; Argument evaluation occurs once before the native access. A guard
        ;; must recover the original call's state without replaying arguments,
        ;; and the invalid byte must never be truncated into the payload.
        (let ((store-deopts-before (egcl-ext:deopt-count)))
        (dolist (bad-byte (list -1 256 'not-a-byte))
          (setf (aref small 3) 73 *u8-native-argument-effects* 0)
          (assert
            (u8-native-type-error-p
              (lambda ()
                (u8-native-store
                  (progn (incf *u8-native-argument-effects*) small)
                  (progn (incf *u8-native-argument-effects*) 3)
                  (progn (incf *u8-native-argument-effects*) bad-byte)))))
          (assert (= *u8-native-argument-effects* 3))
          (assert (= (aref small 3) 73)))
        (assert (> (egcl-ext:deopt-count) store-deopts-before)))
        (let ((before (egcl-ext:deopt-count)))
          (assert (u8-native-type-error-p (lambda () (u8-native-read large -1))))
          (assert (> (egcl-ext:deopt-count) before)))
        (dolist (bad-index (list -1 8 'not-an-index))
          (let ((contents-before (copy-seq small)))
          (setf *u8-native-argument-effects* 0)
          (assert (u8-native-type-error-p
            (lambda ()
              (u8-native-store
                (progn (incf *u8-native-argument-effects*) small)
                (progn (incf *u8-native-argument-effects*) bad-index)
                (progn (incf *u8-native-argument-effects*) 99)))))
          (assert (= *u8-native-argument-effects* 3))
          (assert (= (aref small 3) 73))
          (assert (equalp small contents-before))
          (assert (u8-native-type-error-p (lambda () (u8-native-read small bad-index))))))
        (assert (u8-native-type-error-p (lambda () (u8-native-read large 600000))))
        (assert (u8-native-type-error-p (lambda () (u8-native-read #() 0))))
        ;; SVREF stores retain a distinct vector-only native contract, including
        ;; after an ordinary AREF store has accepted the same packed object.
        (setf (aref small 0) 61)
        (let ((before (egcl-ext:deopt-count)))
          (assert (u8-native-type-error-p (lambda () (u8-native-svstore small 0 99))))
          (assert (> (egcl-ext:deopt-count) before)))
        (assert (= (aref small 0) 61))
        (assert (u8-native-type-error-p (lambda () (u8-native-svstore doubles 0 token))))
        (assert (= (aref doubles 0) 1.25d0))
        (assert (eq (u8-native-svstore general 2 token) token))
        (assert (eq (svref general 2) token)))
      (format t "PACKED-U8-NATIVE-OK~%")
    "#;
    let mut results = Vec::new();
    for stress in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_IR_FULL", "1")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .output()
            .expect("run packed U8 native CLI regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        for target in ["U8-NATIVE-READ", "U8-NATIVE-STORE", "U8-NATIVE-SVSTORE"] {
            for evidence in [
                "ordinary x86-64 Lisp T2 emitted;",
                "installed with ordinary x86-64 Lisp T2 ABI",
            ] {
                assert!(stderr.lines().any(|line|
                    line.contains(&format!("{target}:")) && line.contains(evidence)),
                    "stress={stress}: missing actual {target} {evidence}\n{stderr}");
            }
        }
        for (target, opcode) in [
            ("U8-NATIVE-READ", "VecRef("),
            ("U8-NATIVE-STORE", "VecSet("),
            ("U8-NATIVE-SVSTORE", "VecSet("),
        ] {
            assert!(
                stderr.split("[ir] ===== function ").skip(1).any(|body| body
                    .lines()
                    .next()
                    .is_some_and(|header| header.contains(target))
                    && body.contains(opcode)),
                "stress={stress}: {target} never produced {opcode} native IR\n{stderr}"
            );
        }
        assert!(stdout.contains("PACKED-U8-NATIVE-OK"), "{stdout}\n{stderr}");
        results.push(stdout);
    }
    assert_eq!(
        results[0], results[1],
        "GC stress changes native U8 semantics"
    );
}
