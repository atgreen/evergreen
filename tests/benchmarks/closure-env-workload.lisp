;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Closure and captured-environment microbenchmarks (Backpack stage 0,
;;; bliss-gg3i1.7.15.1). Each workload is an ordinary DEFUN called many
;;; times, so invocation tiering applies; `closure-env.sh` pins the tier with
;;; EGCL_FORCE_TIER. Every workload validates its result.
;;;
;;; Output, one line per workload:
;;;   BENCH-NS <name> <ns-per-op> <ops>
;;; The timer is GET-INTERNAL-REAL-TIME. Each workload doubles its batch until
;;; one batch runs for at least *closure-env-min-ms*; the doubling also warms
;;; the code.

(defvar *closure-env-min-ms* 300)

;;; 1. Closure creation: MAKE-ADDER allocates a capturing closure per call.
(defun closure-env-make-adder (n)
  (lambda (x) (+ x n)))

(defun closure-env-create (count)
  (let ((last nil))
    (dotimes (i count)
      (setq last (closure-env-make-adder i)))
    (funcall last 1)))

;;; 2. A closure reading and incrementing a captured counter.
(defun closure-env-counter (count)
  (let ((counter 0))
    (let ((bump (lambda () (setq counter (+ counter 1)))))
      (dotimes (i count)
        (funcall bump))
      counter)))

;;; 3. LABELS recursion over captured variables.
(defun closure-env-labels (count)
  (let ((step 2)
        (total 0))
    (labels ((walk (i)
               (when (> i 0)
                 (setq total (+ total step))
                 (walk (- i 1)))))
      (dotimes (outer (floor count 64))
        (walk 64)))
    total))

;;; 4. Nested LETs whose variables are captured: every call allocates three
;;; child frames, defines a variable in each and reads all three.
(defun closure-env-nested-lets-1 (i)
  (let ((a i))
    (let ((b (+ a 1)))
      (let ((c (+ b 1)))
        (funcall (lambda () (+ a b c)))))))

(defun closure-env-nested-lets (count)
  (let ((sum 0))
    (dotimes (i count sum)
      (setq sum (+ sum (closure-env-nested-lets-1 1))))))

;;; 5. Deep frame-chain reads: X is three frames above the closure's own frame.
(defun closure-env-deep-read (count)
  (let ((x 1))
    (let ((y 2))
      (let ((z 3))
        (funcall (lambda ()
                   (let ((sum 0))
                     (dotimes (i count)
                       (setq sum (+ sum x)))
                     (+ sum y z))))))))

(defun closure-env-expected (name count)
  (cond ((string= name "create") (+ (1- count) 1))
        ((string= name "counter") count)
        ((string= name "labels") (* 2 64 (floor count 64)))
        ((string= name "nested-lets") (* 6 count))
        ((string= name "deep-read") (+ count 5))))

(defun closure-env-run (name fn)
  (let ((count 64))
    (loop
      (let ((start (get-internal-real-time)))
        (let ((result (funcall fn count)))
          (unless (eql result (closure-env-expected name count))
            (error "closure-env ~a: got ~a for ~a" name result count)))
        (let ((elapsed (- (get-internal-real-time) start)))
          (when (>= (* 1000 elapsed) (* *closure-env-min-ms*
                                         internal-time-units-per-second))
            (format t "BENCH-NS ~a ~,1f ~a~%" name
                    (/ (* elapsed (/ 1000000000 internal-time-units-per-second))
                       count 1.0)
                    count)
            (return))
          (setq count (* count 2)))))))

(defun closure-env-main (names)
  (dolist (entry '(("create" closure-env-create)
                   ("counter" closure-env-counter)
                   ("labels" closure-env-labels)
                   ("nested-lets" closure-env-nested-lets)
                   ("deep-read" closure-env-deep-read)))
    (when (or (null names) (member (first entry) names :test #'string=))
      (closure-env-run (first entry) (symbol-function (second entry))))))
