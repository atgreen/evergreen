;;;; Heap lexical environments (Backpack, bliss-gg3i1.7.15.4): every binding
;;;; shape the rewrite lays out. Each line prints KEY=VALUE; the test compares
;;;; the lines across tiers and against expected values.

(defun hv-report (key value)
  (format t "~A=~S~%" key value))

;; Flat capture of a parameter (captured, never assigned).
(defun hv-adder (n) (lambda (x) (+ x n)))

;; Shared cell: captured and assigned; two closures over one binding.
(defun hv-pair ()
  (let ((n 0))
    (list (lambda (k) (setq n (+ n k)) n)
          (lambda () n))))

;; A captured, assigned parameter (base environment).
(defun hv-counter (start)
  (lambda () (incf start)))

;; LET* where a later init closes over an earlier binding, and a shadowing
;; duplicate name in a nested LET.
(defun hv-let* (a)
  (let* ((b (* a 2))
         (f (lambda () (+ a b))))
    (let ((b 100))
      (list (funcall f) b))))

;; LET: inits see the outer binding, not the one being made.
(defun hv-let-parallel (x)
  (let ((x (lambda () x))
        (y x))
    (list (funcall x) y)))

;; FLET: a definition sees the outer function, not its sibling.
(defun hv-flet (x)
  (flet ((g () (* x 10)))
    (flet ((g () (+ (g) 1))
           (h () (g)))
      (list (g) (h)))))

;; LABELS: mutual recursion over a captured variable.
(defun hv-labels (limit)
  (labels ((ev (n) (if (= n 0) t (od (1- n))))
           (od (n) (if (= n 0) nil (ev (1- n))))
           (count-to (n acc) (if (> n limit) acc (count-to (1+ n) (+ acc n)))))
    (list (ev limit) (od limit) (count-to 0 0) #'ev)))

;; &optional / &key defaults referencing earlier parameters, captured.
(defun hv-optional (a &optional (b (* a 2)) (c (lambda () (+ a b))))
  (list a b (funcall c)))
(defun hv-key (a &key (b (+ a 1)) (f (lambda () (list a b))))
  (funcall f))

;; Closures created in loops: DOTIMES, DOLIST and LOOP rebinding. The
;; per-iteration LET is a fresh binding; the loop variable itself is assigned.
(defun hv-loops ()
  (let ((fresh '()) (shared '()) (looped '()))
    (dotimes (i 3)
      (let ((j i)) (push (lambda () j) fresh))
      (push (lambda () i) shared))
    (dolist (e '(a b c))
      (let ((e2 e)) (push (lambda () e2) looped)))
    (list (mapcar #'funcall fresh)
          (length (remove-duplicates (mapcar #'funcall shared)))
          (mapcar #'funcall looped)
          (loop for k from 1 to 3 collect (let ((k2 k)) (lambda () (* k2 k2))) into fs
                finally (return (mapcar #'funcall fs))))))

;; Closures that mutate shared state created in a loop.
(defun hv-accumulators ()
  (let ((total 0) (fns '()))
    (dotimes (i 4)
      (push (lambda (k) (incf total (* k i))) fns))
    (dolist (f fns) (funcall f 10))
    total))

;; Special declarations are never captured lexically.
(defvar *hv-special* :global)
(defun hv-special ()
  (let ((*hv-special* :bound))
    (let ((f (lambda () *hv-special*)))
      (list (funcall f)
            (let ((x 1))
              (declare (special x))
              (funcall (lambda () x)))))))

;; Symbol macros are expanded away.
(defun hv-symbol-macro (v)
  (let ((cell (list v)))
    (symbol-macrolet ((it (car cell)))
      (let ((get (lambda () it))
            (put (lambda (x) (setf it x))))
        (funcall put (* 3 (funcall get)))
        (funcall get)))))

;; Closures over closures, three levels, mixing flat and shared captures.
(defun hv-nested (a)
  (let ((b (* a 10)) (c 0))
    (lambda (d)
      (let ((e (+ d 1)))
        (lambda (f)
          (incf c)
          (lambda () (list a b c d e f)))))))

;; Assignment after capture, in the closure and in the creator.
(defun hv-late-assign (x)
  (let ((f (lambda () x)))
    (setq x (* x 2))
    (funcall f)))

;; A closure that escapes from a LET in a loop and keeps per-iteration state.
(defun hv-counters (n)
  (loop for i below n
        collect (let ((c i)) (lambda () (incf c 100)))))

(defun hv-run ()
  (hv-report "adder" (funcall (hv-adder 5) 10))
  (let* ((p (hv-pair)) (bump (first p)) (peek (second p)))
    (funcall bump 3) (funcall bump 4)
    (hv-report "pair" (list (funcall peek) (funcall bump 1))))
  (let ((c (hv-counter 10)))
    (funcall c)
    (hv-report "counter" (funcall c)))
  (hv-report "let*" (hv-let* 3))
  (hv-report "let" (hv-let-parallel 7))
  (hv-report "flet" (hv-flet 2))
  (let ((r (hv-labels 7)))
    (hv-report "labels" (list (first r) (second r) (third r) (funcall (fourth r) 4))))
  (hv-report "optional" (list (hv-optional 1) (hv-optional 1 5) (hv-optional 1 5 (lambda () :c))))
  (hv-report "key" (list (hv-key 1) (hv-key 1 :b 9)))
  (hv-report "loops" (hv-loops))
  (hv-report "accumulators" (hv-accumulators))
  (hv-report "special" (hv-special))
  (hv-report "symbol-macro" (hv-symbol-macro 4))
  (let* ((g (hv-nested 2)) (h (funcall g 5)) (k (funcall h 7)))
    (funcall h 8)
    (hv-report "nested" (funcall k)))
  (hv-report "late-assign" (hv-late-assign 21))
  (let ((cs (hv-counters 3)))
    (mapc #'funcall cs)
    (hv-report "counters" (mapcar #'funcall cs))))

;; Run enough times for the invocation-threshold tiers to promote.
(defvar *hv-warm* 30)
(dotimes (i *hv-warm*) (let ((*standard-output* (make-broadcast-stream))) (hv-run)))
(hv-run)
