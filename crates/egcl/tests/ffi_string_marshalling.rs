//! `%ffi-call` `:string` marshalling (bliss-124): a Lisp string argument is
//! passed as a fresh NUL-terminated `char*`, NIL as a null pointer, and a
//! `:string` return is read back into a Lisp string. Exercises the CLI builtin
//! end to end against a tiny shared library, so it also covers the runtime
//! elf_loader path in the default (static) build. Soft-skips when no `cc` is
//! available.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn egcl_bin() -> Command {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    let path = BIN.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_egcl")));
    let mut cmd = Command::new(path);
    // Hermetic against the developer's real init file (mirrors acceptance.rs).
    cmd.env(
        "EGCL_INIT_FILE",
        std::env::temp_dir().join("egcl-tests-nonexistent-init.lisp"),
    );
    cmd
}

/// Compile a tiny shared library with the system C compiler. Returns None (and
/// the test soft-skips) if no `cc` is available.
fn build_test_so(dir: &Path, src: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let c = dir.join("s.c");
    std::fs::write(&c, src).ok()?;
    let so = dir.join("libs.so");
    let ok = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(&c)
        .arg("-o")
        .arg(&so)
        .status()
        .ok()?
        .success();
    ok.then_some(so)
}

#[test]
fn foreign_call_accepts_owned_pointer_objects() {
    let dir = std::env::temp_dir().join(format!("egcl-ffi-memory-{}", std::process::id()));
    let so = build_test_so(
        &dir,
        "#include <stdint.h>\nuint64_t read_word(const uint64_t *p) { return *p; }\nuint64_t *echo_pointer(uint64_t *p) { return p; }",
    )
    .expect("C compiler required for foreign-memory integration test");
    let program = format!(
        r#"
      (let* ((library (egcl::%load-foreign-library {:?}))
             (function (egcl::%foreign-symbol library "read_word"))
             (echo (egcl::%foreign-symbol library "echo_pointer"))
             (pointer (egcl-ffi:foreign-alloc 8)))
        (unwind-protect
            (progn
              (setf (egcl-ffi:mem-ref pointer :uint64) 18446744073709551615)
              (assert (= (egcl::%ffi-call function :uint64 '(:pointer) (list pointer))
                         18446744073709551615))
              (let ((returned (egcl::%ffi-call echo :pointer '(:pointer) (list pointer))))
                (assert (egcl-ffi:pointer-eq returned pointer))
                (assert (= (egcl-ffi:mem-ref returned :uint64) 18446744073709551615)))
              (assert (egcl-ffi:null-pointer-p (egcl::%ffi-call echo :pointer '(:pointer) '(nil))))
              (format t "POINTER-CALL-OK~%"))
          (egcl-ffi:foreign-free pointer)))
    "#,
        so.to_str().unwrap()
    );
    let output = egcl_bin()
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("POINTER-CALL-OK"));
}

#[test]
fn ffi_call_marshals_strings_both_ways() {
    let dir = std::env::temp_dir().join(format!("egcl-ffi-str-{}", std::process::id()));
    let src = "
        long my_strlen(const char* s) { if (!s) return -1; long n = 0; while (s[n]) n++; return n; }
        const char* greeting(void) { return \"hello from C\"; }
        int first_byte(const char* s) { return s ? (unsigned char)s[0] : -1; }
    ";
    let Some(so) = build_test_so(&dir, src) else {
        eprintln!("cc unavailable — skipping :string FFI marshalling test");
        return;
    };
    let so = so.to_str().unwrap();

    // Load the lib and exercise: :string arg (word/empty), NIL -> null,
    // :string return, and a byte read to prove the buffer reached C intact.
    let program = format!(
        "(let* ((lib (egcl::%load-foreign-library {so:?}))
                (strlen (egcl::%foreign-symbol lib \"my_strlen\"))
                (greet  (egcl::%foreign-symbol lib \"greeting\"))
                (fb     (egcl::%foreign-symbol lib \"first_byte\")))
           (format t \"len=~a~%\" (egcl::%ffi-call strlen :long '(:string) '(\"hello\")))
           (format t \"empty=~a~%\" (egcl::%ffi-call strlen :long '(:string) '(\"\")))
           (format t \"nil=~a~%\" (egcl::%ffi-call strlen :long '(:string) '(nil)))
           (format t \"ret=~s~%\" (egcl::%ffi-call greet :string '() '()))
           (format t \"byte=~a~%\" (egcl::%ffi-call fb :int '(:string) '(\"ABC\"))))"
    );

    let output = egcl_bin()
        .args(["--no-init", "--eval", &program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "egcl failed: status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );

    assert!(
        stdout.contains("len=5"),
        "string arg length; got:\n{stdout}"
    );
    assert!(
        stdout.contains("empty=0"),
        "empty string arg; got:\n{stdout}"
    );
    assert!(
        stdout.contains("nil=-1"),
        "NIL must pass a null pointer; got:\n{stdout}"
    );
    assert!(
        stdout.contains("ret=\"hello from C\""),
        "char* return -> Lisp string; got:\n{stdout}"
    );
    assert!(
        stdout.contains("byte=65"),
        "buffer must reach C intact ('A' == 65); got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(all(target_arch = "x86_64", unix))]
fn ffi_call_preserves_double_and_full_width_integer_values() {
    let dir = std::env::temp_dir().join(format!("egcl-ffi-numeric-{}", std::process::id()));
    let src = "
        double identity_double(double value) { return value; }
        unsigned long long identity_unsigned(unsigned long long value) { return value; }
        long long identity_signed(long long value) { return value; }
    ";
    let so = build_test_so(&dir, src).expect("C compiler required for numeric FFI test");
    let so = so.to_str().unwrap();
    let program = format!(
        "(let* ((lib (egcl::%load-foreign-library {so:?}))
                (d (egcl::%foreign-symbol lib \"identity_double\"))
                (u (egcl::%foreign-symbol lib \"identity_unsigned\"))
                (s (egcl::%foreign-symbol lib \"identity_signed\"))
                (result (egcl::%ffi-call d :double '(:double) '(1.0000000000000002d0))))
           (assert (typep result 'double-float))
           (assert (= result 1.0000000000000002d0))
           (assert (= (egcl::%ffi-call u :unsigned-long '(:unsigned-long)
                       '(18446744073709551615)) 18446744073709551615))
           (assert (= (egcl::%ffi-call s :long-long '(:long-long)
                       '(-9223372036854775808)) -9223372036854775808))
           (format t \"exact-ffi-values-ok~%\"))"
    );
    let output = egcl_bin()
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("exact-ffi-values-ok"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(all(target_arch = "x86_64", unix))]
fn ffi_call_variadic_preserves_fixed_float_and_promotes_trailing_values() {
    let dir = std::env::temp_dir().join(format!("egcl-ffi-varargs-{}", std::process::id()));
    let so = build_test_so(
        &dir,
        "
        #include <stdarg.h>
        double mixed(float fixed, int count, ...) {
            va_list args;
            va_start(args, count);
            double result = fixed;
            for (int i = 0; i < count; ++i) {
                int multiplier = va_arg(args, int);
                double value = va_arg(args, double);
                result += multiplier * value;
            }
            va_end(args);
            return result;
        }
    ",
    )
    .expect("C compiler required for variadic FFI test");
    let so = so.to_str().unwrap();
    let program = format!(
        "(let* ((lib (egcl::%load-foreign-library {so:?}))
                (fn (egcl::%foreign-symbol lib \"mixed\"))
                (result (egcl::%ffi-call fn :double
                          '(:float :int :char :float :unsigned-short :double)
                          '(1.25 2 -7 2.5 65535 0.5d0) 2)))
           (assert (= result 32751.25d0))
           (format t \"variadic-ffi-ok~%\"))"
    );
    let output = egcl_bin()
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("variadic-ffi-ok"));
    let _ = std::fs::remove_dir_all(&dir);
}
