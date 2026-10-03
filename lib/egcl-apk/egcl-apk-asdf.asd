;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; The ASDF extension: name this in an application's :defsystem-depends-on to
;;; describe its APK in its own .asd. See asdf-integration.lisp.
(asdf:defsystem "egcl-apk-asdf"
  :description "Build an Android APK from an ASDF system definition"
  :version "0.0.1"
  :depends-on ("egcl-apk")
  :components ((:file "asdf-integration")))
