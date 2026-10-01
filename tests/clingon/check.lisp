;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*local-only* t ocicl-runtime:*download* nil)
(asdf:load-system :clingon.test)
(setf rove:*enable-colors* nil)
(unless (rove:run-suite :clingon.test)
  (error "Clingon upstream suite failed"))
(format t "CLINGON-SUITE-PASS~%")
(uiop:quit 0)
