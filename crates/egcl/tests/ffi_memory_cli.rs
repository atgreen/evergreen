// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Per R8.01 and R8.05, both direct and function-value FFI paths deny sandbox access.
//! Per R8.19, pointer addresses and scalar memory values round-trip without loss.
use std::process::Command;

#[test]
fn public_foreign_memory_api_checks_ownership_and_preserves_values() {
    let program = r#"
      (let* ((p (egcl-ffi:foreign-alloc 32))
             (alias (egcl-ffi:inc-pointer p 1)))
        (assert (egcl-ffi:pointerp p))
        (assert (typep p 'egcl-ffi:foreign-pointer))
        (assert (not (egcl-ffi:pointerp 0)))
        (assert (egcl-ffi:pointer-eq p (egcl-ffi:make-pointer (egcl-ffi:pointer-address p))))
        (assert (egcl-ffi:null-pointer-p (egcl-ffi:null-pointer)))
        (setf (egcl-ffi:mem-ref alias :uint64) 18446744073709551615)
        (assert (= (egcl-ffi:mem-ref p :uint64 1) 18446744073709551615))
        (setf (egcl-ffi:mem-ref p :double 16) 1.0000000000000002d0)
        (assert (= (egcl-ffi:mem-ref p :double 16) 1.0000000000000002d0))
        (setf (egcl-ffi:mem-ref p :pointer 24) (egcl-ffi:null-pointer))
        (assert (egcl-ffi:null-pointer-p (egcl-ffi:mem-ref p :pointer 24)))
        (assert (handler-case (progn (egcl-ffi:mem-ref p :uint64 31) nil) (egcl-ffi:ffi-error () t)))
        (assert (handler-case (progn (egcl-ffi:foreign-free alias) nil) (error () t)))
        (egcl-ffi:foreign-free p)
        (assert (handler-case (progn (egcl-ffi:mem-ref alias :uint8) nil) (error () t)))
        (assert (handler-case (progn (egcl-ffi:foreign-free p) nil) (error () t))))
      (assert (= (egcl-ffi:pointer-address (egcl-ffi:make-pointer 18446744073709551615))
                 18446744073709551615))
      (assert (= (egcl-ffi:foreign-type-size :uint64) 8))
      (assert (= (egcl-ffi:foreign-type-alignment :double) 8))
      (format t "FOREIGN-MEMORY-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
      (let ((slot (egcl-ffi:foreign-alloc 8))
            (owned (egcl-ffi:foreign-alloc 16)))
        (setf (egcl-ffi:mem-ref slot :pointer) owned)
        (let ((reread (egcl-ffi:mem-ref slot :pointer)))
          (assert (egcl-ffi:pointer-eq reread owned))
          (egcl-ffi:foreign-free reread)
          ;; Freed once and only once: the identity is gone with the storage.
          (assert (handler-case (progn (egcl-ffi:foreign-free reread) nil)
                    (egcl-ffi:ffi-error () t)))
          (assert (handler-case (progn (egcl-ffi:foreign-free owned) nil)
                    (egcl-ffi:ffi-error () t))))
        (egcl-ffi:foreign-free slot))
      ;; An address this allocator never handed out is still refused: Rust's
      ;; deallocator must never be given C's storage.
      (assert (handler-case (progn (egcl-ffi:foreign-free (egcl-ffi:make-pointer 4096)) nil)
                (egcl-ffi:ffi-error () t)))
      ;; An interior address is not a base address.
      (let ((p (egcl-ffi:foreign-alloc 16)))
        (assert (handler-case
                    (progn (egcl-ffi:foreign-free
                            (egcl-ffi:make-pointer (+ 8 (egcl-ffi:pointer-address p))))
                           nil)
                  (egcl-ffi:ffi-error () t)))
        (egcl-ffi:foreign-free p))
      (format t "FREE-BY-ADDRESS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
        "(egcl-ffi:foreign-alloc 8)",
        "(funcall #'egcl-ffi:make-pointer 1)",
        "(egcl-ffi:load-foreign-library \"/nonexistent-ffi-sandbox-probe.so\")",
        "(funcall #'egcl::%foreign-library :symbol \"unused\" nil)",
        "(egcl::%load-foreign-library \"/nonexistent-ffi-sandbox-probe.so\")",
        "(egcl::%foreign-symbol 0 \"unused\")",
        "(egcl::%ffi-call 0 :void nil nil)",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
      (let ((vector (egcl-ffi:make-shareable-byte-vector 3)) (saved nil) (evaluations 0))
        (setf (aref vector 0) 42)
        (assert (= 17
          (catch 'done
            (egcl-ffi:with-pointer-to-vector-data (pointer (progn (incf evaluations) vector))
              (setf saved pointer)
              (assert (= 42 (egcl-ffi:mem-ref pointer :uint8)))
              (dotimes (i 30) (list i i i))
              (setf (egcl-ffi:mem-ref pointer :uint8 1) 255)
              (throw 'done 17)))))
        (assert (= evaluations 1))
        (assert (equalp vector #(42 255 0)))
        (assert (handler-case (progn (egcl-ffi:mem-ref saved :uint8) nil) (egcl-ffi:ffi-error () t))))
      (let ((vector (vector 1.25d0 2.5d0)))
        (assert (equal '(7 8)
          (multiple-value-list
            (egcl-ffi:with-pointer-to-vector-data (pointer vector :double)
              (assert (= 2.5d0 (egcl-ffi:mem-ref pointer :double 8)))
              (setf (egcl-ffi:mem-ref pointer :double) 3.75d0)
              (values 7 8)))))
        (assert (= 3.75d0 (aref vector 0))))
      (let ((invalid (vector 1 999)))
        (assert (handler-case
                  (progn (egcl-ffi:with-pointer-to-vector-data (pointer invalid)
                           (error "body must not execute")) nil)
                  (egcl-ffi:ffi-error () t)))
        (assert (equalp invalid #(1 999))))
      (let* ((base (vector 1 2 3 4))
             (view (make-array 2 :displaced-to base :displaced-index-offset 1)))
        (egcl-ffi:with-pointer-to-vector-data (pointer view)
          (assert (= 2 (egcl-ffi:mem-ref pointer :uint8)))
          (setf (egcl-ffi:mem-ref pointer :uint8) 9))
        (assert (equalp base #(1 9 3 4))))
      (let ((vector (make-array 3 :fill-pointer 1 :initial-contents '(1 2 3))))
        (egcl-ffi:with-pointer-to-vector-data (pointer vector)
          (setf (egcl-ffi:mem-ref pointer :uint8 2) 7))
        (assert (= 1 (length vector)))
        (assert (= 7 (aref vector 2))))
      (let ((vector (vector 18446744073709551615 0 18446744073709551614)))
        (egcl-ffi:with-pointer-to-vector-data (pointer vector :uint64)
          (assert (= 18446744073709551615 (egcl-ffi:mem-ref pointer :uint64)))
          (setf (egcl-ffi:mem-ref pointer :uint64 8) 18446744073709551613))
        (assert (equalp vector #(18446744073709551615 18446744073709551613 18446744073709551614))))
      (assert (null (egcl-ffi:with-pointer-to-vector-data (pointer (vector 0)))))
      (let ((vector (vector 1 2)) (original nil))
        (egcl-ffi:with-pointer-to-vector-data (pointer vector)
          (setf original pointer)
          (setf pointer (egcl-ffi:inc-pointer pointer 1))
          (setf (egcl-ffi:mem-ref pointer :uint8) 3))
        (assert (equalp vector #(1 3)))
        (assert (handler-case (progn (egcl-ffi:mem-ref original :uint8) nil)
                  (egcl-ffi:ffi-error () t))))
      (format t "FOREIGN-VECTOR-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
