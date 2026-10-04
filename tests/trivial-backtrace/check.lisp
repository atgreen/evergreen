(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE")) :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*local-only* t ocicl-runtime:*download* nil)
(asdf:load-system :trivial-backtrace)
(load "checks.lisp")
(uiop:quit 0)
