;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; EGCL Android runtime API v1. The host runs one interpreter per Activity.
(defpackage :egcl-android
  (:use :cl) (:nicknames :android)
  (:shadow :log)
  (:export :running-p :paused-p :poll-touch :poll-key :activity :call-on-main
           :save-state :saved-state
           :log :with-c-string :foreign-call))
(in-package :egcl-android)
(defvar *state*)
(defparameter *runtime* (egcl-ffi:load-foreign-library "libegcl_android.so"))

(defun foreign-call (library name return-type argument-types arguments)
  (let ((fn (egcl-ffi:foreign-symbol-pointer name library)))
    (when (egcl-ffi:null-pointer-p fn) (error "Missing foreign function ~A" name))
    (egcl-ffi:foreign-call fn return-type argument-types arguments)))

(defun runtime-call (name return-type argument-types arguments)
  (foreign-call *runtime* name return-type argument-types arguments))

(defmacro with-c-string ((pointer text) &body body)
  (let ((value (gensym "TEXT")))
    `(let* ((,value ,text) (,pointer (egcl-ffi:foreign-alloc (1+ (length ,value)))))
       (unwind-protect
           (progn
             (loop for ch across ,value for i from 0
                   do (egcl-ffi:mem-set (char-code ch) ,pointer :uchar i))
             (egcl-ffi:mem-set 0 ,pointer :uchar (length ,value))
             ,@body)
         (egcl-ffi:foreign-free ,pointer)))))

(defun log (message)
  "Write an ASCII diagnostic to adb logcat, tag egcl."
  (with-c-string (text message)
    (runtime-call "egcl_android_log" :void '(:pointer) (list text))))

(defun running-p ()
  "False when this surface must be released. Check on every render iteration."
  (plusp (runtime-call "egcl_android_running" :int '(:pointer) (list *state*))))
(defun paused-p ()
  (plusp (runtime-call "egcl_android_paused" :int '(:pointer) (list *state*))))
(defun poll-touch ()
  "Return action, x, y (pixels), or NIL if no event; actions 0=down, 1=up, 2=move."
  (let ((out (egcl-ffi:foreign-alloc 8)))
    (unwind-protect
        (let ((action (runtime-call "egcl_android_touch" :int '(:pointer :pointer) (list *state* out))))
          (when (>= action 0)
            (values action (egcl-ffi:mem-ref out :float) (egcl-ffi:mem-ref out :float 4))))
      (egcl-ffi:foreign-free out))))

(defun poll-key ()
  "Return action, key code, meta state, or NIL if no event.
Actions 0=down, 1=up. A key CODE, not a character: turning one into text needs
the keyboard layout and the meta state, which is the caller's decision.

Key events arrive on the same input queue as touches and are drained by the same
loop; they were simply discarded before runtime API 2."
  (let ((out (egcl-ffi:foreign-alloc 8)))
    (unwind-protect
        (let ((action (runtime-call "egcl_android_key" :int '(:pointer :pointer) (list *state* out))))
          (when (>= action 0)
            (values action (egcl-ffi:mem-ref out :int) (egcl-ffi:mem-ref out :int 4))))
      (egcl-ffi:foreign-free out))))

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
  (runtime-call "egcl_android_activity" :pointer nil nil))

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
        (args (egcl-ffi:foreign-alloc 48))
        (out (egcl-ffi:foreign-alloc 8)))
    (unwind-protect
        (progn
          (loop for argument in arguments
                for offset from 0 by 8
                do (egcl-ffi:mem-set (if (integerp argument)
                                          argument
                                          (egcl-ffi:pointer-address argument))
                                      args :ulong offset))
          (let ((status (runtime-call "egcl_android_call_on_main" :int
                                      '(:pointer :pointer :int :pointer :pointer :pointer)
                                      (list function args count
                                            (or promote (egcl-ffi:null-pointer))
                                            (or release (egcl-ffi:null-pointer))
                                            out))))
            (unless (zerop status)
              (error "Main-thread call failed: ~A"
                     (case status
                       (-1 "this Activity has no main-thread gate")
                       (-2 "the main thread did not answer within five seconds")
                       (-3 "too many arguments, or too few")
                       (t status))))
            (ecase result
              (:pointer (egcl-ffi:make-pointer (egcl-ffi:mem-ref out :ulong)))
              (:int (egcl-ffi:mem-ref out :int))
              (:void nil))))
      (egcl-ffi:foreign-free args)
      (egcl-ffi:foreign-free out))))

(defun %utf8-bytes (text)
  "TEXT as a list of UTF-8 bytes. Saved state crosses into C as bytes, and a
LENGTH is not a character count once anyone types a letter with an accent."
  (let ((bytes '()))
    (loop for ch across text
          for code = (char-code ch)
          do (cond ((< code #x80) (push code bytes))
                   ((< code #x800)
                    (push (logior #xc0 (ash code -6)) bytes)
                    (push (logior #x80 (logand code #x3f)) bytes))
                   ((< code #x10000)
                    (push (logior #xe0 (ash code -12)) bytes)
                    (push (logior #x80 (logand (ash code -6) #x3f)) bytes)
                    (push (logior #x80 (logand code #x3f)) bytes))
                   (t
                    (push (logior #xf0 (ash code -18)) bytes)
                    (push (logior #x80 (logand (ash code -12) #x3f)) bytes)
                    (push (logior #x80 (logand (ash code -6) #x3f)) bytes)
                    (push (logior #x80 (logand code #x3f)) bytes))))
    (nreverse bytes)))

(defun %utf8-string (bytes)
  "BYTES, a vector of octets, as a string. A malformed sequence yields the
replacement character rather than an error: this data came back from the
platform, and refusing to start because of it would be worse than a wrong
glyph."
  (let ((out (make-string-output-stream)) (i 0) (n (length bytes)))
    (loop while (< i n)
          do (let* ((b (aref bytes i))
                    (extra (cond ((< b #x80) 0) ((= (logand b #xe0) #xc0) 1)
                                 ((= (logand b #xf0) #xe0) 2)
                                 ((= (logand b #xf8) #xf0) 3) (t -1)))
                    (code (cond ((< b #x80) b) ((= extra 1) (logand b #x1f))
                                ((= extra 2) (logand b #x0f))
                                ((= extra 3) (logand b #x07)) (t 0))))
               (incf i)
               (if (or (minusp extra) (> (+ i extra) n))
                   (write-char (code-char #xfffd) out)
                   (progn
                     (dotimes (k extra)
                       (setf code (logior (ash code 6) (logand (aref bytes i) #x3f)))
                       (incf i))
                     (write-char (code-char code) out)))))
    (get-output-stream-string out)))

(defun save-state (text)
  "Keep TEXT for the next instance of this Activity.

Android destroys an Activity whenever it likes -- a rotation, a configuration
change, or reclaiming memory from a backgrounded app -- and recreates it later
with whatever was handed to onSaveInstanceState. Everything else about the
process, this interpreter included, is gone.

PUSHED, not pulled: onSaveInstanceState arrives on the main thread at a moment
Android chooses, and this interpreter may be anywhere at the time -- mid-frame,
inside a JNI call, or waiting on the main-thread gate. Being asked then would
deadlock or miss the deadline, so the answer is kept ready instead. Call this
whenever the state worth keeping changes; it is a memcpy, not a write to disk.

Small. The whole saved state of every Activity in the system shares one Binder
transaction, and Android kills an app that hands over too much."
  (let* ((bytes (%utf8-bytes text))
         (count (length bytes))
         (buffer (egcl-ffi:foreign-alloc (max 1 count))))
    (unwind-protect
        (progn
          (loop for byte in bytes for i from 0
                do (egcl-ffi:mem-set byte buffer :uchar i))
          (runtime-call "egcl_android_set_saved_state" :void '(:pointer :pointer :ulong)
                        (list *state* buffer count)))
      (egcl-ffi:foreign-free buffer))
    text))

(defun saved-state ()
  "What the PREVIOUS instance of this Activity saved, or NIL if there was none.

NIL on a genuine cold start, and NIL is also what a fresh install gives, so an
application must have a sensible answer for it rather than treating it as an
error."
  (let ((count (runtime-call "egcl_android_saved_state_size" :ulong '(:pointer) (list *state*))))
    (when (plusp count)
      (let ((buffer (egcl-ffi:foreign-alloc count)))
        (unwind-protect
            (let ((got (runtime-call "egcl_android_saved_state" :ulong
                                     '(:pointer :pointer :ulong)
                                     (list *state* buffer count))))
              (let ((bytes (make-array got :element-type '(unsigned-byte 8))))
                (dotimes (i got) (setf (aref bytes i) (egcl-ffi:mem-ref buffer :uchar i)))
                (%utf8-string bytes)))
          (egcl-ffi:foreign-free buffer))))))

(defun run (address)
  ;; AT LEAST, not exactly: a runtime that has grown a capability this
  ;; application never uses is not incompatible with it.
  (when (< (runtime-call "egcl_android_api_version" :int nil nil) 3)
    (error "EGCL Android runtime is older than API 3 (no main-thread gate)"))
  (let ((*state* (egcl-ffi:make-pointer address)))
    (loop for window = (runtime-call "egcl_android_wait_window" :ulong '(:pointer) (list *state*))
          until (zerop window)
          do (unwind-protect
                 (handler-case (cl-user::android-main (egcl-ffi:make-pointer window))
                   (error (e) (log (format nil "Application error: ~A" e))))
               (runtime-call "egcl_android_finish_window" :void '(:pointer) (list *state*))))))
(in-package :cl-user)
