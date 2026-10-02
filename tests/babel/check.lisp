;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*local-only* t ocicl-runtime:*download* nil)
(asdf:load-system :babel-tests)
(multiple-value-bind (passed result) (babel-tests:run)
  (format t "~%BABEL-STATS ~S~%" (hu.dwim.stefil::extract-test-run-statistics result))
  (unless (and passed (plusp (hu.dwim.stefil::assertion-count-of result)))
    (error "Babel upstream suite failed or executed no assertions")))
(format t "BABEL-SUITE-PASS~%")
(uiop:quit 0)
