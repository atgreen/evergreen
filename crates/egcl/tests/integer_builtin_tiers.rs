// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native integer builtins must preserve signed CL semantics, secondary
//! remainders, and fallback behavior for non-fixnums and quotient overflow.
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");
const PROGRAM: &str = r#"
(progn
  (defun nmin (x y) (min x y))
  (defun nmax (x y) (max x y))
  (defun nmod (x y) (mod x y))
  (defun nrem (x y) (rem x y))
  (defun nfloor (x y) (let ((q 0)) (setq q (floor x y)) q))
  (defun ntruncate (x y) (let ((q 0)) (setq q (truncate x y)) q))
  (defun floor-tuple (x y) (floor x y))
  (defun truncate-tuple (x y) (truncate x y))
  ;; Warm the actual functions before asserting their compiled behavior.
  (dotimes (i 1000)
    (nmin i 24) (nmax i 24) (nmod i 24) (nrem i 24)
    (nfloor i 24) (ntruncate i 24) (floor-tuple i 24) (truncate-tuple i 24))
  (format t "TIERS ~s~%" (mapcar #'egcl-ext:function-tier
    '(nmin nmax nmod nrem nfloor ntruncate)))
  (dolist (x '(-100 -25 -24 -7 -1 0 1 7 24 25 100))
    (dolist (y '(-24 -3 -1 1 3 24))
      (format t "ROW ~d ~d ~d ~d ~d ~d ~d ~d ~s ~s~%"
        x y (nmin x y) (nmax x y) (nmod x y) (nrem x y)
        (nfloor x y) (ntruncate x y)
        (multiple-value-list (floor-tuple x y))
        (multiple-value-list (truncate-tuple x y)))))
  (format t "FALLBACK ~s~%"
    (list (nmin 3/2 2) (nmax 3/2 2) (nmod 5/2 2) (nrem 5/2 2)
      (nfloor most-negative-fixnum -1) (ntruncate most-negative-fixnum -1)
      (handler-case (nmod 3 0) (division-by-zero () :zero))
      (handler-case (nrem 3 0) (division-by-zero () :zero))
      (handler-case (nfloor 3 0) (division-by-zero () :zero))
      (handler-case (ntruncate 3 0) (division-by-zero () :zero))))
  (egcl::%exit 0))
"#;

#[test]
fn integer_builtins_match_all_tiers() {
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(BIN)
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_IR_FULL", "1")
            .output()
            .expect("run integer builtin regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        if tier == "t2" && cfg!(target_arch = "x86_64") {
            assert!(
                stdout.contains("TIERS (2 2 2 2 2 2)"),
                "native path missing: {stdout}\n{stderr}"
            );
        }
        if tier == "t2" && cfg!(target_arch = "x86_64") {
            for (name, opcode) in [
                ("NMIN", "FixnumMin"),
                ("NMAX", "FixnumMax"),
                ("NMOD", "FixnumMod"),
                ("NREM", "FixnumRem"),
                ("NFLOOR", "FixnumFloor"),
                ("NTRUNCATE", "FixnumDiv"),
            ] {
                assert!(
                    stderr.split("[ir] ===== function ").any(|section| {
                        section
                            .lines()
                            .next()
                            .is_some_and(|header| header.to_ascii_uppercase().contains(name))
                            && section.lines().any(|line| line.contains(opcode))
                    }),
                    "{name} did not emit {opcode}: {stderr}"
                );
            }
        }
        for x in [-100_i64, -25, -24, -7, -1, 0, 1, 7, 24, 25, 100] {
            for y in [-24_i64, -3, -1, 1, 3, 24] {
                let quotient = x / y;
                let remainder = x % y;
                let adjust = remainder != 0 && (remainder < 0) != (y < 0);
                let floor = quotient - i64::from(adjust);
                let modulo = remainder + if adjust { y } else { 0 };
                let expected = format!(
                    "ROW {x} {y} {} {} {modulo} {remainder} {floor} {quotient} ({floor} {modulo}) ({quotient} {remainder})",
                    x.min(y),
                    x.max(y)
                );
                assert!(
                    stdout.lines().any(|line| line == expected),
                    "{tier}: missing {expected}\n{stdout}\n{stderr}"
                );
            }
        }
        assert!(stdout.contains("FALLBACK (3/2 2 1/2 1/2 1152921504606846976 1152921504606846976 :ZERO :ZERO :ZERO :ZERO)"), "{tier}: {stdout}\n{stderr}");
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn warmed_integer_loop_bodies_emit_native_operations() {
    let program = r#"
      (defmacro def-loop (name form)
        `(defun ,name (n) (let ((sum 0)) (dotimes (i n sum) (incf sum ,form)))))
      (def-loop loop-min (min i 7))
      (def-loop loop-max (max 0 (min 7 i)))
      (def-loop loop-mod (mod i 24))
      (def-loop loop-rem (rem i 24))
      (def-loop loop-floor (floor i 3))
      (def-loop loop-truncate (truncate i 3))
      (dolist (fn '(loop-min loop-max loop-mod loop-rem loop-floor loop-truncate))
        (dotimes (i 100) (funcall fn 1000))
        (format t "RESULT ~a ~a ~a~%" fn (funcall fn 200000) (egcl-ext:function-tier fn)))
      (egcl::%exit 0)
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_LAZY_COMPILE")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_IR_FULL", "1")
        .output()
        .expect("run normally warmed integer loops");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for (name, opcode, answer) in [
        ("LOOP-MIN", "FixnumMin", 1_399_972_i64),
        ("LOOP-MAX", "FixnumMax", 1_399_972),
        ("LOOP-MOD", "FixnumMod", 2_299_936),
        ("LOOP-REM", "FixnumRem", 2_299_936),
        ("LOOP-FLOOR", "FixnumFloor", 6_666_566_667),
        ("LOOP-TRUNCATE", "FixnumDiv", 6_666_566_667),
    ] {
        assert!(
            stdout.contains(&format!("RESULT {name} {answer} 2")),
            "{stdout}\n{stderr}"
        );
        assert!(
            stderr.split("[ir] ===== function ").any(|section| {
                section
                    .lines()
                    .next()
                    .is_some_and(|header| header.to_ascii_uppercase().contains(name))
                    && section.lines().any(|line| line.contains(opcode))
            }),
            "{name} did not emit {opcode}: {stderr}"
        );
    }
}
