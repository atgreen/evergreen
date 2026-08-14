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
    // ── Non-local control flow (nmq.4) — all compiled to bytecode ──
    "(block foo 1 2 3)",
    "(block foo (return-from foo 42) 99)",
    "(block nil (return 7) 8)",
    "(defun f (x) (block b (if (< x 0) (return-from b (quote neg))) (* x 2))) (list (f -1) (f 5))",
    "(catch (quote tag) 1 2 3)",
    "(catch (quote tag) (throw (quote tag) 42) 99)",
    "(catch (quote a) (catch (quote b) (throw (quote a) 10)) 20)",
    "(catch (quote a) (catch (quote b) (throw (quote b) 10)) 20)",
    "(defun th (n) (throw (quote esc) n)) (catch (quote esc) (th 77) 0)",
    "(let ((acc 0)) (tagbody (setq acc 1) (go skip) (setq acc 999) skip (setq acc (+ acc 10))) acc)",
    "(let ((i 0) (s 0)) (tagbody top (setq s (+ s i)) (setq i (+ i 1)) (if (< i 5) (go top))) s)",
    "(let ((log nil)) (unwind-protect (setq log (cons 1 log)) (setq log (cons 2 log))) log)",
    "(let ((log nil)) (catch (quote e) (unwind-protect (throw (quote e) 0) (setq log (cons 99 log)))) log)",
    "(defun g2 (n) (block b (unwind-protect (if (= n 0) (return-from b (quote done)) n) 111))) (g2 0)",
    // Nested unwind-protects all run their cleanups on a throw.
    "(let ((log nil)) (catch (quote e) (unwind-protect (unwind-protect (throw (quote e) 0) (setq log (cons 1 log))) (setq log (cons 2 log)))) log)",
    // return-from crossing an unwind-protect runs the cleanup.
    "(let ((log nil)) (defun h () (block b (unwind-protect (return-from b 5) (setq log (cons 9 log))))) (list (h) log))",
    // SETQ on locals.
    "(let ((x 1)) (setq x (+ x 10)) (setq x (* x 2)) x)",
    // Error propagation (bytecode Propagate path) runs unwind-protect cleanups:
    // a host-call TypeError unwinds through a compiled unwind-protect whose
    // cleanup throws, superseding the error (correct CL semantics).
    "(defun error-out () (car 5)) (defun work () (unwind-protect (error-out) (throw (quote cu) (quote cleanup-ran)))) (catch (quote cu) (work))",
    // A throw crossing many compiled frames.
    "(defun dig (n) (if (= n 0) (throw (quote out) (quote bottom)) (dig (- n 1)))) (catch (quote out) (dig 500))",
    // ── Global / special variable access (nmq.5, LOAD/STORE_SPECIAL) ──
    "(defvar *g* 10) (defun rd () (+ *g* 1)) (rd)",
    "(defparameter *p* 5) (defun bump () (setq *p* (+ *p* 100))) (bump) *p*",
    "(defconstant +k+ 42) (defun kk () +k+) (kk)",
    "(defparameter *c* 0) (defun tick () (setq *c* (+ *c* 1))) (tick) (tick) (tick) *c*",
    // Unbound special read errors the same way in both backends.
    "(defun add-pi () (+ 0 pi)) (add-pi)",
    // ── HANDLER-CASE on bytecode (nmq.7) ──
    "(defun f () (handler-case (car 5) (type-error (e) (quote caught-te)))) (f)",
    "(defun f () (handler-case (+ 1 2) (error (e) (quote nope)))) (f)",
    "(defun f () (handler-case (/ 1 0) (division-by-zero () (quote div0)))) (f)",
    "(defun f () (handler-case (error \"boom\") (error (e) (quote caught-err)))) (f)",
    "(defun f () (handler-case (car 5) (division-by-zero () (quote wrong)) (error (e) (quote generic)))) (f)",
    "(defun sig () (error \"x\")) (defun f () (handler-case (sig) (error (e) (quote from-callee)))) (f)",
    "(defun f () (handler-case (car 5) (error (e) (type-of e)))) (f)",
    "(define-condition my-err (error) ()) (defun g () (error (quote my-err))) (defun f () (handler-case (g) (my-err () (quote got-mine)))) (f)",
    "(defun f () (handler-case (handler-case (car 5) (division-by-zero () (quote inner))) (type-error (e) (quote outer)))) (f)",
    "(defun f () (catch (quote out) (handler-case (car 5) (error (e) (throw (quote out) (quote clause-threw)))))) (f)",
    "(defvar *l* nil) (defun f () (setq *l* nil) (handler-case (unwind-protect (car 5) (setq *l* (cons 1 *l*))) (error (e) *l*))) (f)",
    "(defun f () (handler-case (car 5) (t () (quote catchall)))) (f)",
    "(defun f () (handler-case (car 5) (division-by-zero () (quote no)))) (handler-case (f) (type-error () (quote outer-caught)))",
    // ── HANDLER-BIND on bytecode (nmq.7) ──
    "(defun f () (handler-case (handler-bind ((error (lambda (c) (declare (ignore c)) nil))) (car 5)) (error () (quote outer)))) (f)",
    "(defun f () (catch (quote out) (handler-bind ((error (lambda (c) (declare (ignore c)) (throw (quote out) (quote handled))))) (car 5)))) (f)",
    "(defun f () (block b (handler-bind ((error (lambda (c) (declare (ignore c)) (return-from b (quote via-handler))))) (car 5)))) (f)",
    "(defun f () (handler-bind ((error (lambda (c) c))) (+ 2 3))) (f)",
    "(define-condition my-c (condition) ()) (defun f () (block b (handler-bind ((my-c (lambda (c) (declare (ignore c)) (return-from b (quote sig-handled))))) (signal (quote my-c))))) (f)",
    // ── RESTART-CASE on bytecode (nmq.7) ──
    "(defun f () (restart-case (+ 1 2) (use-value (v) v))) (f)",
    "(defun f () (handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart (quote use-value) 99)))) (restart-case (error \"x\") (use-value (v) v)))) (f)",
    "(defvar *g* 7) (defun f () (handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart (quote r) 3)))) (restart-case (error \"x\") (r (v) (+ v *g*))))) (f)",
    "(defun f (x) (handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart (quote r) 3)))) (restart-case (error \"x\") (r (v) (+ v x))))) (f 10)",
    "(defun f () (handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart (quote continue))))) (restart-case (progn (error \"x\") 5) (continue () 42)))) (f)",
    "(defun g (n) (if (< n 0) (error \"neg\") (* n 2))) (defun f () (handler-case (g -1) (error () (quote was-neg)))) (f)",
];

/// Programs that must run on the bytecode backend (not fall back). Each is a
/// single top-level form whose last trace line under `BLISS_BYTECODE_TRACE`
/// must be `compiled`.
const MUST_COMPILE: &[&str] = &[
    "(block foo (return-from foo 42) 99)",
    "(catch (quote tag) (throw (quote tag) 42) 99)",
    "(let ((i 0) (s 0)) (tagbody top (setq s (+ s i)) (setq i (+ i 1)) (if (< i 5) (go top))) s)",
    "(let ((log nil)) (catch (quote e) (unwind-protect (throw (quote e) 0) (setq log (cons 99 log)))) log)",
    "(let ((n 5)) (if (< n 10) (+ n 1) 0))",
    "(let ((x 1)) (setq x (+ x 10)) x)",
    "(defvar *gg* 3) (defun rr () (setq *gg* (* *gg* 2)))",
    "(defun hc () (handler-case (car 5) (error (e) (quote caught))))",
];

#[test]
fn bytecode_matches_tree_walker_on_corpus() {
    for program in CORPUS {
        assert_agree(program);
    }
}

/// Confirm the control-flow programs actually execute on the bytecode backend
/// rather than silently bailing to the tree-walker (which would make the
/// differential agreement vacuous).
#[test]
fn control_flow_programs_run_on_bytecode() {
    for program in MUST_COMPILE {
        let out = Command::new(BIN)
            .arg("--eval")
            .arg(program)
            .env("BLISS_BACKEND", "bytecode")
            .env("BLISS_BYTECODE_TRACE", "1")
            .output()
            .expect("spawn");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last = stderr
            .lines()
            .filter(|l| l.starts_with("[bytecode]"))
            .next_back()
            .unwrap_or("");
        assert_eq!(
            last, "[bytecode] compiled",
            "program should compile to bytecode, not bail:\n  {program}\n  last trace: {last:?}"
        );
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
