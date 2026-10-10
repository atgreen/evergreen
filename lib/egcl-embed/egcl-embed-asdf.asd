;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; The ASDF extension: name this in an application's :defsystem-depends-on to
;;; carry build-host files in its image. See asdf-integration.lisp.
(asdf:defsystem "egcl-embed-asdf"
  :description "Carry build-host files in an EGCL image as components of an ASDF system"
  :version "0.0.1"
  :components ((:file "asdf-integration")))
