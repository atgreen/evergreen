(defun foo (x) x)

(disassemble 'foo)

(print (loop for i from 0 upto 10000 sum (foo i)))

(disassemble 'foo)

(print (loop for i from 0.0 upto 10000.0 by 0.5 sum (foo i)))

(disassemble 'foo)

