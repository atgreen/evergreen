//! MOD by a constant power of two is a mask, not a call (bliss-enp58).
//!
//! `(mod i 8)` cost 320 ns/iter against a 151.5 ns empty-loop floor, so the
//! operation itself was ~168 ns. That was never the divide instruction --
//! `(logand i 7)` measured 151.5 ns in the same harness, and a divide is a few
//! nanoseconds. It was the GENERIC CALL: `arith_of` in the T2 speculator
//! recognises only `+`, `-`, `*`, so MOD stayed an unspeculated call with a
//! safepoint and an unproven result type.
//!
//! The bead (from Godbolt's AoCO2025 days 6 and 7) asked for multiply-high plus
//! shift. That is the wrong fix here and the measurement says so: it would
//! replace an instruction that is not the cost while leaving the call in place.
//! `FixnumDiv`/`FixnumRem`/`FixnumMod` also turn out to be DEAD IR -- present in
//! ir.rs, lower.rs, infer.rs, opt_licm and opt_fold, emitted by no emitter, and
//! produced by nothing.
//!
//! So: a tagged fixnum is `n << 3` with a zero tag, the shift factors out of a
//! bitwise AND, and floored MOD by a POSITIVE power of two is exactly the low
//! bits in two's complement -- sign included. `(mod x 8)` becomes
//! `LogAnd(x, 7)`, which all four emitters already implement.
//!
//! WHAT THIS FILE IS REALLY FOR is the sign matrix. The failure mode of this
//! transform is not a crash, it is a wrong number:
//!
//!   * MOD is FLOORED and REM is TRUNCATED -- `(mod -7 8)` is 1 but
//!     `(rem -7 8)` is -7, so routing REM through the mask would be silently
//!     wrong for every negative dividend. REM must stay generic, and is checked
//!     here to prove it still answers correctly.
//!   * a NEGATIVE divisor breaks the identity: `(mod 7 -8)` is -1, which the
//!     mask does not produce. Refused, and checked.
//!   * a non-power-of-two divisor is refused, and checked.
//!
//! Every case runs at each tier, because the speculator only exists at T2 and a
//! transform that changed an answer between tiers would be the worst outcome:
//! correct in testing, wrong once a function gets hot.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Warmed so the functions are actually compiled and speculated; a cold DEFUN is
/// run by the tree-walker and would prove nothing about the rewrite.
const PROGRAM: &str = r#"
  (defun m2 (a) (mod a 2))
  (defun m8 (a) (mod a 8))
  (defun m1 (a) (mod a 1))
  (defun m10 (a) (mod a 10))
  (defun mneg (a) (mod a -8))
  (defun r8 (a) (rem a 8))
  (dotimes (i 3000) (m2 5) (m8 5) (m1 5) (m10 5) (mneg 5) (r8 5))
  (dolist (x '(-17 -9 -8 -7 -1 0 1 7 8 9 17))
    (format t "~a ~a ~a ~a ~a ~a ~a~%"
            x (m2 x) (m8 x) (m1 x) (m10 x) (mneg x) (r8 x)))
"#;

/// The reference answers, computed the way CL defines them rather than copied
/// from the implementation under test.
fn expected() -> Vec<String> {
    [-17i64, -9, -8, -7, -1, 0, 1, 7, 8, 9, 17]
        .iter()
        .map(|&x| {
            let cl_mod = |a: i64, b: i64| {
                let r = a % b;
                if r != 0 && (r < 0) != (b < 0) { r + b } else { r }
            };
            format!(
                "{x} {} {} {} {} {} {}",
                cl_mod(x, 2),
                cl_mod(x, 8),
                cl_mod(x, 1),
                cl_mod(x, 10),
                cl_mod(x, -8),
                x % 8 // REM truncates
            )
        })
        .collect()
}

#[test]
fn mod_is_correct_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run egcl");
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
fn the_mask_rewrite_is_what_makes_it_free() {
    // A guard against the test above passing for the wrong reason. `(mod i 8)`
    // must now cost what `(logand i 7)` costs, and clearly less than a divisor
    // the rewrite REFUSES. Measured 152 vs 317 ns/iter; the threshold is loose
    // because this runs on shared CI hardware -- it is checking that the call
    // overhead is gone, not a specific nanosecond count.
    let program = r#"
      (defun k-and (n) (let ((a 0)) (dotimes (i n) (setq a (logand i 7))) a))
      (defun k-m8  (n) (let ((a 0)) (dotimes (i n) (setq a (mod i 8))) a))
      (defun k-m10 (n) (let ((a 0)) (dotimes (i n) (setq a (mod i 10))) a))
      (dolist (f '(k-and k-m8 k-m10))
        (dotimes (w 200) (funcall f 50)) (funcall f 200000)
        (let* ((n 2000000) (s (get-internal-real-time)))
          (funcall f n)
          (format t "~a ~a~%" f (- (get-internal-real-time) s))))
    "#;
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ms = |name: &str| -> f64 {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or_else(|| panic!("no timing for {name} in:\n{stdout}"))
    };
    let (and, m8, m10) = (ms("K-AND"), ms("K-M8"), ms("K-M10"));
    assert!(
        m8 < (and + m10) / 2.0,
        "(mod i 8) should cost about what (logand i 7) does, not what an \
         unrewritten (mod i 10) does: and={and}ms m8={m8}ms m10={m10}ms"
    );
}
