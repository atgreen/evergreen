//! Bitwise and shift ops must give the same answers at every tier, on every
//! architecture (bliss-ys4ha).
//!
//! WHY THIS FILE IS ARCHITECTURE-AGNOSTIC ON PURPOSE: it computes its expected
//! values from the CL definitions in Rust and runs the same program at all four
//! tiers, so it exercises whichever emitter the test binary was built for. Run it
//! on x86-64 and it checks emit.rs; cross-build and run it under qemu-aarch64 and
//! it checks emit_a64.rs. That matters because the AArch64 emitter had NO arms for
//! any of these opcodes, and adding them introduced a bug this shape catches:
//!
//!   the shift DIRECTION was inverted. `(ash -17 -2)` answered -68 -- a left
//!   shift by two -- where CL says -5. On FixnumShl a NEGATIVE constant means a
//!   RIGHT shift, because ASH lowers to FixnumShl whatever the sign and the
//!   emitter reads the constant to choose. Nothing about that is visible from a
//!   successful build, and x86-64 was correct throughout.
//!
//! Before those arms existed, LOGAND/LOGIOR/LOGXOR/LOGNOT/ASH did not merely run
//! slowly on AArch64 -- the emitter declined the opcode, which costs the WHOLE
//! FUNCTION its T2 code. Verified under qemu-aarch64: with the arms absent,
//! `(logand a 12)`, `(ash a -2)` and `(mod a 8)` reported T1 with the tier forced
//! to T2, while `(+ a 1)` reported T2. So on the phone, any function containing a
//! bitwise op or a shift was permanently barred from the top tier.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

const XS: [i64; 9] = [-17, -8, -7, -1, 0, 1, 7, 13, 100];

/// Warmed, so the functions are compiled and speculated rather than tree-walked.
const PROGRAM: &str = r#"
  (defun f-and (a) (logand a 12))
  (defun f-or  (a) (logior a 5))
  (defun f-xor (a) (logxor a 9))
  (defun f-not (a) (lognot a))
  (defun f-shr (a) (ash a -2))
  (defun f-shl (a) (ash a 3))
  (defun f-mod (a) (mod a 8))
  (dotimes (i 3000) (f-and 7) (f-or 7) (f-xor 7) (f-not 7) (f-shr 7) (f-shl 7) (f-mod 7))
  (dolist (x '(-17 -8 -7 -1 0 1 7 13 100))
    (format t "~a ~a ~a ~a ~a ~a ~a ~a~%"
            x (f-and x) (f-or x) (f-xor x) (f-not x) (f-shr x) (f-shl x) (f-mod x)))
"#;

fn expected() -> Vec<String> {
    XS.iter()
        .map(|&x| {
            // CL MOD is floored; Rust's % truncates.
            let cl_mod = |a: i64, b: i64| {
                let r = a % b;
                if r != 0 && (r < 0) != (b < 0) { r + b } else { r }
            };
            format!(
                "{x} {} {} {} {} {} {} {}",
                x & 12,
                x | 5,
                x ^ 9,
                !x,
                x >> 2, // arithmetic: (ash x -2) floors, as Rust's >> does on i64
                x << 3,
                cl_mod(x, 8)
            )
        })
        .collect()
}

#[test]
fn bitwise_and_shift_agree_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", PROGRAM])
            .env("TORCL_FORCE_TIER", tier)
            .output()
            .expect("run torcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        for line in expected() {
            assert!(
                stdout.lines().any(|l| l.trim() == line),
                "tier={tier}: missing {line:?} in:\n{stdout}\n{stderr}"
            );
        }
    }
}

#[test]
fn car_and_cdr_are_compiled_and_still_handle_a_non_cons() {
    // CAR and CDR need TWO opcodes the AArch64 emitter lacked: Opcode::Car/Cdr
    // themselves, and the separate Guard(TypeTag(CONS)) that build.rs emits ahead
    // of each one. Missing either cost the WHOLE FUNCTION its T2 code, so any use
    // of CAR or CDR -- most Lisp code -- was pinned to T1 off x86-64 (bliss-2yews).
    //
    // The interesting case is NIL, which is not a cons: the guard must deopt to
    // the generic path and answer NIL, not fault on a masked pointer. A non-list
    // must still signal a type error.
    let program = r#"
      (defun f-car  (a) (car a))
      (defun f-cdr  (a) (cdr a))
      (defun f-cadr (a) (car (cdr a)))
      (defun f-mix  (a) (logand (car a) 3))
      (dotimes (i 3000) (f-car '(1 2)) (f-cdr '(1 2)) (f-cadr '(1 2)) (f-mix '(7 2)))
      (dolist (l '((1 2 3) (9) (-4 -5) nil))
        (format t "~s ~s ~s ~s~%" l (f-car l) (f-cdr l) (f-cadr l)))
      (format t "MIX ~s ~s~%" (f-mix '(7 2)) (f-mix '(-1 0)))
      (format t "ATOM ~a~%" (handler-case (f-car 5) (error () :type-error)))
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("TORCL_FORCE_TIER", tier)
            .output()
            .expect("run torcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        for line in [
            "(1 2 3) 1 (2 3) 2",
            "(9) 9 NIL NIL",
            "(-4 -5) -4 (-5) -5",
            // NIL is not a cons; the guard deopts and the generic path answers.
            "NIL NIL NIL NIL",
            "MIX 3 3",
            "ATOM TYPE-ERROR",
        ] {
            assert!(
                stdout.lines().any(|l| l.trim() == line),
                "tier={tier}: missing {line:?} in:\n{stdout}\n{stderr}"
            );
        }
    }
}

#[test]
fn a_left_shift_that_leaves_fixnum_range_still_answers() {
    // The overflow path. A left shift can exceed fixnum range, and unlike add/sub
    // there is no flag for it on AArch64 — the emitter shifts back and compares,
    // deopting when the value does not survive the round trip. The answer must be
    // the bignum, not a truncated fixnum.
    let program = r#"
      (defun shl-big (a n) (ash a n))
      (dotimes (i 3000) (shl-big 3 4))
      (format t "~a~%" (shl-big 1 62))
      (format t "~a~%" (shl-big most-positive-fixnum 3))
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("TORCL_FORCE_TIER", tier)
            .output()
            .expect("run torcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "tier={tier}: {stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.lines().any(|l| l.trim() == "4611686018427387904"),
            "tier={tier}: (ash 1 62) wrong in:\n{stdout}"
        );
    }
}
