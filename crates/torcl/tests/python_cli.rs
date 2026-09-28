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

const EXPECTED: &str = r#"PY (3.0d0 5.0d0 "HELLO" 3 10 40 T NIL T T NIL T "5" "None" "True" "linux" NIL)"#;

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
    assert_eq!(
        line.trim(),
        r#"POLICY (7 T T NIL NIL "héllo ☃" 0.5d0 NIL)"#
    );
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
