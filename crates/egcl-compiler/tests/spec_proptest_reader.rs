// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use proptest::prelude::*;
use std::fs;
use std::path::Path;
use egcl_compiler::reader::{read_from_string, symbol_name};
use egcl_rt::value::{NIL, T, EgclVal};

#[path = "../../../tests/proptest/generators/mod.rs"]
mod generators;

use generators::arb_sexp::{Sexp, arb_sexp};

#[repr(C)]
struct ConsCell {
    car: EgclVal,
    cdr: EgclVal,
}

fn render(sexp: &Sexp) -> String {
    match sexp {
        Sexp::Nil => "NIL".to_string(),
        Sexp::Fixnum(n) => n.to_string(),
        Sexp::Character(' ') => "#\\Space".to_string(),
        Sexp::Character('\n') => "#\\Newline".to_string(),
        Sexp::Character(ch) => format!("#\\{ch}"),
        Sexp::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Sexp::Symbol(name) => name.clone(),
        Sexp::List(items) => {
            let body = items.iter().map(render).collect::<Vec<_>>().join(" ");
            format!("({body})")
        }
    }
}

fn to_sexp(value: EgclVal) -> Sexp {
    if value == NIL {
        return Sexp::Nil;
    }
    if value == T {
        return Sexp::Symbol("T".to_string());
    }
    if value.is_fixnum() {
        return Sexp::Fixnum(value.as_fixnum());
    }
    if value.is_character() {
        return Sexp::Character(value.as_char());
    }
    if value.is_string() {
        return Sexp::String(value.as_string());
    }
    if value.is_symbol() {
        return Sexp::Symbol(
            symbol_name(value.as_symbol_index())
                .unwrap_or_else(|| panic!("missing symbol name for {value:?}")),
        );
    }
    if value.is_cons() {
        let mut items = Vec::new();
        let mut cur = value;
        while cur.is_cons() {
            // SAFETY: reader-allocated lists use a stable leaked cons layout in tests.
            let cell = unsafe { &*((cur.0 & !egcl_rt::value::TAG_MASK) as *const ConsCell) };
            items.push(to_sexp(cell.car));
            cur = cell.cdr;
        }
        assert_eq!(cur, NIL, "expected proper list");
        return Sexp::List(items);
    }
    panic!("unsupported round-trip value {value:?}");
}

fn regression_seed_lines() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/proptest/regressions/reader_roundtrip_cases.txt");
    fs::read_to_string(path)
        .expect("read property regression seeds")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[test]
fn persisted_reader_roundtrip_regressions_replay_cleanly() {
    // Per R10.42, failing seeds must be persisted and rerun on every cargo test.
    for seed in regression_seed_lines() {
        let (value, pos) = read_from_string(&seed).unwrap_or_else(|e| panic!("{seed:?}: {e}"));
        assert_eq!(pos, seed.len(), "seed did not fully parse: {seed:?}");
        assert_eq!(render(&to_sexp(value)), seed, "seed round-trip drifted");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn reader_roundtrips_generated_printable_readable_s_expressions(source in arb_sexp()) {
        // Per R10.36, EGCL must employ proptest for Rust components.
        // Per R10.39, printable-readable s-expressions must round-trip through the reader.
        // Per R10.40, the cargo-test default iteration count is 256.
        // Per R10.41, the generator participates in automatic shrinking on failure.
        let printed = render(&source);
        let (value, pos) = read_from_string(&printed)
            .unwrap_or_else(|e| panic!("reader rejected generated form {printed:?}: {e}"));
        prop_assert_eq!(pos, printed.len());
        prop_assert_eq!(to_sexp(value), source);
    }
}
