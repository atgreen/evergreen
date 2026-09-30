//! A LOOP SUM or COUNT accumulator is readable from the first iteration and
//! starts at its identity, not NIL — including from clauses that appear before
//! the accumulating one (bliss-vhr6e). split-sequence's :count path is exactly
//! that shape, so a NIL accumulator made (>= nr-elts count) a type error and
//! (split-sequence #\. "0.0.0.0" :count 5) failed, which stopped iolib parsing
//! an IP address.
use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{stdout}\n{stderr}"))
        .trim()
        .to_string()
}

#[test]
fn a_sum_accumulator_reads_as_zero_before_it_accumulates() {
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s"
                 (loop for i from 1 to 3 collect (list i n) into rows sum 1 into n
                       finally (return (list rows n))))"#
        ),
        "(((1 0) (2 1) (3 2)) 3)"
    );
}

#[test]
fn a_count_accumulator_reads_as_zero_too() {
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s"
                 (loop for i from 1 to 3 collect n into rows count t into n
                       finally (return (list rows n))))"#
        ),
        "((0 1 2) 3)"
    );
}

#[test]
fn a_guard_may_test_the_accumulator_before_the_summing_clause() {
    // split-sequence's shape, reduced.
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s"
                 (loop for i from 1 to 9
                       if (>= n 3) return (list :stopped n)
                       else collect i into acc and sum 1 into n of-type fixnum
                       finally (return (list :ran-out n))))"#
        ),
        "(:STOPPED 3)"
    );
}

#[test]
fn a_float_typed_sum_keeps_its_float_identity() {
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s"
                 (list (loop for i in '() sum i into n of-type double-float finally (return n))
                       (loop for i in '() sum i)
                       (loop for i from 1 to 3 collect n into rows sum 1.0 into n of-type single-float
                             finally (return rows))))"#
        ),
        "(0.0d0 0 (0.0 1.0 2.0))"
    );
}

#[test]
fn maximize_and_minimize_have_no_identity_and_stay_nil() {
    // Unlike SUM and COUNT, MAXIMIZE and MINIMIZE have no identity to start
    // from, so the variable stays NIL until the first accumulation. CLHS 6.1.3
    // leaves the never-accumulated value undefined and SBCL answers 0 here, so
    // this pins TorCL's answer rather than parity; it is unchanged by the SUM and
    // COUNT fix, and bliss-x47s3 tracks whether to follow SBCL.
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s"
                 (loop for i from 1 to 3 collect (list m n) into rows
                       maximize i into m minimize i into n
                       finally (return rows)))"#
        ),
        "((NIL NIL) (1 1) (2 1))"
    );
}
