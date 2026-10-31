// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bodies the T1/T2 IR builder used to refuse leave T0 (bliss-gg3i1.7.11).
//!
//! - A DEFSTRUCT accessor or predicate in a body compiled without deopt
//!   metadata (here a TAGBODY around a LOOP, as SBCL's DO-NODES expands) was
//!   refused by the mapped emitter, since nothing could recover the leaf
//!   helper's miss; T1 was declined and the body stayed in T0. SBCL's
//!   IR1-OPTIMIZE-BLOCK and IR2-CONVERT-BLOCK are such bodies.
//! - A BLOCK or TAGBODY entered with values already on the operand stack, as
//!   in `(+ (length l) (loop ...))`, was refused outright ("handler with
//!   non-empty sp_restore").
//!
//! Each function must reach T2 with the same answers, including after a
//! numeric speculation inside such a BLOCK fails.

use std::process::Command;

const PROGRAM: &str = r#"
(defstruct sb-node (kind 0) (next nil) (flag t))
(defstruct (sb-ref (:include sb-node)))
(defvar *chain*
  (let ((n nil))
    (dotimes (i 6 n)
      (setf n (if (evenp i) (make-sb-ref :kind i :next n) (make-sb-node :kind i :next n))))))
(defun sb-walk (node)
  (let ((sum 0))
    (tagbody
     again
       (loop
         (unless node (return))
         (when (sb-node-flag node)
           (setf (sb-node-flag node) t)
           (typecase node
             (sb-ref (incf sum 10))
             (sb-node (incf sum (sb-node-kind node)))))
         (setf node (sb-node-next node))))
    sum))
(defun sp-block (list limit)
  (+ (length list)
     (loop for x in list
           when (> x limit) return x
           sum x)))
(defun sp-tagbody (n)
  (list :head
        (let ((i 0) (acc 0))
          (tagbody
           top
             (when (< i n)
               (incf acc (if (oddp i) (progn (incf i) (go top)) i))
               (incf i)
               (go top)))
          acc)
        :tail))
(defun sp-nested (list)
  (cons (block outer
          (dolist (x list :none)
            (when (eq x :stop) (return-from outer (list :stopped (length list))))))
        (+ 1 (block inner (dolist (x list 0) (when (numberp x) (return-from inner x)))))))
(defun sc-drive (n)
  (let ((total 0))
    (dotimes (i n total)
      (incf total (sb-walk *chain*))
      (incf total (sp-block '(1 2 3 40 5) 30))
      (incf total (second (sp-tagbody 10)))
      (incf total (length (car (sp-nested '(a 7 :stop b))))))))
(defun sc-tiers ()
  (mapcar #'egcl-ext:function-tier '(sb-walk sp-block sp-tagbody sp-nested)))
;; Promotion is compiled in the background: give it a bounded time to land.
;; FUNCTION-TIER is 0 for T0, 1 for T1 and 2 for T2.
(dotimes (round 400) (sc-drive 100) (when (every (lambda (tier) (= tier 2)) (sc-tiers)) (return)) (sleep 0.025))
(format t "TIERS ~S~%" (sc-tiers))
(format t "RESULT ~S ~S ~S ~S ~S~%" (sc-drive 10) (sp-block '(1 2 3) 30) (sp-tagbody 10)
        (sp-nested '(a 7 :stop b)) (sp-nested '(a b)))
;; A float defeats the loop's fixnum speculation inside the nonempty BLOCK.
(format t "DEOPT ~S ~S~%" (sp-block '(1.5 2 3) 30) (sp-block '(1 2 3 40 5) 30))
"#;

#[test]
fn scoped_bodies_reach_t2() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {PROGRAM})")])
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_DISABLE_T2")
        .env_remove("EGCL_GC_STRESS")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("RESULT 1060 9 (:HEAD 20 :TAIL) ((:STOPPED 4) . 8) (:NONE . 1)"),
        "{stdout}"
    );
    assert!(stdout.contains("DEOPT 9.5 45"), "{stdout}");
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        assert!(stdout.contains("TIERS (2 2 2 2)"), "{stdout}\n{stderr}");
    }
}
