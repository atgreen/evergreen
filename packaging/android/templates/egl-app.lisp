;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(load "egl.lisp")

(defun android-main (window)
  (egcl-egl:with-window (window)
    (android:log "EGL ready; touch the screen to change color")
    (let ((red 0.15) (green 0.35) (blue 0.75))
      (loop while (android:running-p)
            do (if (android:paused-p)
                   (sleep 0.02)
                   (progn
                     (multiple-value-bind (action x y) (android:poll-touch)
                       (when action
                         (setf red (/ (mod (truncate x) 500) 500.0)
                               green (/ (mod (truncate y) 500) 500.0))
                         (when (= action 0) (android:log "Touch received"))))
                     (egcl-egl:clear red green blue)
                     (egcl-egl:swap)))))))
