// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! GETF on an improper property list must signal TYPE-ERROR, not return the
//! default (ansi-test GETF.ERROR.4 and GETF.ERROR.5). The Rust rewrite of GETF
//! lost the check the original DEFUN got from DO's ENDP; the first native s390x
//! run of the ansi "cons" chapter found it (bliss-ljfjg). Every tier is covered
//! because GETF is a builtin the native tiers call through the same body. An
//! odd-length PROPER list is not an error: the set functions parse their
//! keyword arguments by handing GETF `(:key)`, and ansi-test's
//! INTERSECTION.ERROR.4 expects the PROGRAM-ERROR from that parsing, not a
//! TYPE-ERROR from GETF -- the first draft of this fix got exactly that wrong.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(tier: &str, form: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", form])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .expect("run egcl");
    assert!(
        out.status.success(),
        "tier={tier}: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn improper_property_lists_signal_type_error_at_every_tier() {
    let form = "(list (handler-case (getf '(a . b) 'c) (type-error () :type-error)) \
                      (handler-case (getf '(a 10 . b) 'c) (type-error () :type-error)) \
                      (getf '(a 10 b 20) 'b) \
                      (getf '(a 10) 'c :default) \
                      (getf '(:key) 'c :odd) \
                      (getf nil 'c))";
    for tier in ["interp", "t0", "t1", "t2"] {
        assert_eq!(
            eval(tier, form),
            "(:TYPE-ERROR :TYPE-ERROR 20 :DEFAULT :ODD NIL)",
            "tier={tier}"
        );
    }
}
