// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A self-recursive function whose outer self tail call became a loop must
//! still prove its parameters and self-call results fixnum, so nothing stays a
//! GC root across its self-calls and the direct self-call fast path applies
//! (bliss-legi4).
//!
//! tak is the shape. After self-tail-call elimination its parameters are
//! loop-header phis, not entry parameters, and its third inner call feeds the
//! back-edge directly with no later FrameState carrier. Before the fix the entry
//! pre-guard saw no use of the entry parameters (only the edge into the loop),
//! the third result was never guarded, the phi for `z` stayed TOP, and `z` plus
//! that result were rooted across every self-call: on s390x four shadow slots
//! and 1.4 s for the benchmark, 44 ms with the roots gone.
//!
//! The second test is the correctness half: the result guard placed after the
//! third call borrows a SYNTHESISED post-call frame state (the call's locals,
//! the raw result on the operand stack, resume at the next bytecode). A float
//! fed in after warm-up makes those guards fail, and the deopt must resume the
//! interpreter correctly rather than re-run the call or drop the result. (A
//! single-float, not a bignum: tak's recursion depth follows the VALUE of z, so
//! a bignum z does not terminate in any tier, while 2.5 keeps it as short as
//! fixnum inputs and still fails every fixnum guard.)

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TAK: &str = r#"
  (defun tak (x y z)
    (if (not (< y x))
        z
        (tak (tak (1- x) y z) (tak (1- y) z x) (tak (1- z) x y))))
"#;

fn run(program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]);
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    assert!(
        out.status.success(),
        "egcl failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn tak_reaches_t2_with_no_shadow_roots() {
    let program = format!(
        "{TAK} (dotimes (i 40) (tak 12 8 4)) \
         (format t \"RESULT ~D TIER ~D~%\" (tak 18 12 6) (egcl-ext:function-tier 'tak))"
    );
    let (stdout, log) = run(&program, &[("EGCL_T2_LOG", "stderr")]);
    assert!(stdout.contains("RESULT 7 TIER 2"), "{stdout}\n{log}");
    let installed = log
        .lines()
        .find(|line| line.contains("TAK: T2 INSTALLED"))
        .unwrap_or_else(|| panic!("no T2 install line for TAK:\n{log}"));
    assert!(
        installed.contains("gc_shadow_slots=0"),
        "tak must need no GC shadow slots once its loop-header phis and self-call \
         results are proven fixnum: {installed}"
    );
}

#[test]
fn float_after_warmup_deopts_through_the_synthesised_post_call_state() {
    // z is only compared, returned and decremented, so a float z is legitimate
    // Lisp: the fixnum guards fail on it and the interpreter must finish the
    // computation with the same answer.
    let program = format!(
        "{TAK} (dotimes (i 40) (tak 12 8 4)) \
         (let ((before (egcl-ext:deopt-count))) \
           (format t \"RESULT ~A DEOPTS ~D~%\" \
                   (tak 6 3 2.5) (- (egcl-ext:deopt-count) before)))"
    );
    let (reference, _) = run(&program, &[("EGCL_FORCE_TIER", "t0")]);
    let reference_result = reference
        .lines()
        .find(|line| line.starts_with("RESULT "))
        .map(|line| line.split(' ').nth(1).unwrap().to_owned())
        .expect("T0 result");
    let (stdout, log) = run(&program, &[("EGCL_T2_LOG", "stderr")]);
    let tiered = stdout
        .lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("no result line:\n{stdout}\n{log}"));
    let mut fields = tiered.split(' ');
    assert_eq!(
        fields.nth(1),
        Some(reference_result.as_str()),
        "{stdout}\n{log}"
    );
    let deopts: u64 = fields.nth(1).unwrap().parse().unwrap();
    assert!(
        deopts > 0,
        "a float argument must fail the fixnum guards: {stdout}\n{log}"
    );
}
