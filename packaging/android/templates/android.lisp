;;;; TorCL Android runtime API v1. The host runs one interpreter per Activity.
(defpackage :torcl-android
  (:use :cl) (:nicknames :android)
  (:shadow :log)
  (:export :running-p :paused-p :poll-touch :poll-key :log :with-c-string :foreign-call))
(in-package :torcl-android)
(defvar *state*)
(defparameter *runtime* (torcl-ffi:load-foreign-library "libtorcl_android.so"))

(defun foreign-call (library name return-type argument-types arguments)
  (let ((fn (torcl-ffi:foreign-symbol-pointer name library)))
    (when (torcl-ffi:null-pointer-p fn) (error "Missing foreign function ~A" name))
    (torcl-ffi:foreign-call fn return-type argument-types arguments)))

(defun runtime-call (name return-type argument-types arguments)
  (foreign-call *runtime* name return-type argument-types arguments))

(defmacro with-c-string ((pointer text) &body body)
  (let ((value (gensym "TEXT")))
    `(let* ((,value ,text) (,pointer (torcl-ffi:foreign-alloc (1+ (length ,value)))))
       (unwind-protect
           (progn
             (loop for ch across ,value for i from 0
                   do (torcl-ffi:mem-set (char-code ch) ,pointer :uchar i))
             (torcl-ffi:mem-set 0 ,pointer :uchar (length ,value))
             ,@body)
         (torcl-ffi:foreign-free ,pointer)))))

(defun log (message)
  "Write an ASCII diagnostic to adb logcat, tag torcl."
  (with-c-string (text message)
    (runtime-call "torcl_android_log" :void '(:pointer) (list text))))

(defun running-p ()
  "False when this surface must be released. Check on every render iteration."
  (plusp (runtime-call "torcl_android_running" :int '(:pointer) (list *state*))))
(defun paused-p ()
  (plusp (runtime-call "torcl_android_paused" :int '(:pointer) (list *state*))))
(defun poll-touch ()
  "Return action, x, y (pixels), or NIL if no event; actions 0=down, 1=up, 2=move."
  (let ((out (torcl-ffi:foreign-alloc 8)))
    (unwind-protect
        (let ((action (runtime-call "torcl_android_touch" :int '(:pointer :pointer) (list *state* out))))
          (when (>= action 0)
            (values action (torcl-ffi:mem-ref out :float) (torcl-ffi:mem-ref out :float 4))))
      (torcl-ffi:foreign-free out))))

(defun poll-key ()
  "Return action, key code, meta state, or NIL if no event.
Actions 0=down, 1=up. A key CODE, not a character: turning one into text needs
the keyboard layout and the meta state, which is the caller's decision.

Key events arrive on the same input queue as touches and are drained by the same
loop; they were simply discarded before runtime API 2."
  (let ((out (torcl-ffi:foreign-alloc 8)))
    (unwind-protect
        (let ((action (runtime-call "torcl_android_key" :int '(:pointer :pointer) (list *state* out))))
          (when (>= action 0)
            (values action (torcl-ffi:mem-ref out :int) (torcl-ffi:mem-ref out :int 4))))
      (torcl-ffi:foreign-free out))))

(defun run (address)
  ;; AT LEAST, not exactly: a runtime that has grown a capability this
  ;; application never uses is not incompatible with it.
  (when (< (runtime-call "torcl_android_api_version" :int nil nil) 2)
    (error "TorCL Android runtime is older than API 2 (no key events)"))
  (let ((*state* (torcl-ffi:make-pointer address)))
    (loop for window = (runtime-call "torcl_android_wait_window" :ulong '(:pointer) (list *state*))
          until (zerop window)
          do (unwind-protect
                 (handler-case (cl-user::android-main (torcl-ffi:make-pointer window))
                   (error (e) (log (format nil "Application error: ~A" e))))
               (runtime-call "torcl_android_finish_window" :void '(:pointer) (list *state*))))))
(in-package :cl-user)
