// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tests for the standalone tree-walking interpreter in `egcl_compiler::tiered`.

use egcl_compiler::tiered::Interpreter;
use egcl_rt::value::{NIL, T};

#[test]
fn interpreter_eval_nil_is_self_evaluating() {
    let mut interp = Interpreter::new();
    let result = interp.eval(NIL);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), NIL);
}

#[test]
fn interpreter_eval_t_is_self_evaluating() {
    let mut interp = Interpreter::new();
    assert_eq!(interp.eval(T).unwrap(), T);
}

#[test]
fn interpreter_apply_non_function_errors() {
    let mut interp = Interpreter::new();
    assert!(interp.apply(NIL, NIL).is_err());
}
