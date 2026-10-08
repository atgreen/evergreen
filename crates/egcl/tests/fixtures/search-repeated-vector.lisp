;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(let ((text (make-string 200000 :initial-element #\a)))
  (dotimes (iteration 200)
    (assert (= 0 (search "aaa" text)))
    (assert (= 199990 (search "aaa" text :start2 199990))))
  (format t "REPEATED-VECTOR-SEARCH-PASS~%") (finish-output))
