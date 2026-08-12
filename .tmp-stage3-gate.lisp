(defvar *h* (make-hash-table :test (quote equal)))
(setf (gethash "ab" *h*) (subseq "alphabet" 0 5))
(setf (gethash :count *h*) (length (reverse (list 1 2 3 4))))
(print (format nil "~A|~A|~A|~A" (concatenate (quote string) (gethash "ab" *h*) "-SOUP") (gethash :count *h*) (hash-table-count *h*) (subseq (concatenate (quote string) (gethash "ab" *h*) "bet") 0 5)))
