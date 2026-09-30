;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Gabriel CTAK — TAK via catch/throw; stresses non-local exit (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun ctak (x y z)
  (catch 'ctak (ctak-aux x y z)))

(defun ctak-aux (x y z)
  (if (not (< y x))
      (throw 'ctak z)
      (ctak-aux (catch 'ctak (ctak-aux (1- x) y z))
                (catch 'ctak (ctak-aux (1- y) z x))
                (catch 'ctak (ctak-aux (1- z) x y)))))

(run-benchmark "ctak" (lambda () (ctak 18 12 6)))
