;;; Gabriel TAK — deep non-tail recursion, fixnum compares (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun tak (x y z)
  (if (not (< y x))
      z
      (tak (tak (1- x) y z)
           (tak (1- y) z x)
           (tak (1- z) x y))))

(run-benchmark "tak" (lambda () (tak 24 16 8)))
