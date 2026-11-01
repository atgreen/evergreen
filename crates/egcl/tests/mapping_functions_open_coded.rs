// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Open-coded mapping functions (bliss-gg3i1.7.13).
//!
//! Compiled code expands MAPC, MAPCAR, MAPCAN, MAPL, MAPLIST, MAPCON, SOME,
//! EVERY, NOTANY and NOTEVERY over a few lists into loops. Every observable
//! result must stay what the builtins (and the tree-walker) give: return
//! values, the shortest list deciding the length, function designators
//! (a quoted symbol resolves at each call, `#'f` once), nonlocal exits from
//! the mapped function, the errors for improper lists, and NCONC joining for
//! MAPCAN/MAPCON. Each tier, the transform switched off, and a GC-stressed run
//! all have to print the same line.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const DEFINITIONS: &str = r#"
(defun f1 (x) (* x 10))
(defun f2 (x y) (+ x y))
(defvar *trace* nil)
(defun rec (x) (push x *trace*) x)
(defun install-redef ()
  (setf (fdefinition 'redef)
        (lambda (x)
          (when (= x 2) (setf (fdefinition 'redef) (lambda (y) (- y))))
          x)))
(defun t-mapc-ret (l) (setq *trace* nil) (let ((r (mapc #'rec l))) (list (eq r l) (reverse *trace*))))
(defun t-mapc2 (a b) (let ((acc nil)) (mapc (lambda (x y) (push (list x y) acc)) a b) (nreverse acc)))
(defun t-mapcar1 (l) (mapcar #'f1 l))
(defun t-mapcar2 (a b) (mapcar #'f2 a b))
(defun t-mapcar3 (a b c) (mapcar #'list a b c))
(defun t-mapcar-closure (l k) (mapcar (lambda (x) (+ x k)) l))
(defun t-mapcar-car (l) (mapcar #'car l))
(defun t-mapcar-sym (l) (install-redef) (mapcar 'redef l))
(defun t-mapcar-fn (l) (install-redef) (mapcar #'redef l))
(defun t-mapcan (l) (mapcan (lambda (x) (if (evenp x) (list x x) nil)) l))
(defun t-mapcan-share (a b) (let ((r (mapcan #'identity (list a b)))) (list r (eq r a) a)))
(defun t-mapcan-tail (l) (mapcan (lambda (x) (if (= x 3) 'end (list x))) l))
(defun t-mapcan2 (a b) (mapcan (lambda (x y) (list x y)) a b))
(defun t-maplist (a b) (maplist #'list a b))
(defun t-mapl (l) (let ((acc nil)) (list (eq (mapl (lambda (s) (push (length s) acc)) l) l) acc)))
(defun t-mapcon (l) (mapcon (lambda (s) (list (length s))) l))
(defun t-some (l) (some (lambda (x) (and (> x 2) (* x 100))) l))
(defun t-some2 (a b) (some #'> a b))
(defun t-every (l) (every #'numberp l))
(defun t-every2 (a b) (every #'< a b))
(defun t-notany (l) (notany #'symbolp l))
(defun t-notevery (l) (notevery #'oddp l))
(defun t-some-vec (v) (some (lambda (x) (and (char= x #\b) x)) v))
(defun t-every-mixed (l v) (every (lambda (x y) (eql x y)) l v))
(defun t-early (l) (block out (mapc (lambda (x) (when (> x 2) (return-from out (list :exit x)))) l) :none))
(defun t-return (l) (dolist (x '(1)) (mapc (lambda (y) (when (= y 2) (return (list :ret x y)))) l)))
(defun t-throw (l) (catch 'tag (mapcar (lambda (x) (if (= x 3) (throw 'tag :thrown) x)) l)))
(defun t-go (l) (let ((n 0)) (tagbody (mapc (lambda (x) (incf n x) (when (> n 5) (go done))) l) done) n))
(defun t-capture (l) (mapcar #'funcall (mapcar (lambda (x) (lambda () (* x x))) l)))
(defun t-setq-outer (l) (let ((sum 0)) (mapc (lambda (x) (setq sum (+ sum x))) l) sum))
(defun t-docstring (l) (mapcar (lambda (x) "doc" (1+ x)) l))
(defun t-declare (l) (mapcar (lambda (x y) (declare (ignore y) (fixnum x)) (* 2 x)) l l))
(defun t-optional (l) (mapcar (lambda (x &optional (y 5)) (+ x y)) l))
(defun t-nested (l) (mapcar (lambda (x) (mapcar (lambda (y) (* x y)) l)) l))
(defun t-notinline (l) (declare (notinline mapcar)) (mapcar #'1+ l))
(defun t-err (thunk)
  (handler-case (funcall thunk)
    (type-error (e) (list :type-error (type-error-datum e)))
    (error (e) (list :error (type-of e)))))
(defun t-errors ()
  (list (t-err (lambda () (mapcar #'f1 '(1 2 . 3))))
        (t-err (lambda () (mapc #'f1 5)))
        (t-err (lambda () (mapcar #'f2 '(1) '(1 2 . 3))))
        (t-err (lambda () (some #'evenp '(1 3 . 4))))
        (t-err (lambda () (mapl #'identity '(1 2 . 3))))
        (t-err (lambda () (maplist #'identity 7)))
        (t-err (lambda () (mapcan #'identity (list (list 1) 2 (list 3)))))
        (t-err (lambda () (every #'evenp 7)))
        (t-err (lambda () (mapcar 'no-such-function-xyz '(1))))
        (t-err (lambda () (mapcar #'car '(1))))))
(defun t-error-before-call ()
  (let ((n 0))
    (list (t-err (lambda () (mapcar (lambda (x) (incf n) x) '(1 2 . 3)))) n)))
(defun t-mv (l) (multiple-value-list (mapcar #'floor l)))
(defun t-mv2 (l) (multiple-value-list (some (lambda (x) (floor x 2)) l)))
(defun t-mv3 (l) (multiple-value-list (mapc #'floor l)))
(defun t-mv4 (l) (multiple-value-list (every (lambda (x) (floor x 2)) l)))
(defun all-results ()
  (list (t-mapc-ret (list 1 2 3))
        (t-mapc2 '(1 2 3) '(a b))
        (t-mapcar1 '(1 2 3)) (t-mapcar1 nil)
        (t-mapcar2 '(1 2 3) '(10 20))
        (t-mapcar3 '(1 2 3) '(a b c d) '(x y z))
        (t-mapcar-closure '(1 2 3) 100)
        (t-mapcar-car '((a . 1) (b . 2)))
        (t-mapcar-sym '(1 2 3 4))
        (t-mapcar-fn '(1 2 3 4))
        (t-mapcan '(1 2 3 4))
        (t-mapcan-share (list 1 2) (list 3))
        (t-mapcan-tail '(1 2 3))
        (t-mapcan2 '(1 2 3) '(a b))
        (t-maplist '(1 2 3) '(a b))
        (t-mapl (list 1 2 3))
        (t-mapcon '(1 2 3))
        (t-some '(1 2 3 4)) (t-some '(1 2))
        (t-some2 '(1 5 3) '(2 4 6)) (t-some2 '(1 2) '(5 6 0))
        (t-every '(1 2 3)) (t-every '(1 a 3))
        (t-every2 '(1 2) '(2 3 0)) (t-every2 '(1 5) '(2 3))
        (t-notany '(1 2)) (t-notany '(1 a))
        (t-notevery '(1 3)) (t-notevery '(1 2))
        (t-some-vec "abc") (t-some-vec (vector #\x #\y))
        (t-every-mixed '(1 2 3) (vector 1 2)) (t-every-mixed '(1 2) (vector 1 3))
        (t-early '(1 2 3 4)) (t-early '(1 2))
        (t-return '(1 2 3))
        (t-throw '(1 2 3 4)) (t-throw '(1 2))
        (t-go '(1 2 3 4 5)) (t-go '(1 1))
        (t-capture '(1 2 3))
        (t-setq-outer '(1 2 3 4))
        (t-docstring '(1 2))
        (t-declare '(1 2 3))
        (t-optional '(1 2))
        (t-nested '(1 2 3))
        (t-notinline '(1 2))
        (t-errors)
        (t-error-before-call)
        (t-mv '(1 2)) (t-mv2 '(1 3 5)) (t-mv3 '(1 2)) (t-mv4 '(2 3))))
"#;

/// The tree-walker's answer, which every configuration must reproduce.
/// Notable entries: `'redef` sees the redefinition made while mapping
/// (`(1 2 -3 -4)`) but `#'redef` does not (`(1 2 3 4)`); MAPCAN joins with
/// NCONC, so its result is EQ to, and extends, the first list
/// (`((1 2 3) T (1 2 3))`); an improper list signals before the function runs
/// (`((:TYPE-ERROR 3) 0)`).
const EXPECTED: &str = "RESULTS ((T (1 2 3)) ((1 A) (2 B)) (10 20 30) NIL (11 22) \
((1 A X) (2 B Y) (3 C Z)) (101 102 103) (A B) (1 2 -3 -4) (1 2 3 4) (2 2 4 4) \
((1 2 3) T (1 2 3)) (1 2 . END) (1 A 2 B) (((1 2 3) (A B)) ((2 3) (B))) (T (1 2 3)) \
(3 2 1) 300 NIL T NIL T NIL T NIL T NIL NIL T #\\b NIL T NIL (:EXIT 3) :NONE (:RET 1 2) \
:THROWN (1 2) 6 2 (1 4 9) 10 (2 3) (2 4 6) (6 7) ((1 2 3) (2 4 6) (3 6 9)) (2 3) \
((:TYPE-ERROR 3) (:TYPE-ERROR 5) (:TYPE-ERROR 3) (:TYPE-ERROR 4) (:TYPE-ERROR 3) \
(:TYPE-ERROR 7) (:TYPE-ERROR 2) (:TYPE-ERROR 7) (:ERROR UNDEFINED-FUNCTION) (:TYPE-ERROR 1)) \
((:TYPE-ERROR 3) 0) ((1 2)) (0) ((1 2)) (T))";

fn run(program: &str, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(BIN);
    cmd.args(["--no-init", "--eval", program]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_NO_MAPPING_TRANSFORMS",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("EGCL_LAZY_COMPILE", "0");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn results(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|line| line.starts_with("RESULTS"))
        .unwrap_or_else(|| panic!("no RESULTS line in:\n{stdout}"))
}

/// `warm` runs first, so the default configuration reaches the native tiers.
fn program(warm: usize) -> String {
    format!(
        "{DEFINITIONS}
(dotimes (i {warm}) (all-results))
(format t \"RESULTS ~S~%\" (all-results))"
    )
}

#[test]
fn mapping_results_match_in_every_tier() {
    let program = program(40);
    for envs in [
        &[("EGCL_FORCE_TIER", "interp")][..],
        &[("EGCL_FORCE_TIER", "t0")],
        &[("EGCL_FORCE_TIER", "t1")],
        &[("EGCL_FORCE_TIER", "t2")],
        &[],
        &[("EGCL_NO_MAPPING_TRANSFORMS", "1")],
    ] {
        let out = run(&program, envs);
        assert_eq!(results(&out), EXPECTED, "{envs:?}\n{out}");
    }
}

/// A minor GC at every allocation after start-up, with freed nursery memory
/// poisoned, must leave the answers unchanged: the expansion is built and the
/// loops run with every value rooted.
#[test]
fn mapping_results_survive_gc_stress() {
    let out = run(
        &program(2),
        &[
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ],
    );
    assert_eq!(results(&out), EXPECTED, "{out}");
}

/// Warm compiled callers no longer call MAPC or MAPCAN (the `&rest`
/// definitions in lib/boot.lisp) at all; with the transform off they do.
#[test]
fn compiled_callers_do_not_call_the_mapping_functions() {
    let program = r#"
(defun use-mapc (l) (mapc #'identity l))
(defun use-mapcan (l) (mapcan #'list l))
(defun use-some (l) (some #'null l))
(dotimes (i 50) (use-mapc '(1 2)) (use-mapcan '(1 2)) (use-some '(1 2)))
(let ((a (egcl-ext:function-invoke-count 'mapc))
      (b (egcl-ext:function-invoke-count 'mapcan)))
  (dotimes (i 100) (use-mapc '(1 2)) (use-mapcan '(1 2)) (use-some '(1 2)))
  (format t "COUNTS ~S~%"
          (list (- (egcl-ext:function-invoke-count 'mapc) a)
                (- (egcl-ext:function-invoke-count 'mapcan) b))))
"#;
    let line = |out: &str| {
        out.lines()
            .find(|line| line.starts_with("COUNTS"))
            .unwrap_or_else(|| panic!("no COUNTS line in:\n{out}"))
            .to_owned()
    };
    let open_coded = run(program, &[]);
    assert_eq!(line(&open_coded), "COUNTS (0 0)", "{open_coded}");
    let called = run(program, &[("EGCL_NO_MAPPING_TRANSFORMS", "1")]);
    assert_eq!(line(&called), "COUNTS (100 100)", "{called}");
}
