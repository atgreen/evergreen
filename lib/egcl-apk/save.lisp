;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;;
;;; Save an image with ASDF, ocicl, Ironclad and the APK builder already
;;; loaded. Building one APK otherwise spends about 9.3 s loading that world
;;; from source, which was most of a 21 s build once hashing, CRC-32 and byte
;;; reads became native; restoring the image instead costs about 0.3 s.
;;;
;;;   EGCL_APK_IMAGE=/path/to/apk.core \
;;;     egcl --no-init --load lib/asdf.lisp --load lib/egcl-apk/save.lisp
;;;
;;; The image is tied to the exact egcl binary that wrote it -- a different
;;; build is refused with "runtime source mismatch" -- so rebuild it whenever
;;; egcl is rebuilt. `scripts/egcl-apk' uses it when EGCL_APK_IMAGE names one.
(load (merge-pathnames "load.lisp" *load-truename*))
(asdf:load-asd (merge-pathnames "egcl-apk-asdf.asd" *load-truename*))
(asdf:load-system :egcl-apk-asdf)
(let ((output (or (uiop:getenv "EGCL_APK_IMAGE")
                  (namestring (merge-pathnames "apk.core" *load-truename*)))))
  (format t ";; saving ~A~%" output)
  (force-output)
  (egcl-ext:save-lisp-and-die output))
