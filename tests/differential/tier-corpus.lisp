;;; Tier-differential corpus (bliss-19tm).
;;;
;;; Run under TORCL_FORCE_TIER=interp|t0|t1|t2; every run must produce
;;; byte-identical output.  Covers the divergence-prone shapes: all NLX kinds
;;; (tagbody/go, catch/throw, unwind-protect, block/return-from), multiple-
;;; values state (multiple-value-bind/-list/-setq, nth-value, values-list,
;;; truncation, MV through NLX landing pads), closures with captured state,
;;; backward jumps/loops, recursion, speculation/deopt bait (type and overflow
;;; guard failures mid-stream), a discarded-but-must-still-signal form, and
;;; CLOS dispatch (call-next-method, :around ordering).
;;;
;;; Output protocol: one "TAG => VALUE" line per check; a process-independent
;;; djb2 CHECKSUM line over all lines; a final CORPUS-DONE sentinel so a
;;; truncated or crashed run can never pass the diff.

(defvar *hash* 5381)

(defun %note-line (line)
  (dotimes (i (length line))
    (setq *hash* (mod (+ (* *hash* 33) (char-code (char line i))) 4294967296)))
  (write-line line))

(defun note (tag val)
  (%note-line
   (concatenate 'string (prin1-to-string tag) " => " (prin1-to-string val))))

(defmacro check (tag form)
  `(note ,tag (handler-case ,form (error (e) (list :error (type-of e))))))

;;; ── loops / backward jumps ─────────────────────────────────────────────

(defun sum-to (n)
  (let ((s 0))
    (dotimes (i n s)
      (setq s (+ s i)))))

(defun fib-iter (n)
  (do ((a 0 b)
       (b 1 (+ a b))
       (i 0 (1+ i)))
      ((= i n) a)))

(defun collect-odds (n)
  (loop for i from 1 below n
        when (oddp i) collect i))

(defun nested-loop-sum (n m)
  (let ((s 0))
    (dotimes (i n s)
      (dotimes (j m)
        (setq s (+ s (* i j)))))))

(check :sum-to-0 (sum-to 0))
(check :sum-to-1000 (sum-to 1000))
(check :fib-iter-30 (fib-iter 30))
(check :fib-iter-90 (fib-iter 90)) ; bignum territory
(check :collect-odds (collect-odds 20))
(check :nested-loop (nested-loop-sum 20 30))

;;; ── recursion ──────────────────────────────────────────────────────────

(defun fib-rec (n)
  (if (< n 2) n (+ (fib-rec (- n 1)) (fib-rec (- n 2)))))

(defun my-even-p (n) (if (= n 0) t (my-odd-p (- n 1))))
(defun my-odd-p (n) (if (= n 0) nil (my-even-p (- n 1))))

(defun ack (m n)
  (cond ((= m 0) (+ n 1))
        ((= n 0) (ack (- m 1) 1))
        (t (ack (- m 1) (ack m (- n 1))))))

(check :fib-rec-15 (fib-rec 15))
(check :mutual-rec (list (my-even-p 100) (my-odd-p 77)))
(check :ackermann (ack 2 3))

;;; ── closures with captured state ───────────────────────────────────────

(defun make-counter ()
  (let ((n 0))
    (lambda () (setq n (1+ n)))))

(defun make-adder (k)
  (lambda (x) (+ x k)))

(let ((c1 (make-counter))
      (c2 (make-counter)))
  (funcall c1) (funcall c1) (funcall c2)
  (check :counters (list (funcall c1) (funcall c2))))

(check :adders (mapcar (lambda (f) (funcall f 10))
                       (list (make-adder 1) (make-adder 2) (make-adder 3))))

(defun closure-over-loop ()
  (let ((fns nil))
    (dotimes (i 3)
      (let ((j i))
        (push (lambda () j) fns)))
    (mapcar #'funcall (nreverse fns))))

(check :closure-loop (closure-over-loop))

;;; ── NLX: block / return-from ───────────────────────────────────────────

(defun find-first-big (list limit)
  (block scan
    (dolist (x list)
      (when (> x limit)
        (return-from scan x)))
    :none))

(check :return-from-hit (find-first-big '(1 5 20 3) 10))
(check :return-from-miss (find-first-big '(1 5 3) 10))

;;; ── NLX: catch / throw ─────────────────────────────────────────────────

(defun thrower (x)
  (when (> x 5) (throw 'out (* x 10)))
  x)

(defun catcher (x)
  (catch 'out
    (list :no-throw (thrower x))))

(check :catch-throw (catcher 9))
(check :catch-no-throw (catcher 3))
(check :catch-nested
       (catch 'a
         (catch 'b
           (throw 'a (catch 'c (+ 1 (throw 'c 41)))))))

;;; ── NLX: tagbody / go ──────────────────────────────────────────────────

(defun tagbody-loop (n)
  (let ((i 0) (acc nil))
    (tagbody
     top
       (when (>= i n) (go done))
       (push (* i i) acc)
       (setq i (1+ i))
       (go top)
     done)
    (nreverse acc)))

(check :tagbody-loop (tagbody-loop 6))
(check :tagbody-forward
       (let ((path nil))
         (tagbody
            (push :a path)
            (go skip)
            (push :never path)
          skip
            (push :b path))
         (nreverse path)))

;;; ── NLX: unwind-protect (cleanup order, exit through cleanup) ──────────

(defvar *cleanups* nil)

(defun with-cleanup (tag thunk)
  (unwind-protect (funcall thunk)
    (push tag *cleanups*)))

(setq *cleanups* nil)
(check :uwp-normal (with-cleanup :outer (lambda () (with-cleanup :inner (lambda () 42)))))
(check :uwp-order-normal (reverse *cleanups*))

(setq *cleanups* nil)
(check :uwp-throw
       (catch 'esc
         (with-cleanup :one
           (lambda ()
             (with-cleanup :two (lambda () (throw 'esc :escaped)))))))
(check :uwp-order-throw (reverse *cleanups*))

(setq *cleanups* nil)
(check :uwp-return-from
       (block b
         (with-cleanup :rf (lambda () (return-from b :early)))))
(check :uwp-order-rf (reverse *cleanups*))

;;; ── multiple values ────────────────────────────────────────────────────

(defun two-vals () (values 1 2))
(defun no-vals () (values))
(defun five-vals () (values 1 2 3 4 5))

(check :mv-bind (multiple-value-bind (a b) (two-vals) (list a b)))
(check :mv-bind-short (multiple-value-bind (a b c) (two-vals) (list a b c)))
(check :mv-list (multiple-value-list (five-vals)))
(check :mv-list-none (multiple-value-list (no-vals)))
(check :mv-truncate (let ((x (two-vals))) x))
(check :nth-value (list (nth-value 0 (five-vals)) (nth-value 3 (five-vals))))
(check :values-list (multiple-value-list (values-list '(:a :b :c))))
(check :mv-setq (let ((a nil) (b nil))
                  (multiple-value-setq (a b) (two-vals))
                  (list a b)))
(check :mv-call (multiple-value-call #'+ (two-vals) (five-vals)))
(check :mv-floor (multiple-value-list (floor 17 5)))
;; MV state through an NLX landing pad: the values must survive the throw.
(check :mv-through-catch
       (multiple-value-list (catch 'mv (throw 'mv (values 7 8 9)))))
(check :mv-prog1 (multiple-value-list (multiple-value-prog1 (two-vals) (five-vals))))

;;; ── discarded-but-must-still-signal ────────────────────────────────────

(check :discarded-car (progn (car 5) :ok))
(check :discarded-arith (progn (+ 1 "x") :ok))
(check :discarded-in-loop
       (let ((n 0))
         (dotimes (i 3)
           (handler-case (progn (car i) (setq n (+ n 100)))
             (type-error () (setq n (+ n 1)))))
         n))

;;; ── speculation / deopt bait ───────────────────────────────────────────
;;; Warm a function hot on fixnums, then hit it with operands that break the
;;; speculated guards (float, bignum, ratio, overflow).  Every tier must agree.

(defun add2 (a b) (+ a b))
(defun mul-square (x) (* x x))

(let ((s 0))
  (dotimes (i 200) (setq s (add2 s i)))
  (check :warm-fixnum-sum s))
(check :deopt-float (add2 1 2.5))
(check :deopt-bignum (add2 (ash 1 100) 1))
(check :deopt-ratio (add2 1/3 1/6))
(check :deopt-string (handler-case (add2 1 "x") (error (e) (type-of e))))

(let ((s 0))
  (dotimes (i 100) (setq s (+ s (mul-square i))))
  (check :warm-square-sum s))
(check :overflow-square (mul-square (+ (ash 1 30) 3)))
(check :overflow-loop
       (let ((s 1))
         (dotimes (i 70 s)
           (setq s (* s 2))))) ; crosses the fixnum boundary mid-loop

;;; ── CLOS dispatch ──────────────────────────────────────────────────────

(defclass shape () ((name :initarg :name :accessor shape-name)))
(defclass circle (shape) ((r :initarg :r :accessor circle-r)))
(defclass square-shape (shape) ((side :initarg :side :accessor square-side)))

(defgeneric area (s))
(defmethod area ((s circle)) (* 3 (circle-r s) (circle-r s)))
(defmethod area ((s square-shape)) (* (square-side s) (square-side s)))

(defgeneric describe-shape (s))
(defmethod describe-shape ((s shape)) (list :shape (shape-name s)))
(defmethod describe-shape ((s circle))
  (cons :circle (call-next-method)))
(defmethod describe-shape :around ((s circle))
  (list :around (call-next-method)))

(let ((c (make-instance 'circle :name :c1 :r 5))
      (q (make-instance 'square-shape :name :q1 :side 4)))
  (check :clos-areas (list (area c) (area q)))
  (check :clos-cnm (describe-shape c))
  (check :clos-plain (describe-shape q))
  (setf (circle-r c) 10)
  (check :clos-setf-slot (area c))
  ;; dispatch in a loop (inline-cache / dispatch-site shape)
  (check :clos-loop-dispatch
         (let ((acc 0))
           (dolist (s (list c q c q c) acc)
             (setq acc (+ acc (area s)))))))

(defstruct point (x 0) (y 0))
(let ((p (make-point :x 3 :y 4)))
  (check :struct-access (list (point-x p) (point-y p) (point-p p)))
  (setf (point-y p) 40)
  (check :struct-setf (point-y p))
  (check :struct-copy (let ((q (copy-point p))) (list (point-x q) (point-y q)))))

;;; ── mixed library surface in hot loops ─────────────────────────────────

(defun string-churn (n)
  (let ((acc ""))
    (dotimes (i n acc)
      (setq acc (concatenate 'string acc (string (code-char (+ 65 (mod i 26)))))))))

(check :string-churn (string-churn 30))

(defun hash-churn (n)
  (let ((h (make-hash-table)))
    (dotimes (i n)
      (setf (gethash (mod i 7) h) (+ i (gethash (mod i 7) h 0))))
    (let ((acc nil))
      (dotimes (k 7 (nreverse acc))
        (push (gethash k h :missing) acc)))))

(check :hash-churn (hash-churn 50))

(check :logops-hot
       (let ((acc 0))
         (dotimes (i 64 acc)
           (setq acc (logxor (ash acc 1) (logand i 21) (logior i 5))))))

;;; ── epilogue: checksum + completion sentinel ───────────────────────────

(note :checksum *hash*)
(write-line "CORPUS-DONE")
