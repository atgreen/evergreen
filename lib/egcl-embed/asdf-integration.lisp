;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;;
;;; Files that travel with the image, described as ASDF components. A system
;;; lists the build-host files its code reads at run time; LOAD-OP on the
;;; component embeds each one under the path the application will ask for,
;;; through EGCL-EXT:EMBED-FILE, so a saved image, a shaken executable or a
;;; future APK carries them to hosts that do not have them (bliss-ceyqq).
;;;
;;; Only the Lisp image sees embedded files: OPEN, PROBE-FILE, DIRECTORY, LOAD
;;; and the other stream and pathname operations find them; the operating
;;; system, foreign code and child processes do not.
;;;
;;; Two component types. (:embedded-file NAME :path P [:source S]) carries the
;;; one file S (default P) as P. (:embedded-tree NAME :path P [:source S]
;;; [:only PATTERNS]) carries the files under directory S (default P) as the
;;; same relative paths under P, optionally restricted to the files matching
;;; one of PATTERNS, wild relative pathnames such as "America/*" or "**/*.lisp".
;;; :source defaults to :path because "the same place on the build host" is the
;;; usual case; a missing source is an error at load time, which is when it is
;;; cheapest to learn. Both are static files to ASDF: nothing to compile, and
;;; their load is re-performed in each fresh session, which is exactly when the
;;; registry needs filling again.

(defpackage :egcl-embed-asdf
  (:use :cl)
  (:export #:embedded-file #:embedded-tree
           #:embedded-path #:embedded-source #:embedded-only
           #:embedded-files-of))
(in-package :egcl-embed-asdf)

(defclass embedded-file (asdf:static-file)
  ((path :initarg :path :initform nil :reader embedded-path
         :documentation "The absolute path the application opens at run time.")
   (source :initarg :source :initform nil :reader embedded-source
           :documentation "The build-host file to read; default the same path."))
  (:documentation "One build-host file carried in the image at :path."))

(defclass embedded-tree (asdf:static-file)
  ((path :initarg :path :initform nil :reader embedded-path
         :documentation "The absolute directory the application reads at run time.")
   (source :initarg :source :initform nil :reader embedded-source
           :documentation "The build-host directory to read; default the same path.")
   (only :initarg :only :initform nil :reader embedded-only
         :documentation "Wild relative pathnames; when given, only matching files are carried."))
  (:documentation "The files under a build-host directory, carried in the image under :path."))

;; The keyword forms (:embedded-file ...) and (:embedded-tree ...) resolve
;; through the ASDF package, the way cffi-grovel's component types do.
(defclass asdf::embedded-file (embedded-file) ())
(defclass asdf::embedded-tree (embedded-tree) ())

(defun required-path (component)
  (let ((path (embedded-path component)))
    (unless path
      (error "~A: :path is required (the run-time path of the embedded file)"
             (asdf:component-name component)))
    (namestring path)))

(defun source-of (component)
  "The build-host file or directory to read, as a namestring: :source, else
the run-time path, resolved against the system's directory when relative."
  (let ((source (or (embedded-source component) (embedded-path component))))
    (namestring (merge-pathnames source (asdf:component-pathname (asdf:component-parent component))))))

;; The component's own pathname is its source, so ASDF's input-file stamps and
;; "missing input" checks apply to the build-host file.
(defmethod asdf:component-pathname ((c embedded-file))
  (pathname (source-of c)))

(defmethod asdf:component-pathname ((c embedded-tree))
  (uiop:ensure-directory-pathname (source-of c)))

(defmethod asdf:input-files ((o asdf:load-op) (c embedded-file))
  (list (asdf:component-pathname c)))

(defmethod asdf:input-files ((o asdf:load-op) (c embedded-tree))
  (embedded-files-of c))

(defun embedded-files-of (tree)
  "The build-host files an embedded-tree carries, as absolute pathnames."
  (let* ((root (asdf:component-pathname tree))
         (patterns (embedded-only tree))
         (files nil))
    (unless (probe-file root)
      (error "~A: no directory at ~A to embed" (asdf:component-name tree) root))
    (uiop:collect-sub*directories
     root (constantly t) (constantly t)
     (lambda (directory)
       (dolist (file (uiop:directory-files directory))
         (when (or (null patterns)
                   (some (lambda (pattern)
                           (pathname-match-p file (merge-pathnames pattern root)))
                         patterns))
           (push file files)))))
    (sort files #'string< :key #'namestring)))

(defmethod asdf:perform ((o asdf:load-op) (c embedded-file))
  (egcl-ext:embed-file (required-path c) (source-of c))
  nil)

(defmethod asdf:perform ((o asdf:load-op) (c embedded-tree))
  (let* ((root (asdf:component-pathname c))
         (target (uiop:ensure-directory-pathname (required-path c)))
         (files (embedded-files-of c)))
    (dolist (file files)
      (let ((relative (uiop:enough-pathname file root)))
        (egcl-ext:embed-file (namestring (merge-pathnames relative target))
                             (namestring file))))
    nil))
