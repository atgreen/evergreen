;;; Naive Fibonacci — call-heavy fixnum recursion (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun fib (n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))

(run-benchmark "fib" (lambda () (fib 30)))
