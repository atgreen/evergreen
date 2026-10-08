;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; Real clients of the pinned EGCL compatibility forks. No implicit downloads.
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil
      ocicl-runtime:*local-only* t)

(asdf:load-system :babel)
(assert (equalp #(72 101 108 108 111)
                (babel:string-to-octets "Hello" :encoding :utf-8)))
(dolist (alternatives '((:little-endian :big-endian) (:32-bit :64-bit)))
  (assert (= 1 (count-if (lambda (feature) (member feature *features*))
                         alternatives))))
;; Linux implies Unix in trivial-features' canonical set. Asserted here rather
;; than in the fork: the fork carried a tests/egcl-smoke.lisp that duplicated
;; the two checks above and added this one, referenced by nothing -- not its own
;; .asd, not this repo. The durable test belongs in this scenario.
#+linux (assert (member :unix *features*))

(asdf:load-system :flexi-streams)
#+(or egcl sbcl)
(dolist (name '("STREAM-READ-CHAR" "STREAM-READ-BYTE" "STREAM-WRITE-CHAR"
                "STREAM-UNREAD-CHAR" "STREAM-FINISH-OUTPUT"))
  (assert (eq (find-symbol name :trivial-gray-streams)
              (find-symbol name #+egcl :egcl-gray-streams #+sbcl :sb-gray))))
;; Not just the same symbols -- the same CLASSES. A parallel class hierarchy
;; would satisfy the identity check above and still break stream dispatch for
;; clients such as Flexi Streams. Came from the fork's own test/egcl-smoke.lisp,
;; which nothing referenced; the identity check above was already duplicated
;; there, this assertion was not.
(assert (subtypep 'trivial-gray-streams:fundamental-character-input-stream
                  #+egcl 'egcl-gray-streams:fundamental-character-input-stream
                  #+sbcl 'sb-gray:fundamental-character-input-stream
                  #-(or egcl sbcl) 'stream))
;; The bulk sequence bridge (bliss-td6ih). CL:READ-SEQUENCE / WRITE-SEQUENCE on
;; a trivial-gray-streams class must reach the PORTABLE sequence generics, and a
;; class that does not specialize them must still fall back to the scalar
;; methods through OR-FALLBACK. Both halves ran silently wrong before: the
;; native entry points never called the portable generics at all.
(defclass bulk-source (trivial-gray-streams:fundamental-binary-input-stream) ())
(defmethod trivial-gray-streams:stream-read-byte ((s bulk-source))
  (error "scalar read must not be used when the bulk method exists"))
(defmethod trivial-gray-streams:stream-read-sequence ((s bulk-source) seq start end &key)
  (loop for i from start below end do (setf (elt seq i) 42))
  end)
(let ((v (make-array 4 :initial-element 0)))
  (assert (= 3 (read-sequence v (make-instance 'bulk-source) :start 1 :end 3)))
  (assert (equalp v #(0 42 42 0))))
(defclass bulk-sink (trivial-gray-streams:fundamental-character-output-stream)
  ((seen :initform nil :accessor seen)))
(defmethod trivial-gray-streams:stream-write-char ((s bulk-sink) c)
  (error "scalar write must not be used when the bulk method exists"))
(defmethod trivial-gray-streams:stream-write-sequence ((s bulk-sink) seq start end &key)
  (push (subseq seq start end) (seen s))
  seq)
(let ((s (make-instance 'bulk-sink)))
  (write-sequence "portable" s :start 2 :end 6)
  (assert (equal '("rtab") (seen s))))
(defclass scalar-source (trivial-gray-streams:fundamental-binary-input-stream)
  ((bytes :initform (list 5 6 7) :accessor bytes)))
(defmethod trivial-gray-streams:stream-read-byte ((s scalar-source))
  (if (bytes s) (pop (bytes s)) :eof))
;; SBCL's default bulk methods ask the stream for its element type and supply
;; no default for user classes; EGCL defaults it from the fundamental class.
(defmethod stream-element-type ((s scalar-source)) '(unsigned-byte 8))
(let ((v (make-array 4 :initial-element -1)))
  (assert (= 3 (read-sequence v (make-instance 'scalar-source))))
  (assert (equalp v #(5 6 7 -1))))
(defclass scalar-sink (trivial-gray-streams:fundamental-character-output-stream)
  ((chars :initform nil :accessor chars)))
(defmethod trivial-gray-streams:stream-write-char ((s scalar-sink) c)
  (push c (chars s)) c)
(defmethod stream-element-type ((s scalar-sink)) 'character)
(let ((s (make-instance 'scalar-sink)))
  (write-sequence "xyz" s :start 1)
  (assert (equal '(#\z #\y) (chars s))))

(with-open-file (binary "sample-ascii.txt" :element-type '(unsigned-byte 8))
  (let ((stream (flexi-streams:make-flexi-stream
                 binary :external-format '(:utf-8 :eol-style :lf))))
    (assert (string= "Hello" (read-line stream)))
    (assert (eq :eof (read-char stream nil :eof)))))
(format t "LIBRARY-FORKS-OK~%")
