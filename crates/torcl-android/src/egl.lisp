;;;; EGL and GLES 2.0, from Lisp, on Android.
;;;;
;;;; Rust hands over two integers — the ANativeWindow* and the address of a flag
;;;; it clears when the surface dies. Everything else, including compiling the
;;;; shader that scene.lisp generates, happens here through TorCL's FFI.

;;; ── C plumbing ────────────────────────────────────────────────────────

(defun %sym (library name)
  (let ((p (torcl-ffi:foreign-symbol-pointer name library)))
    (when (or (null p) (torcl-ffi:null-pointer-p p))
      (error "no symbol ~A" name))
    p))

(defun %int-array (values)
  (let ((p (torcl-ffi:foreign-alloc (* 4 (max 1 (length values))))))
    (loop for v in values for i from 0
          do (torcl-ffi:mem-set v p :int (* 4 i)))
    p))

(defun %float-array (values)
  (let ((p (torcl-ffi:foreign-alloc (* 4 (max 1 (length values))))))
    (loop for v in values for i from 0
          do (torcl-ffi:mem-set (float v 1.0) p :float (* 4 i)))
    p))

(defun %c-string (text)
  "TEXT as a NUL-terminated C string in foreign memory."
  (let ((p (torcl-ffi:foreign-alloc (1+ (length text)))))
    (loop for ch across text for i from 0
          do (torcl-ffi:mem-set (char-code ch) p :uchar i))
    (torcl-ffi:mem-set 0 p :uchar (length text))
    p))

(defun %read-c-string (pointer limit)
  (with-output-to-string (out)
    (loop for i from 0 below limit
          for byte = (torcl-ffi:mem-ref pointer :uchar i)
          until (zerop byte)
          do (write-char (code-char byte) out))))

;;; ── GL, bound once into a hash table so calls read like calls ─────────

(defparameter *gl* (make-hash-table :test #'equal))

(defun gl (name &rest args)
  "Call GL function NAME. Each entry is (pointer return-type arg-types)."
  (let ((entry (gethash name *gl*)))
    (unless entry (error "GL function not bound: ~A" name))
    (torcl-ffi:foreign-call (first entry) (second entry) (third entry) args)))

(defun bind-gl (library name return-type arg-types)
  (setf (gethash name *gl*) (list (%sym library name) return-type arg-types)))

(defun compile-shader (kind source)
  "Compile SOURCE, reporting the driver's own error text on failure."
  (let ((shader (gl "glCreateShader" kind))
        (strings (torcl-ffi:foreign-alloc 8)))
    ;; Store the pointer AS a pointer. Writing its address as :long asks for a
    ;; signed 64-bit store, and Android's scudo allocator returns addresses with
    ;; bit 63 set (0xB400_...), so a real device rejects it as out of range while
    ;; an emulator — whose addresses are low — accepts it happily.
    (torcl-ffi:mem-set (%c-string source) strings :pointer 0)
    (gl "glShaderSource" shader 1 strings (torcl-ffi:null-pointer))
    (gl "glCompileShader" shader)
    (let ((status (torcl-ffi:foreign-alloc 4)))
      (gl "glGetShaderiv" shader #x8B81 status)   ; GL_COMPILE_STATUS
      (when (zerop (torcl-ffi:mem-ref status :int))
        (let ((log (torcl-ffi:foreign-alloc 2048)))
          (gl "glGetShaderInfoLog" shader 2048 (torcl-ffi:null-pointer) log)
          (error "shader compile failed:~%~A" (%read-c-string log 2048)))))
    shader))

(defun link-program (vertex-source fragment-source)
  (let ((program (gl "glCreateProgram"))
        (vs (compile-shader #x8B31 vertex-source))     ; GL_VERTEX_SHADER
        (fs (compile-shader #x8B30 fragment-source)))  ; GL_FRAGMENT_SHADER
    (gl "glAttachShader" program vs)
    (gl "glAttachShader" program fs)
    (gl "glLinkProgram" program)
    (let ((status (torcl-ffi:foreign-alloc 4)))
      (gl "glGetProgramiv" program #x8B82 status)      ; GL_LINK_STATUS
      (when (zerop (torcl-ffi:mem-ref status :int))
        (let ((log (torcl-ffi:foreign-alloc 2048)))
          (gl "glGetProgramInfoLog" program 2048 (torcl-ffi:null-pointer) log)
          (error "program link failed:~%~A" (%read-c-string log 2048)))))
    program))

(defparameter *vertex-shader* "
attribute vec2 a_pos;
void main(){ gl_Position = vec4(a_pos, 0.0, 1.0); }
")

;;; ── the demo ──────────────────────────────────────────────────────────

(defun torcl-egl-demo (window-address running-address)
  (let* ((egl (torcl-ffi:load-foreign-library "libEGL.so"))
         (gles (torcl-ffi:load-foreign-library "libGLESv2.so"))
         (android (torcl-ffi:load-foreign-library "libandroid.so"))
         (window (torcl-ffi:make-pointer window-address))
         ;; Rust clears this when Android destroys the surface. Drawing into a
         ;; destroyed surface crashes, so the render loop below is endless only
         ;; in the sense that IT does not choose when to stop.
         (running (torcl-ffi:make-pointer running-address))
         (null-ptr (torcl-ffi:null-pointer))
         (get-display (%sym egl "eglGetDisplay"))
         (initialize (%sym egl "eglInitialize"))
         (choose-config (%sym egl "eglChooseConfig"))
         (get-config-attrib (%sym egl "eglGetConfigAttrib"))
         (create-window-surface (%sym egl "eglCreateWindowSurface"))
         (create-context (%sym egl "eglCreateContext"))
         (make-current (%sym egl "eglMakeCurrent"))
         (swap-buffers (%sym egl "eglSwapBuffers"))
         (query-surface (%sym egl "eglQuerySurface"))
         (set-geometry (%sym android "ANativeWindow_setBuffersGeometry")))
    (flet ((egl-call (fn ret types args) (torcl-ffi:foreign-call fn ret types args)))
      ;; GL entry points, declared once.
      (dolist (spec '(("glCreateShader" :int (:int))
                      ("glShaderSource" :void (:int :int :pointer :pointer))
                      ("glCompileShader" :void (:int))
                      ("glGetShaderiv" :void (:int :int :pointer))
                      ("glGetShaderInfoLog" :void (:int :int :pointer :pointer))
                      ("glCreateProgram" :int ())
                      ("glAttachShader" :void (:int :int))
                      ("glLinkProgram" :void (:int))
                      ("glGetProgramiv" :void (:int :int :pointer))
                      ("glGetProgramInfoLog" :void (:int :int :pointer :pointer))
                      ("glUseProgram" :void (:int))
                      ("glGenBuffers" :void (:int :pointer))
                      ("glBindBuffer" :void (:int :int))
                      ;; GLsizeiptr is signed, but every size we pass is small;
                      ;; :long is correct here, unlike for an address.
                      ("glBufferData" :void (:int :long :pointer :int))
                      ("glGetAttribLocation" :int (:int :pointer))
                      ("glGetUniformLocation" :int (:int :pointer))
                      ("glEnableVertexAttribArray" :void (:int))
                      ("glVertexAttribPointer" :void (:int :int :int :int :int :pointer))
                      ("glUniform1f" :void (:int :float))
                      ("glUniform3f" :void (:int :float :float :float))
                      ("glUniform2f" :void (:int :float :float))
                      ("glViewport" :void (:int :int :int :int))
                      ("glClearColor" :void (:float :float :float :float))
                      ("glClear" :void (:int))
                      ("glDrawArrays" :void (:int :int :int))))
        (bind-gl gles (first spec) (second spec) (third spec)))

      (let ((display (egl-call get-display :pointer '(:pointer) (list null-ptr))))
        (when (torcl-ffi:null-pointer-p display) (error "no EGL display"))
        (egl-call initialize :int '(:pointer :pointer :pointer)
                  (list display (torcl-ffi:foreign-alloc 4) (torcl-ffi:foreign-alloc 4)))
        (let* ((attribs (%int-array (list #x3033 #x0004 #x3040 #x0004
                                          #x3024 8 #x3023 8 #x3022 8 #x3025 16 #x3038)))
               (configs (torcl-ffi:foreign-alloc 8))
               (count (torcl-ffi:foreign-alloc 4)))
          (egl-call choose-config :int '(:pointer :pointer :pointer :int :pointer)
                    (list display attribs configs 1 count))
          (when (zerop (torcl-ffi:mem-ref count :int)) (error "no EGL config"))
          (let* ((config (torcl-ffi:mem-ref configs :pointer))
                 (visual (torcl-ffi:foreign-alloc 4)))
            (egl-call get-config-attrib :int '(:pointer :pointer :int :pointer)
                      (list display config #x302E visual))
            ;; Render at half resolution and let the compositor scale up. A
            ;; raymarcher costs per PIXEL, and 1080x2400 of 160-step marching is
            ;; what a phone GPU — never mind an emulator — chokes on. Games do
            ;; exactly this.
            (egl-call set-geometry :int '(:pointer :int :int :int)
                      (list window 540 1200 (torcl-ffi:mem-ref visual :int)))
            (let* ((surface (egl-call create-window-surface :pointer
                                      '(:pointer :pointer :pointer :pointer)
                                      (list display config window null-ptr)))
                   (context (egl-call create-context :pointer
                                      '(:pointer :pointer :pointer :pointer)
                                      (list display config null-ptr
                                            (%int-array (list #x3098 2 #x3038))))))
              (when (torcl-ffi:null-pointer-p surface) (error "no EGL surface"))
              (when (torcl-ffi:null-pointer-p context) (error "no EGL context"))
              (egl-call make-current :int '(:pointer :pointer :pointer :pointer)
                        (list display surface surface context))

              ;; The scene is a Lisp list; this is where it becomes a shader.
              (let* ((fragment (sdf-fragment-shader *scene*))
                     (program (link-program *vertex-shader* fragment))
                     (width (torcl-ffi:foreign-alloc 4))
                     (height (torcl-ffi:foreign-alloc 4)))
                (format t "shader: ~D chars from a ~D-node scene~%"
                        (length fragment) (length *scene*))
                (egl-call query-surface :int '(:pointer :pointer :int :pointer)
                          (list display surface #x3057 width))   ; EGL_WIDTH
                (egl-call query-surface :int '(:pointer :pointer :int :pointer)
                          (list display surface #x3056 height))  ; EGL_HEIGHT
                (let ((w (torcl-ffi:mem-ref width :int))
                      (h (torcl-ffi:mem-ref height :int))
                      (buffer (torcl-ffi:foreign-alloc 4)))
                  ;; One full-screen triangle; the fragment shader does the rest.
                  (gl "glGenBuffers" 1 buffer)
                  (gl "glBindBuffer" #x8892 (torcl-ffi:mem-ref buffer :int)) ; ARRAY_BUFFER
                  (gl "glBufferData" #x8892 24
                      (%float-array '(-1.0 -1.0  3.0 -1.0  -1.0 3.0)) #x88E4) ; STATIC_DRAW
                  (gl "glUseProgram" program)
                  (let* ((a-pos (gl "glGetAttribLocation" program (%c-string "a_pos")))
                         (u-time (gl "glGetUniformLocation" program (%c-string "u_time")))
                         (u-res (gl "glGetUniformLocation" program (%c-string "u_res")))
                         (u-ro (gl "glGetUniformLocation" program (%c-string "u_ro")))
                         (u-ta (gl "glGetUniformLocation" program (%c-string "u_ta")))
                         ;; One location per symbol the scene used. The scene
                         ;; decided which values are live; this just binds them.
                         (scene-locs
                           (mapcar (lambda (sym)
                                     (cons sym
                                           (gl "glGetUniformLocation" program
                                               (%c-string (format nil "u_~A"
                                                                  (uniform-name sym))))))
                                   *sdf-uniforms*)))
                    (gl "glEnableVertexAttribArray" a-pos)
                    (gl "glVertexAttribPointer" a-pos 2 #x1406 0 0 null-ptr) ; GL_FLOAT
                    (gl "glViewport" 0 0 w h)
                    (format t "raymarching ~Dx~D~%" w h)
                    (format t "lisp drives ~D scene uniform~:P: ~{~A ~}~%"
                            (length *sdf-uniforms*) *sdf-uniforms*)
                    ;; THE SIMULATION LOOP. The shader has no clock for any of
                    ;; this: stop running these forms and the world stops.
                    (loop with start = (get-internal-real-time)
                          while (plusp (torcl-ffi:mem-ref running :int))
                          for seconds = (/ (- (get-internal-real-time) start)
                                           internal-time-units-per-second)
                          do (multiple-value-bind (eye target) (camera-at seconds)
                               (gl "glUniform3f" u-ro
                                   (float (first eye) 1.0) (float (second eye) 1.0)
                                   (float (third eye) 1.0))
                               (gl "glUniform3f" u-ta
                                   (float (first target) 1.0) (float (second target) 1.0)
                                   (float (third target) 1.0)))
                             (dolist (pair (scene-uniforms seconds))
                               (let ((loc (cdr (assoc (car pair) scene-locs))))
                                 (when (and loc (>= loc 0))
                                   (gl "glUniform1f" loc (float (cdr pair) 1.0)))))
                             (gl "glUniform1f" u-time (float seconds 1.0))
                             (gl "glUniform2f" u-res (float w 1.0) (float h 1.0))
                             (gl "glClear" #x4000)
                             (gl "glDrawArrays" #x0004 0 3)    ; GL_TRIANGLES
                             (egl-call swap-buffers :int '(:pointer :pointer)
                                       (list display surface)))
                    (format t "surface gone~%")))))))))))
