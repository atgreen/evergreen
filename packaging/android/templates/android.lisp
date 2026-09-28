;;;; TorCL Android runtime API v1. The host runs one interpreter per Activity.
(defpackage :torcl-android
  (:use :cl) (:nicknames :android)
  (:shadow :log)
  (:export :running-p :paused-p :poll-touch :poll-key :activity :call-on-main
           :log :with-c-string :foreign-call))
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

(defun activity ()
  "This process's ANativeActivity, or a null pointer before one exists.

Its FOURTH pointer field is `clazz`: a global reference to the Java
NativeActivity object itself, and therefore the way in to every platform API
that is only offered in Java -- the Context, the Window, the View hierarchy,
getSystemService. Handing the pointer out rather than wrapping each call keeps
this runtime out of the business of deciding which of those an application may
reach.

Not wrapped here, and deliberately: ANativeActivity_showSoftInput. On Android 16
it leaves mInputShown false and no keyboard appears, whereas calling
InputMethodManager.showSoftInput on the decor view through JNI raises it. A
wrapper that reliably does nothing is worse than no wrapper, because its void
return looks like success."
  (runtime-call "torcl_android_activity" :pointer nil nil))

(defun call-on-main (function arguments &key (result :pointer) promote release)
  "Call FUNCTION -- a C function pointer -- with ARGUMENTS on the Android main
thread, and wait for its answer.

Android's view hierarchy has thread affinity: touch a View from anywhere but the
thread that made it and ViewRootImpl throws CalledFromWrongThreadException. This
interpreter runs on a worker thread, because the main thread belongs to the
platform's message loop, so without this an application could never attach a
View -- and therefore never have a focused editor, an InputConnection, or real
text input.

ARGUMENTS are pointers or integers, at most six, each one word. RESULT says how
to read the returned word: :POINTER, :INT, or :VOID. A Java method returning
FLOAT or DOUBLE cannot come back this way, because its result is not in an
integer register.

FUNCTION is a raw pointer rather than a name on purpose. A caller walking the
JNI table already holds these; asking for a name would mean this runtime
deciding which platform calls an application is allowed to make.

PROMOTE and RELEASE, when given, run before the visit ends: the result becomes
(PROMOTE arg0 result), and (RELEASE arg0 result) disposes of what it replaced.
They are there because a handle the main thread returns may be valid only while
the call that produced it is still on the stack. A JNI local reference is
exactly that -- the main thread reaches us from inside nativePollOnce, so its
local reference table is popped the moment the looper returns to Java, and
promoting one on a later call is not an error but a process abort."
  (let ((count (length arguments))
        (args (torcl-ffi:foreign-alloc 48))
        (out (torcl-ffi:foreign-alloc 8)))
    (unwind-protect
        (progn
          (loop for argument in arguments
                for offset from 0 by 8
                do (torcl-ffi:mem-set (if (integerp argument)
                                          argument
                                          (torcl-ffi:pointer-address argument))
                                      args :ulong offset))
          (let ((status (runtime-call "torcl_android_call_on_main" :int
                                      '(:pointer :pointer :int :pointer :pointer :pointer)
                                      (list function args count
                                            (or promote (torcl-ffi:null-pointer))
                                            (or release (torcl-ffi:null-pointer))
                                            out))))
            (unless (zerop status)
              (error "Main-thread call failed: ~A"
                     (case status
                       (-1 "this Activity has no main-thread gate")
                       (-2 "the main thread did not answer within five seconds")
                       (-3 "too many arguments, or too few")
                       (t status))))
            (ecase result
              (:pointer (torcl-ffi:make-pointer (torcl-ffi:mem-ref out :ulong)))
              (:int (torcl-ffi:mem-ref out :int))
              (:void nil))))
      (torcl-ffi:foreign-free args)
      (torcl-ffi:foreign-free out))))

(defun run (address)
  ;; AT LEAST, not exactly: a runtime that has grown a capability this
  ;; application never uses is not incompatible with it.
  (when (< (runtime-call "torcl_android_api_version" :int nil nil) 3)
    (error "TorCL Android runtime is older than API 3 (no main-thread gate)"))
  (let ((*state* (torcl-ffi:make-pointer address)))
    (loop for window = (runtime-call "torcl_android_wait_window" :ulong '(:pointer) (list *state*))
          until (zerop window)
          do (unwind-protect
                 (handler-case (cl-user::android-main (torcl-ffi:make-pointer window))
                   (error (e) (log (format nil "Application error: ~A" e))))
               (runtime-call "torcl_android_finish_window" :void '(:pointer) (list *state*))))))
(in-package :cl-user)
