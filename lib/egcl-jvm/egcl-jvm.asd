;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(asdf:defsystem "egcl-jvm"
  :description "Checked JVM lifecycle, Java objects, calls and Lisp interface adapters for EGCL"
  :version "0.0.1"
  :license "GPL-3.0-or-later WITH Classpath-exception-2.0"
  :serial t
  :components ((:file "package") (:file "jvm") (:file "api")))
