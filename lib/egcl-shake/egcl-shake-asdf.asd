;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; The ASDF extension: name this in an application's :defsystem-depends-on to
;;; describe its shaken executable in its own .asd. See asdf-integration.lisp.
(asdf:defsystem "egcl-shake-asdf"
  :description "Shake a tree-shaken EGCL executable from an ASDF system definition"
  :version "0.0.1"
  :components ((:file "asdf-integration")))
