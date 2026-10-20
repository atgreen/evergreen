// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The T2 trace explains a decline under ONE function name (bliss-ieajy.1).
//!
//! The information was always there; it was unusable. A single function's trace
//! was split across two spellings of its name -- sym_label() prints a
//! CL-USER-homed name bare, while compile_t2_artifact used the raw package-
//! qualified bf.name -- so
//!
//!     [T2] VARSHIFT: queued for background T2 compilation
//!     [T2] COMMON-LISP-USER::VARSHIFT: emit_framed failed: UnsupportedOp(4110)
//!
//! meant that grepping the log for a function dropped exactly the half that says
//! WHY T2 was refused. That cost three wrong guesses in one session: chasing
//! AArch64's missing CAR/CDR (bliss-2yews) and the float loop that never promotes
//! (bliss-k51a), both times concluding "the diagnostics say nothing" when they
//! did, under a name I was not grepping for.
//!
//! The refusal code was also opaque. `emit::op_tag` encodes an opcode as
//! `op as u32 | 0x1000`, so the log read `UnsupportedOp(4110)` -- a number that
//! names nothing. 4110 is 0x100E, i.e. Opcode #14, which is FixnumShl.
//!
//! TWO OTHER THINGS A READER NEEDS TO KNOW, and this test encodes both:
//!   * T2 compiles in the BACKGROUND, so a program that exits promptly logs only
//!     "queued" and the decline never appears. The test sleeps.
//!   * the probe is `(ash a b)` with a VARIABLE shift amount, which emit.rs
//!     refuses on purpose (it folds only a constant count). If someone teaches
//!     the emitter variable shifts, this test should start failing and be
//!     repointed at another refused shape rather than deleted.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

#[test]
fn a_declined_function_explains_itself_under_one_name() {
    let out = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            "(progn (defun varshift (a b) (ash a b)) \
                    (dotimes (i 5000) (varshift 1024 -2)) \
                    (sleep 1) (values))",
        ])
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_FORCE_TIER", "t2")
        .output()
        .expect("run egcl");
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{log}");

    // Every line about this function must use the same spelling, so one grep
    // finds the whole story.
    let ours: Vec<&str> = log
        .lines()
        .filter(|l| l.starts_with("[T2] VARSHIFT:"))
        .collect();
    assert!(
        ours.iter().any(|l| l.contains("queued for background T2")),
        "no queue line under the plain name:\n{log}"
    );
    assert!(
        ours.iter().any(|l| l.contains("considering for T2")),
        "no consideration line under the plain name:\n{log}"
    );
    let decline = ours
        .iter()
        .find(|l| l.contains("refused Opcode #"))
        .unwrap_or_else(|| panic!("no decline reason under the plain name:\n{log}"));

    assert!(
        ours.iter().any(|line| line.contains("=> stay T1")
            || (line.contains("mapped T2 emission declined (")
                && line.contains("); keep mapped T1 or interpreter"))),
        "the decline must explain the retained execution path:\n{log}"
    );
    // And it must say WHICH opcode, not only an opaque tag.
    assert!(
        decline.contains("refused Opcode #14 in t2/ir.rs"),
        "the decline must name the refused opcode index: {decline}"
    );
    // The package-qualified spelling must not reappear for this function, or the
    // split is back.
    assert!(
        !log.contains("COMMON-LISP-USER::VARSHIFT"),
        "the trace still uses two spellings of one name:\n{log}"
    );
}

/// A function T1 declines never reaches the T2 queue on Linux, so it used to be
/// absent from EGCL_T2_LOG and the compile trace said only "mapped native entry
/// unavailable" for every shape (bliss-s5r0c). Both now name the reason under
/// the function's plain name. If either shape learns to compile natively, point
/// the case at another still-declined shape rather than deleting it.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_function_declined_at_t1_explains_itself_in_the_t2_log() {
    let out = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            "(progn (defun keyfn (a &key (b 1)) (+ a b)) \
                    (defun restfn (&rest r) (let ((g (lambda () r))) (funcall g))) \
                    (defun capfn (n) (let ((f (lambda (x) (+ x n)))) (funcall f 1))) \
                    (dotimes (i 50) (keyfn i) (restfn i) (capfn i)) \
                    (values))",
        ])
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_T0_T1_THRESHOLD", "5")
        .output()
        .expect("run egcl");
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{log}");
    // A plain &key lambda list enters mapped T1 now (bliss-shih7.27), so it
    // must not be reported as declined; a variadic body that captures still is.
    assert!(
        !log.contains("[T2] KEYFN: T1 declined"),
        "a plain &key function must no longer decline T1:\n{log}"
    );
    assert!(
        log.contains("[T2] RESTFN: T1 declined (captured variables (closure environment))"),
        "the capturing &rest decline must be explained in the T2 log:\n{log}"
    );
    assert!(
        log.contains("[T2] CAPFN: T1 declined (captured variables (closure environment))"),
        "the closure decline must be explained in the T2 log:\n{log}"
    );
}

/// Run `program` with `envs`, returning (first stdout line, stderr). Fails on a nonzero exit.
fn run(program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new(BIN);
    cmd.args(["--no-init", "--eval", program]);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run egcl");
    // Only the program's own line: --eval then prints the form's value.
    let stdout = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

/// A function the bytecode compiler cannot lower never reaches the tier state
/// machine at all, so it stayed at T0 with nothing in EGCL_T2_LOG (bliss-75b8v).
/// A special variable used as a lambda-list parameter is one such construct;
/// the line names it. If the lowerer learns to bind special parameters, point
/// these at another construct it still refuses rather than deleting them.
#[test]
fn a_function_not_compiled_to_bytecode_explains_itself_in_the_t2_log() {
    let program = "(progn (defvar *kd* 1) \
                    (defun keyfn (a &key ((:b *kd*) 1)) (+ a *kd*)) \
                    (defun restfn (a &rest *kd*) (cons a *kd*)) \
                    (defun reqfn (*kd*) (+ 1 *kd*)) \
                    (defun dbkey (form) (destructuring-bind (s &key (at 0)) form (list s at))) \
                    (dotimes (i 50) (keyfn i) (restfn i) (reqfn i) (dbkey (list i))) \
                    (format t \"~a ~a ~a ~a ~a~%\" (keyfn 2 :b 5) (restfn 1 2 3) (reqfn 4) \
                      (dbkey (list 1 :at 2)) \
                      (mapcar (function egcl-ext:function-tier) \
                              (list (function keyfn) (function restfn) (function reqfn) \
                                    (function dbkey)))))";
    let (out, log) = run(
        program,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T0_T1_THRESHOLD", "5")],
    );
    for name in ["KEYFN", "RESTFN", "REQFN"] {
        let line = format!(
            "[T2] {name}: not compiled to bytecode (lambda-list:special-parameter-name); T1 and T2 need bytecode"
        );
        assert_eq!(
            log.matches(&line).count(),
            1,
            "{name} must explain itself once:\n{log}"
        );
    }
    // DESTRUCTURING-BIND's &key used to bail without recording a reason, which
    // is why ASDF's NORMALIZE-VERSION and PARSE-COMPONENT-FORM logged nothing.
    assert_eq!(
        log.matches("[T2] DBKEY: not compiled to bytecode (destructuring-bind:&KEY)")
            .count(),
        1,
        "the destructuring-bind decline must name its lambda-list keyword:\n{log}"
    );
    assert!(
        log.contains(
            "[compile] [T0] KEYFN: not compiled to bytecode (lambda-list:special-parameter-name)"
        ),
        "the compile trace must carry the same reason:\n{log}"
    );
    assert_eq!(
        out.trim(),
        "7 (1 2 3) 5 (1 2) (0 0 0 0)",
        "results and tiers:\n{log}"
    );
    let (tree, _) = run(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(
        tree, out,
        "the tiered run must compute what the interpreter does"
    );

    // Nothing is logged, and nothing changes, without EGCL_T2_LOG.
    let (quiet, quiet_log) = run(program, &[("EGCL_T0_T1_THRESHOLD", "5")]);
    assert_eq!(quiet, out);
    assert!(
        !quiet_log.contains("[T2]") && !quiet_log.contains("not compiled"),
        "{quiet_log}"
    );
}

/// The captured block/tag control decline (bliss-4u5u) printed only in the
/// compile trace, so EGCL_T2_LOG never said why such a function stayed at T0
/// (bliss-75b8v).
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_captured_control_decline_explains_itself_in_the_t2_log() {
    let program = "(progn (defun ctl (l) \
                      (block out (mapc (lambda (x) (when (eql x 3) (return-from out x))) l) nil)) \
                    (dotimes (i 50) (ctl (list 1 2 3))) \
                    (format t \"~a ~a ~a~%\" (ctl (list 1 2 3)) (ctl (list 4)) \
                      (egcl-ext:function-tier (function ctl))))";
    let (out, log) = run(
        program,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T0_T1_THRESHOLD", "5")],
    );
    assert!(
        log.contains(
            "[T2] CTL: T1 declined (a closure it creates needs captured block/tag control); T2 needs an installed T1"
        ),
        "the captured-control decline must reach the T2 log:\n{log}"
    );
    assert_eq!(out.trim(), "3 NIL 0", "{log}");
    let (tree, _) = run(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(tree.trim(), "3 NIL 0");
}

/// A function blacklisted after repeated guard deopts used to say so only as a
/// suffix on its last deopt line, without naming the guard. One summary line
/// now names the function, where T0 resumed, and the speculation it disproved.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_deopt_blacklist_names_the_function_and_its_guard() {
    let program = "(progn (defun g (a b) (+ (* a a) (* b b))) (g 1 1) (g 2 2) \
                    (dotimes (i 6) (g 1/2 1/3)) \
                    (format t \"~a ~a ~a~%\" (egcl-ext:function-tier (quote g)) (g 1/2 1/3) (g 3.0 4.0)))";
    let (out, log) = run(
        program,
        &[
            ("EGCL_T2_LOG", "stderr"),
            ("EGCL_T1_THRESHOLD", "2"),
            ("EGCL_LAZY_COMPILE", "0"),
            ("EGCL_DISABLE_T2", "1"),
            ("EGCL_DEOPT_BLACKLIST_THRESHOLD", "3"),
        ],
    );
    let summary: Vec<&str> = log
        .lines()
        .filter(|line| line.starts_with("[T2] G: blacklisted at T0"))
        .collect();
    assert_eq!(summary.len(), 1, "exactly one blacklist summary:\n{log}");
    assert!(
        summary[0].contains("after 3 native guard deopts")
            // This deopt re-executes G from its entry, so the resume point is
            // not the guard and must not read as one; the speculation names it.
            && summary[0].contains("T0 resumed at function entry (bytecode 0, re-executed)")
            && summary[0].contains("Fixnum at bytecode 4, CallNamed *")
            && summary[0].ends_with("T1 and T2 are not retried"),
        "the summary must name the guard: {}",
        summary[0]
    );
    // Deopt lines use the same plain spelling, so one grep finds both.
    assert!(
        log.contains("[T2] G: native guard deopt #3 => resuming interpreted (threshold hit"),
        "{log}"
    );
    assert_eq!(out.trim(), "0 13/36 25.0", "{log}");
    let (tree, _) = run(
        program
            .replace("(egcl-ext:function-tier (quote g))", "0")
            .as_str(),
        &[("EGCL_BACKEND", "tree-walker")],
    );
    assert_eq!(tree.trim(), out.trim());
}

/// A DEFMETHOD body the bytecode compiler cannot lower stays tree-walked, and
/// that used to happen without a word: the reified method has no symbol for the
/// tier ladder to report on (bliss-75b8v). The same body as a DEFUN already
/// explained itself. A method defined inside a lexical binding is never
/// compiled at all and now says so too.
#[test]
fn a_method_not_compiled_to_bytecode_explains_itself_in_the_t2_log() {
    let program = "(progn (defgeneric gm (x)) (defgeneric gm2 (x)) (defgeneric gm3 (x)) \
                    (defmethod gm ((x integer)) \
                      (destructuring-bind (a &key (b 0)) (list x :b 2) (+ a b))) \
                    (defmethod gm2 ((x integer)) (+ x (load-time-value 10))) \
                    (let ((k 3)) (defmethod gm3 ((x integer)) (+ x k))) \
                    (dotimes (i 50) (gm i) (gm2 i) (gm3 i)) \
                    (format t \"~a ~a ~a~%\" (gm 1) (gm2 1) (gm3 1)))";
    let (out, log) = run(
        program,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T0_T1_THRESHOLD", "5")],
    );
    for (name, reason) in [
        ("GM", "destructuring-bind:&KEY"),
        ("GM2", "LOAD-TIME-VALUE needs eager evaluation"),
        ("GM3", "method defined inside a lexical binding"),
    ] {
        let line = format!(
            "[T2] (METHOD COMMON-LISP-USER::{name}): not compiled to bytecode ({reason}); T1 and T2 need bytecode"
        );
        assert_eq!(
            log.matches(&line).count(),
            1,
            "{name} must explain itself once:\n{log}"
        );
    }
    assert_eq!(out.trim(), "3 11 4", "{log}");
    let (tree, _) = run(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert_eq!(tree, out);
}

/// SETF of a place the lowerer does not handle names the place by its
/// operator. Its arguments are gensyms after SETF expansion, so printing them
/// gave a reason that changed on every run (bliss-75b8v).
#[test]
fn an_unsupported_setf_place_is_named_without_gensyms() {
    let program = "(progn (defun setget (s) (setf (get s 'zz) 3) (get s 'zz)) \
                    (dotimes (i 50) (setget 'p)) \
                    (format t \"~a~%\" (setget 'q)))";
    let (out, log) = run(
        program,
        &[("EGCL_T2_LOG", "stderr"), ("EGCL_T0_T1_THRESHOLD", "5")],
    );
    assert_eq!(
        log.matches("[T2] SETGET: not compiled to bytecode (setf:unsupported-place:(GET ...)); T1 and T2 need bytecode")
            .count(),
        1,
        "{log}"
    );
    assert!(!log.contains("#:"), "no gensym in the reason:\n{log}");
    assert_eq!(out.trim(), "3");
}
