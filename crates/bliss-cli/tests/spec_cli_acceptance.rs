use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn bliss() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bliss-cli"))
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("bliss-spec-{}-{}", name, unique));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_file(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write temp file");
}

#[test]
fn eval_mode_uses_real_cli_entrypoint_and_overrides_init_discovery() {
    // Per R7.11 and R7.21, explicit CLI mode wins over ambient startup sources.
    // Per R10.06, the acceptance test must drive the real user-facing entrypoint.
    let dir = temp_dir("eval");
    let init = dir.join("init.lisp");
    write_file(&init, "(print 99)\n");

    let output = bliss()
        .env("BLISS_INIT_FILE", &init)
        .args(["--eval", "(+ 1 2)"])
        .output()
        .expect("run bliss --eval");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains('3'), "stdout was: {stdout}");
    assert!(
        !stdout.contains("99"),
        "explicit --eval should suppress init-file execution: {stdout}"
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn load_mode_executes_a_real_file_path() {
    // Per R7.07, Bliss MUST support a file-path load mode.
    // Per R10.06, the end-to-end pipeline must produce observable output.
    let dir = temp_dir("load");
    let script = dir.join("load-test.lisp");
    write_file(&script, "(print (+ 20 22))\n");

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss --load");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("42"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn script_mode_executes_the_positional_script_entrypoint() {
    // Per R7.07 and R7.21, a positional script path is a deterministic startup mode.
    let dir = temp_dir("script");
    let script = dir.join("argv-script.lisp");
    write_file(&script, "(print 'script-ran)\n");

    let output = bliss()
        .arg(script.to_str().expect("utf8 path"))
        .output()
        .expect("run bliss script");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .to_uppercase()
            .contains("SCRIPT-RAN"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn repl_acceptance_drives_the_real_binary_through_read_eval_print_and_exit() {
    // Per R6.01, the REPL MUST implement ANSI read-eval-print loop semantics.
    // Per R10.06, this acceptance test drives the real CLI through stdin/stdout.
    let mut child = bliss()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bliss repl");

    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        stdin
            .write_all(b"(+ 1 2)\n(quit)\n")
            .expect("write repl input");
    }

    let output = child.wait_with_output().expect("wait for repl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Bliss Common Lisp"), "stdout: {stdout}");
    assert!(
        stdout.lines().any(|line| line.trim() == "3"),
        "stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.contains("BLISS>"),
        "the real REPL should print its prompt before evaluating input: stdout: {stdout} stderr: {stderr}"
    );
}

#[test]
fn no_image_eval_mode_supports_bootstrap_without_a_saved_image() {
    // Per R7.21 and R7.22, --no-image is an explicit startup profile and MUST still permit bootstrapping work.
    let output = bliss()
        .args(["--no-image", "--eval", "(+ 4 5)"])
        .output()
        .expect("run bliss --no-image --eval");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains('9'));
}

#[test]
fn stage_zero_gate_round_trips_core_datatypes_and_runs_a_nested_script() {
    // Per §0.4 stage 0 and spec/stages.json, the real CLI must prove
    // datatype read->print round-trips plus nested arithmetic/list evaluation.
    let dir = temp_dir("stage0-gate");
    let script = dir.join("stage0-gate.lisp");
    write_file(
        &script,
        "(print 123)\n\
         (print 3/4)\n\
         (print 1.5)\n\
         (print #\\A)\n\
         (print \"hello\")\n\
         (print 'foo)\n\
         (print :bar)\n\
         (print '(1 (2 3) nil))\n\
         (print t)\n\
         (print nil)\n\
         (print (list (+ 1 (* 2 3))\n\
                      (car (cdr (cons 9 (list 8 7))))\n\
                      (cdr (list 4 5 6))))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss stage-0 gate");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    for expected in [
        "123", "3/4", "1.5", "#\\A", "\"HELLO\"", "FOO", ":BAR", "(1 (2 3) NIL)", "T", "NIL",
        "(7 8 (5 6))",
    ] {
        assert!(stdout.contains(expected), "missing `{expected}` in: {stdout}");
    }

    fs::remove_dir_all(dir).ok();
}

#[test]
#[ignore = "stage 6: ASDF self-host"]
fn bundled_asdf_is_reachable_via_require_with_output_translations_and_t1_metadata() {
    // Per R6.45-R6.48, the real CLI must delegate REQUIRE to bundled ASDF,
    // expose the implementation-owned output translation cache, and record
    // the minimum T1 compilation tier for the bootstrap ASDF load path.
    let output = bliss()
        .args([
            "--eval",
            "(require :asdf)\n(print bliss-ext:*asdf-output-translations*)\n(print asdf:*last-operation-tier*)",
        ])
        .output()
        .expect("run bliss --eval require asdf");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains(".CACHE/BLISS/ASDF"), "stdout: {stdout}");
    assert!(stdout.contains("T1"), "stdout: {stdout}");
}

#[test]
fn eval_when_body_runs_as_implicit_progn() {
    // eval-when must execute its body in the bootstrap evaluator (situations
    // are ignored); ASDF's top-level eval-when forms depend on this.
    let dir = temp_dir("evalwhen");
    let script = dir.join("ew.lisp");
    write_file(
        &script,
        "(eval-when (:load-toplevel :compile-toplevel :execute) (print (+ 20 22)))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss --load eval-when");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("42"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(dir).ok();
}

#[test]
fn bootstrap_defvar_preserves_existing_binding_and_assert_signals() {
    let keep = bliss()
        .args(["--eval", "(progn (setq x 1) (defvar x 2) x)"])
        .output()
        .expect("run bliss --eval defvar");
    assert_eq!(
        keep.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&keep.stderr)
    );
    assert!(
        String::from_utf8_lossy(&keep.stdout).contains('1'),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&keep.stdout),
        String::from_utf8_lossy(&keep.stderr)
    );

    let assert_fail = bliss()
        .args(["--eval", "(let ((x 1)) (assert nil) x)"])
        .output()
        .expect("run bliss --eval assert");
    assert_ne!(assert_fail.status.code(), Some(0), "assert should signal");
}

#[test]
fn bootstrap_define_condition_and_do_symbols_have_real_effects() {
    let define_condition = bliss()
        .args([
            "--eval",
            "(progn
               (define-condition foo (error) ())
               (let ((seen nil))
                 (handler-bind ((foo (lambda (c) (setq seen t))))
                   (signal (make-condition 'foo)))
                 seen))",
        ])
        .output()
        .expect("run bliss --eval define-condition");
    assert_eq!(
        define_condition.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&define_condition.stdout),
        String::from_utf8_lossy(&define_condition.stderr)
    );
    assert!(
        String::from_utf8_lossy(&define_condition.stdout)
            .to_uppercase()
            .contains('T'),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&define_condition.stdout),
        String::from_utf8_lossy(&define_condition.stderr)
    );

    let do_symbols = bliss()
        .args([
            "--eval",
            "(let ((seen nil)) (do-symbols (s) (setq seen t)) seen)",
        ])
        .output()
        .expect("run bliss --eval do-symbols");
    assert_eq!(
        do_symbols.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&do_symbols.stdout),
        String::from_utf8_lossy(&do_symbols.stderr)
    );
    assert!(
        String::from_utf8_lossy(&do_symbols.stdout)
            .to_uppercase()
            .contains('T'),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&do_symbols.stdout),
        String::from_utf8_lossy(&do_symbols.stderr)
    );
}

#[test]
fn prelude_loads_unconditionally_and_no_bootstrap_opts_out() {
    // The prelude defines standard-CL forms (defvar/push/pop/...), so it must
    // load for every invocation WITHOUT any flag. --no-bootstrap opts out.
    let dir = temp_dir("bootstrap");
    let script = dir.join("prelude.lisp");
    write_file(
        &script,
        "(defvar *stack* nil)(push 1 *stack*)(push 2 *stack*)(print (pop *stack*))(print *stack*)\n",
    );
    let path = script.to_str().expect("utf8 path");

    // No flag: prelude is present.
    let plain = bliss()
        .args(["--load", path])
        .output()
        .expect("run bliss --load");
    assert_eq!(
        plain.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(
        stdout.contains('2'),
        "expected popped value 2, stdout: {stdout}"
    );
    assert!(
        stdout.contains("(1)"),
        "expected remaining (1), stdout: {stdout}"
    );

    // --no-bootstrap: prelude absent, defvar is undefined -> failure.
    let opted_out = bliss()
        .args(["--no-bootstrap", "--load", path])
        .output()
        .expect("run bliss --no-bootstrap --load");
    assert_ne!(
        opted_out.status.code(),
        Some(0),
        "prelude must be absent under --no-bootstrap; stdout: {}",
        String::from_utf8_lossy(&opted_out.stdout)
    );

    fs::remove_dir_all(dir).ok();
}

#[test]
fn extended_loop_supports_asdf_load_path_clauses() {
    // The bootstrap LOOP must handle the clause mix ASDF's define-package
    // machinery uses at load time: destructuring :for, :when/:else chains,
    // :append :into named accumulators, and :finally (return ...).
    let dir = temp_dir("loop");
    let script = dir.join("loop.lisp");
    write_file(
        &script,
        "(print (loop :for (kw . args) :in (list (list :a 1 2) (list :b 3))\n\
           :when (eq kw :a) :append args :into as :else :append args :into bs\n\
           :finally (return (list as bs))))\n",
    );

    let output = bliss()
        .args(["--bootstrap", "--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss loop");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // as = (1 2), bs = (3)  ->  ((1 2) (3))
    assert!(stdout.contains("((1 2) (3))"), "stdout: {stdout}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn lambda_lists_bind_optional_rest_and_key() {
    // Functions must bind &optional (with defaults), &rest (as a proper list),
    // and &key — required to run ASDF's own utility functions.
    let dir = temp_dir("lambdalist");
    let script = dir.join("ll.lisp");
    write_file(
        &script,
        "(defun f (a &optional (b 10) &rest r) (list a b r))\n\
         (print (f 1))\n\
         (print (f 1 2 3 4))\n\
         (defun g (&key (n 5 np)) (list n np))\n\
         (print (g))\n\
         (print (g :n 9))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss lambda-list");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("(1 10 NIL)"), "f/1: {stdout}");
    assert!(stdout.contains("(1 2 (3 4))"), "f/rest: {stdout}");
    assert!(stdout.contains("(5 NIL)"), "g default: {stdout}");
    assert!(stdout.contains("(9 T)"), "g key+supplied: {stdout}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn hash_tables_and_funcall_of_builtins_work() {
    // Hash tables (make/setf-gethash/gethash + present-p) and funcall/mapcar of
    // builtin functions are prerequisites for ASDF's package machinery.
    let dir = temp_dir("htfc");
    let script = dir.join("ht.lisp");
    write_file(
        &script,
        "(defvar *h* (make-hash-table :test 'eql))\n\
         (setf (gethash :a *h*) 1)\n\
         (multiple-value-bind (v p) (gethash :a *h*) (print (list v p)))\n\
         (print (funcall #'eql 3 3))\n\
         (print (mapcar #'string '(a b)))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss ht");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("(1 T)"), "gethash present-p: {stdout}");
    assert!(stdout.contains('T'), "funcall eql: {stdout}");
    assert!(stdout.contains("(\"A\" \"B\")"), "mapcar string: {stdout}");
    fs::remove_dir_all(dir).ok();
}

#[test]
fn stage_one_gate_programs_run_through_the_real_cli() {
    // Per §0.4 stage 1 and §11.3 / spec/stages.json, the core-evaluator gate
    // is real CLI execution of recursion, higher-order list processing,
    // closures with captured state, and non-local exits.
    // Per R2.02 and R10.06, this must run through the actual startup/load path.
    let dir = temp_dir("stage1-gate");
    let script = dir.join("core-evaluator.lisp");
    write_file(
        &script,
        "(defun fib (n)\n\
           (if (eq n 0)\n\
               0\n\
               (if (eq n 1)\n\
                   1\n\
                   (+ (fib (- n 1)) (fib (- n 2))))))\n\
         (defun make-counter (start)\n\
           (let ((n start))\n\
             (lambda (&optional (delta 1))\n\
               (setq n (+ n delta))\n\
               n)))\n\
         (print (fib 30))\n\
         (print (mapcar (lambda (x) (* x x)) '(1 2 3 4)))\n\
         (let ((counter (make-counter 7)))\n\
           (print (list (funcall counter) (funcall counter 5))))\n\
         (print (catch 'done\n\
                  (progn\n\
                    (throw 'done '(escaped ok))\n\
                    nil)))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss stage-1 gate");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains("832040"), "fib output missing from: {stdout}");
    assert!(
        stdout.contains("(1 4 9 16)"),
        "mapcar/lambda output missing from: {stdout}"
    );
    assert!(
        stdout.contains("(8 13)"),
        "closure state output missing from: {stdout}"
    );
    assert!(
        stdout.contains("(ESCAPED OK)"),
        "catch/throw output missing from: {stdout}"
    );
    fs::remove_dir_all(dir).ok();
}
