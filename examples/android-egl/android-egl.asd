;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; An APK described entirely here: `asdf:make "android-egl/apk"'.

;; The primary system exists so ASDF accepts the secondary name below. It has no
;; components because this application's sources are TARGET code -- they call
;; the Android EGL bindings that only exist inside the APK runtime -- so loading
;; them on the build host would fail. An application whose code is host-loadable
;; would list it here and name it in the /apk system's :depends-on, which makes
;; a broken app fail the build.
(asdf:defsystem "android-egl"
  :description "EGL demo for the Android runtime (target-only sources)"
  :components ())

(asdf:defsystem "android-egl/apk"
  :defsystem-depends-on ("egcl-apk-asdf")
  :class "egcl-apk-asdf:android-apk"
  :build-operation "egcl-apk-asdf:apk-op"
  :description "The signed APK"
  :version "0.1"                        ; ASDF's :version is the APK version name
  :apk-package "org.egcl.example.egl"
  :apk-label "EGCL EGL"
  :apk-version-code 1
  :apk-min-sdk 28
  :apk-target-sdk 34
  :apk-debuggable t
  :apk-hosts ("aarch64-linux-android")
  :apk-runtime-api 4
  :apk-runtime-version "0.0.4"
  ;; These become the APK's assets, in this order. app.lisp is the entry the
  ;; runtime loads; it loads the others itself.
  :pathname "assets/"
  :components ((:static-file "app.lisp")
               (:static-file "egl.lisp")
               (:static-file "scene.lisp")))
