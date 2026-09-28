//! The `PY` package: calling Python from Lisp (spec §2.7.8, bliss-dk3nr).
//!
//! Driven through the CLI rather than as a library test, because what is being
//! checked is the surface a user types — including that the operators survive the
//! bytecode lowerer, which is where this broke: both the lowerer and the FUNCALL
//! fast path reduce an operator to its BARE name, so `PY:TYPEP` was compiled into
//! `CL:TYPEP` and answered a different question with a straight face. Nothing but
//! running the form catches that.
//!
//! Gated on the `python` feature: embedding needs a target whose loader can open
//! libpython, which in practice means glibc rather than the default static musl.

#![cfg(feature = "python")]

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

/// Run one program, in a tier, optionally under GC stress, and return its stdout.
fn run(program: &str, tier: &str, stress: bool) -> String {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    command.env("TORCL_FORCE_TIER", tier);
    if stress {
        command
            .env("TORCL_GC_STRESS", "1")
            .env("TORCL_GC_POISON", "1");
    }
    let output = command.output().expect("the CLI runs");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        output.status.success(),
        "tier={tier} stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Every operation, its result printed on one line so a single comparison covers
/// the lot. Run in both tiers because the lowerer is where the operators can
/// silently become their COMMON-LISP namesakes.
const SURFACE: &str = r#"
  (py:exec "class Point:
    def __init__(self, x): self.x = x
    def scale(self, k): return self.x * k
p = Point(3)")
  (let ((p (py:resolve "__main__.p")))
    (format t "PY ~S~%"
            (list (py:call "math.sqrt" 9)
                  (py:call "math.hypot" 3 4)
                  (py:call-method "hello" "upper")
                  (py:getattr p "x")
                  (progn (setf (py:getattr p "x") 10) (py:getattr p "x"))
                  (py:call-method p "scale" 4)
                  (py:typep p "__main__.Point")
                  (py:typep p "builtins.dict")
                  (py:typep 5 "builtins.int")
                  (py:objectp p)
                  (py:objectp 5)
                  (typep p 'py:object)
                  (py:str 5)
                  (py:str nil)
                  (py:str t)
                  (py:resolve "sys.platform")
                  (py:exec "pass"))))
"#;

const EXPECTED: &str =
    r#"PY (3.0d0 5.0d0 "HELLO" 3 10 40 T NIL T T NIL T "5" "None" "True" "linux" NIL)"#;

#[test]
fn the_calling_surface_answers_the_same_in_both_tiers() {
    for (tier, stress) in [("t0", false), ("t1", false), ("t0", true)] {
        let stdout = run(SURFACE, tier, stress);
        let line = stdout
            .lines()
            .find(|line| line.starts_with("PY "))
            .unwrap_or("");
        assert_eq!(line.trim(), EXPECTED, "tier={tier} stress={stress}");
    }
}

/// Values crossing back follow ONE policy, and the two classification traps are
/// the point of this test.
///
/// Python's `bool` is a SUBCLASS of `int`, so a classification that tests integers
/// first turns `True` into `1`. And Python's integers are unbounded while a fixnum
/// holds 61 bits, so one that does not fit stays a PYTHON-OBJECT — visible and
/// exact — rather than being truncated or widened to a float.
#[test]
fn values_crossing_back_follow_one_policy() {
    let program = r#"
      (py:exec "small = 7
big = 2 ** 200
truth = True
falsehood = False
nothing = None
text = 'héllo ☃'
fraction = 0.5")
      (format t "POLICY ~S~%"
              (list (py:resolve "__main__.small")
                    (py:objectp (py:resolve "__main__.big"))
                    (py:resolve "__main__.truth")
                    (py:resolve "__main__.falsehood")
                    (py:resolve "__main__.nothing")
                    (py:resolve "__main__.text")
                    (py:resolve "__main__.fraction")
                    ;; True must not arrive as the integer 1.
                    (eql (py:resolve "__main__.truth") 1)))
    "#;
    let stdout = run(program, "t0", false);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("POLICY "))
        .unwrap_or("");
    assert_eq!(line.trim(), r#"POLICY (7 T T NIL NIL "héllo ☃" 0.5d0 NIL)"#);
}

/// A dead proxy owes CPython a reference, and the collector must see to it that it
/// is paid — without calling CPython from inside the collection.
///
/// `__del__` is the observable: it runs when the last reference goes, so counting
/// its calls counts the releases. The queue is drained on the next crossing, which
/// is why the count is read through one.
#[test]
fn unreachable_proxies_release_their_references() {
    let program = r#"
      (py:exec "deaths = 0
class Counted:
    def __del__(self):
        global deaths
        deaths = deaths + 1")
      (dotimes (i 300) (py:call "__main__.Counted"))
      (torcl-ext:gc)
      (let ((first (py:resolve "__main__.deaths")))
        (torcl-ext:gc)
        (let ((second (py:resolve "__main__.deaths")))
          (format t "RELEASED ~S~%" (list (> first 0) (= second 300)))))
    "#;
    let stdout = run(program, "t0", false);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("RELEASED "))
        .unwrap_or("");
    assert_eq!(
        line.trim(),
        "RELEASED (T T)",
        "every proxy's reference should have been released after two collections"
    );
}

/// A failure reports what Python said, and does not poison the next call.
///
/// An exception left set is the worst failure mode available here: it surfaces as
/// the error of some later, unrelated operation.
#[test]
fn errors_report_pythons_own_message() {
    let program = r#"
      (format t "ERRORS ~S~%"
              (list (handler-case (py:call "builtins.int" "not a number")
                      (error (e) (let ((text (princ-to-string e)))
                                   (list (search "ValueError" text)
                                         (not (null (search "not a number" text)))))))
                    (handler-case (py:import "definitely_not_a_module_47")
                      (error () :import-failed))
                    ;; Still usable: the failed calls must not have left an
                    ;; exception set for this one to inherit.
                    (py:call "math.sqrt" 4)
                    (handler-case (py:getattr (py:import "math") "no_such_attribute")
                      (error () :no-attribute))
                    (handler-case (py:call "math.sqrt" (list 1 2))
                      (error () :unconvertible))))
    "#;
    let stdout = run(program, "t0", false);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("ERRORS "))
        .unwrap_or("");
    assert_eq!(
        line.trim(),
        "ERRORS ((0 T) :IMPORT-FAILED 2.0d0 :NO-ATTRIBUTE :UNCONVERTIBLE)"
    );
}

/// A Python raise is a first-class Lisp condition: catchable as `PY:EXCEPTION`,
/// carrying the exception's class, its message, the Python frames, and the exception
/// object itself so a handler can reach its attributes.
#[test]
fn a_python_raise_is_a_lisp_condition() {
    let program = r#"
      (py:exec "def inner(x):
    return int(x)
def outer(x):
    return inner(x)")
      (handler-case (py:call "__main__.outer" "bad")
        (py:exception (e)
          (format t "CONDITION ~S~%"
                  (list (py:exception-kind e)
                        ;; The message, and that it is Python's own.
                        (not (null (search "invalid literal" (py:exception-text e))))
                        ;; Frames are (FILE LINE FUNCTION), outermost first.
                        (py:exception-frames e)
                        ;; py:backtrace reverses them and tags them, for a mixed
                        ;; backtrace a debugger can interleave with Lisp frames.
                        (py:backtrace e)
                        ;; The exception object is reachable, and is a proxy.
                        (py:objectp (py:exception-object e))
                        ;; And its attributes can be read, which is the reason for
                        ;; carrying it rather than only its message.
                        (not (null (search "invalid literal"
                                           (py:str (py:getattr (py:exception-object e) "args")))))
                        ;; It is an ERROR, so an ordinary handler catches it too.
                        (typep e 'error)))))
    "#;
    let stdout = run(program, "t0", false);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("CONDITION "))
        .unwrap_or("");
    assert_eq!(
        line.trim(),
        r#"CONDITION ("ValueError" T (("<string>" 4 "outer") ("<string>" 2 "inner")) ((:PYTHON "inner" "<string>" 2) (:PYTHON "outer" "<string>" 4)) T T T)"#
    );
}

/// The report shows the message and the Python frames, both when caught and when
/// it reaches the top level uncaught — the latter being where a reader most needs
/// to know where in Python it happened.
#[test]
fn the_report_renders_a_mixed_backtrace() {
    let program = r#"
      (py:exec "def inner(x):
    return int(x)
def outer(x):
    return inner(x)")
      (handler-case (py:call "__main__.outer" "bad")
        (py:exception (e) (format t "~&CAUGHT~%~a~%END~%" e)))
    "#;
    let stdout = run(program, "t0", false);
    let caught: Vec<&str> = stdout
        .lines()
        .skip_while(|line| !line.starts_with("CAUGHT"))
        .take_while(|line| !line.starts_with("END"))
        .collect();
    assert_eq!(
        caught,
        vec![
            "CAUGHT",
            "ValueError: invalid literal for int() with base 10: 'bad'",
            "  Python  inner at <string>:2",
            "  Python  outer at <string>:4",
        ],
        "got: {stdout}"
    );

    // Uncaught: the same shape must reach the top level.
    let mut command = Command::new(BIN);
    command.args([
        "--no-init",
        "--eval",
        r#"(progn (py:exec "def inner(x):
    return int(x)
def outer(x):
    return inner(x)") (py:call "__main__.outer" "unhandled"))"#,
    ]);
    let output = command.output().expect("the CLI runs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "an uncaught raise must fail");
    assert!(
        combined.contains("ValueError: invalid literal")
            && combined.contains("Python  inner at <string>:2"),
        "an uncaught raise should show its Python frames, got: {combined}"
    );
}

/// Describing an exception calls back into Python — `str(exception)`, the type's
/// `__qualname__`, `traceback.extract_tb` — and any of those can itself raise. The
/// description must not then describe its own failure, without bound.
///
/// An exception whose `__str__` raises is the smallest way to provoke it. The first
/// version of the guard covered only the traceback walk, which left this open.
#[test]
fn describing_a_hostile_exception_does_not_recurse() {
    let program = r#"
      (py:exec "class Hostile(Exception):
    def __str__(self): raise RuntimeError('str failed too')
def raiser(): raise Hostile()")
      (format t "HOSTILE ~S~%"
              (list (handler-case (py:call "__main__.raiser") (error () :caught))
                    ;; And the guard resets, so the next exception still reports in
                    ;; full — a guard that leaked would silently degrade every
                    ;; later error on this thread.
                    (handler-case (py:call "__main__.deep")
                      (py:exception (e) (py:exception-kind e)))))
      (py:exec "def bottom(x): return int(x)
def deep(): return bottom('bad')")
      (format t "AFTER ~S~%"
              (handler-case (py:call "__main__.deep")
                (py:exception (e) (list (py:exception-kind e)
                                        (length (py:exception-frames e))))))
    "#;
    let stdout = run(program, "t0", false);
    let hostile = stdout
        .lines()
        .find(|line| line.starts_with("HOSTILE "))
        .unwrap_or("");
    assert_eq!(hostile.trim(), r#"HOSTILE (:CAUGHT "AttributeError")"#);
    let after = stdout
        .lines()
        .find(|line| line.starts_with("AFTER "))
        .unwrap_or("");
    assert_eq!(
        after.trim(),
        r#"AFTER ("ValueError" 2)"#,
        "a later exception must still get its kind and its frames"
    );
}

/// A regression guard with nothing to do with Python.
///
/// PY:EXCEPTION was first named PY:ERROR, and because TorCL's class registry is
/// keyed by a class's BARE name that replaced CL:ERROR for the whole image: every
/// user condition's superclass then resolved to it, it was its own superclass, and
/// MAKE-CONDITION of anything recursed until the stack was gone. This asserts the
/// ordinary case still works, so a future rename cannot quietly do it again
/// (bliss-kliz4).
#[test]
fn defining_the_python_condition_does_not_disturb_cl_error() {
    let program = r#"
      (define-condition probe-condition (error) ((k :initarg :k :reader probe-k)))
      (format t "PROBE ~S~%"
              (list (probe-k (make-condition 'probe-condition :k 5))
                    (handler-case (error "plain") (error () :caught))
                    (eq (find-class 'error) (find-class 'cl:error))
                    (not (eq (find-class 'py:exception) (find-class 'error)))))
    "#;
    let stdout = run(program, "t0", false);
    let line = stdout
        .lines()
        .find(|line| line.starts_with("PROBE "))
        .unwrap_or("");
    assert_eq!(line.trim(), "PROBE (5 :CAUGHT T T)");
}

/// A proxy prints as the object, not as an address: `#<PYTHON-OBJECT <module …>>`.
/// That is what makes one legible in a backtrace.
#[test]
fn a_proxy_prints_as_the_object_it_names() {
    let stdout = run(r#"(princ (py:import "math"))"#, "t0", false);
    assert!(
        stdout.contains("#<PYTHON-OBJECT <module 'math'"),
        "got: {stdout}"
    );
}

/// Python's output reaches `*standard-output*` (bliss-c4g9u).
///
/// Before this it was not merely interleaved unpredictably — it was SILENTLY LOST.
/// CPython block-buffers a non-tty stdout, nothing flushed it, and the interpreter is
/// usually never finalized, so `(py:exec "print('hi')")` printed nothing at all.
#[test]
fn pythons_output_reaches_the_lisp_stream() {
    let program = r#"
      (py:exec "import sys")
      (format t "ORDER ~a~%"
              (with-output-to-string (s)
                (let ((*standard-output* s))
                  (princ "lisp-1 ")
                  (py:exec "print('python-1', end=' ')")
                  (princ "lisp-2 ")
                  (py:exec "print('python-2', end='')"))))
      ;; stderr is a separate stream, and goes to *ERROR-OUTPUT*.
      (format t "STDERR ~s~%"
              (with-output-to-string (s)
                (let ((*error-output* s))
                  (py:exec "print('to-stderr', end='', file=sys.stderr)"))))
      ;; Output printed before a raise still arrives: it is exactly what a reader
      ;; needs, and unwind-protect is what keeps it.
      (format t "BEFORE-RAISE ~s~%"
              (with-output-to-string (s)
                (let ((*standard-output* s))
                  (ignore-errors (py:exec "print('said', end=''); raise ValueError('x')")))))
      ;; A value AND its side effects, from py:call rather than py:exec.
      (py:exec "def noisy():
    print('side', end='')
    return 7")
      (format t "CALL ~s~%"
              (with-output-to-string (s)
                (let ((*standard-output* s))
                  (princ (py:call "__main__.noisy")))))
    "#;
    let stdout = run(program, "t0", false);
    let line = |prefix: &str| -> String {
        stdout
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    assert_eq!(
        line("ORDER "),
        "ORDER lisp-1 python-1 lisp-2 python-2",
        "the two runtimes' output must interleave in program order: {stdout}"
    );
    assert_eq!(line("STDERR "), r#"STDERR "to-stderr""#);
    assert_eq!(line("BEFORE-RAISE "), r#"BEFORE-RAISE "said""#);
    assert_eq!(line("CALL "), r#"CALL "side7""#);
}

/// A statement that raises is reported ONCE, as a condition with its frames.
///
/// `PyRun_SimpleString` — the obvious call for this, and the one used first — prints
/// a traceback itself and CONSUMES the exception. The failure was then reported twice,
/// and the Lisp half was a bare FFI error with no type, message or frames, because
/// there was no longer an exception left to describe.
#[test]
fn a_raising_statement_is_reported_once_with_its_frames() {
    let program = r#"
      (handler-case (py:exec "def f(): raise ValueError('deep')
f()")
        (py:exception (e)
          (format t "EXEC ~S~%" (list (py:exception-kind e)
                                      (py:exception-text e)
                                      (py:exception-frames e)))))
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("TORCL_FORCE_TIER", "t0")
        .output()
        .expect("the CLI runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("EXEC "))
            .unwrap_or("")
            .trim(),
        r#"EXEC ("ValueError" "deep" (("<string>" 2 "<module>") ("<string>" 1 "f")))"#
    );
    assert!(
        !stderr.contains("Traceback"),
        "CPython must not also print its own traceback: {stderr}"
    );
}

/// Python can call Lisp: `py:export` binds a Python callable backed by a Lisp
/// function (bliss-89axw).
#[test]
fn python_can_call_an_exported_lisp_function() {
    let program = r#"
      (py:export "add" (lambda (a b) (+ a b)))
      (py:export "greet" (lambda (who) (concatenate 'string "hello " who)))
      (py:exec "def twice(f, x): return f(f(x))")
      (py:export "inc" (lambda (n) (+ n 1)))
      (format t "EXPORT ~S~%"
              (list (py:call "__main__.add" 2 3)
                    (py:call "__main__.greet" "world")
                    ;; The exported function as a VALUE, passed into a Python
                    ;; higher-order function and called from inside Python — the
                    ;; case that a mere name binding would not cover.
                    (py:call "__main__.twice" (py:resolve "__main__.inc") 5)))
    "#;
    let stdout = run(program, "t0", false);
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("EXPORT "))
            .unwrap_or("")
            .trim(),
        r#"EXPORT (5 "hello world" 7)"#
    );
}

/// The exported function is reached through a STABLE HANDLE, which is the whole
/// reason the mechanism exists: TorCL's collector moves objects, and a Python
/// callable outlives any address.
///
/// The closure captures heap data so the collector has something to relocate. A raw
/// pointer would survive the first assertion and then read moved or reclaimed memory.
#[test]
fn an_exported_function_survives_collection() {
    let program = r#"
      (let ((table (list 1 2 3)))
        (py:export "lookup" (lambda (i) (nth i table))))
      (py:exec "before = lookup(1)")
      (dotimes (i 3) (torcl-ext:gc))
      ;; Churn the heap hard enough to reuse whatever the closure occupied.
      (let ((junk nil)) (dotimes (i 20000) (push (list i i) junk)) (length junk))
      (torcl-ext:gc)
      (torcl-ext:gc)
      (format t "HANDLE ~S~%"
              (list (py:resolve "__main__.before")
                    (py:call "__main__.lookup" 1)
                    (py:call "__main__.lookup" 2)))
    "#;
    let stdout = run(program, "t0", false);
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("HANDLE "))
            .unwrap_or("")
            .trim(),
        "HANDLE (2 2 3)",
        "the closure must still answer correctly after being moved"
    );
}

/// A Lisp error inside an exported function becomes a Python exception.
///
/// It must NOT unwind through CPython frames: that would leave their reference
/// counts wrong and corrupt the interpreter. Returning null with an exception set is
/// how a C function reports failure, which is what the entry point does.
#[test]
fn a_lisp_error_becomes_a_python_exception() {
    let program = r#"
      (py:export "boom" (lambda () (error "lisp exploded")))
      (py:export "ok" (lambda () 99))
      (py:exec "
caught = None
try:
    boom()
except Exception as e:
    caught = type(e).__name__ + ':' + ('lisp exploded' in str(e) and 'msg' or 'nomsg')
")
      (format t "ERR ~S~%"
              (list (py:resolve "__main__.caught")
                    ;; And the interpreter is still healthy afterwards.
                    (py:call "__main__.ok")))
    "#;
    let stdout = run(program, "t0", false);
    assert_eq!(
        stdout
            .lines()
            .find(|line| line.starts_with("ERR "))
            .unwrap_or("")
            .trim(),
        r#"ERR ("RuntimeError:msg" 99)"#
    );
}

/// A fault inside CPython must produce CPython's behaviour, not TorCL's
/// (bliss-ztkuw).
///
/// TorCL rewrites a faulting instruction to a native recovery epilogue that unwinds
/// a JIT frame and returns a sentinel, and it decides to purely from the FAULT
/// ADDRESS — anything below one page is "a null guard". Nothing looks at where the
/// fault happened, so a null dereference inside CPython is indistinguishable from one
/// in compiled Lisp. Were recovery armed, this would unwind a Lisp frame that is not
/// on top, leaving CPython mid-operation with its reference counts wrong, and the
/// program would carry on as though a Lisp type error had occurred.
///
/// So the required outcome is that the process DIES — which is what CPython itself
/// does for a null dereference — and specifically that IGNORE-ERRORS does not catch
/// it and execution does not continue. The crossing disarms recovery to guarantee
/// this however the call was reached.
#[test]
fn a_fault_inside_cpython_is_not_swallowed_as_a_lisp_error() {
    let program = r#"
      (py:exec "import ctypes")
      (defun hot (n) (let ((acc 0)) (dotimes (i n) (setq acc (+ acc i))) acc))
      ;; Warm a function so native code is on the stack when the fault happens —
      ;; that is the configuration in which recovery would be armed.
      (hot 300000)
      (format t "BEFORE~%")
      (finish-output)
      (ignore-errors (py:call "ctypes.string_at" 0))
      ;; Must be unreachable: recovery must not have resumed us.
      (format t "SWALLOWED~%")
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("the CLI runs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("BEFORE"),
        "the program should have got as far as the crossing: {combined}"
    );
    assert!(
        !combined.contains("SWALLOWED"),
        "a fault inside CPython must not be recovered into a catchable Lisp error: {combined}"
    );
    assert!(
        !output.status.success(),
        "the process must not exit successfully after faulting inside CPython: {combined}"
    );
}

/// The sandbox must deny embedded Python. It grants strictly more than the FFI
/// does — arbitrary code through `exec`, and the whole filesystem through Python's
/// own library — so a sandbox that denies FFI and allows this would be no sandbox.
#[test]
fn the_sandbox_denies_python() {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--sandbox", "--eval", r#"(py:import "math")"#]);
    let output = command.output().expect("the CLI runs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success() && combined.contains("denied"),
        "the sandbox should refuse to start an interpreter, got: {combined}"
    );
}
