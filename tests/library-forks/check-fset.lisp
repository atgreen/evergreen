;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; The atgreen/fset fork's EGCL lock layer (bliss-q00o). Run after check.lisp,
;;;; preserving the same environment/cache.
;;;;
;;;; fset's Code/port.lisp defines make-lock / with-lock / read-memory-barrier /
;;;; write-memory-barrier under a CLOSED list of per-implementation reader
;;;; conditionals with no #-(or ...) fallback, so before the fork EGCL had none
;;;; of them and loading Code/tuples.lisp died at (make-lock "Tuple Key Lock").
;;;; SBCL passes this file unchanged, which is the point: the fork must not
;;;; change any other implementation's behaviour.
;;;;
;;;; LOAD reads and evaluates one form at a time, so the FSET package exists by
;;;; the time the forms below are READ and can be referenced directly.
(load (merge-pathnames "check.lisp" *load-truename*))

(asdf:load-system :fset)

;;; MAKE-LOCK with both spellings fset itself uses: a string from tuples.lisp
;;; and a SYMBOL from define-atomic-series. EGCL's MAKE-MUTEX takes its name as
;;; a keyword argument, so a port that forgot to coerce breaks on the symbol.
(assert (fset::make-lock "a string name"))
(assert (fset::make-lock 'a-symbol-name))
(assert (fset::make-lock))

;;; WITH-LOCK must evaluate the body and return its value.
(let ((lock (fset::make-lock "body")))
  (assert (eq :ran (fset::with-lock (lock) :ran))))

;;; :wait? nil on a FREE lock still runs the body. (The contended case -- where
;;; it must return WITHOUT running the body -- is what forced the EGCL port to
;;; use GRAB-MUTEX rather than WITH-MUTEX; it needs a second thread, so it is
;;; covered by the EGCL unit tests rather than here.)
(let ((lock (fset::make-lock "free")))
  (assert (eq :ran (fset::with-lock (lock :wait? nil) :ran))))

;;; The barriers must be callable. On EGCL they are a lock round trip, not a
;;; no-op, because fset uses them around lock-free reads of its transients.
(assert (null (fset::read-memory-barrier)))
(assert (null (fset::write-memory-barrier)))

;;; And the library actually works: a structure whose construction takes the
;;; locks the port supplies.
(let ((s (fset:with (fset:with (fset:empty-set) 1) 2)))
  (assert (= 2 (fset:size s)))
  (assert (= 2 (fset:size (fset:with s 1)))))   ; idempotent insert

(format t "FSET-EGCL-OK~%")
