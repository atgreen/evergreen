;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defun android-main (window)
  (declare (ignore window))
  (android:log "Hello from EGCL on Android")
  (loop while (android:running-p)
        do (multiple-value-bind (action x y) (android:poll-touch)
             (when action (android:log (format nil "Touch ~A at ~A,~A" action x y))))
           (sleep 0.02)))
