;;;; -*- indent-tabs-mode: nil -*-
;;;;
;;;; Bordeaux-threads apiv2 backend for the EGCL Common Lisp system.
;;;; See apiv1/impl-egcl.lisp for the rationale: EGCL runs one interpreter
;;;; thread at a time (worker-offload + join), so the lock primitives are correct
;;;; as no-ops. The api-v2 layer wraps these native values in its own THREAD /
;;;; LOCK objects.

(in-package :bordeaux-threads-2)

;;;
;;; Threads
;;;
;;; A EGCL native thread handle is an integer id (EGCL-THREAD:MAKE-THREAD).

(deftype native-thread ()
  'integer)

(defun %make-thread (function name)
  (declare (ignore name))
  (egcl-thread:make-thread function))

(defun %current-thread ()
  (egcl-thread:current-thread))

(defun %thread-name (thread)
  (declare (ignore thread))
  "egcl-thread")

(defun %join-thread (thread)
  (egcl-thread:join-thread thread))

(defun %thread-yield ()
  nil)

;;;
;;; Introspection/debugging
;;;

(defun %all-threads ()
  (list (egcl-thread:current-thread)))

(defun %interrupt-thread (thread function)
  (declare (ignore thread function))
  (error "EGCL bordeaux-threads: INTERRUPT-THREAD is not supported."))

(defun %destroy-thread (thread)
  (declare (ignore thread))
  (error "EGCL bordeaux-threads: DESTROY-THREAD is not supported."))

(defun %thread-alive-p (thread)
  (declare (ignore thread))
  t)

;;;
;;; Non-recursive locks (no-ops; see the file header)
;;;

(defstruct (egcl-lock (:constructor %%make-egcl-lock (name)))
  (name nil))

(deftype native-lock ()
  'egcl-lock)

(defun %make-lock (name)
  (%%make-egcl-lock name))

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
  'egcl-lock)

(defun %make-recursive-lock (name)
  (%%make-egcl-lock name))

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

;; NOTE: egcl is NOT in api-semaphores.lisp's native-semaphore feature list, so
;; that file defines the portable %SEMAPHORE struct + %MAKE-SEMAPHORE /
;; %SIGNAL-SEMAPHORE / %WAIT-ON-SEMAPHORE fallback built on the lock and
;; condition-variable SPI below. The backend must NOT define them here (doing so
;; shadowed the portable struct and broke (%semaphore-lock …)).

;;;
;;; Condition variables
;;;

(defstruct (egcl-condition-variable (:constructor %%make-egcl-condition-variable (name)))
  (name nil))

(deftype condition-variable ()
  'egcl-condition-variable)

(defun %make-condition-variable (name)
  (%%make-egcl-condition-variable name))

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
