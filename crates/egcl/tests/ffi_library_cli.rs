#![cfg(target_os = "linux")]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn public_library_objects_support_lookup_call_close_and_destructors() {
    let dir = std::env::temp_dir().join(format!("egcl-library-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("library.c");
    let library = dir.join("library.so");
    std::fs::write(
        &source,
        r#"
        #include <stdarg.h>
        static int *closed;
        void egcl_fixture_set_closed(int *p) { closed = p; }
        int egcl_fixture_library_answer(void) { return 42; }
        int egcl_fixture_sum(int count, ...) {
            va_list args; va_start(args, count);
            int result = 0;
            for (int i = 0; i < count; i++) result += va_arg(args, int);
            va_end(args); return result;
        }
        __attribute__((destructor)) static void finish(void) { if (closed) *closed = 99; }
    "#,
    )
    .unwrap();
    // Distro stack-protector defaults add a libc dependency (__stack_chk_fail)
    // that the standalone musl ELF loader cannot resolve.
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-fno-stack-protector"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .status()
        .unwrap()
        .success());
    let program = format!(
        r#"
      (let* ((library (egcl-ffi:load-foreign-library {:?}))
             (marker (egcl-ffi:foreign-alloc 4)))
        (assert (typep library 'egcl-ffi:foreign-library))
        (assert (not (egcl-ffi:pointerp library)))
        (egcl-ffi:foreign-call (egcl-ffi:foreign-symbol-pointer "egcl_fixture_set_closed" library)
                                :void '(:pointer) (list marker))
        (assert (= 42 (egcl-ffi:foreign-call
                        (egcl-ffi:foreign-symbol-pointer "egcl_fixture_library_answer") :int nil nil)))
        (assert (= 24 (funcall #'egcl-ffi:foreign-call
                         (egcl-ffi:foreign-symbol-pointer "egcl_fixture_sum" library)
                         :int '(:int :char :short) '(2 -7 31) 1)))
        (egcl-ffi:close-foreign-library library)
        (assert (= 99 (egcl-ffi:mem-ref marker :int)))
        (assert (handler-case (progn (egcl-ffi:foreign-symbol-pointer "missing" library) nil)
                  (egcl-ffi:ffi-error () t)))
        (assert (handler-case (progn (egcl-ffi:close-foreign-library library) nil)
                  (egcl-ffi:ffi-error () t)))
        (egcl-ffi:foreign-free marker))
      (format t "FOREIGN-LIBRARY-OK~%")
    "#,
        library.to_str().unwrap()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
