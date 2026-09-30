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
fn an_allocation_can_be_freed_through_a_pointer_read_back_out_of_memory() {
    // Storing a pointer in foreign memory and reading it back is ordinary FFI
    // practice — CFFI's DEFCVAR of a :string does it — and the value that comes
    // back no longer carries the allocation's identity. FOREIGN-FREE frees by
    // address when the address is a live allocation's base (bliss-06l4z).
    let program = r#"
      (let ((slot (torcl-ffi:foreign-alloc 8))
            (owned (torcl-ffi:foreign-alloc 16)))
        (setf (torcl-ffi:mem-ref slot :pointer) owned)
        (let ((reread (torcl-ffi:mem-ref slot :pointer)))
          (assert (torcl-ffi:pointer-eq reread owned))
          (torcl-ffi:foreign-free reread)
          ;; Freed once and only once: the identity is gone with the storage.
          (assert (handler-case (progn (torcl-ffi:foreign-free reread) nil)
                    (torcl-ffi:ffi-error () t)))
          (assert (handler-case (progn (torcl-ffi:foreign-free owned) nil)
                    (torcl-ffi:ffi-error () t))))
        (torcl-ffi:foreign-free slot))
      ;; An address this allocator never handed out is still refused: Rust's
      ;; deallocator must never be given C's storage.
      (assert (handler-case (progn (torcl-ffi:foreign-free (torcl-ffi:make-pointer 4096)) nil)
                (torcl-ffi:ffi-error () t)))
      ;; An interior address is not a base address.
      (let ((p (torcl-ffi:foreign-alloc 16)))
        (assert (handler-case
                    (progn (torcl-ffi:foreign-free
                            (torcl-ffi:make-pointer (+ 8 (torcl-ffi:pointer-address p))))
                           nil)
                  (torcl-ffi:ffi-error () t)))
        (torcl-ffi:foreign-free p))
      (format t "FREE-BY-ADDRESS-OK~%")
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("FREE-BY-ADDRESS-OK"));
}

#[test]
fn foreign_memory_is_denied_in_sandbox_even_through_funcall() {
    for form in [
        "(torcl-ffi:foreign-alloc 8)",
        "(funcall #'torcl-ffi:make-pointer 1)",
        "(torcl-ffi:load-foreign-library \"/nonexistent-ffi-sandbox-probe.so\")",
        "(funcall #'torcl::%foreign-library :symbol \"unused\" nil)",
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

#[test]
fn scoped_vector_copies_survive_gc_and_copy_back_on_nonlocal_exit() {
    let program = r#"
      (let ((vector (torcl-ffi:make-shareable-byte-vector 3)) (saved nil) (evaluations 0))
        (setf (aref vector 0) 42)
        (assert (= 17
          (catch 'done
            (torcl-ffi:with-pointer-to-vector-data (pointer (progn (incf evaluations) vector))
              (setf saved pointer)
              (assert (= 42 (torcl-ffi:mem-ref pointer :uint8)))
              (dotimes (i 30) (list i i i))
              (setf (torcl-ffi:mem-ref pointer :uint8 1) 255)
              (throw 'done 17)))))
        (assert (= evaluations 1))
        (assert (equalp vector #(42 255 0)))
        (assert (handler-case (progn (torcl-ffi:mem-ref saved :uint8) nil) (torcl-ffi:ffi-error () t))))
      (let ((vector (vector 1.25d0 2.5d0)))
        (assert (equal '(7 8)
          (multiple-value-list
            (torcl-ffi:with-pointer-to-vector-data (pointer vector :double)
              (assert (= 2.5d0 (torcl-ffi:mem-ref pointer :double 8)))
              (setf (torcl-ffi:mem-ref pointer :double) 3.75d0)
              (values 7 8)))))
        (assert (= 3.75d0 (aref vector 0))))
      (let ((invalid (vector 1 999)))
        (assert (handler-case
                  (progn (torcl-ffi:with-pointer-to-vector-data (pointer invalid)
                           (error "body must not execute")) nil)
                  (torcl-ffi:ffi-error () t)))
        (assert (equalp invalid #(1 999))))
      (let* ((base (vector 1 2 3 4))
             (view (make-array 2 :displaced-to base :displaced-index-offset 1)))
        (torcl-ffi:with-pointer-to-vector-data (pointer view)
          (assert (= 2 (torcl-ffi:mem-ref pointer :uint8)))
          (setf (torcl-ffi:mem-ref pointer :uint8) 9))
        (assert (equalp base #(1 9 3 4))))
      (let ((vector (make-array 3 :fill-pointer 1 :initial-contents '(1 2 3))))
        (torcl-ffi:with-pointer-to-vector-data (pointer vector)
          (setf (torcl-ffi:mem-ref pointer :uint8 2) 7))
        (assert (= 1 (length vector)))
        (assert (= 7 (aref vector 2))))
      (let ((vector (vector 18446744073709551615 0 18446744073709551614)))
        (torcl-ffi:with-pointer-to-vector-data (pointer vector :uint64)
          (assert (= 18446744073709551615 (torcl-ffi:mem-ref pointer :uint64)))
          (setf (torcl-ffi:mem-ref pointer :uint64 8) 18446744073709551613))
        (assert (equalp vector #(18446744073709551615 18446744073709551613 18446744073709551614))))
      (assert (null (torcl-ffi:with-pointer-to-vector-data (pointer (vector 0)))))
      (let ((vector (vector 1 2)) (original nil))
        (torcl-ffi:with-pointer-to-vector-data (pointer vector)
          (setf original pointer)
          (setf pointer (torcl-ffi:inc-pointer pointer 1))
          (setf (torcl-ffi:mem-ref pointer :uint8) 3))
        (assert (equalp vector #(1 3)))
        (assert (handler-case (progn (torcl-ffi:mem-ref original :uint8) nil)
                  (torcl-ffi:ffi-error () t))))
      (format t "FOREIGN-VECTOR-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FOREIGN-VECTOR-OK"));
}
