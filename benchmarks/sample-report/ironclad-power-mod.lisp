;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#+egcl (egcl-ext:setenv "EGCL_NATIVE_TRANSFER" "0" t)
(declaim (optimize (speed 3) (safety 1) (debug 0)))
;;; Extracted unchanged from Ironclad src/math.lisp at
;;; f6519450b47a7648f837126e9f269857033e352a. See LICENSE.ironclad.
;;; Only the package form is omitted; the two upstream functions are unchanged.
(defun power-mod-tab (b k m)
  (declare (optimize (speed 3) (safety 0)))
  (let* ((l (ash 1 (1- k)))
         (tab (make-array l :element-type 'integer :initial-element 1))
         (bi b)
         (bb (mod (* b b) m)))
    (setf (svref tab 0) b)
    (do ((i 1 (1+ i)))
        ((= i l) tab)
      (setq bi (mod (* bi bb) m))
      (setf (svref tab i) bi))))

(defun power-mod (b e m)
  (declare (optimize (speed 3) (safety 0)))
  (cond
    ((zerop e)
      (mod 1 m))
    ((typep e 'fixnum)
      (do ((res 1)) (())
        (when (logbitp 0 e)
          (setq res (mod (* res b) m))
          (when (= 1 e) (return res)))
        (setq e (ash e -1)
              b (mod (* b b) m))))
    (t ;; sliding window variant:
      (let* ((l (integer-length e))
             (k (cond ((< l  65) 3)
                      ((< l 161) 4)
                      ((< l 385) 5)
                      ((< l 897) 6)
                      (t         7)))
             (tab (power-mod-tab b k m))
             (res 1) s u tmp)
        (do ((i (1- l)))
            ((< i 0) res)
          (cond
            ((logbitp i e)
              (setq s (max (1+ (- i k)) 0))
              (do () ((logbitp s e)) (incf s))
              (setq tmp (1+ (- i s)))
              (dotimes (h tmp) (setq res (mod (* res res) m)))
              (setq u (ldb (byte tmp s) e))
              (unless (= u 0) (setq res (mod (* res (svref tab (ash u -1))) m)))
              (setq i (1- s)))
            (t
              (setq res (mod (* res res) m))
              (decf i))))))))


(defun bench-validate ()
  (assert (= (power-mod 2 0 1) 0))
  (assert (= (power-mod 2 10 1000) 24))
  (assert (= (power-mod 17 65537 104729) 77933))
  (assert (= (power-mod 23 9223372036854775809 104729) 80330)))
(defparameter *power-count* 100000)
(defun bench-workload ()
  (let ((sum 0))
    (dotimes (i *power-count*)
      (incf sum (power-mod (+ 2 i) 65537 104729)))
    sum))

;;; Same numeric argument types and loop shape, with a short training batch.
(defun bench-train ()
  (let ((*power-count* 1))
    (dotimes (i 10000) (bench-workload))))

#+egcl (egcl-ext:setenv "EGCL_NATIVE_TRANSFER" "1" t)
(bench-validate)
(bench-train)
(dotimes (warmup 3) (unless (= (bench-workload) 5242584863) (error "Warmup checksum failed")))

#+egcl
(defun bench-report-tiers (phase)
  (format t "BENCH-TIER ~a POWER-MOD ~d ~d ~d ~d~%" phase (egcl-ext:function-tier 'power-mod) (egcl-ext:function-invoke-count 'power-mod) (egcl-ext:function-back-edge-count 'power-mod) (egcl-ext:function-osr-count 'power-mod))
(format t "BENCH-TIER ~a BENCH-WORKLOAD ~d ~d ~d ~d~%" phase (egcl-ext:function-tier 'bench-workload) (egcl-ext:function-invoke-count 'bench-workload) (egcl-ext:function-back-edge-count 'bench-workload) (egcl-ext:function-osr-count 'bench-workload)))
#+egcl
(progn
  (dotimes (attempt 1000)
    (when (and (eql 2 (egcl-ext:function-tier 'power-mod)) (eql 2 (egcl-ext:function-tier 'bench-workload))) (return))
    (sleep 0.01))
  (unless (and (eql 2 (egcl-ext:function-tier 'power-mod)) (eql 2 (egcl-ext:function-tier 'bench-workload))) (error "Timed functions did not reach T2"))
  (bench-report-tiers "before"))

(let* ((deopts-before #+egcl (egcl-ext:deopt-count) #-egcl 0)
       (start (get-internal-real-time))
       (value (let ((value nil))
                (dotimes (repeat 1 value)
                  (setf value (bench-workload)))))
       (end (get-internal-real-time))
       (deopts-after #+egcl (egcl-ext:deopt-count) #-egcl 0))
  (format t "BENCH ~d ~d ~d~%" (- end start) internal-time-units-per-second value)
  (format t "BENCH-DEOPTS ~d~%" (- deopts-after deopts-before)))
#+egcl (bench-report-tiers "after")
