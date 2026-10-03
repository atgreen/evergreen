;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;;
;;; Describe an APK in the application's own .asd and let `asdf:make' build it.
;;; This is the ONLY way to describe an APK; the apk.sexp reader it replaced is
;;; gone.
;;;
;;;   (defsystem "my-app"                       ; loadable and testable as usual
;;;     :components ((:file "scene") (:file "egl") (:file "app")))
;;;
;;;   (defsystem "my-app/apk"
;;;     :defsystem-depends-on ("egcl-apk-asdf")
;;;     :class "egcl-apk-asdf:android-apk"
;;;     :build-operation "egcl-apk-asdf:apk-op"
;;;     :depends-on ("my-app")
;;;     :version "0.1"                          ; ASDF's own :version is the version name
;;;     :apk-package "org.example.app"
;;;     :apk-label "My App"
;;;     :apk-hosts ("aarch64-linux-android")
;;;     :components ((:static-file "app.lisp")))
;;;
;;; A SEPARATE system rather than slots on "my-app": `:class' and
;;; `:build-operation' are per-system, so putting them on the application would
;;; make `asdf:make' always mean "build an APK" and would stop the system
;;; loading at all on a machine with no Android runtime. This mirrors how
;;; cffi-grovel and deploy extend ASDF.
;;;
;;; The system's own file components become the APK's flat assets, in
;;; declaration order, so the asset list IS the component list and cannot drift
;;; from it. The component named by :apk-entry (default "app.lisp") must be
;;; among them -- the runtime loads that file, and the rest are loaded by the
;;; application itself, exactly as with a hand-built assets/ directory.
(defpackage :egcl-apk-asdf
  (:use :cl)
  (:export #:android-apk #:apk-op #:*runtime-directory*
           #:build-project))
(in-package :egcl-apk-asdf)

(defclass android-apk (asdf:system)
  ;; ASDF rejects an unknown initarg, so a misspelled slot is an error rather
  ;; than silently ignored -- the same reject-don't-ignore the removed apk.sexp
  ;; reader had.
  ;; Readers are all apk-* : a slot named `identity' would collide with
  ;; CL:IDENTITY in this package.
  ((apk-package :initarg :apk-package :initform nil :reader apk-package)
   (label :initarg :apk-label :initform nil :reader apk-label)
   (version-code :initarg :apk-version-code :initform 1 :reader apk-version-code)
   (min-sdk :initarg :apk-min-sdk :initform 28 :reader apk-min-sdk)
   (target-sdk :initarg :apk-target-sdk :initform 34 :reader apk-target-sdk)
   (debuggable :initarg :apk-debuggable :initform nil :reader apk-debuggable)
   (permissions :initarg :apk-permissions :initform nil :reader apk-permissions)
   (hosts :initarg :apk-hosts :initform '("aarch64-linux-android") :reader apk-hosts)
   (runtime-api :initarg :apk-runtime-api :initform 4 :reader apk-runtime-api)
   (runtime-version :initarg :apk-runtime-version :initform nil :reader apk-runtime-version)
   (entry :initarg :apk-entry :initform "app.lisp" :reader apk-entry)
   ;; A pathname, never key material, relative to the system by default.
   (identity-path :initarg :apk-identity :initform ".egcl-apk-key" :reader apk-identity-path)
   (output :initarg :apk-output :initform nil :reader apk-output)))

;; A selfward operation on LOAD-OP: the application must be loadable before its
;; APK is built, so a broken app fails the build instead of shipping.
(defclass apk-op (asdf:selfward-operation)
  ((asdf:selfward-operation :initform 'asdf:load-op :allocation :class)))

(defun system-asset-files (system)
  "The system's file components, in declaration order."
  (let ((files nil))
    (labels ((walk (component)
               (cond ((typep component 'asdf:parent-component)
                      (mapc #'walk (asdf:component-children component)))
                     ((typep component 'asdf:file-component)
                      (push (asdf:component-pathname component) files)))))
      (walk system))
    (nreverse files)))

(defun apk-config (system)
  "The configuration plist the builder takes, from SYSTEM's slots.
An unset optional is omitted rather than passed as NIL, so the builder applies
its own default instead of being handed an explicit nothing."
  (let ((config (list :package (or (apk-package system)
                                   (error "~A: :apk-package is required"
                                          (asdf:component-name system)))
                      :label (or (apk-label system) (asdf:component-name system))
                      :version-code (apk-version-code system)
                      :min-sdk (apk-min-sdk system)
                      :target-sdk (apk-target-sdk system)
                      :hosts (apk-hosts system)
                      :runtime-api (apk-runtime-api system))))
    ;; ASDF's own :version carries the user-visible version name.
    (when (asdf:component-version system)
      (setf config (append config (list :version-name (asdf:component-version system)))))
    (when (apk-debuggable system) (setf config (append config (list :debuggable t))))
    (when (apk-permissions system)
      (setf config (append config (list :permissions (apk-permissions system)))))
    (when (apk-runtime-version system)
      (setf config (append config (list :runtime-version (apk-runtime-version system)))))
    config))

(defvar *runtime-directory* nil
  "Overrides EGCL_APK_RUNTIME when non-NIL, for a caller that would rather bind
a variable than set an environment variable.")

(defun apk-runtime-directory ()
  "The extracted egcl-target-android runtime. A property of the BUILD HOST, not
of the application, so it stays out of the .asd; the .asd may only constrain it
through :apk-runtime-api and :apk-runtime-version, which the builder checks
against the runtime's own runtime.json."
  (let* ((dir (uiop:ensure-directory-pathname
               (or *runtime-directory*
                   (uiop:getenv "EGCL_APK_RUNTIME")
                   "/usr/libexec/egcl/android/"))))
    (unless (probe-file (merge-pathnames "runtime.json" dir))
      (error "No Android runtime at ~A (no runtime.json).~%~
              Install egcl-target-android, or point EGCL_APK_RUNTIME at an extracted copy."
             dir))
    dir))

(defmethod asdf:operation-done-p ((o apk-op) (s android-apk))
  ;; Always rebuild. The output depends on the Android runtime and the signing
  ;; identity as well as on the sources, and neither is an ASDF input whose
  ;; timestamp we could compare against.
  nil)

(defmethod asdf:perform ((o apk-op) (s android-apk))
  (let* ((root (asdf:system-source-directory s))
         (entry (apk-entry s))
         (files (system-asset-files s))
         (assets (mapcar (lambda (path)
                           (egcl-apk::asset-entry (file-namestring path) path))
                         files))
         (identity-path (merge-pathnames (apk-identity-path s) root))
         ;; The PRIMARY name: this system is conventionally "my-app/apk", and a
         ;; slash in a merge-pathnames component would bury the output in a
         ;; build/my-app/ subdirectory as "apk.apk".
         (output (or (apk-output s)
                     (merge-pathnames (format nil "build/~A.apk"
                                              (asdf:primary-system-name s))
                                      root))))
    (unless files
      (error "~A: no file components, so the APK would have no assets"
             (asdf:component-name s)))
    (unless (member (concatenate 'string "assets/" entry) assets :key #'car :test #'equal)
      (error "~A: the :apk-entry ~S is not one of this system's components: ~{~A~^ ~}"
             (asdf:component-name s) entry (mapcar #'file-namestring files)))
    ;; A missing identity is minted here, by any caller. That used to be
    ;; refused unless a shell wrapper had set umask 077 first, because
    ;; create-identity writes an unencrypted P-256 private key and EGCL had no
    ;; chmod to repair the mode afterwards. It now chmods the key 0600 itself
    ;; (egcl-apk::restrict-to-owner), so the umask no longer decides whether a
    ;; private key is world-readable and a plain `asdf:make' is safe.
    (egcl-apk::build-apk-from (apk-config s) (apk-runtime-directory)
                              assets output identity-path)))

(defun build-project (directory)
  "Build the APK of the project in DIRECTORY: load its .asd and make the
system's /apk component. The directory must hold exactly one .asd, whose
primary system NAME gives the APK system NAME/apk -- the convention the class
and build operation above are written for."
  (let* ((directory (uiop:ensure-directory-pathname (truename directory)))
         (asds (directory (merge-pathnames "*.asd" directory))))
    (unless asds
      (error "No .asd in ~A. An APK is described by an ASDF system definition; ~
              see lib/egcl-apk/README.md." directory))
    (unless (= 1 (length asds))
      (error "~A holds ~D .asd files (~{~A~^ ~}); build the one you want with ~
              asdf:make instead." directory (length asds)
              (mapcar #'file-namestring asds)))
    (let* ((asd (first asds))
           (primary (pathname-name asd))
           (system (format nil "~A/apk" primary)))
      (asdf:load-asd asd)
      (unless (asdf:find-system system nil)
        (error "~A defines no ~S system. Add one with :class ~
                \"egcl-apk-asdf:android-apk\" and :build-operation ~
                \"egcl-apk-asdf:apk-op\"; see lib/egcl-apk/README.md."
               (file-namestring asd) system))
      (asdf:make system))))
