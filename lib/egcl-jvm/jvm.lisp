(in-package :egcl-jvm)

(define-condition jvm-error (error)
  ((message :initarg :message :reader error-message))
  (:report (lambda (condition stream) (write-string (error-message condition) stream))))
(define-condition java-error (jvm-error) ())
(define-condition ambiguous-call (java-error) ())
(defconstant +null+ :java-null)
(defstruct callback-lease trampoline token (owners 1))
(defvar *callback-leases* (make-hash-table))
(defvar *reference-lock* (egcl-thread:make-mutex :name "Java reference ownership"))
(defstruct (java-object (:constructor %make-object)) id lease)
(defstruct (jvm-session (:constructor %make-session)))
(defvar *session* nil)
(defvar *backend* nil)
(defvar *symbols* (make-hash-table :test 'equal))
(defvar *lifecycle-lock* (egcl-thread:make-mutex :name "egcl-jvm lifecycle"))

(defun %fail (control &rest arguments)
  (error 'jvm-error :message (apply #'format nil control arguments)))
(defun %raw (name result types &rest arguments)
  (let ((pointer (or (gethash name *symbols*)
                     (setf (gethash name *symbols*)
                           (egcl-ffi:foreign-symbol-pointer name *backend*)))))
    (egcl-ffi:foreign-call pointer result types arguments)))

(defun %read-utf16 (pointer length)
  (coerce
    (loop with i = 0 while (< i length)
          collect (let ((unit (egcl-ffi:mem-ref pointer :ushort (* i 2))))
                    (incf i)
                    (when (<= #xd800 unit #xdbff)
                      (when (>= i length) (%fail "Unpaired Java UTF-16 surrogate"))
                      (let ((low (egcl-ffi:mem-ref pointer :ushort (* i 2))))
                        (unless (<= #xdc00 low #xdfff) (%fail "Unpaired Java UTF-16 surrogate"))
                        (incf i)
                        (setf unit (+ #x10000 (ash (- unit #xd800) 10) (- low #xdc00)))))
                    (when (<= #xdc00 unit #xdfff) (%fail "Unpaired Java UTF-16 surrogate"))
                    (or (code-char unit) (%fail "Unsupported character U+~X" unit))))
    'string))
(defmacro %with-utf16 ((pointer length text) &body body)
  (let ((units (gensym "UNITS")))
    `(let* ((,units (loop for ch across ,text for n = (char-code ch)
                         append (if (> n #xffff)
                                    (list (+ #xd800 (ash (- n #x10000) -10))
                                          (+ #xdc00 (logand (- n #x10000) #x3ff)))
                                    (list n))))
            (,length (length ,units))
            (,pointer (egcl-ffi:foreign-alloc (* 2 (max 1 ,length)))))
       (unwind-protect
           (progn (loop for unit in ,units for i from 0
                        do (egcl-ffi:mem-set unit ,pointer :ushort (* i 2)))
                  ,@body)
         (egcl-ffi:foreign-free ,pointer)))))
(defun %utf8 (text)
  (loop for ch across text for n = (char-code ch)
        append (cond ((zerop n) (%fail "NUL is not allowed in a JVM path or option"))
                     ((< n #x80) (list n))
                     ((< n #x800) (list (+ #xc0 (ash n -6)) (+ #x80 (logand n #x3f))))
                     ((< n #x10000) (list (+ #xe0 (ash n -12)) (+ #x80 (logand (ash n -6) #x3f)) (+ #x80 (logand n #x3f))))
                     (t (list (+ #xf0 (ash n -18)) (+ #x80 (logand (ash n -12) #x3f))
                              (+ #x80 (logand (ash n -6) #x3f)) (+ #x80 (logand n #x3f)))))))
(defmacro %with-c-string ((pointer text) &body body)
  (let ((bytes (gensym "BYTES")))
    `(let* ((,bytes (%utf8 ,text)) (,pointer (egcl-ffi:foreign-alloc (1+ (length ,bytes)))))
       (unwind-protect
           (progn (loop for byte in ,bytes for i from 0 do (egcl-ffi:mem-set byte ,pointer :uchar i))
                  (egcl-ffi:mem-set 0 ,pointer :uchar (length ,bytes)) ,@body)
         (egcl-ffi:foreign-free ,pointer)))))
(defun %check-error ()
  (let ((kind (%raw "tj_error_kind" :int nil)))
    (unless (zerop kind)
      (let* ((length (%raw "tj_error_length" :int nil))
             (buffer (egcl-ffi:foreign-alloc (* 2 (max 1 length)))))
        (unwind-protect
            (progn (%raw "tj_error_copy" :void '(:pointer) buffer)
                   (error (case kind (2 'java-error) (3 'ambiguous-call) (otherwise 'jvm-error))
                          :message (%read-utf16 buffer length)))
          (egcl-ffi:foreign-free buffer))))))
(defun %native (name result types &rest arguments)
  (handler-case
      (let ((value (apply #'%raw name result types arguments)))
        (%check-error) value)
    (egcl-ffi:ffi-error (condition) (%fail "JVM boundary: ~A" condition))))
(defun %running ()
  (unless (and *backend* (= (%raw "tj_state" :int nil) 2)) (%fail "JVM is not running")))
(defun jvm-running-p () (and *backend* (= (%raw "tj_state" :int nil) 2)))

(defun %ensure-backend (java-home)
  (unless *backend*
    ;; The probe and the verdict must stay apart. %FAIL signals JVM-ERROR, a subtype
    ;; of ERROR, so a single HANDLER-CASE spanning both caught the verdict's own
    ;; %FAIL and relabelled it -- which made the specific diagnosis unreachable and
    ;; reported every cause as one generic sentence (bliss-hllzi).
    (let ((version
            (handler-case (egcl::%foreign-library :jvm-runtime-version)
              (error (condition)
                (%fail "This EGCL cannot be asked whether it supports JVM entry: ~
                        reading :JVM-RUNTIME-VERSION signalled ~S. A native Linux ~
                        x86-64 build with the egcl-rt/c-ffi feature is required."
                       (type-of condition))))))
      (unless (eql version 1)
        ;; The reason lives in Rust: *FEATURES* carries neither target_env nor a
        ;; Cargo feature, so Lisp cannot tell musl from a missing c-ffi.
        (%fail "This EGCL cannot host a JVM in process: ~A. (:JVM-RUNTIME-VERSION ~
                reported ~S; 1 is required.)"
               (handler-case (egcl::%foreign-library :jvm-runtime-diagnostic)
                 (error () "this build predates the JVM coexistence diagnostic"))
               version)))
    ;; Native VM state cannot be serialized, including state left after shutdown.
    (egcl::%foreign-library :inhibit-image)
    (let* ((root (asdf:system-source-directory :egcl-jvm))
           (installed (merge-pathnames "libegcl_jvm.so" root))
           (library
             (cond ((probe-file installed) installed)
                   ((probe-file (merge-pathnames "Makefile" root))
                    (uiop:run-program
                     (list "make" "-C" (namestring root)
                           (concatenate 'string "JAVA_HOME=" (namestring java-home)))
                     :output *standard-output* :error-output *error-output*)
                    (merge-pathnames "build/libegcl_jvm.so" root))
                   (t (%fail "Missing installed JVM bridge in ~A; reinstall egcl" root)))))
      (setf *backend* (egcl-ffi:load-foreign-library (namestring library))))))

;; JDK 17+ keeps libjvm.so at lib/server/. The rest are layouts a user really
;; arrives with: Homebrew's openjdk on Linux installs the JDK under libexec/, so a
;; JAVA_HOME set to the documented "JDK root" (its opt/ prefix) needs libexec/
;; prepended, and some redistributed images retain a jre/ subtree (bliss-hllzi).
(defparameter *libjvm-layouts*
  '("lib/server/libjvm.so" "libexec/lib/server/libjvm.so" "jre/lib/server/libjvm.so"))

(defun %find-libjvm (home)
  "Return the first libjvm.so under HOME as a truename, and the paths searched.
Both values matter to the caller: on failure the second is what the error lists,
so a user can see which layouts were considered rather than guessing."
  (let ((tried '()))
    (dolist (layout *libjvm-layouts* (values nil (nreverse tried)))
      (let ((candidate (merge-pathnames layout home)))
        (push (namestring candidate) tried)
        (when (probe-file candidate)
          (return (values (namestring (truename candidate)) (nreverse tried))))))))

(defun %java-home (home)
  (or home (egcl-ext:getenv "JAVA_HOME")
      (let* ((binary (string-trim '(#\Space #\Newline #\Return #\Tab)
                      (uiop:run-program '("which" "java") :output :string)))
             (resolved (string-trim '(#\Space #\Newline #\Return #\Tab)
                        (uiop:run-program (list "readlink" "-f" binary) :output :string))))
        (namestring (truename (merge-pathnames "../" (make-pathname :name nil :type nil :defaults resolved)))))))
(defun start-jvm (&key java-home (classpath nil) (options nil) attach)
  "Start one in-process JVM, or explicitly attach to an existing VM. JVM restart is unsupported."
  (egcl-thread:with-mutex (*lifecycle-lock*)
    (let* ((home (uiop:ensure-directory-pathname (%java-home java-home)))
           (path (format nil "~{~A~^:~}" classpath))
           (library
             (multiple-value-bind (found tried) (%find-libjvm home)
               (or found
                   (%fail "No libjvm.so under ~A. Tried:~{~%  ~A~}~%Set :java-home or ~
                           JAVA_HOME to the directory holding lib/server/libjvm.so."
                          (namestring home) tried)))))
      (%ensure-backend home)
      (dolist (option options)
        (unless (and (stringp option) (not (find #\Newline option))) (%fail "JVM options must be individual strings without newlines")))
      (%with-c-string (lib library)
        (%with-c-string (cp path)
          (%with-c-string (opts (format nil "~{~A~^~%~}" options))
            (%native "tj_start" :int '(:pointer :pointer :pointer :int) lib cp opts (if attach 1 0)))))
      (setf *session* (%make-session)))))
(defun stop-jvm (session &key (timeout 10))
  "Stop an owned JVM after releasing all references/callbacks. Detach from a guest VM."
  (unless (and *session* (eq session *session*)) (%fail "Invalid JVM session"))
  (unless (and (realp timeout) (<= 0 timeout 2147483)) (%fail "Invalid JVM shutdown timeout"))
  (egcl-thread:with-mutex (*lifecycle-lock*)
    (%native "tj_stop" :int '(:int) (ceiling (* timeout 1000))))
  t)
(defun %id (object)
  (unless (and (java-object-p object) (java-object-id object) (plusp (java-object-id object)))
    (%fail "Expected a live Java object"))
  (java-object-id object))
(defun release (object)
  "Release one reference. The last callback owner revokes the Lisp trampoline."
  (unless (java-object-p object) (%fail "Expected a Java object"))
  (labels ((dispose ()
             ;; Recheck under the reference lock: two threads may release this wrapper.
             (when (java-object-id object)
               (let ((lease (java-object-lease object)))
                 (when (and lease (= 1 (callback-lease-owners lease)))
                   (%native "tj_callback_release" :int '(:int64) (callback-lease-token lease))
                   (egcl-ffi:free-callback (callback-lease-trampoline lease))
                   (remhash (callback-lease-token lease) *callback-leases*))
                 (%native "tj_release" :int '(:int64) (java-object-id object))
                 (when lease (decf (callback-lease-owners lease)))
                 (setf (java-object-id object) nil)))))
    (egcl-thread:with-mutex (*reference-lock*) (dispose)))
  nil)
(defmacro with-java-objects (bindings &body body)
  (if (null bindings) `(progn ,@body)
      `(let ((,(caar bindings) ,(cadar bindings)))
         (unwind-protect (with-java-objects ,(cdr bindings) ,@body)
           (when (java-object-p ,(caar bindings)) (release ,(caar bindings)))))))
(defun same-object-p (a b) (not (zerop (%native "tj_same" :int '(:int64 :int64) (%id a) (%id b)))))
(defun %copy-object (object weak promote)
  (labels ((copy-reference ()
             (let* ((lease (java-object-lease object))
                    (id (%native "tj_copy" :int64 '(:int64 :int :int) (%id object) weak promote)))
               (unless (zerop id)
                 (when lease (incf (callback-lease-owners lease)))
                 (%make-object :id id :lease lease)))))
    (egcl-thread:with-mutex (*reference-lock*) (copy-reference))))
(defun retain (object)
  "Create an independent strong reference, sharing ownership of any Lisp callback."
  (%copy-object object 0 0))
(defun weak-reference (object) (%copy-object object 1 0))
(defun promote (object) (%copy-object object 0 1))
(defun %box (value)
  (let ((kind 0) (integer 0) (real 0d0))
    (cond ((eq value +null+))
          ((eq value t) (setf kind 4 integer 1))
          ((null value) (setf kind 4))
          ((integerp value)
           (unless (<= (- (expt 2 63)) value (1- (expt 2 63))) (%fail "Integer does not fit Java long"))
           (setf kind 1 integer value))
          ((floatp value) (setf kind (if (typep value 'single-float) 6 2) real (coerce value 'double-float)))
          ((characterp value) (setf kind 5 integer (char-code value)))
          ((stringp value)
           (return-from %box (%with-utf16 (pointer length value)
             (%native "tj_box" :int64 '(:int :int64 :double :pointer :int) 3 0 0d0 pointer length))))
          (t (%fail "Unsupported Java argument ~S" value)))
    (%native "tj_box" :int64 '(:int :int64 :double :pointer :int) kind integer real (egcl-ffi:null-pointer) 0)))
(defun %decode (id &optional preserve-object)
  (when preserve-object (return-from %decode (%make-object :id id)))
  (let ((kind (%native "tj_kind" :int '(:int64) id)))
    (when (= kind 7)
      (let ((object (%make-object :id id)))
        (egcl-thread:with-mutex (*reference-lock*)
          (let* ((token (%invoke 19 object nil nil nil))
                 (lease (gethash token *callback-leases*)))
            (when lease
              (incf (callback-lease-owners lease))
              (setf (java-object-lease object) lease))))
        (return-from %decode object)))
    (unwind-protect
        (case kind
          (0 +null+)
          (8 nil)
          (1 (%native "tj_integer" :int64 '(:int64) id))
          ((2 6) (let ((n (%native "tj_real" :double '(:int64) id))) (if (= kind 6) (coerce n 'single-float) n)))
          (3 (let* ((length (%native "tj_text" :int '(:int64 :pointer :int) id (egcl-ffi:null-pointer) 0))
                    (buffer (egcl-ffi:foreign-alloc (* 2 (max 1 length)))))
               (unwind-protect
                   (progn (%native "tj_text" :int '(:int64 :pointer :int) id buffer length)
                          (%read-utf16 buffer length))
                 (egcl-ffi:foreign-free buffer))))
          (4 (not (zerop (%native "tj_integer" :int64 '(:int64) id))))
          (5 (or (code-char (%native "tj_integer" :int64 '(:int64) id)) (%fail "Java char is not a Lisp character")))
          (otherwise (%fail "Unknown Java value kind ~D" kind)))
      (%native "tj_release" :int '(:int64) id))))
;;; Java's standard streams are redirected into buffers at startup (Bridge.java),
;;; and drained here at each crossing back into Lisp. The write happens in Lisp
;;; rather than in the bridge because only here does *STANDARD-OUTPUT* mean what the
;;; caller intends -- including a WITH-OUTPUT-TO-STRING in force. Without this,
;;; System.out.println went straight to fd 1: uncapturable, and ordered against
;;; Lisp's own output by flush timing rather than by program order.
;;;
;;; Never signals. It runs in UNWIND-PROTECT cleanup on the way out of every entry
;;; point, so an error here would MASK the Java error being unwound -- replacing a
;;; useful report with a confusing one from the machinery that was trying to print it.
(defun %drain-stream (which stream)
  (let ((id (%native "tj_drain_output" :int64 '(:int) which)))
    (unless (zerop id)
      (unwind-protect
          (let ((length (%native "tj_text" :int '(:int64 :pointer :int) id (egcl-ffi:null-pointer) 0)))
            (when (plusp length)
              (let ((buffer (egcl-ffi:foreign-alloc (* 2 length))))
                (unwind-protect
                    (progn (%native "tj_text" :int '(:int64 :pointer :int) id buffer length)
                           (write-string (%read-utf16 buffer length) stream))
                  (egcl-ffi:foreign-free buffer)))))
        (%native "tj_release" :int '(:int64) id)))))

(defun drain-output ()
  "Write everything Java has printed since the last drain to the Lisp streams."
  (ignore-errors
    (when (jvm-running-p)
      (%drain-stream 0 *standard-output*)
      (%drain-stream 1 *error-output*)))
  (values))

;;; Every entry point drains on the way out, INCLUDING when it signals: whatever Java
;;; printed before throwing is exactly what a reader needs and would otherwise be lost.
(defmacro draining (&body body)
  `(unwind-protect (progn ,@body) (drain-output)))

(defun %invoke (op target name signature arguments)
  (%running)
  (when (and name (not (stringp name))) (%fail "Java method names must be strings"))
  (when (and signature (not (stringp signature))) (%fail "Java signatures must be strings"))
  ;; Every Java entry point funnels through here -- NEW, CALL, CALL-STATIC,
  ;; FIND-JAVA-CLASS, the array operations, and the whole scoped JAVA API on top of
  ;; them -- so draining here covers all of them with one wrapper.
  (draining
   (let ((owned nil))
    (unwind-protect
        (flet ((handle (value)
                 (if (java-object-p value) (%id value)
                     (let ((id (%box value))) (push id owned) id))))
          (let* ((receiver (handle target)) (method (if name (handle name) 0))
                 (descriptor (if signature (handle signature) 0))
                 (args (mapcar #'handle arguments))
                 (buffer (egcl-ffi:foreign-alloc (* 8 (max 1 (length args))))))
            (unwind-protect
                (progn
                  (loop for id in args for i from 0 do (egcl-ffi:mem-set id buffer :int64 (* 8 i)))
                  (let ((value (%decode (%native "tj_call" :int64 '(:int :int64 :int64 :int64 :pointer :int)
                                                op receiver method descriptor buffer (length args)) (member op '(0 8 23)))))
                    (if (and (member op '(1 2)) signature (char= #\V (char signature (1- (length signature))))) nil value)))
              (egcl-ffi:foreign-free buffer))))
      (dolist (id owned) (%native "tj_release" :int '(:int64) id))))))
(defun new (class signature &rest arguments) (%invoke 0 class nil signature arguments))
(defun call (object method signature &rest arguments) (%invoke 1 object method signature arguments))
(defun call-static (class method signature &rest arguments) (%invoke 2 class method signature arguments))
(defun find-java-class (name &optional loader) (%invoke 3 name nil nil (if loader (list loader) nil)))
(defun array-length (array) (%invoke 5 array nil nil nil))
(defun array-ref (array index) (%invoke 6 array nil nil (list index)))
(defun array-set (array index value) (%invoke 7 array nil nil (list index value)))
(defun %dispatch-callback (function method-id args-id context)
  ;; Lisp re-entered FROM Java. Draining on the way in is what lets a long-running
  ;; Java computation's output reach the user before it returns: this is the only
  ;; point at which Lisp runs while the outer call is still in progress.
  (drain-output)
  (handler-case
      (funcall context
        (lambda ()
          (let ((arguments nil))
            (unwind-protect
                (let* ((name (%decode (%native "tj_copy" :int64 '(:int64 :int :int) method-id 0 0)))
                       (array (%make-object :id args-id)))
                  (dotimes (i (array-length array)) (push (array-ref array i) arguments))
                  (let ((value (apply function name (reverse arguments))))
                    (if (java-object-p value)
                        (%native "tj_copy" :int64 '(:int64 :int :int) (%id value) 0 0)
                        (%box value))))
              (dolist (argument arguments)
                (when (java-object-p argument) (release argument)))))))
    (error (condition)
      (%with-utf16 (pointer length (format nil "Lisp callback: ~A" condition))
        (%raw "tj_callback_error" :void '(:pointer :int) pointer length))
      0)))
(defun %implement (interface function &key signed (context #'funcall))
  "Implement a public Java interface. FUNCTION receives method name followed by converted arguments."
  (%running)
  (unless (functionp function) (%fail "Expected a Lisp function"))
  (let ((callback (egcl-ffi:make-callback
                   (lambda (name args) (%dispatch-callback function name args context)) :int64 '(:int64 :int64)))
        (id nil) (result nil))
    (unwind-protect
        (progn
          (setf id (%native "tj_callback" :int64 '(:pointer) (egcl-ffi:callback-pointer callback)))
          (setf result (%invoke (if signed 12 4) interface nil nil (list id)))
          (egcl-thread:with-mutex (*reference-lock*)
            (setf (java-object-lease result) (make-callback-lease :trampoline callback :token id)
                  (gethash id *callback-leases*) (java-object-lease result)))
          result)
      (unless result
        (when id (%native "tj_callback_release" :int '(:int64) id))
        (egcl-ffi:free-callback callback)))))

(defun implement (interface function)
  "Implement an interface; FUNCTION receives a method name followed by arguments."
  (%implement interface function))
