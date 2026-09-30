;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; SUMLOOP — a single hot dotimes loop called once per run.  Not a Gabriel
;;; benchmark: it exists to expose the on-stack-replacement gap (bliss-izt) —
;;; a loop that is only ever entered once never benefits from invocation-count
;;; tiering, so this number tracks OSR progress directly (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun sumto (n)
  (let ((s 0))
    (dotimes (i n s)
      (setq s (+ s i)))))

(run-benchmark "sumloop" (lambda () (sumto 3000000)))
