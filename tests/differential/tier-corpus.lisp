;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Tier-differential corpus (bliss-19tm).
;;;
;;; Run under EGCL_FORCE_TIER=interp|t0|t1|t2; every run must produce
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
;; MULTIPLE-VALUE-LIST returns exactly one value: the collected tuple must not
;; leak to the caller as stale secondaries (bliss-6z1hh).
(defun mvl-wrap () (multiple-value-list (five-vals)))
(check :mv-list-no-leak (multiple-value-bind (a b) (mvl-wrap) (list a b)))
;; A backquote returns exactly one value: an operand's tuple must not leak
;; through the template's conses (bliss-elwvl).
(defun qq-mv-wrap (a) `(,(values a 2)))
(check :backquote-no-leak (multiple-value-list (qq-mv-wrap 1)))
(check :backquote-shapes (list (qq-mv-wrap 3) (let ((l (list 1 2))) `(x ,@l . ,l))))

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

;;; LOOP steps through an (if first i (+ i 1)) merge (bliss-fva34); g-merge is
;;; the same shape by hand.  Warm, then break the fixnum bound and overflow.
(defun loop-collect-squares (n) (loop for i below n collect (* i i)))
(defun loop-from-above (lo)
  (loop for i from lo below (+ lo 5) collect (- i most-positive-fixnum)))
(defun g-merge (n)
  (let ((i 0) (s 0) (first t))
    (tagbody
     top
       (setq i (if first i (+ i 1)))
       (when (>= i n) (go end))
       (setq s (+ s 1))
       (setq first nil)
       (go top)
     end)
    s))

(dotimes (i 40)
  (loop-collect-squares 6) (g-merge 9) (loop-from-above (- most-positive-fixnum 30)))
(check :loop-collect-squares (loop-collect-squares 6))
(check :loop-collect-float (loop-collect-squares 2.5))
(check :g-merge (g-merge 9))
(check :g-merge-float (g-merge 4.5))
(check :loop-overflow (loop-from-above (- most-positive-fixnum 2)))

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

;;; ── constants wider than imm32 (bliss-mghob) ───────────────────────────

(defun wide-mul (a) (* a 1000000000000))
(defun wide-mul-left (a) (* -1000000000000 a))
(defun wide-ash (a) (ash a 40))
(defun wide-cmp (a) (if (< a 1000000000000) :small :big))
(defun wide-cmp-value (a) (list (< 1000000000000 a) (= a 1000000000000)))
(defun masked-ash (a) (ash (logand a #xFFFFFFFFFF) 30))

(check :wide-arith
       (let ((acc nil))
         (dotimes (i 40 (nreverse acc))
           (push (list (wide-mul i) (wide-mul-left (- i)) (wide-ash i)
                       (wide-cmp (* i 100000000000)) (wide-cmp-value (* i 50000000000))
                       (masked-ash (+ i #xFFFFFFFFF0)))
                 acc))))
(check :wide-arith-overflow
       (list (wide-mul (expt 2 50)) (wide-mul-left (expt 2 50)) (wide-ash (expt 2 25))
             (wide-cmp 3) (wide-cmp 2000000000000) (wide-cmp-value 1000000000000)
             (masked-ash #xFFFFFFFFFF) (masked-ash 3)))

;;; ── double floats behind cold fixnum guesses (bliss-fenm) ──────────────

(defun s-double (a b)
  (declare (double-float a b))
  (+ (* a b) 1d0))

(check :s-double
       (let ((r 0d0))
         (dotimes (i 200 r)
           (setq r (s-double 1.5d0 (float i 1d0))))))

(defun br-cold-double (flag x)
  (if flag (+ x 1) (+ x 1d0)))

(check :br-cold-double
       (let ((acc nil))
         (dotimes (i 50) (push (br-cold-double t i) acc))
         (dotimes (i 50) (push (br-cold-double nil (float i 1d0)) acc))
         (list (reduce #'+ acc) (first acc) (car (last acc)))))

(defun double-loop (n)
  (let ((s 0d0))
    (dotimes (i n s)
      (incf s (* 1.5d0 i)))))

(check :double-loop-1000 (double-loop 1000))
(check :double-loop-100000 (double-loop 100000))

;;; ── &optional entry: omitted arguments padded at native entry ─────────

(defun opt-sp (a &optional (b (* a 10) b-p) c)
  (list a b b-p c))

(defun opt-heap (a &optional (b (list a a)))
  (list* a b))

(defun opt-overflow (a &optional (b most-positive-fixnum))
  (+ a b))

(defun opt-sum (n &optional (acc 0))
  (if (zerop n) acc (opt-sum (1- n) (+ acc n))))

(defun opt-calls (i)
  (list (opt-sp i) (opt-sp i nil) (opt-sp i 2 3) (opt-heap i) (opt-heap i :x)
        (opt-overflow i) (opt-sum i) (opt-sum i 7)))

(check :optional-hot
       (let ((acc nil))
         (dotimes (i 40 acc)
           (setq acc (opt-calls i)))))
(check :optional-overflow (list (opt-overflow 1) (opt-overflow -1 -2)))
(check :optional-too-many (opt-sp 1 2 3 4))

;;; ── &rest / &key / &aux entry: lambda list bound at native entry ──────

(defun key-sp (a &key (b (list a a) b-p) (c 3)) (list a b b-p c))
(defun key-rest (x &rest r) (list* x (length r) r))
(defun key-aux (a &aux (b (* a 2)) (c (cons a b))) (list a b c))
(defun key-allow (&key a &allow-other-keys) a)
(defun key-overflow (a &key (b most-positive-fixnum)) (+ a b))
(defun key-count (n &key acc)
  (if (zerop n) acc (key-count (1- n) :acc (cons n acc))))
(defun key-throw (a &key (b (throw 'key-exit (list :thrown a)))) (list a b))

(defun key-calls (i)
  (list (key-sp i) (key-sp i :b nil) (key-sp i :c 4 :b 5 :b 6) (key-rest i)
        (key-rest i 1 2 3) (key-aux i) (key-allow :z 1 :a i)
        (key-overflow i) (key-count 4 :acc (list i))
        (catch 'key-exit (key-throw i)) (key-throw i :b 0)))

(check :key-hot
       (let ((acc nil))
         (dotimes (i 40 acc)
           (setq acc (key-calls i)))))
(check :key-overflow (list (key-overflow 1) (key-overflow -1 :b -2)))
(check :key-odd (key-sp 1 :b))
(check :key-unknown (key-sp 1 :z 2))
(check :key-too-few (key-rest))

;;; ── #'f function designators (bliss-j3fv8) ────────────────────────────

(defun fd-apply (l) (apply #'+ l))
(defun fd-mapcar (l) (mapcar #'1+ l))
(defun fd-reduce (l) (reduce #'+ l))
(defstruct fd-pt x y)
(defun fd-struct-set (p) (setf (fd-pt-x p) (1+ (fd-pt-x p))))
(defun fd-target (x) (list :old x))
(defun fd-caller (x) (funcall #'fd-target (list x (make-list 3))))

(check :fd-apply (fd-apply '(1 2 3)))
(check :fd-mapcar (fd-mapcar '(1 2)))
(check :fd-reduce (fd-reduce '(1 2 3)))
(check :fd-struct-set
       (let ((p (make-fd-pt :x 0 :y 0)))
         (dotimes (i 30) (fd-struct-set p))
         (list (fd-struct-set p) (fd-pt-x p))))
(check :fd-redefined
       (let ((before (fd-caller 1)))
         (setf (symbol-function 'fd-target) (lambda (x) (list :new x)))
         (list before (fd-caller 2))))

;;; ── non-capturing closures created by compiled code (bliss-ku9w) ─────

(defun nocap-lambda () (let ((f (lambda (x) (+ x 1)))) (funcall f 1)))
(defun mapcar-lambda (l) (mapcar (lambda (x) (* x 2)) l))
(defun make-tagger () (lambda (x) (list x :tagged)))

(check :nocap-lambda (let ((acc 0)) (dotimes (i 30 acc) (incf acc (nocap-lambda)))))
(check :mapcar-lambda (let ((acc nil)) (dotimes (i 30 acc) (setq acc (mapcar-lambda (list i 2 3))))))
(check :fresh-closures
       (let ((a (make-tagger)) (b (make-tagger)))
         (list (eq a b) (funcall a 1) (funcall b 2))))

;; A hot nested lambda creating a closure that exits to a block/tag the
;; enclosing function established (bliss-ku9w review).
(defun nlx-call (f) (funcall f))
(defun nlx-return (l)
  (block b
    (mapcar (lambda (x) (if (eql x 3) (nlx-call (lambda () (return-from b :escaped))) x)) l)))
(defun nlx-go (l)
  (let ((r :none))
    (tagbody
       (mapcar (lambda (x) (if (eql x 3) (nlx-call (lambda () (go out))) x)) l)
       (setq r :fell-through)
       (go done)
     out
       (setq r :went)
     done)
    r))

(check :closure-inherited-exits
       (let ((l (let ((v nil)) (dotimes (i 200 (nreverse v)) (push (if (= i 150) 3 0) v))))
             (r nil))
         (dotimes (i 30 r) (setq r (list (nlx-return l) (nlx-go l))))))

;; Closures whose named exits stay inside the closure that creates them, so
;; their creator can run natively (bliss-uebgs), beside a same-named block
;; that does not enclose the creation and so must not excuse a free exit.
(defun ie-return (l)
  (mapcar (lambda (x) (block inner (nlx-call (lambda () (return-from inner (* x 10)))) :wrong)) l))
(defun ie-go (l)
  (mapcar (lambda (x)
            (let ((r :none))
              (tagbody
                 (nlx-call (lambda () (go out)))
                 (setq r :fell-through)
                 (go done)
               out
                 (setq r (list :went x))
               done)
              r))
          l))
(defun ie-dolist (l)
  (mapcar (lambda (x) (dolist (y x :none) (when (eql y 3) (nlx-call (lambda () (return y)))))) l))
(defun ie-deep (l)
  (mapcar (lambda (x)
            (block b (nlx-call (lambda () (nlx-call (lambda () (return-from b (list :deep x))))))))
          l))
(defun fe-shadow (l)
  (block b
    (mapcar (lambda (x)
              (block b nil)
              (if (eql x 3) (nlx-call (lambda () (return-from b :outer))) x))
            l)))

(check :closure-internal-exits
       (let ((r nil))
         (dotimes (i 30 r)
           (setq r (list (ie-return (list 1 i)) (ie-go (list 1 i)) (ie-dolist (list (list 1 3) (list i)))
                         (ie-deep (list 1 i)) (fe-shadow (list 1 2)) (fe-shadow (list 1 3 2)))))))

;;; ── epilogue: checksum + completion sentinel ───────────────────────────

(note :checksum *hash*)
(write-line "CORPUS-DONE")
