;;;; EGL from Lisp, on Android, through TorCL's FFI.
;;;;
;;;; Rust hands us one integer: the ANativeWindow* address. Everything else —
;;;; display, config, surface, context, and the clear itself — happens here.

(defun %sym (library name)
  (let ((p (torcl-ffi:foreign-symbol-pointer name library)))
    (when (or (null p) (torcl-ffi:null-pointer-p p))
      (error "EGL: no symbol ~A" name))
    p))

(defun %int-array (values)
  "A C int array, as EGL's attribute lists want."
  (let ((p (torcl-ffi:foreign-alloc (* 4 (length values)))))
    (loop for v in values for i from 0
          do (torcl-ffi:mem-set v p :int (* 4 i)))
    p))

(defun torcl-egl-demo (window-address running-address)
  (let* ((egl (torcl-ffi:load-foreign-library "libEGL.so"))
         (gl (torcl-ffi:load-foreign-library "libGLESv2.so"))
         (android (torcl-ffi:load-foreign-library "libandroid.so"))
         (window (torcl-ffi:make-pointer window-address))
         ;; Rust clears this when Android destroys the surface. Drawing into a
         ;; destroyed surface crashes, so the render loop below is endless only
         ;; in the sense that IT does not choose when to stop.
         (running (torcl-ffi:make-pointer running-address))
         (get-display (%sym egl "eglGetDisplay"))
         (initialize (%sym egl "eglInitialize"))
         (choose-config (%sym egl "eglChooseConfig"))
         (get-config-attrib (%sym egl "eglGetConfigAttrib"))
         (create-window-surface (%sym egl "eglCreateWindowSurface"))
         (create-context (%sym egl "eglCreateContext"))
         (make-current (%sym egl "eglMakeCurrent"))
         (swap-buffers (%sym egl "eglSwapBuffers"))
         (get-error (%sym egl "eglGetError"))
         (set-geometry (%sym android "ANativeWindow_setBuffersGeometry"))
         (clear-color (%sym gl "glClearColor"))
         (clear (%sym gl "glClear"))
         (null-ptr (torcl-ffi:null-pointer)))
    (flet ((call (fn ret types args) (torcl-ffi:foreign-call fn ret types args)))
      ;; EGL_DEFAULT_DISPLAY is NULL.
      (let ((display (call get-display :pointer '(:pointer) (list null-ptr))))
        (when (torcl-ffi:null-pointer-p display) (error "EGL: no display"))
        (let ((major (torcl-ffi:foreign-alloc 4))
              (minor (torcl-ffi:foreign-alloc 4)))
          (call initialize :int '(:pointer :pointer :pointer)
                (list display major minor))
          (format t "EGL ~D.~D~%" (torcl-ffi:mem-ref major :int)
                  (torcl-ffi:mem-ref minor :int)))
        ;; EGL_SURFACE_TYPE/WINDOW_BIT, RENDERABLE_TYPE/ES2_BIT, 8/8/8, NONE
        (let* ((attribs (%int-array (list #x3033 #x0004 #x3040 #x0004
                                          #x3024 8 #x3023 8 #x3022 8 #x3038)))
               (configs (torcl-ffi:foreign-alloc 8))
               (count (torcl-ffi:foreign-alloc 4)))
          (call choose-config :int '(:pointer :pointer :pointer :int :pointer)
                (list display attribs configs 1 count))
          (when (zerop (torcl-ffi:mem-ref count :int)) (error "EGL: no config"))
          (let* ((config (torcl-ffi:mem-ref configs :pointer))
                 (visual (torcl-ffi:foreign-alloc 4)))
            ;; EGL_NATIVE_VISUAL_ID: the window must be reconfigured to it.
            (call get-config-attrib :int '(:pointer :pointer :int :pointer)
                  (list display config #x302E visual))
            (call set-geometry :int '(:pointer :int :int :int)
                  (list window 0 0 (torcl-ffi:mem-ref visual :int)))
            (let* ((surface (call create-window-surface :pointer
                                  '(:pointer :pointer :pointer :pointer)
                                  (list display config window null-ptr)))
                   ;; EGL_CONTEXT_CLIENT_VERSION 2
                   (ctx-attribs (%int-array (list #x3098 2 #x3038)))
                   (context (call create-context :pointer
                                  '(:pointer :pointer :pointer :pointer)
                                  (list display config null-ptr ctx-attribs))))
              (when (torcl-ffi:null-pointer-p surface)
                (error "EGL: no surface (0x~X)" (call get-error :int '() '())))
              (when (torcl-ffi:null-pointer-p context)
                (error "EGL: no context (0x~X)" (call get-error :int '() '())))
              (call make-current :int '(:pointer :pointer :pointer :pointer)
                    (list display surface surface context))
              ;; Render until Android takes the surface away. The colour cycles
              ;; so a screenshot cannot be mistaken for a static clear left by
              ;; something else.
              (loop for frame from 0
                    while (plusp (torcl-ffi:mem-ref running :int))
                    do (let ((phase (/ (mod frame 120) 120.0)))
                         (call clear-color :void '(:float :float :float :float)
                               (list (float phase) (float (- 1.0 phase)) 0.35 1.0))
                         (call clear :void '(:int) '(#x4000))
                         (call swap-buffers :int '(:pointer :pointer)
                               (list display surface))
                         (sleep 0.016)))
              (format t "EGL: surface gone~%"))))))))
