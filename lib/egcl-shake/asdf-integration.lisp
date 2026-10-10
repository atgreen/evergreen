;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;;
;;; Shaking described by a system definition. An application names this
;;; system in :defsystem-depends-on, gives a secondary system the class and
;;; build operation below, and `asdf:make' on that system writes the retention
;;; specification from the system's slots, saves a core, runs the shake
;;; tool on it, and leaves a standalone executable under build/.
;;;
;;; Shaking is a separate execution mode of the runtime: it restores a saved
;;; core in a fresh process and refuses to combine with any other mode, and
;;; saving a core terminates the process that saves it. So both steps run as
;;; child processes of the same egcl executable that is running this build;
;;; the parent's LOAD-OP has already compiled the application, which the child
;;; loads from the shared FASL cache before saving.
;;;
;;; Modeled on egcl-apk-asdf: the .asd is the only description of the build.

(defpackage :egcl-shake-asdf
  (:use :cl)
  (:export #:shaken-application #:shake-op
           #:*runtime-pathname* #:runtime-pathname
           #:shake-spec #:build-project))
(in-package :egcl-shake-asdf)

(defclass shaken-application (asdf:system)
  ;; Readers are all shake-* so none collides with ASDF's own accessors; the
  ;; initargs mirror the keys of the specification file one for one, so the
  ;; shake guide's table reads as this class's documentation.
  ((entry :initarg :shake-entry :initform nil :reader shake-entry
          :documentation "The entry function: a symbol, or \"PACKAGE::FUNCTION\".
Falls back to ASDF's :entry-point.")
   (system :initarg :shake-system :initform nil :reader shake-system
           :documentation "The system the saved core loads; default the primary system.")
   (prune-packages :initarg :shake-prune-packages :initform nil :reader shake-prune-packages)
   (keep :initarg :shake-keep :initform nil :reader shake-keep)
   (dynamic :initarg :shake-dynamic :initform :preserve :reader shake-dynamic)
   (runtime :initarg :shake-runtime :initform :full :reader shake-runtime)
   (max-tier :initarg :shake-max-tier :initform :t2 :reader shake-max-tier)
   (runtime-keep :initarg :shake-runtime-keep :initform nil :reader shake-runtime-keep)
   (runtime-source :initarg :shake-runtime-source :initform nil :reader shake-runtime-source
                   :documentation "The EGCL source checkout a specialized runtime is built from.")
   (output :initarg :shake-output :initform nil :reader shake-output
           :documentation "The executable to write; default build/NAME under the system's directory.")))

;; A selfward operation on LOAD-OP: the application must load before it is
;; shaken, so a broken application fails the build instead of shipping, and
;; its FASLs are then current for the child that saves the core.
(defclass shake-op (asdf:selfward-operation)
  ((asdf:selfward-operation :initform 'asdf:load-op :allocation :class)))

;;; The specification

(defun spec-name (designator what)
  "DESIGNATOR as the PACKAGE::NAME the specification wants. A string is read
the way the standard reader would read it, so \"my-app::main\" names MY-APP::MAIN;
a symbol is taken as it is, which is the way to name a mixed-case function."
  (etypecase designator
    (symbol
     (let ((package (symbol-package designator)))
       (unless package
         (error "~A: ~S is uninterned, so it has no PACKAGE::NAME" what designator))
       (format nil "~A::~A" (package-name package) (symbol-name designator))))
    (string
     (let* ((colon (position #\: designator))
            (name (and colon (string-left-trim ":" (subseq designator colon)))))
       (unless (and colon (plusp colon) (plusp (length name)))
         (error "~A: ~S must name PACKAGE::FUNCTION" what designator))
       (format nil "~A::~A" (string-upcase (subseq designator 0 colon)) (string-upcase name))))))

(defun package-spec-name (designator)
  "A package designator as the specification spells it; \"*\" passes through."
  (let ((name (string designator)))
    (if (string= name "*") name (string-upcase name))))

(defun choice (value allowed what)
  (unless (member value allowed)
    (error "~A must be one of ~{~S~^, ~}, not ~S" what allowed value))
  (string-downcase (symbol-name value)))

(defun shake-spec (system)
  "The retention specification text for SYSTEM, from its slots."
  (let ((entry (or (shake-entry system)
                   (asdf::component-entry-point system)
                   (error "~A: :shake-entry (or :entry-point) is required"
                          (asdf:component-name system)))))
    (with-output-to-string (out)
      (format out "# Written by egcl-shake-asdf from ~A; edit the .asd, not this file.~%"
              (asdf:component-name system))
      (format out "version = 1~%entry = ~A~%" (spec-name entry ":shake-entry"))
      (dolist (package (shake-prune-packages system))
        (format out "prune-package = ~A~%" (package-spec-name package)))
      (dolist (name (shake-keep system))
        (format out "keep = ~A~%" (spec-name name ":shake-keep")))
      (format out "dynamic = ~A~%" (choice (shake-dynamic system) '(:preserve :explicit) ":shake-dynamic"))
      (format out "runtime = ~A~%" (choice (shake-runtime system) '(:full :specialized) ":shake-runtime"))
      (format out "max-tier = ~A~%" (choice (shake-max-tier system) '(:t2 :t1 :t0) ":shake-max-tier"))
      (dolist (capability (shake-runtime-keep system))
        (format out "runtime-keep = ~A~%" (string-downcase (string capability)))))))

;;; The runtime that runs the two child steps

(defvar *runtime-pathname* nil
  "The egcl executable the child processes run, when not the one running this
build. Overrides EGCL_RUNTIME.")

(defun runtime-pathname ()
  "The egcl executable that builds the core and shakes it. The running one by
default: the core must be saved by the runtime that will carry it."
  (let ((explicit (or *runtime-pathname* (uiop:getenv "EGCL_RUNTIME"))))
    (cond (explicit
           (or (probe-file explicit)
               (error "No EGCL runtime at ~A" explicit)))
          ;; Linux names the running executable whatever argv[0] says.
          ((probe-file "/proc/self/exe"))
          (t
           (let ((argv0 (first (egcl-ext:raw-command-line-arguments))))
             (or (and (find #\/ argv0) (probe-file argv0))
                 (loop for directory in (uiop:split-string (or (uiop:getenv "PATH") "") :separator ":")
                       for candidate = (and (plusp (length directory))
                                            (merge-pathnames argv0 (uiop:ensure-directory-pathname directory)))
                       when (and candidate (probe-file candidate)) return it)
                 (error "Cannot locate the running EGCL executable from argv[0] ~S; ~
                         set EGCL_RUNTIME or egcl-shake-asdf:*runtime-pathname*."
                        argv0)))))))

(defun run-egcl (arguments)
  "Run the runtime with ARGUMENTS, its output streaming through, and fail
loudly on a non-zero exit."
  (let ((command (cons (namestring (runtime-pathname)) arguments)))
    (multiple-value-bind (output error-output status)
        (uiop:run-program command :output t :error-output t :ignore-error-status t)
      (declare (ignore output error-output))
      (unless (eql status 0)
        (error "~A exited with status ~A~%  ~{~A~^ ~}" (first command) status command)))))

(defun registry-forwarding-forms ()
  "Forms that give a child process the system registry this process resolved
systems with. CL_SOURCE_REGISTRY is inherited through the environment; what the
build set programmatically is forwarded here. Systems found only through an
init file hook are not, because a child started with --eval reads no init file;
name their directories in CL_SOURCE_REGISTRY or asdf:*central-registry*."
  (append
   (when asdf:*central-registry*
     (list (format nil "(setf asdf:*central-registry* '~S)"
                   (mapcar #'namestring asdf:*central-registry*))))
   (when (and (boundp 'asdf::*source-registry-parameter*)
              asdf::*source-registry-parameter*)
     (list (format nil "(asdf:initialize-source-registry '~S)"
                   asdf::*source-registry-parameter*)))))

(defun eval-arguments (forms)
  (loop for form in forms collect "--eval" collect form))

(defun save-core (system core)
  "Save the application's core in a child: load the .asd, load the system,
save. The parent's LOAD-OP has already compiled everything the child loads."
  (let ((asd (asdf:system-source-file system))
        (name (or (shake-system system) (asdf:primary-system-name system))))
    (unless asd
      (error "~A has no source file, so a child cannot load it" (asdf:component-name system)))
    (run-egcl
     (list* "--no-init"
            (eval-arguments
             (append (list "(require :asdf)")
                     (registry-forwarding-forms)
                     (list (format nil "(asdf:load-asd ~S)" (namestring asd))
                           (format nil "(asdf:load-system ~S)" name)
                           (format nil "(egcl-ext:save-lisp-and-die ~S)" (namestring core)))))))))

(defun shake (system spec core output)
  (run-egcl
   (append (list "--image" (namestring core)
                 "--shake" (namestring spec)
                 "--output" (namestring output))
           (when (shake-runtime-source system)
             (list "--runtime-source" (namestring (shake-runtime-source system)))))))

;;; The operation

(defun build-paths (system)
  "The executable, core and specification paths: build/NAME under the system's
directory unless :shake-output names the executable, in which case the other
two sit beside it. NAME is the primary system's, so \"my-app/shake\" yields
build/my-app, not build/my-app/shake."
  (let* ((root (asdf:system-source-directory system))
         (output (if (shake-output system)
                     (merge-pathnames (shake-output system) root)
                     (merge-pathnames (format nil "build/~A" (asdf:primary-system-name system)) root)))
         (base (make-pathname :type nil :defaults output)))
    (values output
            (make-pathname :type "core" :defaults base)
            (make-pathname :type "shake" :defaults base))))

(defmethod asdf:output-files ((o shake-op) (s shaken-application))
  (multiple-value-bind (output core spec) (build-paths s)
    (values (list output core spec) t)))

(defmethod asdf:operation-done-p ((o shake-op) (s shaken-application))
  ;; Always rebuild. The output depends on the runtime executable and on every
  ;; loaded system, neither of which is an input whose timestamp ASDF compares.
  nil)

(defmethod asdf:perform ((o shake-op) (s shaken-application))
  ;; The slots are checked before anything is written, so a misspelled choice
  ;; leaves no half-made build/ behind.
  (let ((text (shake-spec s)))
    (multiple-value-bind (output core spec) (build-paths s)
      (ensure-directories-exist output)
      (with-open-file (out spec :direction :output :if-exists :supersede)
        (write-string text out))
      (save-core s core)
      (shake s spec core output)
      output)))

(defun build-project (directory)
  "Shake the project in DIRECTORY: load its .asd and make the system's
/shake component. The directory must hold exactly one .asd, whose primary
system NAME gives the shaken system NAME/shake -- the convention the class
and build operation above are written for."
  (let* ((directory (uiop:ensure-directory-pathname (truename directory)))
         (asds (directory (merge-pathnames "*.asd" directory))))
    (unless asds
      (error "No .asd in ~A. A shaken executable is described by an ASDF ~
              system definition; see lib/egcl-shake/README.md." directory))
    (unless (= 1 (length asds))
      (error "~A holds ~D .asd files (~{~A~^ ~}); build the one you want with ~
              asdf:make instead." directory (length asds) (mapcar #'file-namestring asds)))
    (let* ((asd (first asds))
           (system (format nil "~A/shake" (pathname-name asd))))
      (asdf:load-asd asd)
      (unless (asdf:find-system system nil)
        (error "~A defines no ~S system. Add one with :class ~
                \"egcl-shake-asdf:shaken-application\" and :build-operation ~
                \"egcl-shake-asdf:shake-op\"; see lib/egcl-shake/README.md."
               (file-namestring asd) system))
      (asdf:make system))))
