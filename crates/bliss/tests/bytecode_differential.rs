//! Differential testing for the T0 bytecode backend (bliss-nmq.1).
//!
//! The bytecode backend (`BLISS_BACKEND=bytecode`) must be observationally
//! identical to the tree-walker oracle: for every program, running the real
//! `bliss-cli` binary with and without the flag must produce the same stdout
//! and the same exit status. Forms the compiler cannot lower fall back to the
//! tree-walker, so equality holds by construction there; forms it *can* lower
//! exercise the explicit-stack bytecode loop over real `BlissStack` frames.
//!
//! This is the "tree-walker as oracle + differential testing" discipline from
//! spec §4.6 (the SBCL/ECL two-backend model).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bliss-cli");

/// Run one `--eval` program under a given backend; return (stdout, exit_ok).
fn run(program: &str, bytecode: bool) -> (String, bool) {
    let mut cmd = Command::new(BIN);
    cmd.arg("--eval").arg(program);
    if bytecode {
        cmd.env("BLISS_BACKEND", "bytecode");
    } else {
        cmd.env_remove("BLISS_BACKEND");
    }
    let out = cmd.output().expect("spawn bliss-cli");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (stdout, out.status.success())
}

/// Assert both backends agree on stdout and success for a program.
fn assert_agree(program: &str) {
    let (tw_out, tw_ok) = run(program, false);
    let (bc_out, bc_ok) = run(program, true);
    assert_eq!(
        tw_out, bc_out,
        "stdout mismatch for program:\n  {program}\n  tree-walker: {tw_out:?}\n  bytecode:    {bc_out:?}"
    );
    assert_eq!(
        tw_ok, bc_ok,
        "exit-status mismatch for program:\n  {program}\n  tree-walker ok={tw_ok}, bytecode ok={bc_ok}"
    );
}

/// The differential corpus. Mixes forms the bytecode compiler lowers
/// (literals, IF, LET/LET*, var refs, arithmetic, recursion) with forms it
/// bails on (so the fallback path is exercised too).
const CORPUS: &[&str] = &[
    // Self-evaluating literals.
    "42",
    "-7",
    "3.5",
    "t",
    "nil",
    "(quote foo)",
    "(quote (a b c))",
    // Arithmetic (primitives delegate to the oracle).
    "(+ 1 2)",
    "(- 10 3 2)",
    "(* 2 3 4)",
    "(+ (* 2 3) (- 10 4))",
    // IF.
    "(if t 1 2)",
    "(if nil 1 2)",
    "(if (< 3 5) (quote yes) (quote no))",
    "(if (> 3 5) (quote yes) (quote no))",
    "(if (= 0 0) 100)",
    "(if (= 0 1) 100)",
    // LET / LET*.
    "(let ((n 5)) (if (< n 10) (+ n 1) 0))",
    "(let ((a 1) (b 2)) (+ a b))",
    "(let ((a 1) (b 2)) (let ((a 10)) (+ a b)))",
    "(let* ((a 1) (b (+ a 1))) (+ a b))",
    "(let ((x 3)) (let ((y 4)) (+ (* x x) (* y y))))",
    // Nested / parallel let semantics (b must not see the new a).
    "(let ((a 1)) (let ((a 2) (b a)) (+ a b)))",
    // PROGN.
    "(progn 1 2 3)",
    "(progn)",
    // User functions and recursion — the bytecode loop on BlissStack.
    "(defun sq (x) (* x x)) (sq 9)",
    "(defun add3 (a b c) (+ a b c)) (add3 4 5 6)",
    "(defun fact (n) (if (= n 0) 1 (* n (fact (- n 1))))) (fact 6)",
    "(defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 15)",
    "(defun down (n) (if (= n 0) (quote done) (down (- n 1)))) (down 50)",
    "(defun even2 (n) (if (= n 0) t (odd2 (- n 1)))) (defun odd2 (n) (if (= n 0) nil (even2 (- n 1)))) (even2 10)",
    // Forms that bail to the tree-walker (must still match).
    "(defun g (n) (cond ((= n 0) (quote zero)) (t (quote other)))) (g 0)",
    "(let ((lst (list 1 2 3))) (car lst))",
    "(mapcar (function 1+) (list 1 2 3))",
    "(format nil \"~a-~a\" 1 2)",
];

#[test]
fn bytecode_matches_tree_walker_on_corpus() {
    for program in CORPUS {
        assert_agree(program);
    }
}

/// Deep recursion under the bytecode backend is bounded by the `BlissStack`
/// capacity and raises a catchable `STORAGE-CONDITION` (R2.20) — it must not
/// abort the process.
#[test]
fn deep_recursion_raises_catchable_storage_condition() {
    let program = "(defun down (n) (if (= n 0) (quote done) (down (- n 1)))) \
                   (handler-case (down 100000000) (storage-condition () (quote caught)))";
    let (out, ok) = run(program, true);
    assert!(ok, "should exit cleanly after catching STORAGE-CONDITION");
    assert!(
        out.contains("CAUGHT"),
        "deep recursion should be caught as STORAGE-CONDITION, got: {out:?}"
    );
}

/// The `BlissStack` bound scales with `BLISS_STACK_SIZE`: a depth that fits a
/// large stack overflows a small one — proving the bound is the CL stack's
/// capacity, not the host Rust stack.
#[test]
fn recursion_bound_scales_with_stack_size() {
    let program = "(defun down (n) (if (= n 0) (quote done) (down (- n 1)))) (down 3000)";

    let big = Command::new(BIN)
        .arg("--eval")
        .arg(program)
        .env("BLISS_BACKEND", "bytecode")
        .env("BLISS_STACK_SIZE", "1m")
        .output()
        .expect("spawn");
    assert!(
        big.status.success(),
        "depth 3000 should fit a 1 MiB stack; stderr: {}",
        String::from_utf8_lossy(&big.stderr)
    );

    let small = Command::new(BIN)
        .arg("--eval")
        .arg(program)
        .env("BLISS_BACKEND", "bytecode")
        .env("BLISS_STACK_SIZE", "16k")
        .output()
        .expect("spawn");
    assert!(
        !small.status.success(),
        "depth 3000 should overflow a 16 KiB stack"
    );
}
