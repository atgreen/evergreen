;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(dolist (pattern '("" (1)))
  (dolist (target (list 42 #\a (make-array '(2 2))))
    (dolist (reversep '(nil t))
      (assert (handler-case
                  (progn (search pattern target :end2 0 :from-end reversep) nil)
                (type-error () t))))))
(assert (= (search "" "" :end2 0) 0))
(assert (null (search '(1) nil :end2 0)))
(format t "SEARCH-INVALID-TARGET-PASS~%")
