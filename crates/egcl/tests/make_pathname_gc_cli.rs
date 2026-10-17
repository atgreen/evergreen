// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! MAKE-PATHNAME copied its keyword arguments out of the rooted argument list
//! and then allocated: rendering a :directory list to a namestring, and
//! coercing a :defaults namestring. A component string still in the nursery
//! was moved by that GC, so the new pathname held a stale pointer -- under
//! stress `PATHNAME-NAME` answered the directory's namestring "/a/" instead of
//! "foo". No crash, only a wrong answer, so the test compares the output.
use std::process::Command;

const PROGRAM: &str = r#"
  (let ((names nil) (types nil) (dirs nil))
    (dotimes (i 20)
      ;; Fresh strings, so each component starts out in the nursery.
      (let ((p (make-pathname :name (copy-seq "foo") :type (copy-seq "lisp")
                              :directory '(:absolute "a"))))
        (push (pathname-name p) names)
        (push (pathname-type p) types))
      (let ((p (make-pathname :name (copy-seq "bar")
                              :defaults (concatenate 'string "/b/c/" "x.txt"))))
        (push (list (pathname-name p) (pathname-type p) (pathname-directory p)) dirs)))
    (format t "RESULT ~S ~S ~S~%"
            (remove-duplicates names :test #'equal)
            (remove-duplicates types :test #'equal)
            (remove-duplicates dirs :test #'equal)))
"#;

const EXPECTED: &str = r#"RESULT ("foo") ("lisp") (("bar" "txt" (:ABSOLUTE "b" "c")))"#;

fn run(env: &[(&str, &str)]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", PROGRAM])
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{text}");
    text
}

#[test]
fn make_pathname_components_survive_a_gc_during_construction() {
    let plain = run(&[]);
    assert!(plain.contains(EXPECTED), "{plain}");
    let stressed = run(&[
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ("EGCL_GC_POISON", "1"),
    ]);
    assert!(stressed.contains(EXPECTED), "{stressed}");
}
