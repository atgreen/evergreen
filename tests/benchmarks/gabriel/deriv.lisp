;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Gabriel DERIV — symbolic differentiation; list building + dispatch (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun deriv-aux (a)
  (list '/ (deriv a) a))

(defun deriv (a)
  (cond ((atom a)
         (if (eq a 'x) 1 0))
        ((eq (car a) '+)
         (cons '+ (mapcar #'deriv (cdr a))))
        ((eq (car a) '-)
         (cons '- (mapcar #'deriv (cdr a))))
        ((eq (car a) '*)
         (list '* a (cons '+ (mapcar #'deriv-aux (cdr a)))))
        ((eq (car a) '/)
         (list '- (list '/ (deriv (cadr a)) (caddr a))
               (list '/ (cadr a)
                     (list '* (caddr a) (caddr a) (deriv (caddr a))))))
        (t 'error)))

(defun deriv-run ()
  (dotimes (i 1000)
    (deriv '(+ (* 3 x x) (* a x x) (* b x) 5))
    (deriv '(+ (* 3 x x) (* a x x) (* b x) 5))
    (deriv '(+ (* 3 x x) (* a x x) (* b x) 5))
    (deriv '(+ (* 3 x x) (* a x x) (* b x) 5))
    (deriv '(+ (* 3 x x) (* a x x) (* b x) 5))))

(run-benchmark "deriv" (lambda () (deriv-run)))
