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
        .find(|l| l.contains("=> stay T1"))
        .unwrap_or_else(|| panic!("no decline reason under the plain name:\n{log}"));

    // And it must say WHICH opcode, not only an opaque tag.
    assert!(
        decline.contains("refused Opcode #"),
        "the decline must name the refused opcode index: {decline}"
    );
    // The package-qualified spelling must not reappear for this function, or the
    // split is back.
    assert!(
        !log.contains("COMMON-LISP-USER::VARSHIFT"),
        "the trace still uses two spellings of one name:\n{log}"
    );
}
