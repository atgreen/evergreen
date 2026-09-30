;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(print 123)
(print 3/4)
(print 1.5)
(print #\A)
(print "hello")
(print 'foo)
(print :bar)
(print '(1 (2 3) nil))
(print t)
(print nil)
(print (list (+ 1 (* 2 3))
             (car (cdr (cons 9 (list 8 7))))
             (cdr (list 4 5 6))))
