//! A redefinition arriving as a SEPARATE top-level form replaces a hot function
//! (bliss-a0ki1).
//!
//! This is the shape a REPL produces, and it is the one bliss-a0ki1 reproduced
//! with: a function LOADed from one file, called by a later top-level form, then
//! redefined by a later one still. Before 0378b575 the old body kept running once
//! the function had been called about four times -- silently, with FBOUNDP true
//! and DEFUN returning the symbol, so nothing reported a problem.
//!
//! Separate from defun_redefinition_retires_bytecode_cli.rs, which drives every
//! case through a single `--eval` form. That is not the same path: within one
//! form the definition and the calls are compiled together, whereas a REPL (or
//! LOAD) hands the reader one form at a time and the redefinition has to
//! invalidate state established by an EARLIER, already-finished form.
//!
//! WHY THIS MATTERS MORE THAN CONFORMANCE: it is the whole blocker for live
//! coding (bliss-1ebb1, slynk to a phone over `adb forward`). A REPL attached to
//! a running program could set a variable and see it take effect, but could not
//! redefine any function the program had actually been running -- which is every
//! function worth redefining. Confirmed on the device at the time: the symbol
//! came back, FBOUNDP was true, and the old body kept drawing.

use std::process::Command;
use std::io::Write;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Run `driver` as a file, so each form is read and evaluated in turn exactly as
/// LOAD and a REPL do.
fn load(dir: &std::path::Path, driver: &str) -> String {
    let path = dir.join("driver.lisp");
    let mut f = std::fs::File::create(&path).expect("write driver");
    f.write_all(driver.as_bytes()).expect("write driver");
    let out = Command::new(BIN)
        .args(["--no-init", "--load", path.to_str().unwrap()])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("egcl-a0ki1-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn a_hot_function_loaded_from_a_file_is_replaced_by_a_later_form() {
    let dir = tempdir("load");
    let lib = dir.join("lib.lisp");
    std::fs::write(&lib, "(defun subject (x) (list :original x))\n").expect("write lib");
    // The call counts bracket the threshold the bug had (between 3 and 5), and
    // then run well past every tier boundary: T1 at 10 invocations by default
    // and T2 beyond it. A redefinition must win at every one of them.
    for calls in [0, 3, 4, 5, 20, 300, 5000] {
        let out = load(
            &dir,
            &format!(
                r#"(load {lib:?})
                   (dotimes (i {calls}) (subject i))
                   (eval '(defun subject (x) (list :replaced x)))
                   (format t "AFTER-~a ~a~%" {calls} (first (subject 1)))
                "#
            ),
        );
        let want = format!("AFTER-{calls} REPLACED");
        assert!(
            out.lines().any(|l| l.trim() == want),
            "{calls} calls then a redefinition must run the NEW body; got:\n{out}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_redefinition_by_loading_a_second_file_also_wins() {
    // The other half of the REPL shape: the replacement arrives by LOAD rather
    // than by EVAL. The bead recorded these as behaving identically, so if they
    // ever diverge the divergence itself is the bug.
    let dir = tempdir("twofile");
    let one = dir.join("one.lisp");
    let two = dir.join("two.lisp");
    std::fs::write(&one, "(defun subject (x) (list :original x))\n").expect("write one");
    std::fs::write(&two, "(defun subject (x) (list :replaced x))\n").expect("write two");
    let out = load(
        &dir,
        &format!(
            r#"(load {one:?})
               (dotimes (i 300) (subject i))
               (load {two:?})
               (format t "RELOADED ~a~%" (first (subject 1)))
            "#
        ),
    );
    assert!(
        out.lines().any(|l| l.trim() == "RELOADED REPLACED"),
        "a redefinition arriving by LOAD must replace a hot function; got:\n{out}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_replacement_is_itself_recompilable() {
    // A redefinition that wins but then never tiers up would trade one live-coding
    // defect for another: the second body must be able to get hot too, not be
    // pinned to the interpreter as a side effect of having replaced something.
    let dir = tempdir("recompile");
    let lib = dir.join("lib.lisp");
    std::fs::write(&lib, "(defun subject (x) (* x 2))\n").expect("write lib");
    let out = load(
        &dir,
        &format!(
            r#"(load {lib:?})
               (dotimes (i 500) (subject i))
               (eval '(defun subject (x) (* x 3)))
               (dotimes (i 500) (subject i))
               (format t "HOT-AGAIN ~a~%" (subject 7))
            "#
        ),
    );
    assert!(
        out.lines().any(|l| l.trim() == "HOT-AGAIN 21"),
        "the replacement must still be running after it too becomes hot; got:\n{out}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
