;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(let* ((text (make-string 100000 :initial-element #\a))
       (items (make-list 100000 :initial-element 1)))
  (assert (= 0 (search "a" text)))
  (format t "SEARCH-EARLY-MATCH-PASS~%") (finish-output)
  (assert (null (search "z" text)))
  (assert (= 99999 (search "a" text :from-end t)))
  (assert (= 99000 (search "aaa" text :start2 99000)))
  (assert (= 99997 (search "aaa" text :start2 99000 :from-end t)))
  (assert (null (search '(2) items)))
  (assert (= 99999 (search '(1) items :from-end t)))
  (assert (= 99000 (search '(1 1) items :start2 99000)))
  (format t "LARGE-SEARCH-PASS~%") (finish-output))
