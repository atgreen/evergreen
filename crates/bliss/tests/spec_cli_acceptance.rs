use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn cargo_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn bliss_bin_path() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let _guard = cargo_lock().lock().unwrap_or_else(|e| e.into_inner());
        let status = Command::new("cargo")
            .current_dir(repo_root())
            .args(["build", "-p", "bliss-cli"])
            .status()
            .expect("build bliss-cli");
        assert!(status.success(), "cargo build -p bliss-cli failed");
        PathBuf::from(env!("CARGO_BIN_EXE_bliss-cli"))
    })
    .as_path()
}

fn bliss() -> Command {
    Command::new(bliss_bin_path())
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
    // Isolate the init file (bliss-5s3): without this the REPL loads the
    // developer's ambient ~/.blissrc — a full ASDF bootstrap — which under a
    // debug build takes minutes and hangs the test. This test only needs a
    // hermetic read-eval-print, so point at a nonexistent init file.
    let mut child = bliss()
        .env(
            "BLISS_INIT_FILE",
            temp_dir("repl").join("nonexistent-init.lisp"),
        )
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
        stderr.contains("CL-USER>"),
        "the real REPL should print its current-package prompt before evaluating input: stdout: {stdout} stderr: {stderr}"
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

    let stdout = String::from_utf8_lossy(&output.stdout);
    // PRINT ends each datum with a SPACE per CLHS (not a newline) — bliss-g5dg —
    // so compare lines whitespace-insensitively. READ ignores trailing WS, so
    // the round-trip intent of the gate (one readable datum per line) is intact.
    let actual_lines: Vec<&str> = stdout
        .lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.is_empty())
        .collect();
    let expected_lines = vec![
        "123",
        "3/4",
        "1.5",
        "#\\A",
        "\"hello\"",
        "FOO",
        ":BAR",
        "(1 (2 3) NIL)",
        "T",
        "NIL",
        "(7 8 (5 6))",
    ];
    assert_eq!(
        actual_lines, expected_lines,
        "unexpected stage-0 gate stdout"
    );

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
fn write_line_and_file_length_builtins() {
    // WRITE-LINE was declared a builtin but never dispatched; FILE-LENGTH was
    // missing entirely. Both are used by ASDF's HTTP-stack dependencies.
    assert_eq!(
        eval_ok("(with-output-to-string (s) (write-line \"ab\" s) (write-string \"cd\" s))"),
        "\"ab\ncd\""
    );
    assert_eq!(
        eval_ok("(write-line \"x\" (make-string-output-stream))"),
        "\"x\""
    );
    // FILE-LENGTH of a freshly-written file returns its element count.
    assert_eq!(
        eval_ok(
            "(let ((p \"/tmp/bliss-fl-test.txt\"))\
               (with-open-file (s p :direction :output :if-exists :supersede) (write-string \"abcde\" s))\
               (with-open-file (s p) (file-length s)))"
        ),
        "5"
    );
}

#[test]
fn fill_pointer_and_adjustable_vectors() {
    // make-array :fill-pointer/:adjustable ⇒ a complex vector whose LENGTH is its
    // fill pointer; vector-push-extend grows it; vector-pop / (setf fill-pointer)
    // shrink it; aref and (setf aref) index the storage. bliss-edt.
    assert_eq!(
        eval_ok(
            "(let ((v (make-array 2 :fill-pointer 0 :adjustable t)))\
               (vector-push-extend 10 v) (vector-push-extend 20 v)\
               (vector-push-extend 30 v)\
               (list (length v) (fill-pointer v) (aref v 2) v))"
        ),
        "(3 3 30 #(10 20 30))"
    );
    // vector-pop returns the top and shrinks; (setf fill-pointer) truncates.
    assert_eq!(
        eval_ok(
            "(let ((v (make-array 3 :fill-pointer 3 :initial-element 7)))\
               (list (vector-pop v) (length v) (progn (setf (fill-pointer v) 1) v)))"
        ),
        "(7 2 #(7))"
    );
    // A fill-pointer vector is a vector and an array; (setf aref) mutates it.
    assert_eq!(
        eval_ok(
            "(let ((v (make-array 3 :fill-pointer 2 :initial-element 0)))\
               (setf (aref v 1) 42)\
               (list (vectorp v) (arrayp v) v))"
        ),
        "(T T #(0 42))"
    );
}

#[test]
fn read_and_write_sequence_builtins() {
    // READ-SEQUENCE fills a mutable sequence (string or vector) from a stream and
    // returns the stop index; WRITE-SEQUENCE writes a bounded subsequence.
    assert_eq!(
        eval_ok(
            "(let ((b (make-string 5))) (with-input-from-string (s \"hello world\") (list (read-sequence b s) b)))"
        ),
        "(5 \"hello\")"
    );
    assert_eq!(
        eval_ok("(with-output-to-string (s) (write-sequence \"abcdef\" s :start 1 :end 4))"),
        "\"bcd\""
    );
    // Into a vector, with a short read at EOF returning the actual count.
    assert_eq!(
        eval_ok(
            "(let ((b (make-array 4))) (with-input-from-string (s \"xy\") (read-sequence b s)))"
        ),
        "2"
    );
}

#[test]
fn typep_keyword() {
    // TYPEP had no KEYWORD case, so (typep :any 'keyword) was NIL despite
    // KEYWORDP being T — babel type-checks encoding names as keywords.
    assert_eq!(
        eval_ok(
            "(list (typep :utf-16 'keyword) (typep :foo 'keyword) (typep 'foo 'keyword) (typep 5 'keyword))"
        ),
        "(T T NIL NIL)"
    );
}

#[test]
fn deftype_body_is_evaluated_and_skips_docstring() {
    // A DEFTYPE body is code returning a type specifier, and may carry a
    // leading docstring (alexandria: POSITIVE-FIXNUM is
    //   (deftype positive-fixnum () "..." `(integer 1 ,most-positive-fixnum))).
    // The body must be evaluated (backquote expanded) and the docstring
    // dropped, otherwise TYPEP/CHECK-TYPE consulted the docstring string.
    assert_eq!(
        eval_ok(
            "(progn (deftype pf () \"a positive fixnum\" `(integer 1 ,most-positive-fixnum)) \
             (list (typep 8 'pf) (typep 0 'pf) (typep -3 'pf)))"
        ),
        "(T NIL NIL)"
    );
    // A plainly-quoted body form still works.
    assert_eq!(
        eval_ok(
            "(progn (deftype small () '(integer 0 9)) (list (typep 5 'small) (typep 10 'small)))"
        ),
        "(T NIL)"
    );
    // Parameterised deftype used bare expands with its parameters defaulted,
    // like alexandria's ARRAY-INDEX = (integer 0 (array-dimension-limit)) — an
    // *exclusive* upper bound.
    assert_eq!(
        eval_ok(
            "(progn (deftype aidx (&optional (n 8)) `(integer 0 (,n))) \
             (list (typep 0 'aidx) (typep 7 'aidx) (typep 8 'aidx)))"
        ),
        "(T T NIL)"
    );
}

#[test]
fn typep_integer_exclusive_bounds() {
    // CLHS integer type: a bound of the form `(n)` is exclusive. This used to
    // panic (as_fixnum on the `(n)` cons) — babel reaches it via ARRAY-INDEX.
    assert_eq!(
        eval_ok(
            "(list (typep 4 '(integer 0 (5))) (typep 5 '(integer 0 (5))) \
             (typep 0 '(integer (0) 5)) (typep 1 '(integer (0) 5)) \
             (typep 3 '(integer * (5))) (typep 9 '(integer (5) *)))"
        ),
        "(T NIL NIL T T T)"
    );
}

#[test]
fn the_function_accepts_interpreter_closures() {
    // (the function X) must agree with FUNCTIONP: an interpreter closure —
    // including one from #'localfn (labels/flet) — is a function. babel's
    // string-to-octets funcalls `(the function (encoder mapping))`.
    assert_eq!(
        eval_ok("(funcall (the function (lambda (x) (* x x))) 7)"),
        "49"
    );
    assert_eq!(
        eval_ok("(funcall (the function (labels ((f (x) (+ x 1))) (function f))) 41)"),
        "42"
    );
}

#[test]
fn schar_reads_and_writes_simple_strings() {
    // SCHAR is CHAR for simple strings; (setf (schar ...)) mutates in place.
    // babel's string-get/string-set expand to schar.
    assert_eq!(eval_ok("(schar \"abc\" 1)"), "#\\b");
    assert_eq!(
        eval_ok("(let ((s (copy-seq \"abc\"))) (setf (schar s 0) #\\X) s)"),
        "\"Xbc\""
    );
}

#[test]
fn loop_for_var_with_type_spec_and_parallel_and() {
    // `for i fixnum from ...` (a bare simple-type-spec) and `and` chaining
    // parallel iteration clauses — both used by babel's encoder loops. The
    // bounded clause drives termination even though the parallel one is
    // unbounded.
    assert_eq!(
        eval_ok("(loop for i fixnum from 0 below 3 and di fixnum from 10 collect (list i di))"),
        "((0 10) (1 11) (2 12))"
    );
    assert_eq!(
        eval_ok(
            "(let ((s 0)) (loop for i fixnum from 0 below 5 and d fixnum from 100 do (setf s (+ s d))) s)"
        ),
        "510"
    );
}

#[test]
fn loop_with_bare_type_and_final_arithmetic_value() {
    // Babel's generated UTF-8 counters combine a typed WITH binding with a
    // FINALLY form that returns both the count and the exhausted loop index.
    // Per ansi-test LOOP.1.40-43, FINALLY sees the last IN-RANGE value of an
    // arithmetic driver, not the stepped-past one: `for i from 0 below 2`
    // leaves i=1, so this is (2 1). The compiled and tree-walked tiers must
    // agree here (S5 tier consistency; bliss-uj7m) — the original (2 2)
    // assertion encoded the compiled path's stepped-past bug.
    assert_eq!(
        eval_ok(
            "(multiple-value-list (loop with n fixnum = 0 for i fixnum from 0 below 2 do (incf n) finally (return (values n i))))"
        ),
        "(2 1)"
    );
}

#[test]
fn equal_compares_pathnames_by_components() {
    // CLHS: EQUAL on pathnames is true when their components match. bliss's
    // EQUAL returned NIL for equal pathnames, so ASDF's pathname-keyed caches
    // and comparisons never matched, breaking asdf:load-system (bliss-nad).
    assert_eq!(
        eval_ok("(equal (pathname \"/a/b\") (pathname \"/a/b\"))"),
        "T"
    );
    assert_eq!(
        eval_ok("(equal (pathname \"/a/b\") (pathname \"/a/c\"))"),
        "NIL"
    );
    // A pathname is not EQUAL to its namestring.
    assert_eq!(eval_ok("(equal (pathname \"/a/b\") \"/a/b\")"), "NIL");
    // EQUAL pathnames must hash together in an EQUAL table (already did).
    assert_eq!(
        eval_ok(
            "(let ((h (make-hash-table :test 'equal))) (setf (gethash (pathname \"/a/b\") h) 1) (nth-value 1 (gethash (pathname \"/a/b\") h)))"
        ),
        "T"
    );
}

#[test]
fn package_used_by_list_reports_using_packages() {
    // PACKAGE-USED-BY-LIST was stubbed to NIL, which broke UIOP's ensure-package
    // reconciliation (ensure-exported walks it to propagate exports into
    // inheriting packages), leaving asdf:load-system unable to converge
    // (bliss-nad).
    assert_eq!(
        eval_ok(
            "(progn (defpackage :ubl-a (:use)) (defpackage :ubl-b (:use :ubl-a)) \
             (mapcar #'package-name (package-used-by-list :ubl-a)))"
        ),
        "(\"UBL-B\")"
    );
    // A package nobody uses reports NIL.
    assert_eq!(
        eval_ok("(progn (defpackage :ubl-lonely (:use)) (package-used-by-list :ubl-lonely))"),
        "NIL"
    );
}

#[test]
fn equalp_compares_vectors_strings_chars_numbers() {
    // EQUALP (unlike EQUAL) compares vectors element-wise, strings/chars
    // case-insensitively, and numbers across types. babel's define-constant
    // tables rely on this. EQUAL must stay case- and type-sensitive.
    assert_eq!(
        eval_ok(
            "(list (equalp #(1 2 3) #(1 2 3)) (equalp #(1 2) #(1 2 3)) (equalp \"ab\" \"AB\") (equalp #\\a #\\A) (equalp 1 1.0) (equalp '(1 #(2 3)) '(1 #(2 3))))"
        ),
        "(T NIL T T T T)"
    );
    assert_eq!(
        eval_ok("(list (equal #(1 2 3) #(1 2 3)) (equal \"ab\" \"AB\"))"),
        "(NIL NIL)"
    );
    // Hash tables: same test, count, and EQUALP values.
    assert_eq!(
        eval_ok(
            "(let ((a (make-hash-table)) (b (make-hash-table)) (c (make-hash-table))) (setf (gethash 1 a) :x (gethash 1 b) :x (gethash 1 c) :y) (list (equalp a b) (equalp a c)))"
        ),
        "(T NIL)"
    );
}

#[test]
fn constantp_recognizes_constant_forms() {
    assert_eq!(
        eval_ok(
            "(list (constantp 5) (constantp :k) (constantp t) (constantp nil) (constantp '(quote x)) (constantp 'foo) (constantp '(+ 1 2)))"
        ),
        "(T T T T T NIL NIL)"
    );
    // A DEFCONSTANT'd symbol is recognised as constant (alexandria's
    // define-constant / babel rely on this); a defparameter is not.
    assert_eq!(
        eval_ok(
            "(progn (defconstant +kk+ 42) (defparameter *pp* 1) (list (constantp '+kk+) (constantp '*pp*)))"
        ),
        "(T NIL)"
    );
}

#[test]
fn setf_writer_function_is_global_across_scopes() {
    // A top-level (defun (setf place) …) is a GLOBAL definition: `(setf (place …)
    // v)` must find it from any scope, including inside another function defined
    // separately — babel defines (setf get-abstract-mapping) in one file and uses
    // it from another. It used to be stored per-Env and lost across files.
    assert_eq!(
        eval_ok(
            "(progn \
               (defvar *h* (make-hash-table)) \
               (defun gam (e) (gethash e *h*)) \
               (defun (setf gam) (v e) (setf (gethash e *h*) v)) \
               (defun use-it (e) (setf (gam e) 99)) \
               (use-it :k) (+ 0 (gam :k)))"
        ),
        "99"
    );
}

#[test]
fn initarg_from_initialize_instance_key_is_valid() {
    // CLHS 7.1.2: an initarg that matches no slot :initarg is still valid if an
    // applicable INITIALIZE-INSTANCE/SHARED-INITIALIZE method declares it as a
    // &key. bliss used to reject it ("Unknown initarg") — babel's
    // CHARACTER-ENCODING consumes :literal-char-code-limit exactly this way.
    assert_eq!(
        eval_ok(
            "(progn \
               (defclass c () ((x :initarg :x :reader cx))) \
               (defmethod initialize-instance :after ((o c) &key extra) \
                 (when extra (setf (slot-value o 'x) (+ (slot-value o 'x) extra)))) \
               (cx (make-instance 'c :x 1 :extra 41)))"
        ),
        "42"
    );
}

#[test]
fn sharp_quote_local_flet_labels_function_is_callable() {
    // #'localfn / (function localfn) on an flet/labels function must yield a
    // callable value that survives the flet scope (CLHS 3.1.2.1.2.2) — e.g.
    // passed to MAPCAR. It used to return the bare symbol, which resolved
    // globally (undefined) once out of scope; library macros that build
    // #'<gensym> (babel's encoders) surfaced this as "undefined function G<n>".
    assert_eq!(
        eval_ok("(flet ((sq (x) (* x x))) (mapcar #'sq '(1 2 3)))"),
        "(1 4 9)"
    );
    assert_eq!(
        eval_ok("(labels ((tri (x) (if (<= x 0) 0 (+ x (tri (1- x)))))) (mapcar #'tri '(1 2 3)))"),
        "(1 3 6)"
    );
    // Also correct after promotion to the native tier (was a T1 crash).
    assert_eq!(
        eval_ok(
            "(progn (defun fq (x) (flet ((g (y) (* y y))) (funcall #'g x))) (dotimes (i 3000) (fq 2)) (fq 9))"
        ),
        "81"
    );
}

#[test]
fn conditions_princ_to_their_report_string() {
    // CLHS 9.1: princ/~A of a condition prints its report; ~S keeps #<TYPE …>.
    // Without a report, an UNDEFINED-FUNCTION printed as an opaque
    // #<UNDEFINED-FUNCTION> with no name, making failed ASDF compiles (missing
    // builtins) undiagnosable.
    assert_eq!(
        eval_ok(
            "(format nil \"~a\" (handler-case (funcall (read-from-string \"no-such-fn\")) (undefined-function (e) e)))"
        ),
        "\"The function NO-SUCH-FN is undefined.\""
    );
    assert_eq!(
        eval_ok(
            "(format nil \"~a\" (handler-case (symbol-value 'no-such-var) (unbound-variable (e) e)))"
        ),
        "\"The variable NO-SUCH-VAR is unbound.\""
    );
    // ~S stays the opaque object form.
    assert_eq!(
        eval_ok(
            "(format nil \"~s\" (handler-case (funcall (read-from-string \"nope-fn\")) (undefined-function (e) e)))"
        ),
        "\"#<UNDEFINED-FUNCTION>\""
    );
}

#[test]
fn loop_it_anaphor_binds_conditional_test_value() {
    // CLHS 6.1.5: inside a selected when/if branch, `it` denotes the value of the
    // test. ASDF's REGISTER-SYSTEM-DEFINITION relies on this
    //   (loop :for spec :in dep-forms :when (resolve-dependency-spec nil spec) :collect :it)
    // so a broken anaphor collected the literal keyword :IT and tried to
    // load-system it (phantom MISSING-COMPONENT :IT for every :defsystem-depends-on).
    assert_eq!(
        eval_ok("(loop for x in '(1 nil 2 nil 3) when x collect it)"),
        "(1 2 3)"
    );
    // LOOP matches anaphora by name, so the keyword :it works exactly as `it`.
    assert_eq!(
        eval_ok("(loop for x in '(1 nil 2 nil 3) when x collect :it)"),
        "(1 2 3)"
    );
    // The binding is visible to a nested `it`, and to `if`/`sum`/`do`.
    assert_eq!(
        eval_ok("(loop for x in '(1 nil 2) when x collect (cons it it))"),
        "((1 . 1) (2 . 2))"
    );
    assert_eq!(eval_ok("(loop for x in '(1 2 3) if (* x 10) sum it)"), "60");
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
fn hash_table_predicate_snapshots_and_clear_work() {
    assert_eq!(
        eval_ok(
            "(let ((h (make-hash-table))) (setf (gethash 'a h) 1 (gethash 'b h) 2) (let ((keys (hash-table-keys h)) (values (hash-table-values h))) (list (hash-table-p h) (hash-table-p nil) (length keys) (length values) (eq h (clrhash h)) (hash-table-count h))))"
        ),
        "(T NIL 2 2 T 0)"
    );
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
         (print (fib 20))\n\
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
    assert!(stdout.contains("6765"), "fib output missing from: {stdout}");
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

#[test]
fn stage_two_gate_loads_a_real_file_with_user_macros_standard_macros_and_environment_aware_expansion()
 {
    // Per §0.4 stage 2 and spec/stages.json, the real CLI gate is loading a
    // multi-form file that defines and uses its own macros plus standard macros.
    let dir = temp_dir("stage2-gate");
    let script = dir.join("stage2-macros.lisp");
    write_file(
        &script,
        "(defmacro expand-local (form &environment env)\n\
           (macroexpand form env))\n\
         (defmacro sum-pairs (pairs)\n\
           `(let ((total 0))\n\
              (dolist (pair ,pairs)\n\
                (destructuring-bind (a . b) pair\n\
                  (when t\n\
                    (incf total (+ a b)))))\n\
              total))\n\
         (print\n\
           (list\n\
             (macrolet ((local-answer () 41))\n\
               (+ 1 (expand-local (local-answer))))\n\
             (sum-pairs '((1 . 2) (3 . 4)))))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss stage-2 gate");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("(42 10)"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    fs::remove_dir_all(dir).ok();
}

#[test]
fn stage_three_gate_runs_a_library_heavy_program_through_the_real_cli() {
    // Per spec/stages.json stage 3, the real CLI gate is a library-heavy
    // program with observable string/sequence/hash-table/format results.
    // Per R5.59, R5.60, R5.62, R5.63, and R5.64, the bootstrap path that
    // makes these library entrypoints available must complete before user code.
    let dir = temp_dir("stage3-gate");
    let script = dir.join("stage3-library.lisp");
    write_file(
        &script,
        "(let* ((text (format nil \"~A-~D\" 'bliss 3))\n\
                (numbers '(1 3 5))\n\
                (table (make-hash-table :test 'equal))\n\
                (seq (concatenate 'list '(1 2) '(3 4 5))))\n\
           (setf (gethash \"TEXT\" table) text)\n\
           (setf (gethash \"SLICE\" table) (subseq seq 1 4))\n\
           (print (list (gethash \"TEXT\" table)\n\
                        (gethash \"SLICE\" table)\n\
                        seq\n\
                        (format nil \"~{~A~^, ~}\" numbers))))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss stage-3 gate");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(
        stdout.contains("(\"BLISS-3\" (2 3 4) (1 2 3 4 5) \"1, 3, 5\")"),
        "stdout: {stdout}"
    );

    fs::remove_dir_all(dir).ok();
}

#[test]
fn stage_four_gate_runs_conditions_clos_and_restarts_through_the_real_cli() {
    // Per spec/stages.json stage 4, the real CLI gate must prove user-defined
    // classes/generic functions plus condition handling/restarts end-to-end.
    let dir = temp_dir("stage4-gate");
    let script = dir.join("stage4-conditions-clos.lisp");
    write_file(
        &script,
        "(define-condition pet-error (error) ())\n\
         (defclass animal () ())\n\
         (defclass dog (animal) ())\n\
         (defclass cat (animal) ())\n\
         (defgeneric pair-speak (x y))\n\
         (defmethod pair-speak ((x animal) (y dog)) 'animal-dog)\n\
         (defmethod pair-speak ((x dog) (y animal)) 'dog-animal)\n\
         (print (pair-speak (make-instance 'animal) (make-instance 'dog)))\n\
         (print (handler-case\n\
                  (progn\n\
                    (signal (make-condition 'pet-error))\n\
                    'after)\n\
                  (pet-error (c)\n\
                    (declare (ignore c))\n\
                    'caught)))\n\
         (print (handler-bind\n\
                  ((error (lambda (c)\n\
                            (declare (ignore c))\n\
                            (invoke-restart 'continue))))\n\
                  (restart-case\n\
                    (error \"boom\")\n\
                    (continue () 'recovered))))\n",
    );

    let output = bliss()
        .args(["--load", script.to_str().expect("utf8 path")])
        .output()
        .expect("run bliss stage-4 gate");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains("ANIMAL-DOG"), "stdout: {stdout}");
    assert!(stdout.contains("CAUGHT"), "stdout: {stdout}");
    assert!(stdout.contains("RECOVERED"), "stdout: {stdout}");

    fs::remove_dir_all(dir).ok();
}

/// Stage-5 gate (spec/stages.json stage 5, "hotspot-engine"): a hot loop is
/// observably promoted through tiers with identical results at each tier, and an
/// invalidated speculation deoptimizes and still returns the correct result. All
/// through the real CLI binary.
///
/// The script warms a pure fixnum loop until it promotes to T1, then triggers a
/// deopt by overflowing the fixnum range. It prints tier/deopt observability
/// lines plus `RESULT` lines. We run it twice — once forcing tier-up
/// (BLISS_T1_THRESHOLD low) and once under the pure tree-walker (T0) — and prove
/// the `RESULT` values are identical across tiers, while the forced run also
/// observes the promotion (TIER 1) and a deoptimization (DEOPTS > 0).
#[test]
fn stage_five_gate_hot_loop_promotes_through_tiers_and_deopts_with_identical_results() {
    let dir = temp_dir("stage5-gate");
    let script = dir.join("stage5-hotspot.lisp");
    write_file(
        &script,
        // sumsq: a hot pure-fixnum loop (tagbody/go) — promotes to native T1.
        // f: overflows the fixnum range for large x, forcing a speculative
        // deopt; the result is reduced mod a fixnum so it prints identically in
        // both tiers (bignums print opaquely).
        "(defun sumsq (n)\n\
           (let ((s 0) (i 0))\n\
             (tagbody top (when (< i n) (setq s (+ s (* i i))) (setq i (+ i 1)) (go top)))\n\
             s))\n\
         (defun f (x) (mod (* x x) 1000000))\n\
         (sumsq 20) (sumsq 20) (sumsq 20) (sumsq 20) (sumsq 20)\n\
         (dotimes (i 40) (f (+ 3 i)))\n\
         (format t \"RESULT loop ~a~%\" (sumsq 100))\n\
         (format t \"RESULT deopt ~a~%\" (f 3037000500))\n\
         (format t \"TIER ~a~%\" (bliss-ext:function-tier 'sumsq))\n\
         (format t \"DEOPTS ~a~%\" (bliss-ext:deopt-count))\n",
    );
    let path = script.to_str().expect("utf8 path");

    // Forced tier-up run: low promotion threshold so the loop reaches T1.
    let t1 = bliss()
        .env("BLISS_T1_THRESHOLD", "2")
        .args(["--load", path])
        .output()
        .expect("run bliss stage-5 gate (T1)");
    assert_eq!(
        t1.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&t1.stdout),
        String::from_utf8_lossy(&t1.stderr)
    );
    let t1_out = String::from_utf8_lossy(&t1.stdout);

    // Pure interpreter run (T0) for the result oracle.
    let t0 = bliss()
        .env("BLISS_BACKEND", "tree-walker")
        .args(["--load", path])
        .output()
        .expect("run bliss stage-5 gate (T0)");
    assert_eq!(t0.status.code(), Some(0), "T0 run failed");
    let t0_out = String::from_utf8_lossy(&t0.stdout);

    let results = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| l.starts_with("RESULT "))
            .map(str::to_string)
            .collect()
    };
    // Identical results at each tier — the heart of the gate.
    assert_eq!(
        results(&t1_out),
        results(&t0_out),
        "hot-loop and deopt results must be identical across tiers\nT1:\n{t1_out}\nT0:\n{t0_out}"
    );
    assert!(
        !results(&t1_out).is_empty(),
        "expected RESULT lines: {t1_out}"
    );

    // The forced run must observe the promotion and at least one deopt.
    assert!(
        t1_out.contains("TIER 1"),
        "the hot loop must be observably promoted to T1: {t1_out}"
    );
    let deopts: u64 = t1_out
        .lines()
        .find_map(|l| l.strip_prefix("DEOPTS "))
        .and_then(|n| n.trim().parse().ok())
        .expect("DEOPTS line present");
    assert!(
        deopts >= 1,
        "the overflow must trigger a deoptimization: {t1_out}"
    );

    fs::remove_dir_all(dir).ok();
}

#[test]
fn stage_four_cli_exposes_condition_readers_and_core_clos_slot_protocol() {
    let output = bliss()
        .args([
            "--eval",
            "(progn
               (define-condition pet-error (error) ((name :initarg :name :reader pet-error-name)))
               (defclass animal () ((name :initarg :name)))
               (defclass dog (animal) ((breed :initarg :breed)))
               (let ((pet (make-instance 'animal :name 'spot))
                     (dog (make-instance 'dog :name 'fido :breed 'collie))
                     (err (make-condition 'pet-error :name 'bad-dog)))
                 (list (pet-error-name err)
                       (slot-value pet 'name)
                       (slot-boundp dog 'breed)
                       (class-of dog)
                       (progn
                         (reinitialize-instance dog :breed 'shepherd)
                         (slot-value dog 'breed))
                       (progn
                         (change-class pet 'dog)
                         (slot-boundp pet 'breed)))))",
        ])
        .output()
        .expect("run bliss stage-4 clos slot protocol");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains("BAD-DOG"), "stdout: {stdout}");
    assert!(stdout.contains("SPOT"), "stdout: {stdout}");
    assert!(stdout.contains("SHEPHERD"), "stdout: {stdout}");
    assert!(stdout.contains("T"), "stdout: {stdout}");
    assert!(stdout.contains("NIL"), "stdout: {stdout}");
}

#[test]
fn stage_four_cli_supports_call_next_method_eql_specializers_and_short_form_combination() {
    let output = bliss()
        .args([
            "--eval",
            "(progn
               (defclass animal () ())
               (defclass dog (animal) ())
               (defgeneric speak (x))
               (defmethod speak :before ((x animal)) (print 'before-animal))
               (defmethod speak :after ((x animal)) (print 'after-animal))
               (defmethod speak :around ((x animal)) (call-next-method))
               (defmethod speak ((x animal)) 'animal)
               (defmethod speak ((x dog)) (call-next-method))
               (defgeneric pick (x))
               (defmethod pick ((x (eql 7))) 'seven)
               (defmethod pick ((x t)) 'other)
               (defgeneric collect (x) (:method-combination list))
               ;; Two DISTINCT specializers, both applicable to an integer, so the
               ;; list combination collects from both (most-specific first). Same
               ;; parameter specializers would be one method — the second replaces
               ;; the first per CLHS 7.6.2 (bliss-day).
               (defmethod collect ((x integer)) 'a)
               (defmethod collect ((x t)) 'b)
               (defgeneric sum-values (x) (:method-combination +))
               (defmethod sum-values + ((x integer)) 1)
               (defmethod sum-values + ((x t)) 2)
               (list (speak (make-instance 'dog))
                     (pick 7)
                     (collect 0)
                     (sum-values 0)))",
        ])
        .output()
        .expect("run bliss stage-4 generic dispatch");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains("BEFORE-ANIMAL"), "stdout: {stdout}");
    assert!(stdout.contains("AFTER-ANIMAL"), "stdout: {stdout}");
    assert!(stdout.contains("ANIMAL"), "stdout: {stdout}");
    assert!(stdout.contains("SEVEN"), "stdout: {stdout}");
    assert!(stdout.contains("(A B)"), "stdout: {stdout}");
    assert!(stdout.contains("3"), "stdout: {stdout}");
}

#[test]
fn stage_four_cli_exposes_restart_queries_and_interactive_invocation() {
    let output = bliss()
        .args([
            "--eval",
            "(restart-bind
               ((visible (lambda (x) x)
                  :interactive-function (lambda () 99)
                  :test-function (lambda (c) t))
                (hidden (lambda () 'hidden)
                  :test-function (lambda (c) nil)))
               (list (compute-restarts 'dummy)
                     (find-restart 'visible 'dummy)
                     (invoke-restart-interactively 'visible)))",
        ])
        .output()
        .expect("run bliss stage-4 restart apis");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_uppercase();
    assert!(stdout.contains("VISIBLE"), "stdout: {stdout}");
    assert!(!stdout.contains("HIDDEN"), "stdout: {stdout}");
    assert!(stdout.contains("99"), "stdout: {stdout}");
}

#[test]
fn stage_four_cli_rejects_builtin_class_instantiation() {
    let output = bliss()
        .args(["--eval", "(make-instance (class-of 7))"])
        .output()
        .expect("run bliss built-in instantiation rejection");

    assert_ne!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Evaluate a single expression through the real CLI and return trimmed stdout.
fn eval_ok(expr: &str) -> String {
    let output = bliss()
        .args(["--eval", expr])
        .output()
        .expect("run bliss --eval");
    assert_eq!(
        output.status.code(),
        Some(0),
        "expr `{expr}` exited non-zero; stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn clos_inherited_slot_initforms_are_evaluated_during_make_instance() {
    // R5.80: make-instance must evaluate :initform for every effective slot,
    // including slots inherited from superclasses — not just self-evaluating
    // literals. The initform here is a non-trivial form that must be *evaluated*.
    assert_eq!(
        eval_ok(
            "(progn (defclass base () ((x :initform (+ 40 2) :accessor base-x))) \
                    (defclass mid (base) ()) \
                    (defclass leaf (mid) ()) \
                    (base-x (make-instance 'leaf)))"
        ),
        "42",
        "inherited :initform must be evaluated through the full class precedence list"
    );
}

#[test]
fn clos_inherited_initform_reachable_via_slot_value() {
    assert_eq!(
        eval_ok(
            "(progn (defclass base () ((x :initform (* 6 7)))) \
                    (defclass sub (base) ()) \
                    (slot-value (make-instance 'sub) 'x))"
        ),
        "42"
    );
}

#[test]
fn clos_subclass_without_initform_keeps_inherited_default() {
    // A subclass that redeclares a slot but supplies no :initform must not
    // shadow the inherited default (ANSI effective-slot :initform inheritance).
    assert_eq!(
        eval_ok(
            "(progn (defclass base () ((x :initform 7 :accessor bx))) \
                    (defclass sub (base) ((x :accessor sx))) \
                    (bx (make-instance 'sub)))"
        ),
        "7"
    );
}

#[test]
fn clos_explicit_initarg_overrides_inherited_initform() {
    assert_eq!(
        eval_ok(
            "(progn (defclass base () ((x :initarg :x :initform 1 :accessor bx))) \
                    (defclass sub (base) ()) \
                    (bx (make-instance 'sub :x 99)))"
        ),
        "99"
    );
}

#[test]
fn clos_typep_honours_user_class_hierarchy() {
    // typep must consult the CLOS class precedence list for user instances,
    // matching a class against every one of its superclasses.
    assert_eq!(
        eval_ok(
            "(progn (defclass animal () ()) (defclass dog (animal) ()) \
                    (let ((d (make-instance 'dog))) \
                      (list (typep d 'dog) (typep d 'animal) \
                            (typep d 'standard-object) (typep d 't))))"
        ),
        "(T T T T)"
    );
}

#[test]
fn clos_typep_rejects_unrelated_user_class() {
    assert_eq!(
        eval_ok(
            "(progn (defclass animal () ()) (defclass plant () ()) \
                    (typep (make-instance 'animal) 'plant))"
        ),
        "NIL"
    );
}

#[test]
fn condition_make_condition_evaluates_slot_initforms() {
    // Conditions are CLOS objects: MAKE-CONDITION must run the same initform
    // protocol as MAKE-INSTANCE, evaluating a non-literal :initform.
    assert_eq!(
        eval_ok(
            "(progn (define-condition my-err (error) \
                      ((code :initform (+ 1 41) :reader err-code))) \
                    (err-code (make-condition 'my-err)))"
        ),
        "42"
    );
}

#[test]
fn condition_inherits_slot_initform_from_parent_condition() {
    assert_eq!(
        eval_ok(
            "(progn (define-condition base-err (error) \
                      ((code :initform 5 :reader err-code))) \
                    (define-condition sub-err (base-err) ()) \
                    (err-code (make-condition 'sub-err)))"
        ),
        "5"
    );
}

#[test]
fn condition_error_accepts_type_designator_symbol() {
    // (error 'type ...) must build a condition instance and dispatch handlers
    // through the CLOS class hierarchy — the slot :initform is applied too.
    assert_eq!(
        eval_ok(
            "(progn (define-condition my-err (error) \
                      ((code :initform 7 :reader err-code))) \
                    (handler-case (error 'my-err) (my-err (c) (err-code c))))"
        ),
        "7"
    );
}

#[test]
fn condition_error_symbol_passes_initargs() {
    assert_eq!(
        eval_ok(
            "(progn (define-condition my-err (error) \
                      ((code :initarg :code :reader err-code))) \
                    (handler-case (error 'my-err :code 99) (my-err (c) (err-code c))))"
        ),
        "99"
    );
}

#[test]
fn condition_signal_accepts_type_designator_symbol() {
    assert_eq!(
        eval_ok(
            "(progn (define-condition w (warning) ()) \
                    (handler-case (signal 'w) (w (c) 'caught)))"
        ),
        "CAUGHT"
    );
}

#[test]
fn condition_handler_matches_via_class_hierarchy() {
    // A handler for a superclass condition catches a subclass condition, and an
    // unrelated handler does not — proving handler matching walks the CLOS CPL.
    assert_eq!(
        eval_ok(
            "(progn (define-condition my-err (error) ()) \
                    (handler-case (error 'my-err) \
                      (warning (c) 'wrong) \
                      (error (c) 'right)))"
        ),
        "RIGHT"
    );
}

#[test]
fn condition_warn_signals_catchable_warning() {
    assert_eq!(
        eval_ok(
            "(progn (define-condition w (warning) ()) \
                    (handler-case (warn 'w) (warning (c) 'caught)))"
        ),
        "CAUGHT"
    );
}

#[test]
fn condition_warn_string_is_simple_warning() {
    assert_eq!(
        eval_ok("(handler-case (warn \"heads up ~a\" 42) (warning (c) 'caught))"),
        "CAUGHT"
    );
}

#[test]
fn condition_warn_muffle_restart_returns_nil() {
    // MUFFLE-WARNING suppresses the default warning and lets execution continue.
    assert_eq!(
        eval_ok(
            "(progn (handler-bind ((warning (lambda (c) (invoke-restart 'muffle-warning)))) \
                      (warn \"muffled\")) \
                    'after)"
        ),
        "AFTER"
    );
}

#[test]
fn clos_unbound_slot_signals_catchable_condition() {
    // R5.71: reading an unbound slot signals a real UNBOUND-SLOT condition that
    // handler-case can catch (also matchable as the more general ERROR).
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ((x))) \
                    (handler-case (slot-value (make-instance 'a) 'x) \
                      (unbound-slot (c) 'unbound) (error (c) 'other)))"
        ),
        "UNBOUND"
    );
}

#[test]
fn clos_no_applicable_method_is_catchable_error() {
    assert_eq!(
        eval_ok(
            "(progn (defgeneric g (x)) (defmethod g ((x integer)) 'int) \
                    (handler-case (g \"str\") (error (c) 'no-method)))"
        ),
        "NO-METHOD"
    );
}

#[test]
fn clos_no_next_method_is_catchable_error() {
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ()) (defmethod g ((x a)) (call-next-method)) \
                    (handler-case (g (make-instance 'a)) (error (c) 'no-next)))"
        ),
        "NO-NEXT"
    );
}

#[test]
fn clos_dispatch_on_builtin_type_specializers() {
    // Methods specialized on built-in CL types must dispatch on immediate values.
    assert_eq!(
        eval_ok(
            "(progn (defmethod k ((x integer)) 'int) (defmethod k ((x string)) 'str) \
                    (defmethod k ((x symbol)) 'sym) (defmethod k ((x character)) 'char) \
                    (defmethod k ((x cons)) 'cons) \
                    (list (k 5) (k \"s\") (k 'a) (k #\\x) (k '(1))))"
        ),
        "(INT STR SYM CHAR CONS)"
    );
}

#[test]
fn clos_builtin_specializer_specificity_orders_subtype_first() {
    // A fixnum matches both INTEGER and NUMBER; the more specific INTEGER wins.
    assert_eq!(
        eval_ok(
            "(progn (defmethod h ((x number)) 'num) (defmethod h ((x integer)) 'int) \
                    (list (h 5) (h 1.5)))"
        ),
        "(INT NUM)"
    );
}

#[test]
fn clos_setf_slot_value_writes_slot() {
    assert_eq!(
        eval_ok(
            "(progn (defclass p () ((x))) (let ((o (make-instance 'p))) \
                    (setf (slot-value o 'x) 7) (slot-value o 'x)))"
        ),
        "7"
    );
}

#[test]
fn clos_with_slots_reads_and_writes() {
    assert_eq!(
        eval_ok(
            "(progn (defclass p () ((x :initform 1))) (let ((o (make-instance 'p))) \
                    (with-slots (x) o (setf x 10)) (slot-value o 'x)))"
        ),
        "10"
    );
}

#[test]
fn clos_with_slots_supports_renamed_bindings() {
    assert_eq!(
        eval_ok(
            "(progn (defclass p () ((x :initform 5))) \
                    (with-slots ((a x)) (make-instance 'p) a))"
        ),
        "5"
    );
}

#[test]
fn clos_with_accessors_reads_and_writes() {
    assert_eq!(
        eval_ok(
            "(progn (defclass p () ((x :initform 3 :accessor px))) (let ((o (make-instance 'p))) \
                    (with-accessors ((a px)) o (setf a 9)) (px o)))"
        ),
        "9"
    );
}

#[test]
fn condition_standard_type_error_accessors() {
    assert_eq!(
        eval_ok(
            "(handler-case (error 'type-error :datum 5 :expected-type 'string) \
               (type-error (c) (list (type-error-datum c) (type-error-expected-type c))))"
        ),
        "(5 STRING)"
    );
}

#[test]
fn condition_simple_condition_format_control_accessor() {
    assert_eq!(
        eval_ok(
            "(handler-case (error \"boom\") (simple-error (c) (simple-condition-format-control c)))"
        ),
        "\"boom\""
    );
}

#[test]
fn condition_cell_error_name_on_unbound_slot() {
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ((x))) \
               (handler-case (slot-value (make-instance 'a) 'x) \
                 (unbound-slot (c) (cell-error-name c))))"
        ),
        "X"
    );
}

#[test]
fn format_aesthetic_prints_lists() {
    // ~A must print a cons as a list, not an object address.
    assert_eq!(eval_ok("(format nil \"~a\" (list 1 2 3))"), "\"(1 2 3)\"");
}

#[test]
fn format_nested_and_dotted_lists() {
    assert_eq!(
        eval_ok("(format nil \"~a\" '(a (b c) d))"),
        "\"(A (B C) D)\""
    );
    assert_eq!(eval_ok("(format nil \"~a\" (cons 1 2))"), "\"(1 . 2)\"");
}

#[test]
fn format_standard_escapes_strings_inside_lists() {
    // ~S escapes strings/symbols recursively; ~A does not escape strings.
    assert_eq!(
        eval_ok("(format nil \"~s\" (list \"a\" 'b 3))"),
        "\"(\\\"a\\\" B 3)\""
    );
    assert_eq!(
        eval_ok("(format nil \"~a\" (list \"x\" \"y\"))"),
        "\"(x y)\""
    );
}

#[test]
fn printers_princ_and_prin1_to_string_handle_lists() {
    assert_eq!(eval_ok("(princ-to-string (list 1 2 3))"), "\"(1 2 3)\"");
    assert_eq!(
        eval_ok("(prin1-to-string (list \"a\" 'b))"),
        "\"(\\\"a\\\" B)\""
    );
}

#[test]
fn clos_instance_is_not_an_integer_despite_fixnum_representation() {
    // Instances are internally tagged fixnum ids; typep must not leak that.
    assert_eq!(
        eval_ok("(progn (defclass a () ()) (typep (make-instance 'a) 'integer))"),
        "NIL"
    );
    assert_eq!(
        eval_ok("(progn (defclass a () ()) (typep (make-instance 'a) 'number))"),
        "NIL"
    );
}

#[test]
fn clos_integer_specializer_does_not_capture_instances() {
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ()) (defmethod k ((x integer)) 'int) (defmethod k ((x a)) 'is-a) \
                    (list (k 5) (k (make-instance 'a))))"
        ),
        "(INT IS-A)"
    );
}

#[test]
fn macro_typecase_dispatches_on_type() {
    assert_eq!(
        eval_ok("(typecase 5 (string 's) (integer 'i) (t 'other))"),
        "I"
    );
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ()) (typecase (make-instance 'a) (integer 'i) (a 'is-a) (t 'other)))"
        ),
        "IS-A"
    );
}

#[test]
fn macro_ecase_and_etypecase_error_on_fall_through() {
    assert_eq!(eval_ok("(ecase 2 (1 'one) (2 'two))"), "TWO");
    assert_eq!(
        eval_ok("(handler-case (ecase 9 (1 'one)) (error (c) 'no-key))"),
        "NO-KEY"
    );
    assert_eq!(
        eval_ok("(handler-case (etypecase 5 (string 's)) (error (c) 'fell-through))"),
        "FELL-THROUGH"
    );
}

#[test]
fn macro_ignore_errors_returns_nil_on_error() {
    // IGNORE-ERRORS returns TWO values on error: NIL and the condition (CLHS;
    // bliss-xy7t). eval_ok renders every returned value, so the caught SIMPLE-ERROR
    // shows as the secondary value.
    assert_eq!(
        eval_ok("(ignore-errors (error \"boom\"))"),
        "NIL\n#<SIMPLE-ERROR>"
    );
    assert_eq!(eval_ok("(ignore-errors (+ 1 2))"), "3");
}

#[test]
fn clos_find_class_and_class_name_roundtrip() {
    assert_eq!(
        eval_ok("(progn (defclass foo () ()) (class-name (find-class 'foo)))"),
        "FOO"
    );
    assert_eq!(
        eval_ok("(handler-case (find-class 'no-such-class-xyz) (error (c) 'no-class))"),
        "NO-CLASS"
    );
}

#[test]
fn clos_slot_exists_p_and_slot_makunbound() {
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ((x))) (list (slot-exists-p (make-instance 'a) 'x) (slot-exists-p (make-instance 'a) 'y)))"
        ),
        "(T NIL)"
    );
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ((x :initform 1))) (let ((o (make-instance 'a))) (slot-makunbound o 'x) (slot-boundp o 'x)))"
        ),
        "NIL"
    );
}

#[test]
fn clos_method_lambda_list_supports_optional_rest_key() {
    assert_eq!(
        eval_ok("(progn (defmethod g ((x integer) &optional y) (list x y)) (list (g 5) (g 5 9)))"),
        "((5 NIL) (5 9))"
    );
    assert_eq!(
        eval_ok("(progn (defmethod g ((x integer) &rest r) (list x r)) (g 5 6 7))"),
        "(5 (6 7))"
    );
    assert_eq!(
        eval_ok(
            "(progn (defmethod g ((x integer) &key (scale 1)) (* x scale)) (list (g 5) (g 5 :scale 3)))"
        ),
        "(5 15)"
    );
}

#[test]
fn clos_defgeneric_method_options_register_methods() {
    assert_eq!(
        eval_ok(
            "(progn (defgeneric g (x) (:method ((x integer)) 'int) (:method ((x string)) 'str)) \
                    (list (g 5) (g \"a\")))"
        ),
        "(INT STR)"
    );
}

#[test]
fn definitions_inside_loops_persist_globally() {
    // Regression: iteration constructs run in a fresh variable frame but must
    // keep the shared global tables mutable in place, so definitions made in
    // their body (intern, use-package, defun) persist after the loop.
    assert_eq!(
        eval_ok(
            "(progn (make-package \"LP\") (dolist (n '(\"A\")) (intern n \"LP\")) (nth-value 1 (find-symbol \"A\" \"LP\")))"
        ),
        ":INTERNAL"
    );
    assert_eq!(
        eval_ok(
            "(progn (make-package \"LP2\") (loop for n in '(\"A\") do (intern n \"LP2\")) (nth-value 1 (find-symbol \"A\" \"LP2\")))"
        ),
        ":INTERNAL"
    );
    // PACKAGE-USE-LIST returns package OBJECTS (CLHS; bliss-bhs first-class
    // packages), so map through PACKAGE-NAME to check the persisted use.
    assert_eq!(
        eval_ok(
            "(progn (defpackage :usrc (:export #:uq)) (make-package \"UDST\") (dolist (p '(\"UDST\")) (use-package :usrc p)) (mapcar #'package-name (package-use-list (find-package \"UDST\"))))"
        ),
        "(\"USRC\")"
    );
}

#[test]
fn uninterned_symbol_names_resolve() {
    // #: uninterned symbols and make-symbol must carry a resolvable name.
    assert_eq!(eval_ok("(string '#:foo)"), "\"FOO\"");
    assert_eq!(
        eval_ok("(symbol-name (make-symbol \"HELLO\"))"),
        "\"HELLO\""
    );
    assert_eq!(eval_ok("(eq '#:a '#:a)"), "NIL");
}

#[test]
fn defpackage_export_and_import_from_are_interned() {
    assert_eq!(
        eval_ok("(progn (defpackage :ep (:export #:foo)) (nth-value 1 (find-symbol \"FOO\" :ep)))"),
        ":EXTERNAL"
    );
    assert_eq!(
        eval_ok(
            "(progn (defpackage :isrc (:export #:sfoo)) (defpackage :idst (:import-from :isrc #:sfoo)) (nth-value 1 (find-symbol \"SFOO\" :idst)))"
        ),
        ":INTERNAL"
    );
    assert_eq!(
        eval_ok(
            "(progn (defpackage :dx (:export #:a #:b)) (let (r) (do-external-symbols (s :dx) (push (string s) r)) (sort r #'string<)))"
        ),
        "(\"A\" \"B\")"
    );
}

#[test]
fn iteration_variables_shadow_outer_bindings() {
    // A loop/dolist/dotimes variable must shadow an outer lexical binding of the
    // same name — symbol lookup consults the symbol-indexed store first, so the
    // iteration binding has to be installed there too.
    assert_eq!(
        eval_ok("(let ((name :outer)) (loop for name in '(1 2 3) collect name))"),
        "(1 2 3)"
    );
    assert_eq!(
        eval_ok("(progn (defun f (name) (dolist (name '(1 2 3) name) name)) (f :outer))"),
        "NIL"
    );
    assert_eq!(
        eval_ok(
            "(progn (defun g (name) (let ((r nil)) (dolist (name '(:a :b) (reverse r)) (push name r)))) (g :outer))"
        ),
        "(:A :B)"
    );
}

#[test]
fn dolist_and_dotimes_establish_fresh_scope() {
    // The iteration variable does not leak past the loop.
    assert_eq!(
        eval_ok("(let ((x :outer)) (dolist (x '(1 2)) nil) x)"),
        ":OUTER"
    );
    assert_eq!(
        eval_ok("(let ((i :outer)) (dotimes (i 3) nil) i)"),
        ":OUTER"
    );
}

#[test]
fn loop_hash_key_iteration_binds_in_do_body() {
    assert_eq!(
        eval_ok(
            "(let ((name :outer) (h (make-hash-table :test 'equal)) (r nil)) \
               (setf (gethash \"A\" h) t) \
               (loop for name being the hash-keys of h do (push name r)) r)"
        ),
        "(\"A\")"
    );
}

#[test]
fn string_of_symbol_returns_bare_symbol_name() {
    // (string sym) is SYMBOL-NAME: no package prefix, for keywords too.
    assert_eq!(eval_ok("(string :uiop/package*)"), "\"UIOP/PACKAGE*\"");
    assert_eq!(eval_ok("(string 'foo)"), "\"FOO\"");
    assert_eq!(eval_ok("(string \"hi\")"), "\"hi\"");
    assert_eq!(eval_ok("(string #\\a)"), "\"a\"");
    assert_eq!(eval_ok("(symbol-name :foo)"), "\"FOO\"");
    // A keyword name coerced by STRING now satisfies (check-type x string).
    assert_eq!(
        eval_ok("(let ((n (string :uiop/package*))) (check-type n string) n)"),
        "\"UIOP/PACKAGE*\""
    );
}

#[test]
fn symbol_package_reports_real_package() {
    // SYMBOL-PACKAGE returns the home PACKAGE object (CLHS), not its name string
    // (bliss-bhs: first-class packages).
    assert_eq!(eval_ok("(symbol-package :foo)"), "#<PACKAGE KEYWORD>");
    assert_eq!(eval_ok("(symbol-package 'car)"), "#<PACKAGE COMMON-LISP>");
    assert_eq!(
        eval_ok("(package-name (symbol-package 'car))"),
        "\"COMMON-LISP\""
    );
}

#[test]
fn find_symbol_resolves_package_nicknames_and_two_values() {
    // CL is a nickname for COMMON-LISP; find-symbol must resolve it and return
    // the external status as a second value.
    assert_eq!(
        eval_ok("(multiple-value-list (find-symbol \"CAR\" :cl))"),
        "(CAR :EXTERNAL)"
    );
    // Nickname as a string designator also resolves (two values collected).
    assert_eq!(
        eval_ok("(multiple-value-list (find-symbol \"CAR\" \"CL\"))"),
        "(CAR :EXTERNAL)"
    );
}

#[test]
fn find_package_resolves_nicknames() {
    assert_eq!(
        eval_ok("(package-name (find-package :cl))"),
        "\"COMMON-LISP\""
    );
    assert_eq!(
        eval_ok("(package-name (find-package :cl-user))"),
        "\"COMMON-LISP-USER\""
    );
}

#[test]
fn packagep_predicate() {
    assert_eq!(eval_ok("(packagep (find-package :cl))"), "T");
    assert_eq!(eval_ok("(packagep 5)"), "NIL");
}

#[test]
fn defpackage_nicknames_are_registered_and_resolvable() {
    assert_eq!(
        eval_ok(
            "(progn (defpackage :foopkg (:nicknames :fp :foolib) (:use :cl)) (package-name (find-package :foolib)))"
        ),
        "\"FOOPKG\""
    );
    assert_eq!(
        eval_ok(
            "(progn (defpackage :barpkg (:nicknames :bp)) (package-nicknames (find-package :barpkg)))"
        ),
        "(\"BP\")"
    );
}

#[test]
fn make_package_with_nicknames() {
    assert_eq!(
        eval_ok(
            "(progn (make-package \"MYPKG\" :nicknames '(\"MP\")) (package-name (find-package :mp)))"
        ),
        "\"MYPKG\""
    );
}

#[test]
fn package_storage_is_registry_backed() {
    // bliss-bhs Stage 4: all package storage lives in the single stdlib
    // PackageRegistry. Exercise the full surface — creation, nickname identity,
    // use-list inheritance, export status, rename, delete — through one program.
    assert_eq!(
        eval_ok(
            "(progn
               (defpackage :s4 (:use :cl) (:nicknames :s4n) (:export :foo))
               (let ((p (find-package :s4)))
                 (list
                   (packagep p)                                    ; T
                   (eq p (find-package :s4n))                      ; T (nickname identity)
                   (package-name p)                                ; \"S4\"
                   (mapcar #'package-name (package-use-list p))    ; (\"COMMON-LISP\")
                   (nth-value 1 (find-symbol \"FOO\" :s4))         ; :EXTERNAL
                   (nth-value 1 (find-symbol \"CAR\" :s4)))))" // :INHERITED from CL
        ),
        "(T T \"S4\" (\"COMMON-LISP\") :EXTERNAL :INHERITED)"
    );
    // RENAME-PACKAGE moves name + nicknames in the registry.
    assert_eq!(
        eval_ok(
            "(progn (make-package \"OLDP\" :nicknames '(\"OP\"))
                    (rename-package :oldp \"NEWP\" '(\"NP\"))
                    (list (find-package :oldp)          ; NIL — old name gone
                          (package-name (find-package :np))))" // \"NEWP\" via new nick
        ),
        "(NIL \"NEWP\")"
    );
    // DO-EXTERNAL-SYMBOLS over KEYWORD enumerates interned keywords — the
    // registry keys them "KEYWORD:NAME", not with a bare ":" prefix
    // (bliss-u5vq, which made this enumeration silently empty).
    assert_eq!(
        eval_ok(
            "(progn (list :u5vq-kw-a :u5vq-kw-b)
                    (let ((a nil))
                      (do-external-symbols (s :keyword) (push s a))
                      (list (and (member :u5vq-kw-a a) t)
                            (and (member :u5vq-kw-b a) t))))"
        ),
        "(T T)"
    );
    // CLHS 11.1.1.2.1 (bliss-jnzb): a package inherits only the EXTERNAL
    // symbols of directly-used packages — internal symbols are not inherited,
    // use is not transitive, and DO-EXTERNAL-SYMBOLS enumerates exports only.
    assert_eq!(
        eval_ok(
            "(progn
               (defpackage :jnzb-a (:use :cl) (:export #:ex))
               (defpackage :jnzb-b (:use :cl :jnzb-a))
               (defpackage :jnzb-c (:use :cl :jnzb-b))
               (eval (read-from-string \"(progn (in-package :jnzb-a) (defun int-fn () 2) (in-package :cl-user))\"))
               (list (nth-value 1 (find-symbol \"INT-FN\" :jnzb-b))   ; NIL — internal not inherited
                     (nth-value 1 (find-symbol \"EX\" :jnzb-b))       ; :INHERITED
                     (nth-value 1 (find-symbol \"EX\" :jnzb-c))       ; NIL — use not transitive
                     (nth-value 1 (find-symbol \"INT-FN\" :jnzb-a))   ; :INTERNAL
                     (let ((acc nil)) (do-external-symbols (s :jnzb-a) (push s acc)) (length acc))))"
        ),
        "(NIL :INHERITED NIL :INTERNAL 1)"
    );
    // RENAME-PACKAGE follows through to already-interned symbols (bliss-9fi3):
    // SYMBOL-PACKAGE reports the new package, the symbol prints with the new
    // qualifier, identity is preserved, and definitions keyed under the old
    // qualifier (a DEFUN of an exported symbol) remain callable.
    assert_eq!(
        eval_ok(
            "(progn
               (defpackage :ren9fi3 (:use :cl) (:export #:thing #:f))
               (defvar *ren-s* 'ren9fi3::thing)
               (eval (read-from-string \"(defun ren9fi3:f () 42)\"))
               (rename-package :ren9fi3 :ren9fi3-new)
               (list *ren-s*
                     (package-name (symbol-package *ren-s*))
                     (eq *ren-s* (find-symbol \"THING\" :ren9fi3-new))
                     (funcall (read-from-string \"ren9fi3-new:f\"))
                     (find-package :ren9fi3)))"
        ),
        "(REN9FI3-NEW:THING \"REN9FI3-NEW\" T 42 NIL)"
    );
    // DELETE-PACKAGE removes it from the registry.
    assert_eq!(
        eval_ok("(progn (make-package \"DELP\") (delete-package :delp) (find-package :delp))"),
        "NIL"
    );
}

#[test]
fn destructuring_key_matches_bare_keyword_for_qualified_vars() {
    // bliss-dyh2: the &key indicator is the variable's BARE name (ANSI:
    // (intern (symbol-name var) :keyword)) even when the variable's symbol
    // resolved package-qualified (an exported symbol read inside its own
    // package). Deriving it from the full name made ASDF's
    // parse-component-form destructure :components to NIL, so every defsystem
    // parsed to zero children and load-system silently loaded nothing.
    assert_eq!(
        eval_ok(
            "(progn
               (defpackage :kwq (:use :cl) (:export #:components))
               (eval (read-from-string \"
                 (progn (in-package :kwq)
                   (cl:defmacro kwq-probe (cl:&rest opts)
                     (cl:destructuring-bind (cl:&key components serial) opts
                       (cl:list 'cl:quote (cl:list components serial))))
                   (in-package :cl-user))\"))
               (eval (read-from-string \"(kwq::kwq-probe :components (1 2) :serial t)\")))"
        ),
        "((1 2) T)"
    );
}

#[test]
fn ansi_special_operators_are_external_in_common_lisp() {
    // bliss-xmxf step (a): the 25 ANSI special operators are seeded PRESENT +
    // EXTERNAL in the COMMON-LISP package table (they are compiler-handled by
    // name and previously had no table entry — (find-symbol "EVAL-WHEN" :cl)
    // was NIL in a bare session). Identity must match the read symbol, and a
    // package using CL inherits them.
    assert_eq!(
        eval_ok(
            "(list (nth-value 1 (find-symbol \"EVAL-WHEN\" :common-lisp))
                   (nth-value 1 (find-symbol \"LET*\" :common-lisp))
                   (nth-value 1 (find-symbol \"UNWIND-PROTECT\" :common-lisp))
                   (nth-value 1 (find-symbol \"THE\" :cl-user))
                   (eq 'the (find-symbol \"THE\" :common-lisp)))"
        ),
        "(:EXTERNAL :EXTERNAL :EXTERNAL :INHERITED T)"
    );
}

#[test]
fn return_from_through_closure_is_lexical() {
    // CLHS 3.1: BLOCK/RETURN-FROM (and TAGBODY/GO) are LEXICAL. A closure that
    // does (return-from tag …) must exit the block it was written inside, even
    // when it is invoked while another block of the same name is dynamically
    // active in an intervening frame — e.g. a (return) funcalled inside another
    // function's LOOP, whose implicit `block nil` is on the stack. bliss
    // resolved return-from by name against the shared dynamic block stack, so
    // the closure returned from the wrong frame (bliss-4u5u root cause; a
    // closure now captures its lexical block/tag exit points). Verified against
    // SBCL: both cases yield :FROM-OUTER.
    assert_eq!(
        eval_ok(
            "(progn
               (defun run-in-loop (fn) (loop :repeat 1 :do (funcall fn)))
               (defun outer-nil () (block nil (run-in-loop (lambda () (return :from-outer))) :normal))
               (defun run (fn) (block tag (funcall fn) :run))
               (defun outer-tag () (block tag (run (lambda () (return-from tag :from-outer))) :normal))
               (list (outer-nil) (outer-tag)))"
        ),
        "(:FROM-OUTER :FROM-OUTER)"
    );
}

#[test]
fn return_from_through_closure_is_lexical_when_compiled() {
    // bliss-4u5u, compiled tiers: the same lexical BLOCK/RETURN-FROM contract must
    // hold once the enclosing function is JIT-compiled. A closure that does
    // `(return …)` must exit the block it was written inside even when funcalled
    // by an intervening function whose own `block nil` is active. Two failure
    // modes this guards: (1) T0/native `env.block_stack` was a shared dynamic
    // accumulation, so the exit resolved to the intervening frame's same-named
    // block; (2) native lowers PushBlock to a no-op, so a compiled `outer` never
    // published its `block nil` for the closure to find. The fix resets the
    // block/tag scope on every call, has closures capture their creation scope,
    // and keeps functions whose closures escape their own blocks at T0. The
    // `dotimes` drives `outer` past the tiering threshold so this exercises the
    // compiled path, not just the tree-walker. Verified against SBCL: (50 70).
    assert_eq!(
        eval_ok(
            "(progn
               (defun helper (fn) (block nil (funcall fn) :helper-fell-through))
               (defun outer (x)
                 (block nil
                   (helper (lambda () (return (* x 10))))
                   :outer-fell-through))
               (dotimes (i 300) (outer i))
               (list (outer 5) (outer 7)))"
        ),
        "(50 70)"
    );
}

#[test]
fn float_literal_overflow_signals_catchable_error() {
    // CLHS 2.3.2.2 (bliss-37sr): a float literal outside the target format's
    // range signals a catchable error (SBCL: FLOATING-POINT-OVERFLOW) rather
    // than reading as infinity; in-range literals are unaffected.
    assert_eq!(
        eval_ok(
            "(list (handler-case (read-from-string \"1e40\") (error () :overflow))
                   (handler-case (read-from-string \"1d400\") (error () :overflow))
                   (read-from-string \"1e38\")
                   (read-from-string \"1d308\"))"
        ),
        "(:OVERFLOW :OVERFLOW 1.0e38 1.0d308)"
    );
}

#[test]
fn read_default_float_format_is_honored() {
    // CLHS 2.3.2.2 (bliss-un1x): marker-less and e/E-marked literals read in
    // *READ-DEFAULT-FLOAT-FORMAT*; f/s markers force single, d/l double; the
    // binding is dynamic (restored after the LET); and a literal that fits the
    // bound format no longer overflows (1e40 as a double).
    assert_eq!(
        eval_ok(
            "(list (read-from-string \"1.5\")
                   (let ((*read-default-float-format* 'double-float))
                     (list (read-from-string \"1.5\")
                           (read-from-string \"1.5e0\")
                           (read-from-string \"1.5f0\")
                           (read-from-string \"1e40\")))
                   (read-from-string \"2.5\"))"
        ),
        "(1.5 (1.5d0 1.5d0 1.5 1.0d40) 2.5)"
    );
}

#[test]
fn macro_lambda_list_supports_nested_destructuring() {
    // ASDF's (defmacro with-upgradability ((&optional) &body body) ...) shape:
    // a nested destructuring pattern with lambda-list keywords must expand.
    assert_eq!(
        eval_ok("(progn (defmacro wu ((&optional) &body body) `(progn ,@body)) (wu () (+ 1 2)))"),
        "3"
    );
    assert_eq!(
        eval_ok(
            "(progn (defmacro foo ((&optional x) &rest body) `(list ,x ,@body)) (foo (5) 6 7))"
        ),
        "(5 6 7)"
    );
    assert_eq!(
        eval_ok(
            "(progn (defmacro bar ((&key (v 10)) &body body) `(list ,v ,@body)) (list (bar () 1) (bar (:v 99) 2)))"
        ),
        "((10 1) (99 2))"
    );
}

#[test]
fn macro_lambda_list_deeply_nested_destructuring() {
    assert_eq!(
        eval_ok(
            "(progn (defmacro d ((a (b &optional c)) &body body) `(list ,a ,b ,c ,@body)) (d (1 (2 3)) 9))"
        ),
        "(1 2 3 9)"
    );
    // Missing optional in the nested pattern defaults to NIL.
    assert_eq!(
        eval_ok(
            "(progn (defmacro d ((a (b &optional c)) &body body) `(list ,a ,b ,c ,@body)) (d (1 (2)) 9))"
        ),
        "(1 2 NIL 9)"
    );
}

#[test]
fn condition_prints_as_its_report_message() {
    // ~A / princ of a condition renders its report, not the raw instance id.
    assert_eq!(
        eval_ok("(format nil \"~a\" (make-condition 'simple-error :format-control \"boom\"))"),
        "\"boom\""
    );
    assert_eq!(
        eval_ok("(handler-case (error \"val is ~a\" 42) (error (c) (format nil \"got: ~a\" c)))"),
        "\"got: val is 42\""
    );
}

#[test]
fn condition_format_control_uses_stdlib_format() {
    // This is the shape of ASDF's invalid-version diagnostic: nested
    // conditionals inside a justification/logical-block control, plus pretty
    // printing directives. ERROR, WARN, CERROR, and a SIMPLE-CONDITION report
    // must all render it through the same stdlib FORMAT implementation.
    let expr = r#"
        (let* ((control "~@<Invalid :version specifier ~S~@[ for component ~S~]~@[ in ~S~]~@[ from file ~A~]~@[, using NIL instead~]~3i~_~@:>")
               (expected "Invalid :version specifier VERSION for component \"completions\" from file /tmp/completions.asd, using NIL instead")
               (error-report
                 (handler-case
                     (error control 'version "completions" nil "/tmp/completions.asd" t)
                   (error (c) (format nil "~A" c))))
               (warning-report
                 (handler-case
                     (warn control 'version "completions" nil "/tmp/completions.asd" t)
                   (warning (c) (format nil "~A" c))))
               (cerror-report
                 (let ((report nil))
                   (handler-bind
                       ((error (lambda (c)
                                 (setq report (format nil "~A" c))
                                 (invoke-restart 'continue))))
                     (cerror "Continue" control 'version "completions" nil
                             "/tmp/completions.asd" t))
                   report))
               (simple-report
                 (format nil "~A"
                         (make-condition
                           'simple-error
                           :format-control control
                           :format-arguments
                           (list 'version "completions" nil
                                 "/tmp/completions.asd" t)))))
          (and (string= error-report expected)
               (string= warning-report expected)
               (string= cerror-report expected)
               (string= simple-report expected)))
    "#;
    assert_eq!(eval_ok(expr), "T");
}

#[test]
fn instance_without_report_prints_as_class_tag() {
    // A STANDARD-OBJECT with no print-object method prints as the unreadable
    // class tag #<POINT>.
    assert_eq!(
        eval_ok(
            "(progn (defclass point () ((x :initarg :x))) (format nil \"~a\" (make-instance 'point :x 1)))"
        ),
        "\"#<POINT>\""
    );
    // A DEFSTRUCT instance with no print-object method prints in the *readable*
    // #S(NAME :slot val …) syntax (CLHS 22.1.3.12), not the class tag — this is
    // the conformant default the fixed-width string layout (bliss-pd0) unblocked.
    assert_eq!(
        eval_ok("(progn (defstruct pt x) (format nil \"~a\" (make-pt :x 1)))"),
        "\"#S(PT :X 1)\""
    );
}

#[test]
fn defstruct_constructor_and_accessors() {
    assert_eq!(
        eval_ok(
            "(progn (defstruct point x y) (let ((p (make-point :x 1 :y 2))) (list (point-x p) (point-y p))))"
        ),
        "(1 2)"
    );
}

#[test]
fn defstruct_slot_defaults_are_evaluated() {
    assert_eq!(
        eval_ok(
            "(progn (defstruct pt (x 0) (y 10)) (let ((p (make-pt :x 5))) (list (pt-x p) (pt-y p))))"
        ),
        "(5 10)"
    );
    assert_eq!(
        eval_ok("(progn (defstruct pt (x (+ 2 3))) (pt-x (make-pt)))"),
        "5"
    );
}

#[test]
fn defstruct_predicate_and_typep() {
    assert_eq!(
        eval_ok("(progn (defstruct pt x) (list (pt-p (make-pt :x 1)) (pt-p 5)))"),
        "(T NIL)"
    );
    assert_eq!(
        eval_ok("(progn (defstruct pt x) (typep (make-pt :x 1) 'pt))"),
        "T"
    );
}

#[test]
fn defstruct_copier_is_independent() {
    assert_eq!(
        eval_ok(
            "(progn (defstruct pt x y) \
                    (let* ((a (make-pt :x 1 :y 2)) (b (copy-pt a))) \
                      (setf (pt-x b) 99) (list (pt-x a) (pt-x b))))"
        ),
        "(1 99)"
    );
}

#[test]
fn defstruct_accessor_is_setfable() {
    assert_eq!(
        eval_ok("(progn (defstruct pt x) (let ((p (make-pt :x 1))) (setf (pt-x p) 42) (pt-x p)))"),
        "42"
    );
}

#[test]
fn clos_subtypep_classes_and_builtins() {
    // subtypep returns two values; wrap in IF to observe just the primary.
    assert_eq!(
        eval_ok("(progn (defclass a () ()) (defclass b (a) ()) (if (subtypep 'b 'a) 'yes 'no))"),
        "YES"
    );
    assert_eq!(
        eval_ok("(progn (defclass a () ()) (defclass b () ()) (if (subtypep 'b 'a) 'yes 'no))"),
        "NO"
    );
    assert_eq!(eval_ok("(if (subtypep 'integer 'number) 'yes 'no)"), "YES");
    assert_eq!(eval_ok("(if (subtypep 'integer 'string) 'yes 'no)"), "NO");
    assert_eq!(eval_ok("(if (subtypep 'string 'sequence) 'yes 'no)"), "YES");
}

#[test]
fn clos_subtypep_returns_certainty_second_value() {
    assert_eq!(
        eval_ok("(multiple-value-bind (s c) (subtypep 'fixnum 'real) (list s c))"),
        "(T T)"
    );
}

#[test]
fn clos_class_precedence_list_orders_supers() {
    // The CPL of B (subclass of A) starts B, A and ends with T.
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ()) (defclass b (a) ()) \
                    (let ((cpl (class-precedence-list (find-class 'b)))) \
                      (list (class-name (car cpl)) (class-name (car (cdr cpl))))))"
        ),
        "(B A)"
    );
}

#[test]
fn clos_initialize_instance_after_runs_during_make_instance() {
    // The canonical construction hook: make-instance must run user
    // (defmethod initialize-instance :after ...) methods.
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ((x :initarg :x :accessor ax))) \
                    (defmethod initialize-instance :after ((o a) &rest args) \
                      (setf (ax o) (* 2 (ax o)))) \
                    (ax (make-instance 'a :x 5)))"
        ),
        "10"
    );
}

#[test]
fn clos_initialize_instance_after_can_derive_from_initargs() {
    assert_eq!(
        eval_ok(
            "(progn (defclass rect () ((w :initarg :w) (h :initarg :h) (area :accessor area))) \
                    (defmethod initialize-instance :after ((r rect) &rest a) \
                      (setf (area r) (* (slot-value r 'w) (slot-value r 'h)))) \
                    (area (make-instance 'rect :w 3 :h 4)))"
        ),
        "12"
    );
}

#[test]
fn clos_shared_initialize_after_runs_during_make_instance() {
    assert_eq!(
        eval_ok(
            "(progn (defvar *r* nil) (defclass a () ()) \
                    (defmethod shared-initialize :after ((o a) slots &rest args) (setq *r* t)) \
                    (make-instance 'a) *r*)"
        ),
        "T"
    );
}

#[test]
fn clos_initialize_instance_after_least_specific_first() {
    assert_eq!(
        eval_ok(
            "(progn (defvar *o* nil) (defclass a () ()) (defclass b (a) ()) \
                    (defmethod initialize-instance :after ((x a) &rest r) (push :a *o*)) \
                    (defmethod initialize-instance :after ((x b) &rest r) (push :b *o*)) \
                    (make-instance 'b) (reverse *o*))"
        ),
        "(:A :B)"
    );
}

#[test]
fn clos_defgeneric_method_options_support_qualifiers() {
    assert_eq!(
        eval_ok(
            "(progn (defvar *l* nil) \
                    (defgeneric g (x) (:method ((x integer)) (push :primary *l*)) \
                                      (:method :before ((x integer)) (push :before *l*))) \
                    (g 5) (reverse *l*))"
        ),
        "(:BEFORE :PRIMARY)"
    );
}

#[test]
fn clos_next_method_p_reflects_chain() {
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ()) (defclass b (a) ()) (defmethod g ((x a)) 'base) \
                    (defmethod g ((x b)) (if (next-method-p) (call-next-method) 'none)) \
                    (g (make-instance 'b)))"
        ),
        "BASE"
    );
    assert_eq!(
        eval_ok(
            "(progn (defclass a () ()) (defmethod g ((x a)) (if (next-method-p) 'has 'none)) \
                    (g (make-instance 'a)))"
        ),
        "NONE"
    );
}

#[test]
fn setf_of_package_qualified_class_accessor() {
    // A SETF of a DEFCLASS :accessor whose symbol is referenced package-qualified
    // at the call site (a different *PACKAGE* than where the class was defined)
    // must resolve the writer. bliss matched the accessor by printed name, so the
    // qualified "PKG:ACC" at the setf site missed the bare "ACC" the slot stored
    // and raised "SETF: unsupported place" — which blocked ASDF's
    // (push … (%additional-input-files c)) and (asdf:load-system :split-sequence).
    // Verified against SBCL: 42.
    assert_eq!(
        eval_ok(
            "(progn \
               (defpackage :sqa (:use :cl) (:export #:acc #:kls)) \
               (defclass sqa:kls () ((s :accessor sqa:acc :initform 0))) \
               (let ((o (make-instance 'sqa:kls))) (setf (sqa:acc o) 42) (sqa:acc o)))"
        ),
        "42"
    );
}

#[test]
fn quasiquote_template_expands_only_unquotes_not_data() {
    // bliss-jmde: macroexpand-all must treat a quasiquote template as DATA and
    // expand only the argument of each unquote — never the template's own forms.
    // A macro whose backquote template contains a form like (defvar ,x 0) with a
    // SYMBOL-MACRO x (from with-slots/symbol-macrolet) previously had the DEFVAR
    // macro expanded inside the template, which quoted its name arg and dropped
    // the unquote, so x was lost -> "variable X unbound". This blocked trivia,
    // lisp-namespace, serapeum. Here H is a symbol-macro for a compound form;
    // the template's DEFVAR must survive as data with ,H substituted. SBCL: the
    // expansion is (DEFVAR 99 0).
    assert_eq!(
        eval_ok(
            "(progn \
               (defmacro dtq (n) (declare (ignore n)) \
                 (let ((v 99)) (symbol-macrolet ((h (+ v 0))) `(defvar ,h 0)))) \
               (nth-value 0 (macroexpand-1 '(dtq foo))))"
        ),
        "(DEFVAR 99 0)"
    );
}

#[test]
fn file_position_query_and_set_on_streams() {
    // R5.124: FILE-POSITION was undefined at the CL level even though the stdlib
    // implemented it (bliss-51f3 follow-up). The builtin now delegates to
    // bliss-stdlib for both the query and the (setf) forms, incl. :START/:END.
    assert_eq!(eval_ok("(fboundp 'file-position)"), "T");
    // Query advances with reads.
    assert_eq!(
        eval_ok("(with-input-from-string (s \"abc\") (read-char s) (read-char s) (file-position s))"),
        "2"
    );
    // Integer set returns T and repositions; :START and :END designators work.
    assert_eq!(
        eval_ok(
            "(let ((s (make-string-input-stream \"0123456789\"))) \
               (list (file-position s 5) (read-char s) (file-position s :start) (read-char s) \
                     (file-position s :end) (file-position s)))"
        ),
        "(T #\\5 T #\\0 T 10)"
    );
    // File streams: seek then read.
    assert_eq!(
        eval_ok(
            "(with-input-from-string (s \"hello world\") (file-position s 6) (read s))"
        ),
        "WORLD"
    );
}

#[test]
fn make_array_element_type_bit_builds_a_bit_vector() {
    // Regression: (make-array n :element-type 'bit) returned a general T-vector
    // of NILs instead of a SIMPLE-BIT-VECTOR initialised to 0 (bliss-51f3
    // follow-up). It must be a real bit-vector, defaulting to 0, honouring
    // :initial-element and :initial-contents, with ARRAY-ELEMENT-TYPE => BIT.
    assert_eq!(eval_ok("(bit-vector-p (make-array 4 :element-type 'bit))"), "T");
    assert_eq!(eval_ok("(make-array 4 :element-type 'bit)"), "#*0000");
    assert_eq!(eval_ok("(aref (make-array 4 :element-type 'bit) 0)"), "0");
    assert_eq!(
        eval_ok("(make-array 3 :element-type 'bit :initial-element 1)"),
        "#*111"
    );
    assert_eq!(
        eval_ok("(make-array 4 :element-type 'bit :initial-contents '(1 0 1 0))"),
        "#*1010"
    );
    assert_eq!(
        eval_ok("(array-element-type (make-array 4 :element-type 'bit))"),
        "BIT"
    );
    assert_eq!(eval_ok("(array-element-type #*1010)"), "BIT");
    // A non-bit initial-element is a type error, not a silent bad vector.
    assert_eq!(
        eval_ok(
            "(handler-case (make-array 2 :element-type 'bit :initial-element 5) \
               (type-error () :caught))"
        ),
        ":CAUGHT"
    );
}

#[test]
fn compile_produces_a_callable_function() {
    // bliss-uhuw: COMPILE was undefined. Minimal conforming COMPILE — bliss
    // functions are already compiled/callable, so it yields a callable function
    // (or recompiles a named one) and returns (values result nil nil).
    assert_eq!(eval_ok("(funcall (compile nil (lambda (x) (* x 2))) 21)"), "42");
    // A quoted lambda EXPRESSION (a list) must be compiled, not returned as data.
    assert_eq!(eval_ok("(funcall (compile nil '(lambda (x) (+ x 100))) 5)"), "105");
    // (compile name) recompiles/returns the named function; call still works.
    assert_eq!(
        eval_ok("(progn (defun cpf-sq (x) (* x x)) (compile 'cpf-sq) (cpf-sq 9))"),
        "81"
    );
    assert_eq!(eval_ok("(fboundp 'compile)"), "T");
    // Standard 3-value consumer: (fn warnings-p failure-p) with no problems.
    assert_eq!(
        eval_ok(
            "(multiple-value-bind (fn w f) (compile nil (lambda () 1)) \
               (list (functionp fn) w f))"
        ),
        "(T NIL NIL)"
    );
}

#[test]
fn bit_vector_boolean_operations() {
    // bliss-4s5y: bit-and/ior/xor/not (+ eqv/nand/nor/andc1/andc2/orc1/orc2)
    // were undefined. They return a fresh simple-bit-vector of the elementwise
    // boolean (bliss bit-vectors are immutable, so the optional result arg always
    // allocates fresh). Values checked against the ANSI truth tables.
    assert_eq!(eval_ok("(bit-and #*1100 #*1010)"), "#*1000");
    assert_eq!(eval_ok("(bit-ior #*1100 #*1010)"), "#*1110");
    assert_eq!(eval_ok("(bit-xor #*1100 #*1010)"), "#*0110");
    assert_eq!(eval_ok("(bit-not #*1100)"), "#*0011");
    assert_eq!(eval_ok("(bit-eqv #*1100 #*1010)"), "#*1001");
    assert_eq!(eval_ok("(bit-nand #*1100 #*1010)"), "#*0111");
    assert_eq!(eval_ok("(bit-nor #*1100 #*1010)"), "#*0001");
    assert_eq!(eval_ok("(bit-andc1 #*1100 #*1010)"), "#*0010");
    assert_eq!(eval_ok("(bit-andc2 #*1100 #*1010)"), "#*0100");
    assert_eq!(eval_ok("(bit-orc1 #*1100 #*1010)"), "#*1011");
    assert_eq!(eval_ok("(bit-orc2 #*1100 #*1010)"), "#*1101");
    assert_eq!(eval_ok("(bit-vector-p (bit-and #*11 #*10))"), "T");
    // Callable as a function value (fboundp / #').
    assert_eq!(
        eval_ok("(reduce #'bit-and (list #*111 #*110 #*011))"),
        "#*010"
    );
}

#[test]
fn builtins_are_first_class_functions() {
    // bliss-dnst: FDEFINITION/SYMBOL-FUNCTION returned the bare symbol for
    // builtins (not FUNCTIONP, diverging from #'), and several standard math
    // functions were absent from the builtin table entirely (#'sinh etc. were
    // not functions). Both are fixed: every standard function is now first-class.
    assert_eq!(eval_ok("(functionp (fdefinition 'car))"), "T");
    assert_eq!(eval_ok("(functionp (symbol-function '+))"), "T");
    assert_eq!(eval_ok("(funcall (fdefinition 'list) 1 2 3)"), "(1 2 3)");
    assert_eq!(eval_ok("(mapcar (fdefinition '1+) '(1 2 3))"), "(2 3 4)");
    // Previously-unregistered functions are now first-class + fbound.
    assert_eq!(eval_ok("(list (fboundp 'sinh) (functionp #'sinh))"), "(T T)");
    assert_eq!(eval_ok("(mapcar #'rational '(0.5 0.25))"), "(1/2 1/4)");
    assert_eq!(eval_ok("(functionp #'array-rank)"), "T");
    assert_eq!(eval_ok("(functionp #'asin)"), "T");
    // A user DEFUN is first-class through FDEFINITION too.
    assert_eq!(
        eval_ok("(progn (defun fcf (x) (* x 3)) (funcall (fdefinition 'fcf) 5))"),
        "15"
    );
}
