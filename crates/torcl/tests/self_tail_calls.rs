//! A self tail call reuses the frame instead of growing the stack (bliss-ieajy.3).
//!
//! `(defun count-down (n acc) (if (zerop n) acc (count-down (1- n) (1+ acc))))`
//! died with a FATAL, uncatchable "stack overflow in thread 1" somewhere between
//! 6,000 and 8,000 frames once the function was compiled. That is not a depth a
//! program has to be unusual to reach, and CL code written in the accumulator
//! style is ordinary code, not a trick.
//!
//! The pass runs in the LOWERER rather than in a native emitter, so every tier
//! gets it and there is one implementation to be right rather than four (there
//! are four T2 emitters: x86-64, AArch64, ppc64le, s390x).
//!
//! What has to survive the rewrite is the point of this file. The bead asks for
//! multiple values, dynamic bindings, block/cleanup state, and arity checks to
//! be preserved, so each is checked here rather than assumed:
//!
//!   * the accumulator loop itself, a million deep;
//!   * `(values 1 2 3)` returned through the loop, not truncated to one;
//!   * `RETURN-FROM` out of the middle, which is the implicit block DEFUN wraps
//!     every body in -- the block is established ONCE and deliberately not
//!     popped by the back-edge, so a stale or unpopped frame would show here;
//!   * a NON-tail self call, which must be left alone: its result is used, so
//!     turning it into a jump would compute the wrong answer;
//!   * a dynamic binding around the call, which the pass refuses to rewrite --
//!     checked for the right ANSWER, since a bail must be invisible;
//!   * a parameter reassigned before the call, the classic ordering hazard: the
//!     arguments must all be evaluated before any parameter slot is overwritten.
//!
//! Every case also runs with the pass switched off via TORCL_TCO_DISABLE, which
//! is how the numbers above were taken and what keeps this honest: the answers
//! must be identical either way, and only the reachable DEPTH may differ.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

/// Each program warms its functions with 500 shallow calls before the real one.
/// That is not padding: a DEFUN is interpreted by the tree-walker until it is
/// hot, and the tree-walker has its own much deeper limit -- so without the
/// warm-up these would test the interpreter and pass having never run a single
/// lowered instruction.
///
/// Cases that must give the same answer with the pass on and off.
const SAME_EITHER_WAY: &str = r#"
  (defun sum-to (n) (if (zerop n) 0 (+ n (sum-to (1- n)))))
  (defvar *depth* 0)
  (defun dyn-loop (n) (if (zerop n) *depth* (let ((*depth* (1+ *depth*))) (dyn-loop (1- n)))))
  (defun swap-args (a b n) (if (zerop n) (list a b) (swap-args b a (1- n))))
  (dotimes (i 500) (sum-to 5) (dyn-loop 3) (swap-args 1 2 3))
  (format t "NONTAIL ~A~%" (sum-to 1000))
  (format t "DYNAMIC ~A~%" (dyn-loop 50))
  (format t "SWAP ~A~%" (swap-args :x :y 101))
"#;

/// The COND shape, which is where adjacency was not enough. Each arm of a COND
/// lowers to a jump to a shared join point, so the self call is followed by
/// `Br`, not by the epilogue -- exactly UIOP's LEXICOGRAPHIC<, whose lowering
/// reads `CallNamed{self} ; Br(42) ; … ; 42 PopHandler ; 43 Return`.
const COND_SHAPED: &str = r#"
  (defun walk (n acc)
    (cond ((zerop n) acc)
          ((evenp n) (walk (1- n) (1+ acc)))
          (t (walk (1- n) acc))))
  ;; Four arms, and the tail call is the middle one rather than the last, so the
  ;; branch being threaded is a forward jump over the arms that follow it.
  (defun pick (n acc)
    (cond ((zerop n) (list :zero acc))
          ((> n 0) (pick (- n 1) (+ acc 2)))
          ((< n -5) (list :low acc))
          (t (list :neg acc))))
  ;; Multiple values out of a COND-shaped loop: they must not be truncated on
  ;; the way through the threaded branch.
  (defun cond-mv (n)
    (cond ((zerop n) (values :a :b :c))
          (t (cond-mv (1- n)))))
  (dotimes (i 500) (walk 5 0) (pick 5 0) (cond-mv 3))
  (format t "COND ~A~%" (walk 1000000 0))
  (format t "ARMS ~A~%" (pick 1000000 0))
  (format t "COND-VALUES ~A~%" (multiple-value-list (cond-mv 1000000)))
"#;

/// Cases that need the frame reuse to reach the depth they ask for.
const DEEP: &str = r#"
  (defun count-down (n acc) (if (zerop n) acc (count-down (1- n) (1+ acc))))
  (defun mv-loop (n) (if (zerop n) (values 1 2 3) (mv-loop (1- n))))
  (defun rf-loop (n acc)
    (if (> acc 50) (return-from rf-loop (list :early acc))
        (if (zerop n) acc (rf-loop (1- n) (1+ acc)))))
  (dotimes (i 500) (count-down 5 0) (mv-loop 3) (rf-loop 3 0))
  (format t "DEEP ~A~%" (count-down 1000000 0))
  (format t "VALUES ~A~%" (multiple-value-list (mv-loop 1000000)))
  (format t "RETURN-FROM ~A~%" (rf-loop 1000000 0))
"#;

fn run(program: &str, disable: bool) -> (bool, String, String) {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    if disable {
        command.env("TORCL_TCO_DISABLE", "1");
    }
    let output = command.output().expect("run torcl");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn expect(stdout: &str, stderr: &str, lines: &[&str]) {
    for line in lines {
        assert!(
            stdout.lines().any(|l| l.trim() == *line),
            "missing {line:?} in:\n{stdout}\n{stderr}"
        );
    }
}

#[test]
fn the_rewrite_does_not_change_any_answer() {
    for disable in [false, true] {
        let (ok, stdout, stderr) = run(SAME_EITHER_WAY, disable);
        assert!(ok, "disable={disable}: {stdout}\n{stderr}");
        expect(
            &stdout,
            &stderr,
            &[
                // Not a tail call -- the result is consumed by +.
                "NONTAIL 500500",
                // The pass refuses a body that binds a special, so the binding
                // still nests 50 deep and the innermost call sees 50.
                "DYNAMIC 50",
                // Odd count, so the arguments end up exchanged. Wrong if the
                // rewrite stored A before evaluating the argument that reads it.
                "SWAP (Y X)",
            ],
        );
    }
}

#[test]
fn a_self_tail_call_runs_a_million_deep() {
    let (ok, stdout, stderr) = run(DEEP, false);
    assert!(ok, "{stdout}\n{stderr}");
    expect(
        &stdout,
        &stderr,
        &[
            "DEEP 1000000",
            // Not "(1)": the values must not be truncated on the way out.
            "VALUES (1 2 3)",
            // Stops at 51, so it left through the RETURN-FROM, not the base case.
            "RETURN-FROM (EARLY 51)",
        ],
    );
}

#[test]
fn a_cond_arm_is_a_tail_call_too() {
    let (ok, stdout, stderr) = run(COND_SHAPED, false);
    assert!(ok, "{stdout}\n{stderr}");
    expect(
        &stdout,
        &stderr,
        &[
            // Half the million are even, so acc counts them.
            "COND 500000",
            "ARMS (ZERO 2000000)",
            "COND-VALUES (A B C)",
        ],
    );
    // And the same answers without the pass, at a depth both can reach.
    let shallow = COND_SHAPED.replace("1000000", "300");
    let (ok, plain, err) = run(&shallow, true);
    assert!(ok, "{plain}\n{err}");
    let (ok, opt, err2) = run(&shallow, false);
    assert!(ok, "{opt}\n{err2}");
    assert_eq!(plain, opt, "the rewrite changed a COND-shaped answer");
}

#[test]
fn the_loop_keeps_its_roots_straight_under_gc_stress() {
    // The rewrite stores into parameter SLOTS on a back-edge, which is a write
    // to GC-scanned frame state on a path the lowerer did not previously emit.
    // An accumulator that CONSES makes every iteration allocate, so with a
    // collection forced at (almost) every allocation the list is rebuilt under
    // continuous relocation -- and poison turns a stale slot into a segfault
    // rather than a quietly wrong answer.
    //
    // 3,000 deep, not the million above: this is about whether the roots hold,
    // and one forced collection per cons is expensive enough that depth would
    // buy minutes and no extra information.
    let program = r#"
      (defun cd-gc (n acc) (if (zerop n) acc (cd-gc (1- n) (cons n acc))))
      (dotimes (i 500) (cd-gc 5 nil))
      (let ((r (cd-gc 3000 nil)))
        (format t "GC ~A ~A ~A~%" (length r) (first r) (car (last r))))
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("TORCL_GC_STRESS", "1")
        .env("TORCL_GC_POISON", "1")
        .output()
        .expect("run torcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}
{stderr}");
    expect(&stdout, &stderr, &["GC 3000 1 3000"]);
}

#[test]
fn and_that_depth_is_the_passs_doing() {
    // The guard against this test quietly passing for the wrong reason: with the
    // pass off the very same program must fail to reach that depth. If this ever
    // starts succeeding, the depth above no longer demonstrates anything.
    let (ok, stdout, stderr) = run(DEEP, true);
    assert!(
        !ok || !stdout.lines().any(|l| l.trim() == "DEEP 1000000"),
        "a million frames succeeded with TORCL_TCO_DISABLE=1:\n{stdout}\n{stderr}"
    );
}
