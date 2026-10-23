// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Character case uses Unicode simple 1:1 case mappings (CLHS 13.1.4.3),
//! consistently across the character, string, comparison and EQUALP
//! operators (bliss-ne14). Expected values were produced by SBCL 2.x.

use std::process::Command;

const PROBE: &str = r#"
(defun case-fingerprint (lo hi)
  (let ((nu 0) (su 0) (nl 0) (sl 0) (sup 0) (sdown 0))
    (do ((i lo (+ i 1))) ((>= i hi))
      (let ((c (code-char i)))
        (when (upper-case-p c) (setq nu (+ nu 1) su (+ su i)))
        (when (lower-case-p c) (setq nl (+ nl 1) sl (+ sl i)))
        (setq sup (+ sup (char-code (char-upcase c)))
              sdown (+ sdown (char-code (char-downcase c))))))
    (list nu su nl sl sup sdown)))
(defun case-probe ()
  (list (char-code (char-upcase (code-char 233)))
        (char-code (char-downcase (code-char 201)))
        (lower-case-p (code-char 233))
        (upper-case-p (code-char 201))
        (both-case-p (code-char 233))
        (string-upcase "ö")
        (string-downcase "ÄÖÜ")
        (string-capitalize "élan ÉCOLE")
        (nstring-upcase (copy-seq "äbc"))
        (nstring-downcase (copy-seq "ÄBC"))
        (nstring-capitalize (copy-seq "äbc ÄBC"))
        (char-equal (code-char 233) (code-char 201))
        (char-lessp (code-char 233) #\F)
        (char-not-equal (code-char 246) (code-char 214))
        (string-equal "Ö" "ö")
        (string-lessp "é" "F")
        (string-greaterp "é" "E")
        (string-not-equal "äbc" "ÄBD")
        (equalp "Ö" "ö")
        (equalp (code-char 233) (code-char 201))
        (let ((h (make-hash-table :test 'equalp)))
          (setf (gethash "ÖL" h) 1)
          (gethash "öl" h))
        (char-code (char-upcase (code-char 223)))
        (both-case-p (code-char 223))
        (char-code (char-upcase (code-char 181)))
        (char-code (char-upcase (code-char 255)))
        (char-code (char-upcase (code-char 305)))
        (char-code (char-downcase (code-char 8490)))
        (string-upcase "straße")
        (char-equal (code-char 223) #\S)
        (alpha-char-p (code-char 233))
        (format nil "~:@(~a~)" "straße")
        (format nil "~(~a~)" "ÉCOLE")
        (format nil "~@(~a~)" "élan ÉCOLE")
        (format nil "~:(~a~)" "élan école")
        (string-upcase "αβγ абв")
        (string-downcase "ΑΒΓ АБВ")
        (case-fingerprint 0 #x180)
        (case-fingerprint #x370 #x500)))
"#;

/// SBCL's printed value of `(case-probe)`, whitespace-normalised.
const SBCL_EXPECTED: &str = r#"(201 233 T T T "Ö" "äöü" "Élan École" "ÄBC" "äbc" "Äbc Äbc" T NIL NIL T NIL 0 2 T T 1 223 NIL 181 376 305 8490 "STRAßE" NIL T "STRAßE" "école" "Élan école" "Élan École" "ΑΒΓ АБВ" "αβγ абв" (118 28006 118 29738 71804 75268) (180 194557 180 197866 428491 435109))"#;

fn normalise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn run(tier: Option<&str>, program: &str) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_egcl"));
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    let output = cmd
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "tier {tier:?}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn unicode_case_matches_sbcl_interpreted_and_compiled() {
    // Call the probe enough times to clear the tier-up thresholds before the
    // printed call, so the forced T2 run executes compiled code.
    let program = format!(
        "{PROBE}\n(dotimes (i 30) (case-probe))\n(print (case-probe))\n(terpri)"
    );
    for tier in [None, Some("t2")] {
        let out = normalise(&run(tier, &program));
        assert!(
            out.contains(SBCL_EXPECTED),
            "tier {tier:?}:\n got: {out}\nwant: {SBCL_EXPECTED}"
        );
    }
}

/// The invariants ansi-test CHAR-UPCASE.2 / CHAR-DOWNCASE.2 check over the
/// Basic Multilingual Plane (the stdlib unit tests cover every code point): a
/// character the conversion changes is one the predicate
/// classifies, the conversion is idempotent and the pairs round-trip.
#[test]
fn case_predicates_agree_with_conversions_over_all_characters() {
    let program = r#"
      (defun case-consistent-p (c)
        (let ((u (char-upcase c)) (d (char-downcase c)))
          (and (or (lower-case-p c) (eql u c))
               (or (upper-case-p c) (eql d c))
               (eql u (char-upcase u))
               (eql d (char-downcase d))
               (eq (both-case-p c) (or (upper-case-p c) (lower-case-p c)))
               (not (and (upper-case-p c) (lower-case-p c)))
               (if (lower-case-p c) (eql (char-downcase u) c) t)
               (if (upper-case-p c) (eql (char-upcase d) c) t)
               (if (both-case-p c) (alpha-char-p c) t)
               (char-equal u d c))))
      (let ((bad nil))
        (dotimes (i #x10000)
          (let ((c (code-char i)))
            (when (and c (not (case-consistent-p c)))
              (push i bad))))
        (print (list :bad (length bad) (subseq bad 0 (min 5 (length bad))))))
      (terpri)
    "#;
    let out = normalise(&run(None, program));
    assert!(out.contains("(:BAD 0 NIL)"), "{out}");
}
