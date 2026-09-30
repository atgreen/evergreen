;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(let* ((text (format nil "~A-~D" 'egcl 3))
       (numbers '(1 3 5))
       (table (make-hash-table :test 'equal))
       (seq (concatenate 'list '(1 2) '(3 4 5))))
  (setf (gethash "TEXT" table) text)
  (setf (gethash "SLICE" table) (subseq seq 1 4))
  (print (list (gethash "TEXT" table)
               (gethash "SLICE" table)
               seq
               (format nil "~{~A~^, ~}" numbers))))
