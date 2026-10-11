;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(load (merge-pathnames "prelude.lisp" *load-truename*))
(load (merge-pathnames "../native-call-workload.lisp" *load-truename*))

(run-benchmark "native-call-base"
  (lambda () (check-native-call-result (native-call-base-loop 1000000) 1000000)))
