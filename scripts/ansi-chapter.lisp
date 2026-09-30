;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;; Generic ansi-test chapter harness (bliss-30be).
;;
;; Loads the Dietz ansi-test framework (rt + aux + universe) and ONE chapter,
;; then runs rt:do-tests and prints the stable two-line tally the gate script
;; parses:
;;
;;   passed: N
;;   failed: N
;;
;; The chapter and the ansi-test checkout are injected by scripts/ansi-gate.sh
;; via --eval BEFORE this file loads:
;;   (defparameter cl-user::*ansi-chapter* "cons")
;;   (defparameter cl-user::*ansi-test-root* "/path/to/ansi-test/")
;;
;; Load order per the cl-amiga crib sheet (bliss-30be): compile-and-load ->
;; rt-package -> rt -> cl-test-package -> (in-package :cl-test) as its OWN
;; top-level form -> ansi-aux-macros -> universe -> ansi-aux -> random-aux ->
;; cl-symbol-names -> <chapter>/load.lsp. Paths are absolute (compile-and-load
;; merges against *load-pathname*). Stale *.fasl files must be cleared by the
;; caller (compile-and-load keys on mtime; bliss-89lj).

(defparameter *ansi-test-dir* (truename *ansi-test-root*))
(defparameter *aux-dir* (truename (merge-pathnames "auxiliary/" *ansi-test-dir*)))
(setq *default-pathname-defaults* *ansi-test-dir*)

(format t "~%=== ANSI chapter ~a (egcl gate) ===~%" *ansi-chapter*)

(load (merge-pathnames "compile-and-load.lsp" *ansi-test-dir*))
(load (merge-pathnames "rt-package.lsp" *ansi-test-dir*))
(compile-and-load (merge-pathnames "rt.lsp" *ansi-test-dir*))
(load (merge-pathnames "cl-test-package.lsp" *ansi-test-dir*))

(in-package :cl-test)

(common-lisp-user::compile-and-load
 (common-lisp:merge-pathnames "ansi-aux-macros.lsp"
                              (common-lisp:symbol-value
                               'common-lisp-user::*aux-dir*)))

(common-lisp:load
 (common-lisp:merge-pathnames "universe.lsp"
                              (common-lisp:symbol-value
                               'common-lisp-user::*ansi-test-dir*)))

(common-lisp-user::compile-and-load
 (common-lisp:merge-pathnames "ansi-aux.lsp"
                              (common-lisp:symbol-value
                               'common-lisp-user::*aux-dir*)))

(common-lisp-user::compile-and-load
 (common-lisp:merge-pathnames "random-aux.lsp"
                              (common-lisp:symbol-value
                               'common-lisp-user::*aux-dir*)))

(common-lisp:load
 (common-lisp:merge-pathnames "cl-symbol-names.lsp"
                              (common-lisp:symbol-value
                               'common-lisp-user::*ansi-test-dir*)))

(common-lisp:load
 (common-lisp:merge-pathnames
  (common-lisp:concatenate 'common-lisp:string
                           (common-lisp:symbol-value
                            'common-lisp-user::*ansi-chapter*)
                           "/load.lsp")
  (common-lisp:symbol-value 'common-lisp-user::*ansi-test-dir*)))

(do-tests)
(format t "~%passed: ~A~%" (length regression-test::*passed-tests*))
(format t "failed: ~A~%" (length regression-test::*failed-tests*))
(when regression-test::*failed-tests*
  (format t "~%--- Failed tests ---~%")
  (dolist (n regression-test::*failed-tests*)
    (format t "  ~A~%" n)))
