;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defun rejects-slot-value (thunk value expected)
  (assert
   (handler-case (progn (funcall thunk) nil)
     (type-error (condition)
       (and (equal value (type-error-datum condition))
            (not (typep value (type-error-expected-type condition)))
            (or (null expected)
                (equal expected (type-error-expected-type condition))))))))

(defstruct typed-point (x 0d0 :type double-float))
(assert (= 0d0 (typed-point-x (make-typed-point))))
(assert (= 2d0 (typed-point-x (make-typed-point :x 2d0))))
(rejects-slot-value (lambda () (make-typed-point :x 1)) 1 'double-float)

(defvar *slot-argument-count* 0)
(assert (= 3d0 (typed-point-x
               (make-typed-point :x (progn (incf *slot-argument-count*) 3d0)))))
(assert (= *slot-argument-count* 1))

(defvar *slot-default-count* 0)
(defstruct invalid-default
  (x (progn (incf *slot-default-count*) "bad") :type integer))
(rejects-slot-value #'make-invalid-default "bad" 'integer)
(assert (= *slot-default-count* 1))

(defstruct (boa-point (:constructor point-with (x &optional y))
                     (:constructor point-defaults ()))
  (x 0 :type (integer 0 9)) (y :left :type (member :left :right)))
(assert (eq :left (boa-point-y (point-with 3))))
(assert (= 0 (boa-point-x (point-defaults))))
(rejects-slot-value (lambda () (point-with 10)) 10 nil)
(rejects-slot-value (lambda () (point-with 2 :other)) :other nil)

(defstruct slot-parent (count 1 :type integer))
(defstruct (slot-child (:include slot-parent (count 2 :type (integer 0 3)))))
(defstruct (slot-grandchild (:include slot-child)))
(assert (= 2 (slot-child-count (make-slot-child))))
(assert (= 2 (slot-grandchild-count (make-slot-grandchild))))
(rejects-slot-value (lambda () (make-slot-child :count :bad)) :bad nil)
(rejects-slot-value (lambda () (make-slot-grandchild :count 4)) 4 nil)

(defstruct (list-point (:type list)) (x 0 :type integer))
(defstruct (vector-point (:type vector) (:constructor vector-with (x)))
  (x 0 :type integer))
(assert (equal '(4) (make-list-point :x 4)))
(assert (equalp #(5) (vector-with 5)))
(rejects-slot-value (lambda () (make-list-point :x :bad)) :bad 'integer)
(rejects-slot-value (lambda () (vector-with :bad)) :bad 'integer)

(defstruct permissive-slot (x nil))
(assert (eq :anything (permissive-slot-x (make-permissive-slot :x :anything))))
(format t "STRUCT-SLOT-TYPES-OK~%")
