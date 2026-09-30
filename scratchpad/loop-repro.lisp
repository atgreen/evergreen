;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defun loop-probe (expressions)
  (loop for expr in expressions
        for length = (length expr)
        for type = (if (< 2 length) (first expr) 'function)
        collect (list type length)))
