;;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defpackage :egcl-pprint (:use :common-lisp))
(in-package :egcl-pprint)

;; Output is queued as nested blocks so a child's line breaks can be chosen
;; with the enclosing block's actual column, rather than its speculative one.
(defclass %pretty-stream (egcl-gray-streams:fundamental-character-output-stream)
  ((frames :initform nil :accessor %pp-frames)
   (column :initform 0 :accessor %pp-column)))

(defun %pp-enqueue (stream event)
  (push event (car (%pp-frames stream))))

(defun %pp-enqueue-events (stream events)
  (let ((section (%pp-column stream)))
    (dolist (event events)
      (%pp-enqueue stream event)
      (%pp-note-event-column stream event section)
      (when (and (consp event) (eq (first event) :newline))
        (setf section (%pp-column stream)))))
  nil)

(defun %pp-note-event-column (stream event &optional (section 0))
  ;; FORMAT queues a whole semantic document at once.  Keep the speculative
  ;; column consistent with STREAM-WRITE-CHAR for code which asks for the
  ;; column before the enclosing logical block is finally laid out.
  (cond ((characterp event)
         (setf (%pp-column stream)
               (if (char= event #\Newline) 0 (1+ (%pp-column stream)))))
        ((and (consp event) (eq (first event) :block))
         (map nil (lambda (character) (%pp-note-event-column stream character section))
              (second event))
         (let ((nested-section (%pp-column stream)))
           (dolist (nested (fifth event))
             (%pp-note-event-column stream nested nested-section)
             (when (and (consp nested) (eq (first nested) :newline))
               (setf nested-section (%pp-column stream)))))
         (map nil (lambda (character) (%pp-note-event-column stream character section))
              (fourth event)))
        ((and (consp event) (member (first event) '(:case-start :case-end))))
        ((and *print-pretty* (consp event) (eq (first event) :newline)
              (eq (second event) :mandatory))
         (setf (%pp-column stream) 0))
        ((and (consp event) (eq (first event) :tab))
         (let* ((column (%pp-column stream))
                (colnum (third event))
                (colinc (fourth event))
                (kind (second event))
                (origin (if (member kind '(:section :section-relative)) section 0))
                (current (max 0 (- column origin)))
                (relative (member kind '(:line-relative :section-relative)))
                (spaces
                  (if relative
                      (+ colnum (if (and (plusp colinc)
                                         (not (zerop (mod (+ current colnum) colinc))))
                                    (- colinc (mod (+ current colnum) colinc)) 0))
                      (cond ((< current colnum) (- colnum current))
                            ((plusp colinc) (- colinc (mod (- current colnum) colinc)))
                            (t 0)))))
           (incf (%pp-column stream) spaces)))))

(defmethod egcl-gray-streams:stream-write-char ((stream %pretty-stream) character)
  (%pp-enqueue stream character)
  (setf (%pp-column stream) (if (char= character #\Newline) 0 (1+ (%pp-column stream))))
  character)

(defmethod egcl-gray-streams:stream-line-column ((stream %pretty-stream))
  (%pp-column stream))

(defun %pp-render (node destination)
  (egcl::%pprint-render-layout node destination))

(defun %pp-output-stream (stream)
  (let ((stream (cond ((null stream) *standard-output*) ((eq stream t) *terminal-io*) (t stream))))
    (unless (and (streamp stream) (output-stream-p stream))
      (error 'type-error :datum stream :expected-type 'stream))
    stream))

(defun %pp-pop (state stream exit)
  (let ((remaining (first state)) (count (second state)))
    (cond ((not (listp remaining))
           (write-string ". " stream) (write remaining :stream stream) (throw exit nil))
          ((and *print-length* (not *print-readably*) (>= count *print-length*))
           (write-string "..." stream) (throw exit nil))
          ((and (plusp count) (egcl::%pprint-circle :shared remaining))
           (write-string ". " stream) (write remaining :stream stream) (throw exit nil)))
    (incf (second state))
    (pop (first state))))

(defun %pp-call (destination object prefix per-line suffix function)
  (check-type prefix string)
  (check-type suffix string)
  (setf destination (%pp-output-stream destination))
  (unless (and (streamp destination) (output-stream-p destination))
    (error 'type-error :datum destination :expected-type 'stream))
  (egcl::%pprint-circle :enter object)
  (unwind-protect
       (cond ((not (listp object)) (write object :stream destination))
             ((and *print-level* (not *print-readably*) (<= *print-level* 0))
              (write-char #\# destination))
             (t
              (let ((mark (egcl::%pprint-circle :mark object)))
                (when (not (zerop mark))
                  (format destination "#~D~A" (abs mark) (if (minusp mark) "#" "=")))
                (unless (minusp mark)
                  (let* ((nested (typep destination '%pretty-stream))
                         (stream (if nested destination (make-instance '%pretty-stream)))
                         (exit (gensym)) (state (list object 0))
                         (*print-level* (and *print-level* (not *print-readably*) (1- *print-level*))))
                    (push nil (%pp-frames stream))
                    (incf (%pp-column stream) (length prefix))
                    (unwind-protect
                         (catch exit (funcall function stream state exit))
                      (let ((node (list :block prefix per-line suffix
                                        (nreverse (pop (%pp-frames stream))))))
                        (incf (%pp-column stream) (length suffix))
                        (if nested (%pp-enqueue stream node) (%pp-render node destination)))))))))
    (egcl::%pprint-circle :exit nil))
  nil)

(in-package :common-lisp)

(defmacro pprint-logical-block ((stream object &rest options) &body body)
  "Group output into a logical block with lexical list traversal and print limits."
  (unless (symbolp stream) (error "PPRINT-LOGICAL-BLOCK requires a stream variable"))
  (let ((binding (cond ((null stream) '*standard-output*) ((eq stream t) '*terminal-io*) (t stream)))
        (value (gensym)) (state (gensym)) (exit (gensym)) (output (gensym))
        (bindings nil) (keywords nil))
    (loop while options do
      (let ((key (pop options)) (temporary (gensym)))
        (unless (and options (member key '(:prefix :per-line-prefix :suffix)))
          (error "Invalid PPRINT-LOGICAL-BLOCK options"))
        (push (list temporary (pop options)) bindings)
        (unless (assoc key keywords) (push (cons key temporary) keywords))))
    (when (and (assoc :prefix keywords) (assoc :per-line-prefix keywords))
      (error "PPRINT-LOGICAL-BLOCK cannot combine :PREFIX and :PER-LINE-PREFIX"))
    `(let* ((,value ,object) ,@(nreverse bindings))
       (egcl-pprint::%pp-call ,binding ,value
                      ,(or (cdr (assoc :prefix keywords)) (cdr (assoc :per-line-prefix keywords)) "")
                      ,(not (null (assoc :per-line-prefix keywords)))
                      ,(or (cdr (assoc :suffix keywords)) "")
                      (lambda (,output ,state ,exit)
                        (let ((,binding ,output))
                         (macrolet ((pprint-pop () '(egcl-pprint::%pp-pop ,state ,binding ,exit))
                                   (pprint-exit-if-list-exhausted ()
                                     '(when (null (first ,state)) (throw ,exit nil))))
                          ,@body)))))))

(defmacro pprint-pop ()
  (error "PPRINT-POP requires a lexical PPRINT-LOGICAL-BLOCK"))

(defmacro pprint-exit-if-list-exhausted ()
  (error "PPRINT-EXIT-IF-LIST-EXHAUSTED requires a lexical PPRINT-LOGICAL-BLOCK"))

(defun pprint-newline (kind &optional stream)
  "Queue a conditional newline in the current pretty-printing stream."
  (unless (member kind '(:linear :fill :miser :mandatory))
    (error 'type-error :datum kind :expected-type '(member :linear :fill :miser :mandatory)))
  (let ((stream (egcl-pprint::%pp-output-stream stream)))
    (when (and *print-pretty* (typep stream 'egcl-pprint::%pretty-stream))
      (egcl-pprint::%pp-enqueue stream (list :newline kind))))
  nil)

(defun pprint-indent (relative-to n &optional stream)
  (unless (member relative-to '(:block :current))
    (error 'type-error :datum relative-to :expected-type '(member :block :current)))
  (check-type n real)
  (let ((stream (egcl-pprint::%pp-output-stream stream)))
    (when (and *print-pretty* (typep stream 'egcl-pprint::%pretty-stream))
      (egcl-pprint::%pp-enqueue
       stream (list :indent relative-to
                    (max -2147483648 (min 2147483647 (truncate n)))))))
  nil)

(defun pprint-tab (kind colnum colinc &optional stream)
  "Queue a line- or section-relative tab in the current pretty-printing stream."
  (unless (member kind '(:line :section :line-relative :section-relative))
    (error 'type-error :datum kind
           :expected-type '(member :line :section :line-relative :section-relative)))
  (check-type colnum (integer 0))
  (check-type colinc (integer 0))
  (let ((stream (egcl-pprint::%pp-output-stream stream)))
    (when (and *print-pretty* (typep stream 'egcl-pprint::%pretty-stream))
      (egcl-pprint::%pp-enqueue
       stream (list :tab kind (min colnum 4294967295) (min colinc 4294967295)))))
  nil)

(defun egcl-pprint::%pp-write-object (stream object)
  (multiple-value-bind (printer foundp) (pprint-dispatch object)
    (cond (foundp (funcall printer stream object))
          ((consp object)
           (pprint-logical-block (stream object :prefix "(" :suffix ")")
             (pprint-exit-if-list-exhausted)
             (loop
               (egcl-pprint::%pp-write-object stream (pprint-pop))
               (pprint-exit-if-list-exhausted)
               (write-char #\Space stream)
               (pprint-newline :fill stream))))
          (t (let ((*print-pretty* nil))
               (write object :stream stream :escape t))))))

(defun pprint (object &optional stream)
  "Print OBJECT readably, using logical blocks and the active pprint dispatch table."
  (let ((stream (egcl-pprint::%pp-output-stream stream)))
    (terpri stream)
    (let ((*print-pretty* t) (*print-escape* t))
      (egcl-pprint::%pp-write-object stream object)))
  (values))

(defun pprint-fill (stream object &optional (colon-p t) at-sign-p)
  "Print list elements with fill-style line breaks."
  (declare (ignore at-sign-p))
  (pprint-logical-block (stream object :prefix (if colon-p "(" "") :suffix (if colon-p ")" ""))
    (pprint-exit-if-list-exhausted)
    (loop (write (pprint-pop) :stream stream)
          (pprint-exit-if-list-exhausted)
          (write-char #\Space stream)
          (pprint-newline :fill stream))))

(defun pprint-linear (stream object &optional (colon-p t) at-sign-p)
  "Print list elements with linear-style line breaks."
  (declare (ignore at-sign-p))
  (pprint-logical-block (stream object :prefix (if colon-p "(" "") :suffix (if colon-p ")" ""))
    (pprint-exit-if-list-exhausted)
    (loop (write (pprint-pop) :stream stream)
          (pprint-exit-if-list-exhausted)
          (write-char #\Space stream)
          (pprint-newline :linear stream))))

(in-package :cl-user)
