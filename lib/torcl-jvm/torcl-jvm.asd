(asdf:defsystem "torcl-jvm"
  :description "Checked JVM lifecycle, Java objects, calls and Lisp interface adapters for TorCL"
  :version "0.1.0"
  :license "MIT OR Apache-2.0"
  :serial t
  :components ((:file "package") (:file "jvm") (:file "api")))
