//! Differential testing for the T0 bytecode backend (bliss-nmq.1).
//!
//! The bytecode backend (`TORCL_BACKEND=bytecode`) must be observationally
//! identical to the tree-walker oracle: for every program, running the real
//! `torcl` binary with and without the flag must produce the same stdout
//! and the same exit status. Forms the compiler cannot lower fall back to the
//! tree-walker, so equality holds by construction there; forms it *can* lower
//! exercise the explicit-stack bytecode loop over real `TorclStack` frames.
//!
//! This is the "tree-walker as oracle + differential testing" discipline from
//! spec §4.6 (the SBCL/ECL two-backend model).

use std::process::Command;
use std::sync::{Mutex, OnceLock};

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

fn gc_stress_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Run one `--eval` program under a given backend; return (stdout, exit_ok).
fn run(program: &str, bytecode: bool, bootstrap: bool) -> (String, bool) {
    let mut cmd = Command::new(BIN);
    // Skipping bootstrap cuts per-spawn startup from ~100ms to ~0. The corpus
    // spawns the CLI twice per program, so this dominates the suite's wall-clock.
    if !bootstrap {
        cmd.arg("--no-bootstrap");
    }
    cmd.arg("--eval").arg(program);
    if bytecode {
        // Bytecode is the default now; be explicit anyway.
        cmd.env("TORCL_BACKEND", "bytecode");
    } else {
        // The tree-walker is the differential oracle.
        cmd.env("TORCL_BACKEND", "tree-walker");
    }
    let out = cmd.output().expect("spawn torcl");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (stdout, out.status.success())
}

/// Assert both backends agree on stdout and success for a program.
///
/// Compares without bootstrap first (fast). A handful of forms — e.g. `CASE` —
/// are lowered natively by the bytecode compiler but provided to the tree-walker
/// as bootstrap macros, so a no-bootstrap mismatch is retried with bootstrap
/// (giving both backends the same macro environment) before it is reported.
/// This keeps full corpus coverage while paying the ~100ms bootstrap cost only
/// for the few programs that actually need it.
fn assert_agree(program: &str) {
    let (tw, tw_ok) = run(program, false, false);
    let (bc, bc_ok) = run(program, true, false);
    if tw == bc && tw_ok == bc_ok {
        return;
    }
    let (tw, tw_ok) = run(program, false, true);
    let (bc, bc_ok) = run(program, true, true);
    assert_eq!(
        tw, bc,
        "stdout mismatch (with bootstrap) for program:\n  {program}\n  tree-walker: {tw:?}\n  bytecode:    {bc:?}"
    );
    assert_eq!(
        tw_ok, bc_ok,
        "exit-status mismatch (with bootstrap) for program:\n  {program}\n  tree-walker ok={tw_ok}, bytecode ok={bc_ok}"
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
    "(let ((x 3)) `(a ,x))",
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
    // User functions and recursion — the bytecode loop on TorclStack.
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
    // A handler lambda directly capturing an enclosing let local (visible to the
    // pre-scan, so boxed and reachable) must stay correct on bytecode.
    "(defvar *o* nil) (defun f () (let ((secret 42)) (handler-case (handler-bind ((error (lambda (c) (declare (ignore c)) (setf *o* secret)))) (error \"boom\")) (error () nil)) *o*)) (f)",
    // A handler lambda that captures an enclosing local only revealed AFTER macro
    // expansion (compute_captured_names pre-scan miss): the local lives in a plain
    // frame slot the handler cannot reach, so lowering must bail to the tree-walker
    // rather than silently lose it (bliss-pgu, sibling of the restart-case guard).
    "(defvar *o* nil) (defmacro hbm () (quote (handler-bind ((error (lambda (c) (declare (ignore c)) (setf *o* secret)))) (error \"boom\")))) (defun f () (let ((secret 42)) (handler-case (hbm) (error () nil)) *o*)) (f)",
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
    // bliss-9kc: a HANDLER-BIND handler must be able to INVOKE-RESTART a restart
    // established INSIDE its body, even when the condition is a RAW evaluator error
    // (TYPE-ERROR from (car 5)) — which never passed through SIGNAL. Both tiers must
    // signal in-context, before the restart is disestablished.
    "(defun f () (handler-bind ((type-error (lambda (e) (declare (ignore e)) (invoke-restart (quote k))))) (restart-case (car 5) (k () (quote ok))))) (f)",
    // A declining HANDLER-BIND handler runs exactly once, then an outer HANDLER-CASE
    // still catches the (raw) condition.
    "(defvar *n* 0) (defun f () (setq *n* 0) (handler-case (handler-bind ((type-error (lambda (e) (declare (ignore e)) (incf *n*)))) (restart-case (car 5) (k () 1))) (type-error (e) (declare (ignore e)) (list :outer *n*)))) (f)",
    // Invoke an OUTER restart from inside a nested RESTART-CASE body on a raw error.
    "(defun f () (handler-bind ((type-error (lambda (e) (declare (ignore e)) (invoke-restart (quote outer))))) (restart-case (restart-case (car 5) (inner () :inner)) (outer () :outer)))) (f)",
    // An unhandled raw error inside a RESTART-CASE still propagates (and is caught by
    // an enclosing HANDLER-CASE) rather than vanishing.
    "(defun f () (handler-case (restart-case (car 5) (k () 1)) (type-error (e) (declare (ignore e)) :caught))) (f)",
    // ── Multiple values on bytecode (nmq.5) ──
    "(values 1 2 3)",
    "(multiple-value-bind (a b) (values 1 2) (list a b))",
    "(multiple-value-bind (a b c) (values 10 20) (list a b c))",
    "(multiple-value-bind (a b) 42 (list a b))",
    "(multiple-value-list (values 1 2 3))",
    "(multiple-value-list 7)",
    // mv discipline quirks — must match the tree-walker exactly:
    "(multiple-value-bind (a b) (progn (values 1 2) 3) (list a b))",
    "(multiple-value-bind (a b) (+ (values 1 2) 3) (list a b))",
    "(defun f () (multiple-value-bind (a b) (let ((x 0)) (setq x (values 1 2)) x) (list a b))) (f)",
    "(defun vv () (values 7 8 9)) (multiple-value-bind (a b c) (vv) (list a b c))",
    "(defun sv () (+ 1 2)) (multiple-value-bind (a b) (sv) (list a b))",
    "(multiple-value-bind (q r) (floor 17 5) (list q r))",
    "(multiple-value-bind (a b) (values 1 2) (multiple-value-bind (c d) (values 3 4) (list a b c d)))",
    "(multiple-value-bind (a b) (if t (values 1 2) 0) (list a b))",
    // ── Non-capturing closures / higher-order on bytecode (nmq.5) ──
    "(funcall (function +) 3 4)",
    "(funcall (lambda (x) (* x x)) 6)",
    "(mapcar (lambda (x) (* x 2)) (list 1 2 3))",
    "(mapcar (function 1+) (list 10 20))",
    "(apply (function +) (list 1 2 3 4))",
    "(reduce (function +) (list 1 2 3 4 5))",
    "(defun compose2 (f g x) (funcall f (funcall g x))) (compose2 (function 1+) (function 1+) 5)",
    "(remove-if (lambda (x) (> x 3)) (list 1 2 3 4 5))",
    // ── Capturing closures on bytecode (nmq.5) ──
    "(defun adder (n) (lambda (x) (+ x n))) (funcall (adder 10) 5)",
    "(defun make-counter () (let ((n 0)) (lambda () (setq n (+ n 1))))) (let ((c (make-counter))) (list (funcall c) (funcall c) (funcall c)))",
    "(defun f () (let ((k 100)) (funcall (lambda (x) (+ x k)) 7))) (f)",
    "(defun pair () (let ((n 0)) (list (lambda () (setq n (+ n 1))) (lambda () n)))) (let ((p (pair))) (funcall (first p)) (funcall (first p)) (funcall (second p)))",
    "(defun add-n (n lst) (mapcar (lambda (x) (+ x n)) lst)) (add-n 100 (list 1 2 3))",
    "(defun adders () (let ((a 1) (b 2)) (list (lambda () a) (lambda () b)))) (let ((l (adders))) (list (funcall (first l)) (funcall (second l))))",
    // Captured variadic parameters are boxed after the shared lambda-list
    // binder resolves defaults, supplied-p flags, rest lists, and keywords.
    "(defun captured-opt (&optional (x 41 xp)) (funcall (lambda () (list x xp)))) (list (captured-opt) (captured-opt 9))",
    "(defun captured-rest (&rest xs) (funcall (lambda () (length xs)))) (captured-rest 1 2 3 4)",
    "(defun captured-key (&key (x 7 xp)) (funcall (lambda () (list x xp)))) (list (captured-key) (captured-key :x 12))",
    "(flet ((captured-local (&optional (x 4)) (funcall (lambda () x)))) (list (captured-local) (captured-local 9)))",
    // ── Non-local GO out of a capturing closure (bliss-x8t) ──
    // A `go` in a lambda/flet closure targeting a tag in the enclosing function
    // lowers to GoNamed and unwinds through the shared tag-token stack.
    "(let ((r nil)) (tagbody (funcall (lambda () (go done))) (setq r :nope) done (setq r :ok)) r)",
    "(let ((r nil)) (tagbody (flet ((f () (go done))) (f)) (setq r :nope) done (setq r :ok)) r)",
    // Resume at the correct named tag among several, not merely the last.
    "(let ((r nil)) (tagbody (funcall (lambda () (go two))) one (setq r (cons :one r)) two (setq r (cons :two r))) r)",
    // A closure-driven loop: the non-local go is a back-edge re-entered per call.
    "(let ((i 0) (log nil)) (tagbody top (when (< i 3) (funcall (lambda () (setq log (cons i log)) (setq i (+ i 1)) (go top)))) done) (list i log))",
    // Nested closures each unwinding a frame to the same enclosing tag.
    "(let ((r nil)) (tagbody (funcall (lambda () (funcall (lambda () (go out))))) (setq r :nope) out (setq r :ok)) r)",
    // Non-local go crossing an unwind-protect runs the cleanup.
    "(let ((log nil)) (tagbody (unwind-protect (funcall (lambda () (go done))) (setq log (cons :cleanup log))) (setq log (cons :nope log)) done (setq log (cons :done log))) log)",
    // A tree-walked closure whose go targets an eager-compiled enclosing tagbody
    // (cross-backend token bridge): mapcar's lambda bails to T0 yet still exits.
    "(let ((r nil)) (tagbody (mapcar (lambda (x) (declare (ignore x)) (go done)) (list 1)) (setq r :nope) done (setq r :ok)) r)",
    // A tree-walked closure called from T0 must update the boxed binding in the
    // bytecode frame, even when an older captured frame has the same variable.
    "(defparameter *capture-table* (make-hash-table)) (defun capture-table-keys (table) (let ((keys nil)) (maphash (lambda (key value) (declare (ignore value)) (push key keys)) table) keys)) (let ((keys nil)) (declare (ignore keys)) (defmacro captured-key-count (&key (items (capture-table-keys *capture-table*))) (list (quote quote) (length items)))) (setf (gethash (quote a) *capture-table*) 1) (setf (gethash (quote b) *capture-table*) 2) (captured-key-count)",
    "(defun hash-surfaces () (let ((h (make-hash-table))) (setf (gethash (quote a) h) 1 (gethash (quote b) h) 2) (let ((n (+ (length (hash-table-keys h)) (length (hash-table-values h))))) (clrhash h) (list (hash-table-p h) (hash-table-p nil) n (hash-table-count h))))) (hash-surfaces)",
    "(defun hash-map-inline () (let ((h (make-hash-table)) (sum 0)) (setf (gethash (quote a) h) 1 (gethash (quote b) h) 2) (maphash (lambda (key value) (declare (ignore key)) (setq sum (+ sum value))) h) sum)) (hash-map-inline)",
    // ── cond / when / unless / and / or on bytecode (nmq.6 coverage) ──
    "(when t 1 2 3)",
    "(when nil 1 2)",
    "(unless nil (quote yes))",
    "(unless t (quote no))",
    "(and 1 2 3)",
    "(and 1 nil 3)",
    "(and)",
    "(or nil nil 5)",
    "(or 1 2)",
    "(or nil nil)",
    "(cond ((= 1 2) (quote a)) ((= 1 1) (quote b)) (t (quote c)))",
    "(cond (nil 1) (2))",
    "(cond (nil 1))",
    "(defun classify (n) (cond ((< n 0) (quote neg)) ((= n 0) (quote zero)) (t (quote pos)))) (list (classify -5) (classify 0) (classify 5))",
    "(defun evenp2 (n) (cond ((= n 0) t) ((= n 1) nil) (t (evenp2 (- n 2))))) (evenp2 10)",
    "(defun fact (n) (if (and (integerp n) (> n 0)) (* n (fact (- n 1))) 1)) (fact 5)",
    // ── case on bytecode (nmq.6 coverage) ──
    "(case 2 (1 (quote one)) (2 (quote two)) (t (quote other)))",
    "(case 5 (1 (quote one)) (2 (quote two)) (t (quote other)))",
    "(case 3 ((1 2 3) (quote low)) ((4 5 6) (quote high)))",
    "(case (quote b) (a 1) (b 2) (otherwise 99))",
    "(case 9 (1 (quote one)))",
    "(defun day (n) (case n (0 (quote sun)) (1 (quote mon)) (otherwise (quote other)))) (list (day 0) (day 1) (day 5))",
    // Regressions found by running the full suite under bytecode default:
    // a handler-bind handler that captures + mutates a boxed local,
    "(let ((result nil)) (handler-bind ((error (lambda (c) (setq result \"handled\")))) (signal (make-condition (quote error)))) result)",
    // and sequence functions with :key/:test (these bail — must still match).
    "(sort (list (list 2) (list 1)) (function <) :key (function car))",
    "(count 2 '(1 2 2 3 2) :test (function =))",
    "(remove 1 '((1 a) (2 b) (1 c)) :key (function car) :test-not (function =))",
    "(let ((x 0)) (list (setf (documentation (progn (setq x (+ x 1)) (quote f)) t) (progn (setq x (+ x 10)) \"doc\")) x))",
    // ── flet / labels on bytecode (nmq.6 coverage) ──
    "(flet ((sq (x) (* x x))) (sq 7))",
    "(flet ((add (a b) (+ a b)) (mul (a b) (* a b))) (+ (add 2 3) (mul 2 3)))",
    "(labels ((g (k) (if (= k 0) 0 (g (- k 1))))) (g 100))",
    "(labels ((ev (n) (if (= n 0) t (od (- n 1)))) (od (n) (if (= n 0) nil (ev (- n 1))))) (ev 11))",
    "(defun f (n) (labels ((g (k acc) (if (= k 0) acc (g (- k 1) (+ acc k))))) (g n 0))) (f 100)",
    // ── macroexpand-then-lower: dotimes/dolist and ignore-errors (nmq.6) ──
    "(defun sumto (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s i))))) (sumto 100)",
    "(defun sumlist (l) (let ((s 0)) (dolist (x l s) (setq s (+ s x))))) (sumlist (list 1 2 3 4 5))",
    "(loop for (a . b) in (quote ((1 . 2) (3 . 4))) for n = (+ a b) collect n)",
    "(loop for x in (quote (1 2 3)) for y = (* x 2) for z = (+ y 1) sum z)",
    // bliss-8ai: :until textually after `:for VAR = FORM` must test the CURRENT
    // iteration's value (post-reassignment), not the previous one — the compiled
    // lowerer used to test at loop-top and collected the terminating value.
    "(let ((c 0)) (loop for x = (incf c) until (> x 3) collect x))",
    "(let ((c 0)) (loop for x = (incf c) while (< x 4) collect x))",
    "(ignore-errors (error \"boom\"))",
    "(ignore-errors (+ 1 2))",
    // handler-case clause secondary values are discarded (child-env semantics).
    "(handler-case (error \"x\") (error (c) (values 42 99)))",
    "(multiple-value-bind (a b) (handler-case (values 1 2) (error () 0)) (list a b))",
    // ── format / I/O in (recursive) bodies (nmq.6 coverage) ──
    "(defun f () (format nil \"~a-~a\" 1 2)) (f)",
    "(defun countdown (n) (format nil \"~a \" n) (if (> n 0) (countdown (- n 1)) (quote done))) (countdown 3)",
    "(format nil \"~d items and ~a\" 5 (quote x))",
    "(princ-to-string 42)",
    // ── declare / the / locally (nmq.6) ──
    "(defun f (x) (declare (ignore x)) 5) (f 99)",
    "(the integer (+ 1 2))",
    "(locally (declare (optimize speed)) (+ 3 4))",
    "(handler-case (car 5) (error (e) (declare (ignore e)) (quote caught)))",
];

/// Full programs whose deep recursion must raise a catchable STORAGE-CONDITION
/// through the default (bytecode) backend — proving the retired host-SP guard is
/// not needed for compiled recursion (it is TorclStack-bounded).
const GUARD_FREE_STORAGE_CONDITION: &[&str] = &[
    "(handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) (storage-condition (e) (declare (ignore e)) :caught))",
    "(handler-case (labels ((f () (f))) (f)) (condition (e) (declare (ignore e)) :caught))",
];

/// Full programs whose deep recursion must be bounded by the TorclStack
/// (raising a catchable STORAGE-CONDITION), proving they run on the bytecode
/// backend rather than the tree-walker's host-stack guard.
const DEEP_RECURSION_BOUNDED: &[&str] = &[
    "(defun down (n) (if (= n 0) (quote done) (down (- n 1)))) (handler-case (down 100000000) (storage-condition () (quote caught)))",
    "(handler-case (labels ((g (k) (if (= k 0) 0 (g (- k 1))))) (g 100000000)) (storage-condition () (quote caught)))",
];

/// Programs that must run on the bytecode backend (not fall back). Each is a
/// single top-level form whose last trace line under `TORCL_BYTECODE_TRACE`
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
    "(defun hash-surfaces-compile () (let ((h (make-hash-table))) (setf (gethash (quote a) h) 1) (let ((n (+ (length (hash-table-keys h)) (length (hash-table-values h))))) (clrhash h) (list (hash-table-p h) n (hash-table-count h)))))",
    "(defun hash-map-inline-compile () (let ((h (make-hash-table)) (sum 0)) (setf (gethash (quote a) h) 1) (maphash (lambda (key value) (declare (ignore key)) (setq sum (+ sum value))) h) sum))",
    "(multiple-value-bind (a b) (values 1 2) (list a b))",
    "(values 1 2 3)",
    "(member (quote b) (quote (a b c)))",
    "(coerce (quote (#\\a #\\b)) (quote string))",
    "(make-pathname :name \"compiled-builtin\" :type \"lisp\")",
    "(let ((x 0)) (setf (documentation (progn (setq x (+ x 1)) (quote f)) t) (progn (setq x (+ x 10)) \"doc\")))",
    "(fmakunbound (quote bytecode-never-defined))",
    "(defun forward-caller (x) (forward-callee x)) (defun forward-callee (x) (+ x 1)) (forward-caller 4)",
    "(loop for (a . b) in (quote ((1 . 2) (3 . 4))) for n = (+ a b) collect n)",
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
            .env("TORCL_BACKEND", "bytecode")
            .env("TORCL_BYTECODE_TRACE", "1")
            // Definition-only cases must compile eagerly; otherwise the last
            // trace can belong to a bootstrap library instead of this program.
            .env("TORCL_LAZY_COMPILE", "0")
            .output()
            .expect("spawn");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last = stderr
            .lines()
            .rfind(|l| l.starts_with("[bytecode]"))
            .unwrap_or("");
        assert_eq!(
            last, "[bytecode] compiled",
            "program should compile to bytecode, not bail:\n  {program}\n  last trace: {last:?}"
        );
    }
}

/// A variadic parameter captured by a nested closure must still compile. This
/// used to hit the explicit `lambda-list:captured-variadic-param` bail before
/// the call-time binder could place resolved values in the heap environment.
#[test]
fn captured_variadic_parameters_compile_to_bytecode() {
    let program = "\
        (defun captured-opt (&optional (x 41 xp)) (funcall (lambda () (list x xp)))) \
        (defun captured-rest (&rest xs) (funcall (lambda () (length xs)))) \
        (defun captured-key (&key (x 7 xp)) (funcall (lambda () (list x xp)))) \
        (format t \"~a ~a ~a ~a ~a~%\" \
          (captured-opt) (captured-opt 9) (captured-rest 1 2 3) \
          (captured-key) (captured-key :x 12))";
    let out = Command::new(BIN)
        .arg("--no-bootstrap")
        .arg("--eval")
        .arg(program)
        .env("TORCL_BACKEND", "bytecode")
        .env("TORCL_BYTECODE_TRACE_NAMES", "1")
        // Assert definition-time compile coverage: pin eager so these functions
        // compile at definition rather than deferring under the lazy default.
        .env("TORCL_LAZY_COMPILE", "0")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "captured variadic run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).lines().next(),
        Some("(41 NIL) (9 T) 3 (7 NIL) (12 T)")
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for name in ["CAPTURED-OPT", "CAPTURED-REST", "CAPTURED-KEY"] {
        assert!(
            stderr.contains(&format!("[bytecode] {name}: compiled")),
            "{name} should compile instead of bailing:\n{stderr}"
        );
    }
}

/// bliss-pgu: a HANDLER-BIND handler lambda that captures an enclosing local
/// only revealed after macro expansion must not miscompile. The
/// `compute_captured_names` pre-scan runs on the UNEXPANDED body, so it never
/// boxes `secret`; the handler, evaluated against the activation's heap frame,
/// cannot reach the plain frame slot. Lowering must bail to the tree-walker (the
/// same Slot-capture guard `lower_restart_case` applies) so the result stays
/// correct. Runs eager so the enclosing DEFUN actually reaches the compiler.
#[test]
fn macro_hidden_handler_bind_capture_bails_and_stays_correct() {
    // Both an eager-compiled DEFUN and the tree-walker must return 42 (the
    // captured value), never :UNTOUCHED (capture lost).
    let program = "\
        (defvar *o* :untouched) \
        (defmacro hbm () \
          (quote (handler-bind ((error (lambda (c) (declare (ignore c)) (setf *o* secret)))) \
                   (error \"boom\")))) \
        (defun f () (setq *o* :untouched) \
          (let ((secret 42)) (handler-case (hbm) (error () nil)) *o*)) \
        (format t \"~a~%\" (f))";
    let mut outputs = Vec::new();
    for backend in ["tree-walker", "bytecode"] {
        let out = Command::new(BIN)
            .arg("--eval")
            .arg(program)
            .env("TORCL_BACKEND", backend)
            .env("TORCL_LAZY_COMPILE", "0")
            .output()
            .expect("spawn");
        assert!(
            out.status.success(),
            "{backend} run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        outputs.push(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string(),
        );
    }
    assert_eq!(outputs[0], "42", "tree-walker oracle should capture secret");
    assert_eq!(
        outputs[1], outputs[0],
        "eager-compiled bytecode must match the oracle, not lose the macro-hidden capture"
    );
}

/// bliss-9u6d: an eager-compiled DEFUN whose nested closure (lambda / flet /
/// labels) captures an enclosing frame-slot local revealed only after macro
/// expansion must stay correct on the non-portable runtime backend too. The
/// once-per-function up-front macroexpand surfaces the reference so it is boxed
/// (lambda) or the form bails (flet/labels); either way the result matches the
/// tree-walker, never :UNTOUCHED / unbound-variable.
#[test]
fn macro_hidden_closure_capture_stays_correct_when_eager() {
    let program = "\
        (defvar *o* :untouched) \
        (defmacro gx () 'x) \
        (defun via-lambda () (setq *o* :untouched) \
          (let ((x 42)) (funcall (lambda () (setf *o* (gx)))) *o*)) \
        (defun via-flet () (setq *o* :untouched) \
          (let ((x 7)) (flet ((g () (setf *o* (gx)))) (g)) *o*)) \
        (defun via-labels () (setq *o* :untouched) \
          (let ((x 9)) (labels ((g () (setf *o* (gx)))) (g)) *o*)) \
        (format t \"~a ~a ~a~%\" (via-lambda) (via-flet) (via-labels))";
    let mut outputs = Vec::new();
    for backend in ["tree-walker", "bytecode"] {
        let out = Command::new(BIN)
            .arg("--eval")
            .arg(program)
            .env("TORCL_BACKEND", backend)
            .env("TORCL_LAZY_COMPILE", "0")
            .output()
            .expect("spawn");
        assert!(
            out.status.success(),
            "{backend} run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        outputs.push(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string(),
        );
    }
    assert_eq!(outputs[0], "42 7 9", "tree-walker oracle");
    assert_eq!(
        outputs[1], outputs[0],
        "eager bytecode must capture the macro-hidden slot on the non-portable path"
    );
}

/// bliss-9u6d: the once-per-function up-front macroexpand walks and rebuilds a
/// closure-bearing body (LET* / FLET) under the moving GC. A regression in
/// `expand_let` (an unrooted expanded body across the binding rebuild) produced a
/// cyclic form that later walkers looped on — a crash only under GC stress.
/// Eager-compiling such a function under stress must stay clean and correct.
#[test]
fn gc_stress_macroexpand_letstar_closure_stays_acyclic() {
    let _guard = gc_stress_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A LET* binding an init-form wrapping a FLET closure whose body calls a
    // global macro — the shape of stdlib MISMATCH that first tripped this.
    let program = "(defmacro gm (x) `(+ ,x 1)) \
        (defun r () (let* ((a 1) (b (+ a 1))) \
          (flet ((f (z) (gm z))) (+ (f a) (f b))))) \
        (format t \"~a~%\" (r))";
    for backend in ["bytecode", "tree-walker"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("TORCL_BACKEND", backend)
            .env("TORCL_LAZY_COMPILE", "0")
            .env("TORCL_HEAP_MB", "2048")
            .env("TORCL_GC_STRESS", "4")
            .env("TORCL_GC_POISON", "1")
            .output()
            .expect("spawn torcl");
        assert!(
            out.status.success(),
            "{backend} failed under GC stress\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).lines().next(),
            Some("5")
        );
    }
}

/// Deep recursion under the bytecode backend is bounded by the `TorclStack`
/// capacity and raises a catchable `STORAGE-CONDITION` (R2.20) — it must not
/// abort the process. Covers plain recursion and labels-local recursion.
#[test]
fn deep_recursion_raises_catchable_storage_condition() {
    for program in DEEP_RECURSION_BOUNDED {
        let (out, ok) = run(program, true, true);
        assert!(
            ok,
            "should exit cleanly after catching STORAGE-CONDITION: {program}"
        );
        assert!(
            out.contains("CAUGHT"),
            "deep recursion should be caught as STORAGE-CONDITION for {program}, got: {out:?}"
        );
    }
    // The interim host-SP guard is retired (nmq.6); these must still be caught
    // via the TorclStack bound on the default backend, exiting cleanly.
    for program in GUARD_FREE_STORAGE_CONDITION {
        let out = Command::new(BIN)
            .arg("--eval")
            .arg(program)
            .output()
            .expect("spawn");
        assert!(
            out.status.success(),
            "guard-free deep recursion should be caught (exit 0), not abort: {program}"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .to_uppercase()
                .contains("CAUGHT"),
            "storage-condition must fire for {program}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// The `TorclStack` bound scales with `TORCL_STACK_SIZE`: a depth that fits a
/// large stack overflows a small one — proving the bound is the CL stack's
/// capacity, not the host Rust stack.
#[test]
fn recursion_bound_scales_with_stack_size() {
    let program = "(defun down (n) (if (= n 0) (quote done) (down (- n 1)))) (down 3000)";

    let big = Command::new(BIN)
        .arg("--eval")
        .arg(program)
        .env("TORCL_BACKEND", "bytecode")
        .env("TORCL_STACK_SIZE", "1m")
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
        .env("TORCL_BACKEND", "bytecode")
        .env("TORCL_STACK_SIZE", "16k")
        .output()
        .expect("spawn");
    assert!(
        !small.status.success(),
        "depth 3000 should overflow a 16 KiB stack"
    );
}

#[test]
fn gc_stress_defun_dotimes_push_keeps_body_roots() {
    let _guard = gc_stress_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let program = "(progn (defun r () (let ((a nil)) (dotimes (i 5) (push (* i i) a)) a)) (r))";

    for backend in ["bytecode", "tree-walker", "treewalk"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("TORCL_BACKEND", backend)
            .env("TORCL_HEAP_MB", "4096")
            .env("TORCL_GC_STRESS", "3")
            .env("TORCL_GC_POISON", "1")
            .output()
            .expect("spawn torcl");
        assert!(
            out.status.success(),
            "{backend} failed under GC stress\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "(16 9 4 1 0)",
            "{backend} produced wrong output under GC stress"
        );
    }
}

#[test]
fn gc_stress_macrolet_symbol_macrolet_labels_keeps_symbol_macro_value() {
    let _guard = gc_stress_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let program = "(macrolet ((m (x) `(list ,x y))) (symbol-macrolet ((y 40)) (labels ((f (z) (m z))) (f 2))))";

    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("TORCL_BACKEND", "bytecode")
        .env("TORCL_HEAP_MB", "4096")
        .env("TORCL_GC_STRESS", "8")
        .env("TORCL_GC_POISON", "1")
        .output()
        .expect("spawn torcl");
    assert!(
        out.status.success(),
        "bytecode failed under GC stress\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "(2 40)",
        "bytecode produced wrong output under GC stress"
    );
}

#[test]
fn gc_stress_mx_clean_macrolet_symbol_macrolet_labels() {
    let _guard = gc_stress_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let program = "(progn
        (defun run ()
          (macrolet ((sq (x) `(* ,x ,x))
                     (add3 (a b c) `(+ ,a ,b ,c))
                     (twice (f x) `(,f (,f ,x)))
                     (mklist (&rest xs) `(list ,@xs)))
            (symbol-macrolet ((ten 10))
              (labels ((f (n) (add3 (sq n) (twice sq n) ten)))
                (mklist (f 1) (f 2) (f 3) (add3 ten ten (sq 4)))))))
        (run))";

    for backend in ["bytecode", "tree-walker"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("TORCL_BACKEND", backend)
            .env("TORCL_HEAP_MB", "4096")
            .env("TORCL_GC_STRESS", "8")
            .env("TORCL_GC_POISON", "1")
            .output()
            .expect("spawn torcl");
        assert!(
            out.status.success(),
            "{backend} failed under GC stress\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "(12 30 100 36)",
            "{backend} produced wrong output under GC stress"
        );
    }
}

/// Regression for the NTH-VALUE miscompile: NTH-VALUE is a special operator, but
/// the bytecode lowerer had no case for it, so it fell through to a plain call —
/// evaluating the form in single-value context and yielding NIL for every index
/// `> 0` (index 0 worked by luck). This silently broke compiled `(nth-value k …)`,
/// `k > 0` — e.g. cl-cookie's `(nth-value 5 (get-decoded-time))` at load. The
/// bytecode backend must agree with the tree-walker.
#[test]
fn nth_value_compiles_to_the_right_value() {
    assert_agree("(princ (nth-value 0 (values 10 20 30 40)))");
    assert_agree("(princ (nth-value 2 (values 10 20 30 40)))");
    assert_agree("(princ (nth-value 1 (floor 17 5)))");
    assert_agree("(princ (nth-value 3 (values 1 2 3 4 5)))");
    // Index past the produced values yields NIL, not an error.
    assert_agree("(princ (nth-value 9 (values 1 2 3)))");
    // In a function body (the real miscompile site was a compiled defun).
    assert_agree("(defun f (x) (nth-value 1 (floor x 5))) (princ (list (f 17) (f 23) (f 100)))");
}
