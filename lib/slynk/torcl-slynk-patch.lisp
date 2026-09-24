;;;; torcl-slynk-patch.lisp --- loaded AFTER slynk. Route single-threaded
;;;; (nil communication-style) connections through the real SLIME message
;;;; protocol (HANDLE-REQUESTS) instead of the raw SIMPLE-REPL, which cannot
;;;; parse framed :emacs-rex messages. The nil style is kept so SETUP-SERVER's
;;;; blocking accept loop (SERVE-LOOP) still drives connection acceptance.
(in-package :slynk)

(defun serve-requests (connection)
  (etypecase connection
    (multithreaded-connection
     (spawn-threads-for-connection connection))
    (singlethreaded-connection
     ;; Drive the real SLIME message loop (HANDLE-REQUESTS) instead of the raw
     ;; SIMPLE-REPL (which cannot parse framed :emacs-rex messages), guaranteeing
     ;; the connection is closed on exit. WITH-TOP-LEVEL-RESTART uses RESTART-CASE
     ;; (which torcl has); we avoid WITH-SIMPLE-RESTART (torcl lacks it).
     (unwind-protect
          (with-connection (connection)
            (tagbody toplevel
               (with-top-level-restart (connection (go toplevel))
                 (handle-requests connection))))
       (close-connection connection nil (safe-backtrace))))))

;;; Match icl's backend pre-configuration: disable SLY-only client auth and the
;;; swank<->slynk retro translation (translating-read, which relies on backquote
;;; the plain reader doesn't need), so a stock SLIME client connects cleanly.
(let ((secret (find-symbol "SLY-SECRET" :slynk)))
  (when secret (setf (symbol-function secret) (lambda () nil))))
(let ((auth (find-symbol "AUTHENTICATE-CLIENT" :slynk)))
  (when auth (setf (symbol-function auth)
                   (lambda (stream) (declare (ignore stream)) nil))))
(let ((x (find-symbol "*TRANSLATING-SWANK-TO-SLYNK*" :slynk-rpc)))
  (when x (setf (symbol-value x) nil)))

(format t "; torcl-slynk-patch loaded~%")
