;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defun foo (x) (* x 5))

(disassemble 'foo)

(print (loop for i from 0 upto 10000 sum (foo i)))

(disassemble 'foo)

(print (loop for i from 0.0 upto 10000.0 by 0.5 sum (foo i)))

(disassemble 'foo)

