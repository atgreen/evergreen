//! Per R8.01 and R8.05, both direct and function-value FFI paths deny sandbox access.
//! Per R8.19, pointer addresses and scalar memory values round-trip without loss.
use std::process::Command;

#[test]
fn public_foreign_memory_api_checks_ownership_and_preserves_values() {
    let program = r#"
      (let* ((p (torcl-ffi:foreign-alloc 32))
             (alias (torcl-ffi:inc-pointer p 1)))
        (assert (torcl-ffi:pointerp p))
        (assert (typep p 'torcl-ffi:foreign-pointer))
        (assert (not (torcl-ffi:pointerp 0)))
        (assert (torcl-ffi:pointer-eq p (torcl-ffi:make-pointer (torcl-ffi:pointer-address p))))
        (assert (torcl-ffi:null-pointer-p (torcl-ffi:null-pointer)))
        (setf (torcl-ffi:mem-ref alias :uint64) 18446744073709551615)
        (assert (= (torcl-ffi:mem-ref p :uint64 1) 18446744073709551615))
        (setf (torcl-ffi:mem-ref p :double 16) 1.0000000000000002d0)
        (assert (= (torcl-ffi:mem-ref p :double 16) 1.0000000000000002d0))
        (setf (torcl-ffi:mem-ref p :pointer 24) (torcl-ffi:null-pointer))
        (assert (torcl-ffi:null-pointer-p (torcl-ffi:mem-ref p :pointer 24)))
        (assert (handler-case (progn (torcl-ffi:mem-ref p :uint64 31) nil) (torcl-ffi:ffi-error () t)))
        (assert (handler-case (progn (torcl-ffi:foreign-free alias) nil) (error () t)))
        (torcl-ffi:foreign-free p)
        (assert (handler-case (progn (torcl-ffi:mem-ref alias :uint8) nil) (error () t)))
        (assert (handler-case (progn (torcl-ffi:foreign-free p) nil) (error () t))))
      (assert (= (torcl-ffi:pointer-address (torcl-ffi:make-pointer 18446744073709551615))
                 18446744073709551615))
      (assert (= (torcl-ffi:foreign-type-size :uint64) 8))
      (assert (= (torcl-ffi:foreign-type-alignment :double) 8))
      (format t "FOREIGN-MEMORY-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FOREIGN-MEMORY-OK"));
}

#[test]
fn foreign_memory_is_denied_in_sandbox_even_through_funcall() {
    for form in [
        "(torcl-ffi:foreign-alloc 8)",
        "(funcall #'torcl-ffi:make-pointer 1)",
        "(torcl::%load-foreign-library \"/nonexistent-ffi-sandbox-probe.so\")",
        "(torcl::%foreign-symbol 0 \"unused\")",
        "(torcl::%ffi-call 0 :void nil nil)",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
            .args(["--no-init", "--sandbox", "--eval", form])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("FFI access denied"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
