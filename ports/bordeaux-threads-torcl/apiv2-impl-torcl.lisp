;;;; -*- indent-tabs-mode: nil -*-
;;;;
;;;; Bordeaux-threads apiv2 backend for the TorCL Common Lisp system.
;;;; See apiv1/impl-torcl.lisp for the rationale: TorCL runs one interpreter
;;;; thread at a time (worker-offload + join), so the lock primitives are correct
;;;; as no-ops. The api-v2 layer wraps these native values in its own THREAD /
;;;; LOCK objects.

(in-package :bordeaux-threads-2)

;;;
;;; Threads
;;;
;;; A TorCL native thread handle is an integer id (TORCL-THREAD:MAKE-THREAD).

(deftype native-thread ()
  'integer)

(defun %make-thread (function name)
  (declare (ignore name))
  (torcl-thread:make-thread function))

(defun %current-thread ()
  (torcl-thread:current-thread))

(defun %thread-name (thread)
  (declare (ignore thread))
  "torcl-thread")

(defun %join-thread (thread)
  (torcl-thread:join-thread thread))

(defun %thread-yield ()
  nil)

;;;
;;; Introspection/debugging
;;;

(defun %all-threads ()
  (list (torcl-thread:current-thread)))

(defun %interrupt-thread (thread function)
  (declare (ignore thread function))
  (error "TorCL bordeaux-threads: INTERRUPT-THREAD is not supported."))

(defun %destroy-thread (thread)
  (declare (ignore thread))
  (error "TorCL bordeaux-threads: DESTROY-THREAD is not supported."))

(defun %thread-alive-p (thread)
  (declare (ignore thread))
  t)

;;;
;;; Non-recursive locks (no-ops; see the file header)
;;;

(defstruct (torcl-lock (:constructor %%make-torcl-lock (name)))
  (name nil))

(deftype native-lock ()
  'torcl-lock)

(defun %make-lock (name)
  (%%make-torcl-lock name))

(defun %acquire-lock (lock waitp timeout)
  (declare (ignore lock waitp timeout))
  t)

(defun %release-lock (lock)
  (declare (ignore lock))
  nil)

(defmacro %with-lock ((place timeout) &body body)
  (declare (ignore timeout))
  `(progn ,place ,@body))

;;;
;;; Recursive locks (same no-op object)
;;;

(deftype native-recursive-lock ()
  'torcl-lock)

(defun %make-recursive-lock (name)
  (%%make-torcl-lock name))

(defun %acquire-recursive-lock (lock waitp timeout)
  (declare (ignore lock waitp timeout))
  t)

(defun %release-recursive-lock (lock)
  (declare (ignore lock))
  nil)

(defmacro %with-recursive-lock ((place timeout) &body body)
  (declare (ignore timeout))
  `(progn ,place ,@body))

;;;
;;; Semaphores
;;;

;; NOTE: torcl is NOT in api-semaphores.lisp's native-semaphore feature list, so
;; that file defines the portable %SEMAPHORE struct + %MAKE-SEMAPHORE /
;; %SIGNAL-SEMAPHORE / %WAIT-ON-SEMAPHORE fallback built on the lock and
;; condition-variable SPI below. The backend must NOT define them here (doing so
;; shadowed the portable struct and broke (%semaphore-lock …)).

;;;
;;; Condition variables
;;;

(defstruct (torcl-condition-variable (:constructor %%make-torcl-condition-variable (name)))
  (name nil))

(deftype condition-variable ()
  'torcl-condition-variable)

(defun %make-condition-variable (name)
  (%%make-torcl-condition-variable name))

(defun %condition-wait (cv lock timeout)
  ;; No other interpreter thread can notify us; return NIL immediately.
  (declare (ignore cv lock timeout))
  nil)

(defun %condition-notify (cv)
  (declare (ignore cv))
  nil)

(defun %condition-broadcast (cv)
  (declare (ignore cv))
  nil)

;;;
;;; Timeouts
;;;

(defmacro with-timeout ((timeout) &body body)
  `(progn ,timeout ,@body))
