;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(defclass eof-octets (egcl-gray-streams:fundamental-binary-input-stream) ())
(defmethod egcl-gray-streams:stream-read-byte ((s eof-octets)) :eof)
(let ((s (make-instance 'eof-octets)))
  (assert (null (read-byte s nil)))
  (assert (eq :finished (read-byte s nil :finished)))
  (assert (eq :finished (funcall #'read-byte s nil :finished)))
  (assert (handler-case (progn (read-byte s) nil) (end-of-file () t))))
(format t "GRAY-EOF-OK~%")
