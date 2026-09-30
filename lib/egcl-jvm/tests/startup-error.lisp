;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(require :asdf)
(asdf:load-asd (truename "lib/egcl-jvm/egcl-jvm.asd"))
(asdf:load-system :egcl-jvm)
(assert (handler-case (progn (egcl-jvm:start-jvm :options '("-XX:EgclInvalidTestOption")) nil)
          (egcl-jvm:jvm-error () t)))
(assert (handler-case (progn (egcl-jvm:start-jvm) nil)
          (egcl-jvm:jvm-error (e) (search "restart is unsupported" (egcl-jvm:error-message e)))))
(format t "JVM-STARTUP-ERROR-PASS~%")
