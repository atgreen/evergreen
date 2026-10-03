;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; Load the APK builder and the dependencies ocicl.csv pins.
(require :asdf)
(defparameter *apk-source* (uiop:pathname-directory-pathname *load-truename*))
(setf *default-pathname-defaults* *apk-source*)

(defun apk-csv-fields (line)
  "LINE split on commas, each field trimmed."
  (loop with start = 0
        for comma = (position #\, line :start start)
        collect (string-trim " " (subseq line start comma))
        while comma do (setf start (1+ comma))))

(defun apk-pinned-directories ()
  "The one directory per dependency that ocicl.csv pins.

Registering exactly these replaces ocicl-runtime, which this used to load from
the user's home. The tree under `ocicl/' is vendored and version-pinned, so
there is nothing to resolve and nothing to fetch -- and the builder then works
from an installed RPM, where there is no ocicl at all."
  (with-open-file (stream (merge-pathnames "ocicl.csv" *apk-source*)
                          :if-does-not-exist nil)
    (unless stream
      (error "No ocicl.csv beside ~A: the pinned dependencies are unknown."
             *apk-source*))
    (loop for line = (read-line stream nil)
          while line
          for asd = (third (apk-csv-fields line))
          for slash = (and asd (position #\/ asd))
          when slash
            collect (merge-pathnames
                     (make-pathname :directory
                                    (list :relative "ocicl" (subseq asd 0 slash)))
                     *apk-source*))))

(let ((pinned (apk-pinned-directories)))
  (dolist (directory pinned)
    (unless (probe-file directory)
      (error "Missing vendored dependency ~A. Run `ocicl install' in ~A."
             directory *apk-source*)))
  (asdf:initialize-source-registry
   `(:source-registry :ignore-inherited-configuration
                      ,@(mapcar (lambda (directory) `(:directory ,directory)) pinned))))
(asdf:load-asd (merge-pathnames "egcl-apk.asd" *apk-source*))
(asdf:load-system :egcl-apk)
