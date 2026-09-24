;;;; -*- indent-tabs-mode: nil -*-
;;;;
;;;; Bordeaux-threads apiv1 backend for the TorCL Common Lisp system.
;;;;
;;;; TorCL exposes OS-backed native threads (TORCL-THREAD:MAKE-THREAD /
;;;; JOIN-THREAD / CURRENT-THREAD). A native-thread handle is an integer id, and
;;;; — like the SBCL backend, whose THREAD type is sb-thread:thread — we use that
;;;; id directly as the bordeaux-threads THREAD (no wrapper). TorCL runs one
;;;; interpreter thread at a time (the worker-offload + join model), so the
;;;; lock/condition/semaphore primitives are correct as no-ops: there is never a
;;;; second interpreter thread mutating shared state to exclude. NOTE: %MAKE-THREAD
;;;; is a SPI symbol shared with the apiv2 backend, so both define it to return
;;;; the raw native id.

(in-package #:bordeaux-threads)

;;;
;;; Threads
;;;


;; Cosmetic thread names, keyed by native id. Weak so finished threads don't
;; pin their names forever (falls back to a strong table on TorCL).
(defvar *thread-names*
  (trivial-garbage:make-weak-hash-table :weakness :key :test 'eql))

(defun %make-thread (function name)
  (let ((id (torcl-thread:make-thread function)))
    (when name (setf (gethash id *thread-names*) name))
    id))

;; Override the portable MAKE-THREAD: its default wraps FUNCTION in
;; BINDING-DEFAULT-SPECIALS, a closure that captures FUNCTION as a lexical, and
;; TorCL cannot yet carry a closure's captured lexical frame to another thread.
;; Hand FUNCTION to the worker directly. A worker function that references only
;; globals and its own parameters runs correctly; one that closes over lexical
;; variables finds them unbound on the worker.
(defun make-thread (function &key name)
  (%make-thread function name))

(defun current-thread ()
  (torcl-thread:current-thread))

(defun threadp (object)
  (integerp object))

(defun thread-name (thread)
  (or (gethash thread *thread-names*) "torcl-thread"))

;;;
;;; Resource contention: locks and recursive locks (no-ops; see the header)
;;;

(defstruct (torcl-lock (:constructor %make-torcl-lock (name)))
  (name nil))


(defun lock-p (object) (typep object 'torcl-lock))
(defun recursive-lock-p (object) (typep object 'torcl-lock))

(defun make-lock (&optional name)
  (%make-torcl-lock (or name "Anonymous lock")))

(defun acquire-lock (lock &optional (wait-p t))
  (declare (ignore lock wait-p))
  t)

(defun release-lock (lock)
  (declare (ignore lock))
  nil)

(defmacro with-lock-held ((place) &body body)
  `(progn ,place ,@body))

(defun make-recursive-lock (&optional name)
  (%make-torcl-lock (or name "Anonymous recursive lock")))

(defmacro with-recursive-lock-held ((place) &body body)
  `(progn ,place ,@body))

;;;
;;; Resource contention: condition variables
;;;

(defstruct (torcl-condition-variable (:constructor %make-torcl-condition-variable (name)))
  (name nil))

(defun make-condition-variable (&key name)
  (%make-torcl-condition-variable (or name "Anonymous condition variable")))

(defun condition-wait (condition-variable lock &key timeout)
  ;; No other interpreter thread can notify us, so a genuine wait would block
  ;; forever. Return NIL immediately; a correct predicate loop re-checks.
  (declare (ignore condition-variable lock timeout))
  nil)

(defun condition-notify (condition-variable)
  (declare (ignore condition-variable))
  nil)

(defun thread-yield ()
  nil)

;;;
;;; Timeouts
;;;

(defmacro with-timeout ((timeout) &body body)
  `(progn ,timeout ,@body))

;;;
;;; Semaphores
;;;

(defstruct (torcl-semaphore (:constructor %make-torcl-semaphore (name count)))
  (name nil)
  (count 0))


(defun make-semaphore (&key name (count 0))
  (%make-torcl-semaphore name count))

(defun signal-semaphore (semaphore &key (count 1))
  (incf (torcl-semaphore-count semaphore) count))

(defun wait-on-semaphore (semaphore &key timeout)
  (declare (ignore timeout))
  (when (plusp (torcl-semaphore-count semaphore))
    (decf (torcl-semaphore-count semaphore))
    t))

;;;
;;; Introspection/debugging
;;;

(defun all-threads ()
  (list (torcl-thread:current-thread)))

(defun interrupt-thread (thread function &rest args)
  (declare (ignore thread function args))
  (error "TorCL bordeaux-threads: INTERRUPT-THREAD is not supported."))

(defun destroy-thread (thread)
  (declare (ignore thread))
  (error "TorCL bordeaux-threads: DESTROY-THREAD is not supported."))

(defun thread-alive-p (thread)
  (declare (ignore thread))
  ;; TorCL exposes no liveness query; approximate as always alive until joined.
  t)

(defun join-thread (thread)
  (torcl-thread:join-thread thread))

(mark-supported)
