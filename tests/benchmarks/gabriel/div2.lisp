;;; Gabriel DIV2 (iterative + recursive) — cdr-chasing over a 200-element list
;;; (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun create-n (n)
  (do ((n n (1- n))
       (a () (push () a)))
      ((= n 0) a)))

(defun iterative-div2 (l)
  (do ((l l (cddr l))
       (a () (push (car l) a)))
      ((null l) a)))

(defun recursive-div2 (l)
  (cond ((null l) ())
        (t (cons (car l) (recursive-div2 (cddr l))))))

(let ((test-list (create-n 200)))
  (defun div2-run (l)
    (dotimes (i 300)
      (iterative-div2 l)
      (iterative-div2 l)
      (iterative-div2 l)
      (iterative-div2 l)
      (recursive-div2 l)
      (recursive-div2 l)
      (recursive-div2 l)
      (recursive-div2 l)))
  (run-benchmark "div2" (lambda () (div2-run test-list))))
