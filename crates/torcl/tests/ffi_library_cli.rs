#![cfg(target_os = "linux")]
use std::process::Command;

#[test]
fn public_library_objects_support_lookup_call_close_and_destructors() {
    let dir = std::env::temp_dir().join(format!("torcl-library-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("library.c");
    let library = dir.join("library.so");
    std::fs::write(
        &source,
        r#"
        #include <stdarg.h>
        static int *closed;
        void torcl_fixture_set_closed(int *p) { closed = p; }
        int torcl_fixture_library_answer(void) { return 42; }
        int torcl_fixture_sum(int count, ...) {
            va_list args; va_start(args, count);
            int result = 0;
            for (int i = 0; i < count; i++) result += va_arg(args, int);
            va_end(args); return result;
        }
        __attribute__((destructor)) static void finish(void) { if (closed) *closed = 99; }
    "#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    let program = format!(
        r#"
      (let* ((library (torcl-ffi:load-foreign-library {:?}))
             (marker (torcl-ffi:foreign-alloc 4)))
        (assert (typep library 'torcl-ffi:foreign-library))
        (assert (not (torcl-ffi:pointerp library)))
        (torcl-ffi:foreign-call (torcl-ffi:foreign-symbol-pointer "torcl_fixture_set_closed" library)
                                :void '(:pointer) (list marker))
        (assert (= 42 (torcl-ffi:foreign-call
                        (torcl-ffi:foreign-symbol-pointer "torcl_fixture_library_answer") :int nil nil)))
        (assert (= 24 (funcall #'torcl-ffi:foreign-call
                         (torcl-ffi:foreign-symbol-pointer "torcl_fixture_sum" library)
                         :int '(:int :char :short) '(2 -7 31) 1)))
        (torcl-ffi:close-foreign-library library)
        (assert (= 99 (torcl-ffi:mem-ref marker :int)))
        (assert (handler-case (progn (torcl-ffi:foreign-symbol-pointer "missing" library) nil)
                  (torcl-ffi:ffi-error () t)))
        (assert (handler-case (progn (torcl-ffi:close-foreign-library library) nil)
                  (torcl-ffi:ffi-error () t)))
        (torcl-ffi:foreign-free marker))
      (format t "FOREIGN-LIBRARY-OK~%")
    "#,
        library.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FOREIGN-LIBRARY-OK"));
}
