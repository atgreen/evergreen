// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The shapes that kept cl-ppcre's matchers in the interpreter compile now,
//! and compute what the tree-walker computes (bliss-3k0xm).
//!
//! - A parameter whose name is special binds through a LET in the body, so a
//!   method such as cl-ppcre's `(scan (scanner function) target &key ...
//!   ((:real-start-pos *real-start-pos*) nil))` is compiled.
//! - A non-constant `&key` default is evaluated in the body.
//! - A FLET/LABELS function declared INLINE is open-coded at its calls, so its
//!   RETURN-FROM to the caller's block is a local exit and no closure is made.
//! - A local exit that leaves special bindings unbinds them first, so native
//!   code no longer declines it.
//! - Generic dispatch is cached for calls with keyword arguments and for
//!   effective methods with several primaries.
//!
//! Every expected value below was checked against SBCL.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new(BIN);
    cmd.args(["--no-init", "--eval", program]);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

/// Run `program` tiered and under the tree-walker; both must print `expected`.
fn agree(program: &str, expected: &str, envs: &[(&str, &str)]) -> String {
    let (out, log) = run(program, envs);
    assert_eq!(out.trim(), expected, "tiered run:\n{log}");
    let (tree, tree_log) = run(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(tree.trim(), expected, "tree-walker run:\n{tree_log}");
    log
}

#[test]
fn special_named_parameters_and_key_defaults_compile() {
    let program = r#"(progn
  (defvar *s* :outer)
  (defun read-s () *s*)
  (defun sp-req (*s*) (list *s* (read-s)))
  (defun sp-key-alias (x &key ((:mode *s*) :md) (y 3)) (list x y (read-s)))
  (defun sp-key-self (&key ((:s *s*) *s*)) (read-s))
  (defun sp-setq (*s*) (setq *s* :changed) (read-s))
  (defun sp-return (*s*) (return-from sp-return (read-s)))
  (defgeneric gsp (a &key))
  (defmethod gsp ((a integer) &key ((:real *s*) :r)) (list a (read-s)))
  (defun kd-basic (s &key (start 0) (end (length s))) (list start end))
  (defun kd-chain (&key (a (list 1)) (b (cons 2 a))) (list a b))
  (defun kd-supplied (&key (a (+ 1 1) a-p)) (list a a-p))
  (defun kd-typed (s &key (end (length s))) (declare (fixnum end)) (* end 2))
  (defun sp-flet (n) (flet ((g (*s*) (read-s))) (g n)))
  (defun sp-labels (n) (labels ((g (*s*) (read-s))) (g n)))
  (dotimes (i 40)
    (sp-req 1) (sp-key-alias 1) (sp-key-self) (sp-setq 1) (sp-return 1) (gsp 1)
    (kd-basic "abc") (kd-chain) (kd-supplied) (kd-typed "ab") (sp-flet 1) (sp-labels 1))
  (format t "~s~%"
    (list (sp-req 1) *s*
          (sp-key-alias 1) (sp-key-alias 1 :mode :m :y 4)
          (sp-key-self) (sp-key-self :s 5)
          (sp-setq 1) (sp-return 7) *s*
          (gsp 1) (gsp 2 :real :x) *s*
          (kd-basic "abcd") (kd-basic "abcd" :end 2) (kd-basic "abcd" :start 1)
          (kd-chain) (kd-chain :a 0)
          (kd-supplied) (kd-supplied :a 9)
          (kd-typed "abc") (kd-typed "abc" :end 5)
          (sp-flet 1) (sp-labels 2) *s*
          (mapcar (lambda (f) (plusp (egcl-ext:function-tier f)))
                  (list #'sp-req #'sp-key-alias #'kd-basic)))))"#;
    let expected = "((1 1) :OUTER (1 3 :MD) (1 4 :M) :OUTER 5 :CHANGED 7 :OUTER (1 :R) (2 :X) :OUTER \
(0 4) (0 2) (1 4) ((1) (2 1)) (0 (2 . 0)) (2 NIL) (9 T) 6 10 1 2 :OUTER (T T T))";
    let log = agree(
        program,
        expected,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T0_T1_THRESHOLD", "5")],
    );
    assert!(
        !log.contains("lambda-list:special-parameter-name"),
        "special-named parameters must compile:\n{log}"
    );
}

#[test]
fn inline_local_functions_open_code_and_exit_through_special_bindings() {
    // cl-ppcre's scanner shape: a BLOCK around special bindings, an INLINE
    // LABELS function that returns from that block, and a LOOP calling it.
    let program = r#"(progn
  (defvar *string* "")
  (defvar *end-pos* 0)
  (defun make-searcher (chr)
    (lambda (start)
      (loop for i from start below *end-pos*
            thereis (and (char= (schar *string* i) chr) i))))
  (defun make-scanner (test)
    (lambda (string start end)
      (block scan
        (let* ((*string* string) (*end-pos* end) (limit (- end 1)))
          (labels ((advance-fn (pos)
                     (unless (setq pos (funcall test pos))
                       (return-from scan nil))
                     pos))
            (declare (inline advance-fn))
            (loop for pos = (advance-fn start) then (advance-fn (1+ pos))
                  while (<= pos limit)
                  do (when (and (< (1+ pos) end) (char= (schar *string* (1+ pos)) #\b))
                       (return-from scan (values pos (+ pos 2))))))))))
  (let ((sc (make-scanner (make-searcher #\a))) (r nil))
    (dotimes (i 300)
      (setq r (list (multiple-value-list (funcall sc "xxaxabyy" 0 8))
                    (multiple-value-list (funcall sc "xxxxxx" 0 6)))))
    (format t "~s~%" (list r *string* *end-pos*))))"#;
    let expected = r#"(((4 6) (NIL)) "" 0)"#;
    let log = agree(
        program,
        expected,
        &[("EGCL_LOG", "compile=trace"), ("EGCL_T2_LOG", "stderr")],
    );
    assert!(
        log.contains(r#"local functions {"COMMON-LISP-USER::ADVANCE-FN"}: open-coded"#),
        "the INLINE local function must be open-coded:\n{log}"
    );
    for refusal in [
        "direct exit requires dynamic unwinding",
        "a closure it creates needs captured block/tag control",
        "PushBlock needs an executing identity",
    ] {
        assert!(
            !log.contains(refusal),
            "no tier may refuse the scanner ({refusal}):\n{log}"
        );
    }
}

/// Open coding must not change what a name means: a call site that rebinds
/// a variable, BLOCK or function the definition uses keeps the closure.
#[test]
fn inline_local_functions_keep_their_definition_scope() {
    let program = r#"(progn
  (defun li-shadow (x)
    (flet ((get-x () x))
      (declare (inline get-x))
      (let ((x (* x 10))) (list x (get-x)))))
  (defun li-block-shadow ()
    (block b
      (flet ((leave () (return-from b :outer-b)))
        (declare (inline leave))
        (list (block b (leave)) :not-reached))))
  (defun li-return (list)
    (block outer
      (flet ((check (v) (when (eq v :stop) (return-from outer :stopped)) v))
        (declare (inline check))
        (dolist (v list :done) (check v)))))
  (defun li-closure (n)
    (flet ((adder (k) (lambda (v) (+ v k n))))
      (declare (inline adder))
      (funcall (adder 1) 10)))
  (defun li-args-order (log)
    (flet ((pair (a b) (list a b)))
      (declare (inline pair))
      (pair (progn (push 1 (car log)) :a) (progn (push 2 (car log)) :b))))
  (defun li-recursive (n)
    (labels ((fact (k) (if (< k 2) 1 (* k (fact (1- k))))))
      (declare (inline fact))
      (fact n)))
  (dotimes (i 300)
    (li-shadow 1) (li-block-shadow) (li-return '(1 :stop)) (li-closure 1)
    (li-args-order (list nil)) (li-recursive 3))
  (format t "~s~%"
    (list (li-shadow 2) (li-block-shadow) (li-return '(1 2 :stop 3)) (li-return '(1 2))
          (li-closure 5) (let ((log (list nil))) (list (li-args-order log) (car log)))
          (li-recursive 5))))"#;
    let expected = "((20 2) :OUTER-B :STOPPED :DONE 16 ((:A :B) (2 1)) 120)";
    let log = agree(program, expected, &[("EGCL_LOG", "compile=trace")]);
    for name in ["GET-X", "LEAVE"] {
        assert!(
            log.contains(&format!("{name}\"}}: not open-coded")),
            "{name}'s call site rebinds a name it uses, so it keeps the closure:\n{log}"
        );
    }
    for name in ["CHECK", "ADDER", "PAIR"] {
        assert!(
            log.contains(&format!("{name}\"}}: open-coded")),
            "{name} is open-coded:\n{log}"
        );
    }
}

#[test]
fn generic_dispatch_with_keywords_and_several_primaries() {
    let program = r#"(progn
  (defgeneric gd (a b &key c d e f))
  (defmethod gd ((a integer) b &key c d e f) (list :int a b c d e f))
  (defmethod gd ((a number) b &key c d e f) (list :num a b c d e f))
  (defmethod gd ((a t) b &key c d e f) (list :t a b c d e f))
  (defgeneric gn (a))
  (defmethod gn ((a integer)) (list :int (call-next-method)))
  (defmethod gn ((a number)) (list :num (call-next-method)))
  (defmethod gn ((a t)) :t)
  (defgeneric g5 (a b c d e))
  (defmethod g5 (a b c d (e integer)) (list :int-e a e))
  (defmethod g5 (a b c d (e symbol)) (list :sym-e a e))
  (dotimes (i 40)
    (gd 1 2 :c 3 :d 4 :e 5 :f 6) (gd 1.5 2) (gd "x" 2 :f 1) (gn 1) (g5 1 2 3 4 5) (g5 1 2 3 4 :k))
  (let ((before (list (gd 1 2 :c 3 :d 4 :e 5 :f 6) (gd 1.5 2 :c 1) (gd "x" 2 :f 1) (gd 1 2)
                      (gn 1) (gn 1.5) (gn "s") (g5 1 2 3 4 5) (g5 1 2 3 4 :k) (g5 :a :b :c :d 7))))
    (defmethod gd ((a integer) b &key c d e f) (list :int2 a b c d e f))
    (format t "~s~%" (list before (gd 1 2 :c 3 :d 4 :e 5 :f 6)))))"#;
    let expected = "(((:INT 1 2 3 4 5 6) (:NUM 1.5 2 1 NIL NIL NIL) (:T \"x\" 2 NIL NIL NIL 1) \
(:INT 1 2 NIL NIL NIL NIL) (:INT (:NUM :T)) (:NUM :T) :T (:INT-E 1 5) (:SYM-E 1 :K) (:INT-E :A 7)) \
(:INT2 1 2 3 4 5 6))";
    agree(program, expected, &[]);
}
