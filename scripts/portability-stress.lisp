;; Runs without the prelude, with every allocation stressed, then compared
;; byte-for-byte with a non-stress execution of this same file.
(defun portable-check (condition)
  (if condition t (error "portability stress check failed")))
(defun portable-list (n)
  (if (= n 0) nil
      (cons (format nil "item-~D" n) (portable-list (- n 1)))))
(defun portable-last (items)
  (if (cdr items) (portable-last (cdr items)) (car items)))
(let ((items (portable-list 20)))
  (portable-check (= (length items) 20))
  (portable-check (string= (car items) "item-20"))
  (portable-check (string= (portable-last items) "item-1")))
(let ((numbers (list (expt 2 90) 1.25d0 2.5 1/3)))
  (portable-check (= (car numbers) 1237940039285380274899124224))
  (portable-check (= (+ (car (cdr numbers)) 2.5d0) 3.75d0))
  (portable-check (= (portable-last numbers) 1/3)))
(format t "STRESS-OK~%")
