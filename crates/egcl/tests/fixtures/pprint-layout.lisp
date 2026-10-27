(in-package :cl-user)

(defvar *layout-failures* 0)

(defclass %pretty-stream (egcl-gray-streams:fundamental-character-output-stream)
  ((characters :initform nil :accessor fake-pretty-characters)))

(defmethod egcl-gray-streams:stream-write-char ((stream %pretty-stream) character)
  (push character (fake-pretty-characters stream))
  character)

(defmethod egcl-gray-streams:stream-line-column ((stream %pretty-stream)) 0)

(defclass error-column-stream (egcl-gray-streams:fundamental-character-output-stream) ())

(defmethod egcl-gray-streams:stream-write-char ((stream error-column-stream) character)
  character)

(defmethod egcl-gray-streams:stream-line-column ((stream error-column-stream))
  (error "column unavailable"))

(defclass semantic-layout-object () ())

(defun semantic-layout-printer (stream object)
  (declare (ignore object))
  (pprint-logical-block (stream nil :prefix "[" :suffix "]")
    (write-string "aa " stream)
    (pprint-newline :fill stream)
    (write-string "bb" stream)))

(defclass write-modifier-object () ())

(defmethod print-object ((object write-modifier-object) stream)
  (declare (ignore object))
  (write-char (if *print-pretty* #\P #\p) stream)
  (write-char (if *print-level* #\L #\l) stream)
  (write-char (if *print-length* #\N #\n) stream))

(defun slash-semantic-layout (stream object colon-p at-sign-p &rest parameters)
  (declare (ignore object colon-p at-sign-p parameters))
  (semantic-layout-printer stream nil))

(defun slash-current-column (stream object colon-p at-sign-p &rest parameters)
  (declare (ignore object colon-p at-sign-p parameters))
  (format stream "~D" (egcl-gray-streams:stream-line-column stream)))

(defun check-layout (name expected function)
  (let ((actual (funcall function)))
    (format t "~A: ~:[FAIL~;PASS~] expected ~S actual ~S~%" name (equal expected actual) expected actual)
    (unless (equal expected actual) (incf *layout-failures*))))

(defun multiline-within-p (text margin)
  (let ((column 0) (multiline nil) (within t))
    (loop for character across text do
      (if (char= character #\Newline)
          (setf column 0 multiline t)
          (progn
            (incf column)
            (when (> column margin) (setf within nil)))))
    (and multiline within)))

(let ((*print-pretty* t) (*print-miser-width* nil))
  (check-layout :linear (format nil "(111~% 222~% 333)")
    (lambda () (let ((*print-right-margin* 12))
      (with-output-to-string (s) (pprint-linear s '(111 222 333))))))
  (check-layout :line-limit "(111 ..)"
    (lambda () (let ((*print-right-margin* 6) (*print-lines* 1))
      (with-output-to-string (s) (pprint-linear s '(111 222 333))))))
  (check-layout :newline-suffix-limit (format nil "(a ..~%)")
    (lambda () (let ((*print-lines* 1))
      (with-output-to-string (s)
        (pprint-logical-block (s nil :prefix "(" :suffix (format nil "~%)"))
          (write-string "a" s) (pprint-newline :mandatory s) (write-string "b" s))))))
  (check-layout :suffix-written-once 1
    (lambda () (let ((*print-lines* 1))
      (count #\Y (with-output-to-string (s)
        (pprint-logical-block (s nil :suffix (format nil "Y~%Z"))
          (write-string "a" s)))))))
  (check-layout :miser (format nil "[abcd~% efghijk]")
    (lambda () (let ((*print-right-margin* 10) (*print-miser-width* 9))
      (with-output-to-string (s)
        (pprint-logical-block (s nil :prefix "[" :suffix "]")
          (write-string "abcd " s) (pprint-newline :miser s)
          (write-string "efghijk" s))))))
  (check-layout :fractional-indent (format nil "a~% b")
    (lambda () (with-output-to-string (s)
      (pprint-logical-block (s nil)
        (write-string "a" s) (pprint-indent :block 1.6 s)
        (pprint-newline :mandatory s) (write-string "b" s)))))
  (check-layout :prefix-newline (format nil "x~%ab cd")
    (lambda () (let ((*print-right-margin* 5))
      (with-output-to-string (s)
        (pprint-logical-block (s nil :prefix (format nil "x~%"))
          (write-string "ab " s) (pprint-newline :fill s) (write-string "cd" s))))))
  (check-layout :nested-prefix-newline (format nil "a~%x~%b")
    (lambda () (let ((*print-right-margin* 80))
      (with-output-to-string (s)
        (pprint-logical-block (s nil)
          (write-string "a" s) (pprint-newline :linear s)
          (pprint-logical-block (s nil :prefix (format nil "x~%"))
            (write-string "b" s)))))))
  (check-layout :multiline-section (format nil "a~%b~%c")
    (lambda () (with-output-to-string (s)
      (pprint-logical-block (s nil)
        (write-string "a" s) (terpri s) (write-string "b " s)
        (pprint-newline :fill s) (write-string "c" s)))))
  ;; Exercise the same layout decisions through both public interfaces.  The
  ;; FORMAT cases must enqueue into the surrounding logical block rather than
  ;; render into a temporary one-line string.
  (let ((fill-output (format nil "A A A~%A A A~%A A A~%A ")))
    (check-layout :direct-fill fill-output
      (lambda () (let ((*print-right-margin* 6))
        (with-output-to-string (s)
          (pprint-logical-block (s nil)
            (dotimes (i 10)
              (declare (ignore i))
              (write-string "A " s)
              (pprint-newline :fill s)))))))
    (check-layout :format-fill fill-output
      (lambda () (let ((*print-right-margin* 6))
        (format nil "~@<A ~:_A ~:_A ~:_A ~:_A ~:_A ~:_A ~:_A ~:_A ~:_A ~:_~:>")))))
  (let ((indent-output (format nil "(M~%   M)")))
    (check-layout :direct-indent indent-output
      (lambda () (with-output-to-string (s)
        (pprint-logical-block (s nil :prefix "(" :suffix ")")
          (write-string "M" s)
          (pprint-indent :current 1 s)
          (pprint-newline :mandatory s)
          (write-string "M" s)))))
    (check-layout :format-indent indent-output
      (lambda () (format nil "~:<M~1:I~@:_M~:>" '(m m)))))
  (let ((*print-right-margin* 20) (*print-miser-width* 20)
        (miser-output (format nil "~%A")))
    (check-layout :direct-miser miser-output
      (lambda () (with-output-to-string (s)
        (pprint-logical-block (s nil)
          (pprint-newline :miser s)
          (write-string "A" s)))))
    (check-layout :format-miser miser-output
      (lambda () (format nil "~@<~@_A~:>"))))
  (check-layout :direct-section-tab "[ ]"
    (lambda () (with-output-to-string (s)
      (pprint-logical-block (s nil :prefix "[" :suffix "]")
        (pprint-tab :section 1 0 s)))))
  (check-layout :format-section-tab "[ ]"
    (lambda () (format nil "~<[~;~,0:T~;]~:>" '(a))))
  (check-layout :format-case-layout (format nil "Foo~%Bar")
    (lambda () (let ((*print-right-margin* 4))
      (format nil "~@<~:(FOO ~:_BAR~)~:>"))))
  (check-layout :format-case-layout-whitespace
      (list "Foo  Bar" "Foobar" (format nil "Foo~%Bar"))
    (lambda ()
      (list (format nil "~:(foo~5Tbar~)")
            (let ((*print-right-margin* 80))
              (format nil "~@<~:(foo~:_bar~)~:>"))
            (let ((*print-right-margin* 3))
              (format nil "~@<~:(foo~:_bar~)~:>")))))
  ;; Lisp printers and callback formatters must contribute their conditional
  ;; newlines to the enclosing FORMAT document.  If any callback is flattened
  ;; through a temporary string stream, [aa bb] is laid out at column zero and
  ;; incorrectly overflows once it is pasted after "1234 ".
  (let ((*print-right-margin* 10)
        (*print-pprint-dispatch* (copy-pprint-dispatch nil))
        (object (make-instance 'semantic-layout-object))
        (expected (format nil "1234 [aa~%      bb]")))
    (set-pprint-dispatch 'semantic-layout-object #'semantic-layout-printer
                         0 *print-pprint-dispatch*)
    (dolist (control '("~@<1234 ~A~:>" "~@<1234 ~S~:>" "~@<1234 ~W~:>"))
      (check-layout :format-object-semantic-layout expected
        (lambda () (format nil control object))))
    (check-layout :formatter-iteration-semantic-layout
        (format nil "1234 [aa~%     bb]")
      (lambda ()
        (format nil "~@<1234 ~{~}~:>"
                (formatter "~*~@<[aa ~:_bb]~:>") (list nil)))))
  (check-layout :format-write-modifiers '("pLN" "PLN" "pln" "Pln" "pLN")
    (lambda ()
      (let ((*print-pretty* nil) (*print-level* 1) (*print-length* 1)
            (object (make-instance 'write-modifier-object)))
        (list (format nil "~W" object)
              (format nil "~:W" object)
              (format nil "~@W" object)
              (format nil "~:@W" object)
              (format nil "~W" object)))))
  (check-layout :format-slash-semantic-layout (format nil "1234 [aa~%      bb]")
    (lambda ()
      (let ((*print-right-margin* 10))
        (format nil "~@<1234 ~/slash-semantic-layout/~:>"
                (make-instance 'semantic-layout-object)))))
  (check-layout :format-callback-column "1234 5"
    (lambda ()
      (format nil "~@<1234 ~/slash-current-column/~:>" nil)))
  (check-layout :format-callback-column-after-break (format nil "abc~%0")
    (lambda ()
      (format nil "~@<abc~:@_~/slash-current-column/~:>" nil)))
  (check-layout :format-callback-destination-column "abc 4"
    (lambda ()
      (with-output-to-string (stream)
        (write-string "abc " stream)
        (format stream "~/slash-current-column/" nil))))
  (check-layout :format-callback-pretty-stream-column "abc 4"
    (lambda ()
      (with-output-to-string (destination)
        (pprint-logical-block (destination nil)
          (write-string "abc " destination)
          (format destination "~/slash-current-column/" nil)))))
  (check-layout :format-line-tab-start-column "abc  x"
    (lambda () (with-output-to-string (s)
      (write-string "abc" s)
      (format s "~5Tx"))))
  (check-layout :format-section-tab-after-unbroken-break "[ab x]"
    (lambda () (let ((*print-right-margin* 80))
      (format nil "~@<[a~:_b~2,0:Tx]~:>"))))
  (check-layout :format-mandatory-not-pretty "ab"
    (lambda () (let ((*print-pretty* nil))
      (format nil "~@<a~:@_b~:>"))))
  (check-layout :format-readably-ignores-line-limit (format nil "(a~%b)")
    (lambda () (let ((*print-lines* 1) (*print-readably* t))
      (format nil "~@<(a~:@_b)~:>"))))
  (check-layout :format-bridge-column 3
    (lambda () (let ((column nil))
      (with-output-to-string (s)
        (pprint-logical-block (s nil)
          (format s "abc")
          (setf column (egcl-gray-streams:stream-line-column s))))
      column)))
  (check-layout :format-bridge-semantic-columns '(5 1)
    (lambda () (let ((columns nil))
      (with-output-to-string (s)
        (pprint-logical-block (s nil)
          (format s "a~5T")
          (push (egcl-gray-streams:stream-line-column s) columns)
          (format s "~:@_b")
          (push (egcl-gray-streams:stream-line-column s) columns)))
      (nreverse columns))))
  (check-layout :format-bridge-section-column 6
    (lambda () (let ((column nil))
      (with-output-to-string (s)
        (pprint-logical-block (s nil)
          (format s "~@<[a~:_b~2,0:Tx]~:>")
          (setf column (egcl-gray-streams:stream-line-column s))))
      column)))
  (check-layout :same-bare-class-name "ok"
    (lambda () (let ((stream (make-instance '%pretty-stream)))
      (format stream "ok")
      (coerce (nreverse (fake-pretty-characters stream)) 'string))))
  (check-layout :gray-column-error t
    (lambda () (handler-case
      (let ((stream (make-instance 'error-column-stream)))
        (pprint-logical-block (stream nil)
          (write-string "x" stream))
        nil)
      (error () t))))
  (check-layout :large-indent-tab "a .."
    (lambda () (let ((*print-lines* 1))
      (with-output-to-string (s)
        (pprint-logical-block (s nil)
          (write-string "a" s)
          (pprint-indent :block -100000000000000000000 s)
          (pprint-newline :mandatory s)
          ;; The line limit stops rendering here, but the bridge must still
          ;; decode this valid Common Lisp integer without wrapping it.
          (pprint-tab :line-relative 100000000000000000000 0 s))))))
  (check-layout :format-circle "(#1=(0) #1#)"
    (lambda () (let ((*print-circle* t) (shared (list 0)))
      (format nil "~:<~@{~A~^ ~}~:>" (list shared shared)))))
  (check-layout :pprint-margin t
    (lambda () (let ((*print-right-margin* 40))
      (multiline-within-p
       (with-output-to-string (s)
         (pprint '(defun area-of-ring (outer inner)
                    (let ((a (* pi outer outer)) (b (* pi inner inner)))
                      (if (> a b) (- a b)
                          (error "inner ~a exceeds outer ~a" inner outer))))
                 s))
       40))))
  (dolist (pretty '(nil t))
    (let ((*print-pretty* pretty))
      (check-layout :newline-stream t (lambda () (handler-case (pprint-newline :fill 42) (type-error () t))))
      (check-layout :indent-stream t (lambda () (handler-case (pprint-indent :block 1 42) (type-error () t)))))))
(assert (zerop *layout-failures*))
(format t "PPRINT-LAYOUT-OK~%")
