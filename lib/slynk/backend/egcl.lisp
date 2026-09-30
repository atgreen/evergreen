;;;; slynk-egcl.lisp --- SLY/Slynk backend for the EGCL Common Lisp system.
;;;; Single-threaded (communication-style NIL). Public Domain.

(defpackage slynk-egcl
  (:use cl slynk-backend))

(in-package slynk-egcl)

;;; ---- Communication style: single-threaded ------------------------------
(defimplementation preferred-communication-style ()
  nil)

;;; ---- TCP sockets (wraps egcl's EGCL::%SOCKET-* primitives) ------------
(defimplementation create-socket (host port &key backlog)
  (declare (ignore host))
  (egcl::%socket-listen "127.0.0.1" port (or backlog 5)))

(defimplementation local-port (socket)
  (egcl::%socket-local-port socket))

(defimplementation close-socket (socket)
  (egcl::%socket-close socket))

(defimplementation accept-connection (socket &key external-format buffering timeout)
  (declare (ignore external-format buffering timeout))
  (egcl::%socket-accept socket))

(defimplementation socket-fd (socket)
  (if (integerp socket) socket (egcl::%socket-fd socket)))

(defimplementation wait-for-input (streams &optional timeout)
  ;; Return the ready streams, or NIL on timeout. TIMEOUT: NIL = block, T =
  ;; poll once, a real = seconds. The nil-style serve loop waits on one stream.
  (let ((ms (cond ((null timeout) nil)
                  ((eq timeout t) 0)
                  (t (max 0 (round (* timeout 1000)))))))
    (if (and streams (null (cdr streams)))
        (if (egcl::%socket-wait-for-input (car streams) ms)
            streams
            nil)
        (remove-if-not (lambda (s) (egcl::%socket-wait-for-input s 0)) streams))))

;;; ---- Streams: use the process's own std streams (icl pipes stdout) ------
(defimplementation make-output-stream (write-string)
  (declare (ignore write-string))
  *standard-output*)

(defimplementation make-input-stream (read-string)
  (declare (ignore read-string))
  *standard-input*)

(defimplementation make-fd-stream (fd external-format)
  (declare (ignore fd external-format))
  ;; Not used in nil communication style.
  (error "make-fd-stream not supported on egcl"))

(defimplementation dup (fd) fd)

;;; ---- Process ------------------------------------------------------------
(defimplementation getpid ()
  (egcl::%getpid))

(defimplementation quit-lisp ()
  (egcl::%exit 0))

(defimplementation lisp-implementation-type-name ()
  "egcl")

;;; ---- Compilation: egcl compiles as it evaluates -----------------------
(defimplementation call-with-compilation-hooks (function)
  (funcall function))

(defimplementation slynk-compile-string (string &key buffer position filename line column policy)
  (declare (ignore buffer position filename line column policy))
  (with-input-from-string (s string)
    (loop for form = (read s nil :eof)
          until (eq form :eof)
          do (eval form)))
  (values t nil nil))

(defimplementation slynk-compile-file (input-file output-file load-p external-format &key policy)
  (declare (ignore output-file external-format policy))
  (when load-p (load input-file))
  (values t nil nil))

;;; ---- MOP: minimal, slynk uses it only lightly --------------------------
(defimplementation gray-package-name ()
  "SLYNK-BACKEND")
