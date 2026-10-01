;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(load "scene.lisp")
(load "egl.lisp")

(defun android-main (window)
  (egcl-egl-demo window))
