(defun fib (n)
  (if (eq n 0)
      0
      (if (eq n 1)
          1
          (+ (fib (- n 1)) (fib (- n 2))))))
(defun make-counter (start)
  (let ((n start))
    (lambda (&optional (delta 1))
      (setq n (+ n delta))
      n)))
(print (fib 30))
(print (mapcar (lambda (x) (* x x)) '(1 2 3 4)))
(let ((counter (make-counter 7)))
  (print (list (funcall counter) (funcall counter 5))))
(print (catch 'done
         (progn
           (throw 'done '(escaped ok))
           nil)))
