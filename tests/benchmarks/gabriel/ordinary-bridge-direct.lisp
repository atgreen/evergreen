;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(load (merge-pathnames "prelude.lisp" *load-truename*))
(load (merge-pathnames "../ordinary-bridge-workload.lisp" *load-truename*))

(run-benchmark "ordinary-bridge-direct"
  (lambda ()
    (check-ordinary-bridge-result (ordinary-bridge-direct-loop 1000000) 1000000)))
