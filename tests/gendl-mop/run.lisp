;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Run from an isolated project containing this scenario's ocicl.csv.
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations
       (list t (or (uiop:getenv "EGCL_GENDL_CACHE")
                   (error "Set EGCL_GENDL_CACHE to a private cache directory.")))
       :ignore-inherited-configuration))
(load (or (uiop:getenv "EGCL_GENDL_RUNTIME")
          (error "Set EGCL_GENDL_RUNTIME to ocicl-runtime.lisp.")))
(setf ocicl-runtime:*download* nil
      ocicl-runtime:*local-only* t)

;; BASE and GENDL-ASDF are the only Gendl dependencies of this system.
;; Do not load the full GENDL system and its geometry/web dependencies.
(asdf:load-system :gendl-mop)
(dolist (entry '((:base . "base/") (:gendl-asdf . "gendl-asdf/")
                 (:gendl-mop . "gendl-mop/")))
  (assert
   (equal (truename (asdf:system-source-directory (car entry)))
          (truename (merge-pathnames
                     (concatenate 'string "ocicl/gendl-5675eb3/" (cdr entry))
                     (uiop:getcwd))))))

(let ((gdl:*kernel* :mop))
  (load (asdf:system-relative-pathname :gendl-mop "tests/smoke.lisp")))
(assert (= gendl-mop-test::*checks* 166))
(assert (zerop gendl-mop-test::*failures*))
(format t "GENDL-MOP-SMOKE-166-OK~%")
