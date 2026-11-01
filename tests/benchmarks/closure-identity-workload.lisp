;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Closure identity microbenchmarks (Backpack stage 2, bliss-gg3i1.7.15.3):
;;; the cost of creating closure instances of one template, of calling a
;;; fresh instance once, and of calling one existing instance, plus the host
;;; memory a retained instance costs and the collections a creation loop
;;; causes. Each workload is an ordinary DEFUN called with doubling batches,
;;; so invocation tiering applies as in an application.
;;;
;;; Output:
;;;   BENCH-NS <name> <ns-per-op> <ops>
;;;   BENCH-BYTES <name> <host-bytes-per-closure> <count>
;;; The timer is GET-INTERNAL-REAL-TIME.

(defvar *closure-identity-min-ms* 300)

(defun closure-identity-make-adder (n)
  (lambda (x) (+ x n)))

(defun closure-identity-make-constant ()
  (lambda (x) (+ x 1)))

(defun closure-identity-make-exit (n)
  ;; A free RETURN-FROM: the instance carries its creator's block scope.
  (block outer
    (lambda (x) (if (eql x n) (return-from outer x) (+ x n)))))

;;; 1. Creation of a capturing closure.
(defun closure-identity-create (count)
  (let ((last nil))
    (dotimes (i count)
      (setq last (closure-identity-make-adder i)))
    (funcall last 1)))

;;; 1b. Creation in the creator's own activation: no call per closure, so
;;; this isolates MakeClosure from the cost of entering a capturing caller.
(defun closure-identity-create-inline (count)
  (let ((k 1) (last nil))
    (dotimes (i count)
      (setq last (lambda (x) (+ x k))))
    (+ (funcall last count) -1)))

;;; 2. Creation of a closure that captures nothing.
(defun closure-identity-create-constant (count)
  (let ((last nil))
    (dotimes (i count)
      (setq last (closure-identity-make-constant)))
    (+ (funcall last 0) count -1)))

;;; 3. Creation of a closure carrying a lexical exit scope.
(defun closure-identity-create-exit (count)
  (let ((last nil))
    (dotimes (i count)
      (setq last (closure-identity-make-exit i)))
    (funcall last 1)))

;;; 4. Create an instance and call it once.
(defun closure-identity-call-once (count)
  (let ((sum 0))
    (dotimes (i count)
      (setq sum (+ sum (funcall (closure-identity-make-adder 1) 0))))
    sum))

;;; 5. Call one existing instance.
(defun closure-identity-call-existing (count)
  (let ((f (closure-identity-make-adder 1))
        (sum 0))
    (dotimes (i count)
      (setq sum (funcall f sum)))
    sum))

(defun closure-identity-expected (name count)
  (cond ((string= name "create") count)
        ((string= name "create-inline") count)
        ((string= name "create-constant") count)
        ((string= name "create-exit") count)
        ((string= name "call-once") count)
        ((string= name "call-existing") count)))

(defun closure-identity-run (name fn)
  (let ((count 64))
    (loop
      (let ((start (get-internal-real-time)))
        (let ((result (funcall fn count)))
          (unless (eql result (closure-identity-expected name count))
            (error "closure-identity ~a: got ~a for ~a" name result count)))
        (let ((elapsed (- (get-internal-real-time) start)))
          (when (>= (* 1000 elapsed) (* *closure-identity-min-ms*
                                         internal-time-units-per-second))
            (format t "BENCH-NS ~a ~,1f ~a~%" name
                    (/ (* elapsed (/ 1000000000 internal-time-units-per-second))
                       count 1.0)
                    count)
            (return))
          (setq count (* count 2)))))))

(defun closure-identity-rss-bytes ()
  ;; Resident pages from /proc/self/statm, in bytes (4 KiB pages).
  (with-open-file (in "/proc/self/statm")
    (let ((line (read-line in)))
      (* 4096 (parse-integer line :start (1+ (position #\Space line))
                                  :junk-allowed t)))))

;;; 6. Host bytes per retained capturing closure: RSS growth while COUNT
;;; instances stay reachable, net of the list that holds them.
(defun closure-identity-retained-bytes (count)
  (egcl-ext:gc :full t)
  (let* ((before (closure-identity-rss-bytes))
         (cells (make-list count))
         (after-list (closure-identity-rss-bytes)))
    (do ((tail cells (cdr tail))
         (i 0 (1+ i)))
        ((null tail))
      (setf (car tail) (closure-identity-make-adder i)))
    (let ((after (closure-identity-rss-bytes)))
      (format t "BENCH-BYTES retained ~,1f ~a~%"
              (/ (- after after-list) count 1.0) count)
      (unless (eql (funcall (car (last cells)) 1) count)
        (error "closure-identity retained: wrong result"))
      (- after before))))

(defun closure-identity-main (names)
  (dolist (entry '(("create" closure-identity-create)
                   ("create-inline" closure-identity-create-inline)
                   ("create-constant" closure-identity-create-constant)
                   ("create-exit" closure-identity-create-exit)
                   ("call-once" closure-identity-call-once)
                   ("call-existing" closure-identity-call-existing)))
    (when (or (null names) (member (first entry) names :test #'string=))
      (closure-identity-run (first entry) (symbol-function (second entry)))))
  (when (or (null names) (member "retained" names :test #'string=))
    (closure-identity-retained-bytes 200000))
  (when (or (null names) (member "gc" names :test #'string=))
    ;; Collections caused by creation loops of 4M dropped instances.
    (time (closure-identity-create 4000000))
    (time (closure-identity-create-inline 4000000))))
