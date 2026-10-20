// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Non-capturing `&rest`, `&key` and `&aux` lambda lists enter the mapped
//! native activation on x86-64 Linux: the entry binds them with the same
//! lambda-list binder the interpreter uses, then runs T1 or T2 code over the
//! filled slots (bliss-shih7.27).
use std::process::{Command, Output};

/// Hot variadic functions, called with every keyword shape, followed by the
/// lambda-list errors and the exits a default form can take. Prints the
/// results, then each tier.
const PROGRAM: &str = r#"
(progn
  (defun kw (a &key (b 1) (c (list a b) c-p)) (list a b c c-p))
  (defun rst (&rest r) (cons (length r) r))
  (defun req-rest (x &rest r) (list* x (length r) r))
  (defun aux (a &aux (b (* a 2)) (c (list a b))) (list a b c))
  (defun allow (&key a &allow-other-keys) a)
  (defun opt-key (a &optional (b 2) &key (c 3)) (list a b c))
  (defun overflow-key (a &key (b most-positive-fixnum)) (+ a b))
  (defun count-down (n &key acc)
    (if (zerop n) acc (count-down (1- n) :acc (cons n acc))))
  (defun nlx-key (a &key (b (throw 'nlx (list :thrown a)))) (list a b))
  (defun signal-key (&key (b (error "default ~a" :failed))) b)
  ;; A captured environment still keeps this one interpreted; its default
  ;; must see the enclosing binding either way.
  (let ((outer 7))
    (defun outer-default (&key (b outer)) (list b)))
  (defun all-calls (i)
    (list (kw i) (kw i :b 2) (kw i :c 3 :b 4) (kw i :b i :b 9)
          (kw i :z 1 :allow-other-keys t)
          (rst) (rst i i i) (req-rest i) (req-rest i 1 2)
          (aux i) (allow) (allow :a i :z 3)
          (opt-key i) (opt-key i 5) (opt-key i 5 :c 6)
          (overflow-key 0) (count-down 5)
          (nlx-key i :b 0) (catch 'nlx (nlx-key i))
          (signal-key :b i) (handler-case (signal-key) (error (e) (princ-to-string e)))
          (outer-default) (outer-default :b i)))
  ;; T2 compiles on a background thread: keep calling (bounded) until every
  ;; function is published, then take the answers from the promoted code.
  (loop for i from 0 below 5000
        do (all-calls i)
        until (every (lambda (f) (eql (egcl-ext:function-tier f) 2))
                     '(kw rst req-rest aux allow opt-key overflow-key count-down
                       nlx-key signal-key)))
  (print (all-calls 199))
  (print (list (overflow-key 1) (overflow-key -1 :b 2) (count-down 30)
               (apply #'rst (make-list 40 :initial-element :x))
               (handler-case (progn (kw) nil) (program-error () :few))
               (handler-case (progn (kw 1 :b) nil) (program-error () :odd))
               (handler-case (progn (kw 1 :z 2) nil) (program-error () :unknown))
               (handler-case (progn (funcall 'kw 1 :b 2 :z) nil)
                 (program-error () :odd-funcall))
               (handler-case (progn (aux 1 2) nil) (program-error () :many))))
  (terpri)
  (format t "TIERS ~{~a~^ ~}~%"
          (mapcar #'egcl-ext:function-tier
                  '(kw rst req-rest aux allow opt-key overflow-key count-down
                    nlx-key signal-key))))
"#;

const THRESHOLDS: &[(&str, &str)] = &[
    ("EGCL_T0_T1_THRESHOLD", "2"),
    ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
    ("EGCL_T2_LOG", "stderr"),
];

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

fn egcl(program: &str, envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

/// Splits the program's output into its result lines and its tier line.
fn results_and_tiers(output: &Output) -> (String, String) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (results, tiers) = stdout.split_once("TIERS ").expect("tier line");
    let tiers = tiers.lines().next().unwrap_or_default();
    (results.trim().to_string(), tiers.to_string())
}

#[test]
fn variadic_parameters_reach_t2_with_identical_results() {
    let tiered = egcl(PROGRAM, THRESHOLDS);
    let (results, tiers) = results_and_tiers(&tiered);
    assert!(
        results.contains(":FEW :ODD :UNKNOWN :ODD-FUNCALL :MANY"),
        "{results}"
    );
    if MAPPED_LINUX {
        assert_eq!(
            tiers,
            "2 2 2 2 2 2 2 2 2 2",
            "{}",
            String::from_utf8_lossy(&tiered.stderr)
        );
        let log = String::from_utf8_lossy(&tiered.stderr);
        for name in [
            "KW",
            "REQ-REST",
            "AUX",
            "OPT-KEY",
            "COUNT-DOWN",
            "SIGNAL-KEY",
        ] {
            assert!(
                log.contains(&format!("[T2] {name}: optimized worker result installed")),
                "{name} was not installed at T2:\n{log}"
            );
        }
    }
    for envs in [
        &[("EGCL_BACKEND", "tree-walker")][..],
        &[("EGCL_DISABLE_T2", "1"), ("EGCL_T0_T1_THRESHOLD", "2")][..],
        &[("EGCL_T0_T1_THRESHOLD", "1000000000")][..],
    ] {
        let (other, _) = results_and_tiers(&egcl(PROGRAM, envs));
        assert_eq!(results, other, "results differ under {envs:?}");
    }
}

/// The entry conses `&rest` lists and `&key` defaults before the native body
/// runs. Under a collection on every allocation it must compute the same values.
#[test]
fn variadic_t2_entry_survives_gc_stress() {
    let (plain, _) = results_and_tiers(&egcl(PROGRAM, THRESHOLDS));
    let mut envs = THRESHOLDS.to_vec();
    envs.extend([
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    let stressed = egcl(PROGRAM, &envs);
    let (results, tiers) = results_and_tiers(&stressed);
    assert_eq!(plain, results);
    if MAPPED_LINUX {
        assert_eq!(
            tiers,
            "2 2 2 2 2 2 2 2 2 2",
            "{}",
            String::from_utf8_lossy(&stressed.stderr)
        );
    }
}

/// Prelude functions with `&key`/`&rest` lambda lists reach T2 too.
#[test]
fn prelude_keyword_functions_reach_t2() {
    let program = r#"
(progn
  (defun drive (i)
    (list (string-upcase "abc" :start 1) (string-upcase "xyz")
          (position-if #'evenp (list 1 3 i 5))
          (position-if #'oddp '(2 4 5 6) :from-end t)
          (reduce #'+ (list 1 2 i)) (reduce #'list '(1 2 3) :from-end t :initial-value i)
          (parse-integer "  42  ") (parse-integer "ff" :radix 16)
          (parse-integer "12x" :junk-allowed t)))
  (loop for i from 0 below 5000
        do (drive (* 2 i))
        until (every (lambda (f) (eql (egcl-ext:function-tier f) 2))
                     '(string-upcase position-if reduce parse-integer)))
  (print (drive 10))
  (terpri)
  (format t "TIERS ~{~a~^ ~}~%"
          (mapcar #'egcl-ext:function-tier
                  '(string-upcase position-if reduce parse-integer))))
"#;
    let tiered = egcl(program, THRESHOLDS);
    let (results, tiers) = results_and_tiers(&tiered);
    assert!(
        results.contains(r#"("aBC" "XYZ" 2 2 13 (1 (2 (3 10))) 42 255 12)"#),
        "{results}"
    );
    if MAPPED_LINUX {
        assert_eq!(
            tiers,
            "2 2 2 2",
            "{}",
            String::from_utf8_lossy(&tiered.stderr)
        );
    }
    let (other, _) = results_and_tiers(&egcl(program, &[("EGCL_DISABLE_T2", "1")]));
    assert_eq!(results, other);
}

/// A default form that THROWs to a CATCH in the interpreted caller reaches
/// that catch instead of escaping as an internal control token. A warmed
/// caller pushes the callee's frame itself, which is where the token escaped.
#[test]
fn interpreted_default_form_throw_reaches_its_catch() {
    let program = r#"
(progn
  (defun nlx-key (a &key (b (throw 'nlx (list :thrown a)))) (list a b))
  (defun caller (a) (catch 'nlx (nlx-key a)))
  (dotimes (i 50) (caller i))
  (format t "RESULT ~s~%" (list (caller 1) (caller 2) (nlx-key 3 :b 4))))
"#;
    let output = egcl(program, &[("EGCL_T0_T1_THRESHOLD", "1000000000")]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("RESULT ((:THROWN 1) (:THROWN 2) (3 4))"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
