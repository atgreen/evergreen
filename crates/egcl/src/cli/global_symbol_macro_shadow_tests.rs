// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
fn lexical_parameters_shadow_global_symbol_macros_for_reads_writes_and_later_defaults() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let symbol = resolve_sym("GSM-SHADOW").unwrap();
    let global = resolve_sym(":GLOBAL-SYMBOL-MACRO").unwrap();
    let right = resolve_sym(":RIGHT").unwrap();
    let written = resolve_sym(":WRITTEN").unwrap();
    let default = resolve_sym(":DEFAULT").unwrap();
    let key_default = resolve_sym(":KEY-DEFAULT").unwrap();
    env.define_global_symbol_macro(symbol, global);
    assert_eq!(read_eval_all_env("gsm-shadow", &mut env).unwrap(), global);
    // Invoke the common source binder directly: this is independent of either
    // legacy cons closures or the new heap source representation/tiering.
    for (params, body, args, expected) in [
        ("(gsm-shadow)", "(gsm-shadow)", vec![right], right),
        (
            "(gsm-shadow)",
            "((setq gsm-shadow :written) gsm-shadow)",
            vec![right],
            written,
        ),
        (
            "(gsm-shadow)",
            "((setf gsm-shadow :written) gsm-shadow)",
            vec![right],
            written,
        ),
        (
            "(gsm-shadow &optional (seen gsm-shadow))",
            "(seen)",
            vec![right],
            right,
        ),
        (
            "(&optional (gsm-shadow :default) (seen gsm-shadow))",
            "(seen)",
            vec![],
            default,
        ),
        (
            "(&key (gsm-shadow :key-default) (seen gsm-shadow))",
            "(seen)",
            vec![],
            key_default,
        ),
    ] {
        egcl_rt::rooted!(args = args);
        egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
        egcl_rt::rooted!(body = reader::read_from_string(body).unwrap().0);
        let parent = FrameCell::new_arc(EnvFrame::default());
        egcl_rt::rooted!(
            result = eval_lambda_call_ex(
                &mut env,
                *params,
                *body,
                &args,
                parent,
                LexicalControl::Fresh,
            )
            .unwrap()
        );
        assert_eq!(*result, expected);
        assert_eq!(
            read_eval_all_env("gsm-shadow", &mut env).unwrap(),
            global,
            "leaving a lexical scope restores the global symbol macro"
        );
    }
    let mut child = env.child();
    egcl_rt::rooted_ref!(_child = &mut child);
    egcl_rt::rooted!(uninterned = egcl_rt::symbols::make_uninterned("GSM-SHADOW"));
    let other_package = resolve_sym("EGCL::GSM-SHADOW").unwrap();
    assert_ne!(*uninterned, symbol);
    assert_ne!(other_package, symbol);
    child.define_local_symbol(*uninterned, right);
    child.define_local_symbol(other_package, right);
    assert_eq!(
        child.lookup_symbol_macro(symbol),
        Some(global),
        "same printed name in another package or gensym is not a lexical shadow"
    );
    child.define_local_symbol(symbol, right);
    assert_eq!(child.lookup_symbol_macro(symbol), None);
    assert_eq!(read_eval_all_env("gsm-shadow", &mut child).unwrap(), right);
    let inner = resolve_sym(":INNER-SYMBOL-MACRO").unwrap();
    child.define_symbol_macro(symbol, inner);
    assert_eq!(
        child.lookup_symbol_macro(symbol),
        Some(inner),
        "an explicit inner SYMBOL-MACROLET keeps precedence over outer lexicals"
    );
    assert_eq!(read_eval_all_env("gsm-shadow", &mut child).unwrap(), inner);
    assert_eq!(read_eval_all_env("gsm-shadow", &mut env).unwrap(), global);
}
