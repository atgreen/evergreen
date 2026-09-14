(defun loop-probe (expressions)
  (loop for expr in expressions
        for length = (length expr)
        for type = (if (< 2 length) (first expr) 'function)
        collect (list type length)))
