use std::process::Command;

const PROGRAM: &str = r#"
(progn
  (defun array-adjust-error-p (thunk)
    (handler-case (progn (funcall thunk) nil) (error () t)))
  (defun array-adjust-program-error-p (thunk)
    (handler-case (progn (funcall thunk) nil) (program-error () t)))
  (defun array-adjust-elements (array)
    (loop for i below (array-total-size array) collect (row-major-aref array i)))
  (let* ((a (make-array 5 :initial-contents '(a b c d e) :adjustable t))
         (result (adjust-array a 4 :initial-contents '(w x y z))))
    (assert (eq a result))
    (assert (equal (array-dimensions a) '(4)))
    (assert (= (length a) 4))
    (assert (equal (array-adjust-elements a) '(w x y z)))
    (assert (not (array-has-fill-pointer-p a))))
  (let* ((a (make-array 5 :initial-contents '(a b c d e) :fill-pointer 3))
         (result (adjust-array a 4)))
    (assert (not (adjustable-array-p a)))
    (assert (not (eq a result)))
    (assert (array-has-fill-pointer-p result))
    (assert (= (fill-pointer result) 3))
    (assert (= (array-total-size result) 4))
    (assert (eq (aref result 3) 'd))
    (assert (equal (array-dimensions a) '(5))))
  (let* ((a (make-array nil :adjustable t :initial-contents 'old))
         (result (adjust-array a nil :initial-contents 'new)))
    (assert (adjustable-array-p a))
    (assert (eq a result))
    (assert (= (array-rank a) 0))
    (assert (null (array-dimensions a)))
    (assert (eq (aref a) 'new))
    (assert (typep a (type-of a)))
    (assert (not (typep a 'simple-array))))
  (let* ((a (make-array '(2 3) :adjustable t :initial-contents '((0 1 2) (3 4 5))))
         (alias (make-array 3 :displaced-to a :displaced-index-offset 2))
         (result (adjust-array a '(3 4) :initial-element 9)))
    (assert (eq a result))
    (assert (adjustable-array-p a))
    (assert (equal (array-dimensions a) '(3 4)))
    (assert (equal (array-adjust-elements a) '(0 1 2 9 3 4 5 9 9 9 9 9)))
    (assert (equal (array-adjust-elements alias) '(2 9 3)))
    (assert (eq (array-displacement alias) a))
    (let ((base (make-array 20 :initial-contents '(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19))))
      (assert (eq a (adjust-array a '(2 2) :displaced-to base :displaced-index-offset 3)))
      (assert (equal (array-adjust-elements a) '(3 4 5 6)))
      (assert (eq (array-displacement a) base))
      (assert (= (aref alias 0) 5))
      (assert (eq a (adjust-array a '(2 2) :initial-contents '((w x) (y z)))))
      (assert (null (array-displacement a)))
      (assert (equal (array-adjust-elements a) '(w x y z)))
      (assert (= (aref base 3) 3))))
  (dolist (row (list (list t '(w x y z) 'old)
                    (list 'character "wxyz" #\a)
                    (list 'base-char "wxyz" #\a)
                    (list 'bit #*1010 0)
                    (list '(unsigned-byte 8) #(1 2 3 255) 0)
                    (list 'double-float #(1d0 2d0 3d0 4d0) 0d0)))
    (let* ((element-type (first row)) (contents (second row)) (fill (third row))
           (a (make-array 5 :element-type element-type :initial-element fill :adjustable t :fill-pointer 3))
           (actual-type (array-element-type a))
           (result (adjust-array a 4 :initial-contents contents)))
      (assert (eq a result))
      (assert (= (fill-pointer a) 3))
      (assert (equal (array-dimensions a) '(4)))
      (assert (equal (array-element-type a) actual-type))
      (assert (equalp (array-adjust-elements a) (coerce contents 'list)))
      (assert (typep a (type-of a)))
      (assert (not (typep a 'simple-array)))))
  (dolist (row (list (list t 'old 'new) (list 'character #\a #\z)
                    (list 'base-char #\a #\z) (list 'bit 0 1)
                    (list '(unsigned-byte 8) 1 255) (list 'double-float 1d0 2d0)))
    (let* ((element-type (first row)) (old (second row)) (fill (third row))
           (a (make-array '(2 2) :element-type element-type :initial-element old :adjustable t))
           (actual-type (array-element-type a)))
      (assert (eq a (adjust-array a '(2 3) :initial-element fill)))
      (assert (adjustable-array-p a))
      (assert (equal (array-element-type a) actual-type))
      (assert (equalp (array-adjust-elements a) (list old old fill old old fill)))
      (assert (typep a (type-of a)))
      (assert (not (vectorp a)))
      (assert (not (array-has-fill-pointer-p a)))))
  ;; Array indexing uses capacity even when a displaced backing has a shorter FP.
  (let* ((backing (make-array 6 :initial-contents '(10 11 12 13 14 15)
                              :fill-pointer 1 :adjustable t))
         (matrix (make-array '(2 2) :displaced-to backing :displaced-index-offset 1)))
    (assert (= (aref matrix 1 1) 14))
    (setf (aref matrix 1 1) 99)
    (assert (= (aref backing 4) 99))
    (assert (= (row-major-aref matrix 3) 99))
    (setf (row-major-aref matrix 2) 88)
    (assert (= (aref backing 3) 88))
    (assert (= (length backing) 1)))
  ;; Readable octet MD output retains rank, specialization, and adjustability.
  (let* ((array (make-array '(2 2) :element-type '(unsigned-byte 8)
                                 :initial-contents '((1 2) (3 255)) :adjustable t))
         (*print-readably* t) (*print-base* 16) (*read-eval* t)
         (via-write (read-from-string (write-to-string array)))
         (via-format (read-from-string (format nil "~S" array))))
    (dolist (copy (list via-write via-format))
      (assert (equal (array-dimensions copy) '(2 2)))
      (assert (equal (array-element-type copy) '(unsigned-byte 8)))
      (assert (adjustable-array-p copy))
      (assert (equal (array-adjust-elements copy) '(1 2 3 255)))))
  (let ((a (make-array '(2 0) :adjustable t)))
    (assert (eq a (adjust-array a '(3 0))))
    (assert (equal (array-dimensions a) '(3 0)))
    (assert (= (array-total-size a) 0)))
  (let* ((base (make-array 7 :initial-contents '(1 2 3 4 5 6 7)))
         (a (make-array 5 :initial-contents '(a b c d e)))
         (result (adjust-array a 4 :displaced-to base :displaced-index-offset 1)))
    (assert (not (eq a result)))
    (assert (equal (multiple-value-list (array-displacement result)) (list base 1)))
    (assert (equal (array-adjust-elements result) '(2 3 4 5)))
    (assert (equal (array-adjust-elements a) '(a b c d e))))
  (dolist (bad (list (list :bad t) (list :initial-element)
                    (list :allow-other-keys nil :allow-other-keys t :bad t)))
    (assert (array-adjust-program-error-p (lambda () (apply #'make-array 4 bad))))
    (assert (array-adjust-program-error-p
             (lambda () (apply #'adjust-array (make-array 4) 3 bad)))))
  (assert (= (array-total-size (make-array 2 :allow-other-keys t :bad t)) 2))
  (let ((a (make-array 3 :initial-element 'old :adjustable t)))
    (assert (array-adjust-error-p (lambda () (adjust-array a 2 :initial-contents '(short)))))
    (assert (array-adjust-error-p (lambda () (adjust-array a '(1 2)))))
    (assert (array-adjust-error-p (lambda () (adjust-array a 3 :displaced-to a))))
    (assert (equal (array-dimensions a) '(3)))
    (assert (equal (array-adjust-elements a) '(old old old)))
    (assert (null (array-displacement a))))
  (let ((events nil) (a (make-array 3 :initial-element 'old :adjustable t)))
    (assert (array-adjust-error-p
             (lambda () (adjust-array (progn (push :array events) a)
                                      (progn (push :dimensions events) 2)
                                      :initial-contents (progn (push :contents events) '(wrong))))))
    (assert (equal (reverse events) '(:array :dimensions :contents)))
    (assert (equal (array-adjust-elements a) '(old old old))))
  (defun array-adjust-native-call (array n)
    (declare (type fixnum n))
    (let ((result (adjust-array array n :initial-element 'new))) result))
  (let ((a (make-array 3 :initial-contents '(a b c) :adjustable t)))
    (dotimes (i 12) (assert (eq a (array-adjust-native-call a 4))))
    (assert (equal (array-adjust-elements a) '(a b c new)))
    (when (string= (egcl-ext:getenv "EGCL_FORCE_TIER") "t2")
      (assert (= (egcl-ext:function-tier 'array-adjust-native-call) 2))))
  (assert (eq (upgraded-array-element-type t) t))
  (assert (null (upgraded-array-element-type nil)))
  (assert (null (egcl-internal::%bit-element-type-p '(t 0 1))))
  (assert (null (egcl-internal::%bit-element-type-p '(nil 0 1))))
  (dolist (type '(bit (unsigned-byte 0) (unsigned-byte 1) (mod 1) (mod 2)
                 (integer 0 0) (integer 1 1) (integer 0 1) (integer 0 (2))))
    (assert (eq (upgraded-array-element-type type) 'bit))
    (let* ((initial (if (equal type '(integer 1 1)) 1 0))
           (array (make-array 3 :element-type type :initial-element initial)))
      (assert (eq (array-element-type array) 'bit))
      (assert (typep array (list 'array type '(3))))
      (assert (every (lambda (element) (= element initial)) array))))
  (defvar *array-adjust-type-expansions* 0)
  (deftype array-adjust-octet-alias ()
    (incf *array-adjust-type-expansions*)
    '(unsigned-byte 8))
  (let ((before *array-adjust-type-expansions*))
    (assert (equal (upgraded-array-element-type 'array-adjust-octet-alias) '(unsigned-byte 8)))
    (assert (= *array-adjust-type-expansions* (1+ before))))
  (assert (equal (upgraded-array-element-type '(integer 0 (4))) '(unsigned-byte 8)))
  (assert (subtypep (upgraded-array-element-type '(integer 0 (2)))
                    (upgraded-array-element-type '(integer 0 (4)))))
  (dolist (args (list nil (list (vector 1)) (list (vector 1) 0 :extra)))
    (assert (array-adjust-program-error-p (lambda () (apply #'svref args)))))
  (format t "ARRAY-ADJUSTMENT-OK~%"))
"#;

fn run(tier: &str, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        command.env_remove(key);
    }
    command
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_THREADS", "1");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run array adjustment regression");
    assert!(
        output.status.success(),
        "tier={tier} stress={stress}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if stress {
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT:")
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert_eq!(stdout, "ARRAY-ADJUSTMENT-OK\nNIL");
    stdout
}

#[test]
fn adjust_array_preserves_options_identity_and_errors_across_tiers() {
    let expected = run("t0", false);
    assert_eq!(run("t1", false), expected);
    assert_eq!(run("t2", false), expected);
}

#[test]
fn adjust_array_preserves_results_under_gc_stress_and_poison() {
    assert_eq!(run("t2", true), run("t0", false));
}
