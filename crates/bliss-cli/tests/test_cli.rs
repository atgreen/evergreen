//! Tests for bliss-cli: CliArgs parsing, ReplConfig defaults, and CLI driver functions.

use bliss_cli::cli::{CliArgs, ReplConfig};

// ── Helper ────────────────────────────────────────────────────────

/// Convenience: build a `Vec<String>` from string slices.
fn args(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

// ══════════════════════════════════════════════════════════════════
// CliArgs::parse — basic flags
// ══════════════════════════════════════════════════════════════════

#[test]
fn parse_empty_args_gives_defaults() {
    let parsed = CliArgs::parse(&args(&[])).expect("empty args should parse");
    assert!(parsed.image.is_none());
    assert!(parsed.eval.is_none());
    assert!(parsed.load.is_none());
    assert!(!parsed.no_image);
    assert!(!parsed.sandbox);
    assert!(!parsed.no_init);
    assert!(!parsed.bootstrap);
    assert!(parsed.workers.is_none());
    assert!(parsed.heap_size.is_none());
    assert!(!parsed.help);
    assert!(!parsed.version);
    assert!(parsed.cl_args.is_empty());
    assert!(parsed.script.is_none());
}

#[test]
fn parse_help_flag() {
    let parsed = CliArgs::parse(&args(&["--help"])).expect("--help should parse");
    assert!(parsed.help);
}

#[test]
fn parse_version_flag() {
    let parsed = CliArgs::parse(&args(&["--version"])).expect("--version should parse");
    assert!(parsed.version);
}

#[test]
fn parse_eval_long_flag() {
    let parsed = CliArgs::parse(&args(&["--eval", "(+ 1 2)"])).expect("--eval should parse");
    assert_eq!(parsed.eval.as_deref(), Some("(+ 1 2)"));
}

#[test]
fn parse_eval_short_flag() {
    let parsed = CliArgs::parse(&args(&["-e", "(print 42)"])).expect("-e should parse");
    assert_eq!(parsed.eval.as_deref(), Some("(print 42)"));
}

#[test]
fn parse_load_flag() {
    let parsed = CliArgs::parse(&args(&["--load", "boot.lisp"])).expect("--load should parse");
    assert_eq!(parsed.load.as_deref(), Some("boot.lisp"));
}

#[test]
fn parse_image_flag() {
    let parsed = CliArgs::parse(&args(&["--image", "core.img"])).expect("--image should parse");
    assert_eq!(parsed.image.as_deref(), Some("core.img"));
}

#[test]
fn parse_no_image_flag() {
    let parsed = CliArgs::parse(&args(&["--no-image"])).expect("--no-image should parse");
    assert!(parsed.no_image);
}

#[test]
fn parse_bootstrap_flag() {
    let parsed = CliArgs::parse(&args(&["--bootstrap"])).expect("--bootstrap should parse");
    assert!(parsed.bootstrap);
}

#[test]
fn parse_sandbox_flag() {
    let parsed = CliArgs::parse(&args(&["--sandbox"])).expect("--sandbox should parse");
    assert!(parsed.sandbox);
}

#[test]
fn parse_no_init_flag() {
    let parsed = CliArgs::parse(&args(&["--no-init"])).expect("--no-init should parse");
    assert!(parsed.no_init);
}

#[test]
fn parse_workers_flag() {
    let parsed = CliArgs::parse(&args(&["--workers", "4"])).expect("--workers should parse");
    assert_eq!(parsed.workers, Some(4));
}

#[test]
fn parse_heap_size_flag() {
    let parsed = CliArgs::parse(&args(&["--heap-size", "512M"])).expect("--heap-size should parse");
    assert_eq!(parsed.heap_size.as_deref(), Some("512M"));
}

#[test]
fn parse_runtime_tuning_flags_are_forwarded() {
    let parsed = CliArgs::parse(&args(&[
        "--tlab-size",
        "2M",
        "--nursery-size",
        "32M",
        "--stack-size",
        "1M",
        "--gc-log",
        "gc.log",
        "--jit-dump",
        "--log-level",
        "debug",
    ]))
    .expect("runtime tuning flags should parse");
    assert!(parsed.eval.is_none());
    assert!(parsed.load.is_none());
    assert!(parsed.cl_args.is_empty());
}

// ══════════════════════════════════════════════════════════════════
// CliArgs::parse — positional & passthrough
// ══════════════════════════════════════════════════════════════════

#[test]
fn parse_script_positional_arg() {
    let parsed = CliArgs::parse(&args(&["my_script.lisp"])).expect("script arg should parse");
    assert_eq!(parsed.script.as_deref(), Some("my_script.lisp"));
}

#[test]
fn parse_cl_args_after_double_dash() {
    let parsed =
        CliArgs::parse(&args(&["--", "foo", "bar", "baz"])).expect("-- passthrough should parse");
    assert_eq!(parsed.cl_args, vec!["foo", "bar", "baz"]);
}

#[test]
fn parse_script_with_cl_args() {
    let parsed = CliArgs::parse(&args(&["app.lisp", "--", "--verbose", "out.txt"]))
        .expect("script + -- should parse");
    assert_eq!(parsed.script.as_deref(), Some("app.lisp"));
    assert_eq!(parsed.cl_args, vec!["--verbose", "out.txt"]);
}

// ══════════════════════════════════════════════════════════════════
// CliArgs::parse — combined flags
// ══════════════════════════════════════════════════════════════════

#[test]
fn parse_multiple_flags_combined() {
    let parsed = CliArgs::parse(&args(&[
        "--image",
        "core.img",
        "--workers",
        "8",
        "--heap-size",
        "1G",
        "--bootstrap",
    ]))
    .expect("combined flags should parse");
    assert_eq!(parsed.image.as_deref(), Some("core.img"));
    assert_eq!(parsed.workers, Some(8));
    assert_eq!(parsed.heap_size.as_deref(), Some("1G"));
    assert!(parsed.bootstrap);
    assert!(!parsed.help);
    assert!(!parsed.version);
}

#[test]
fn parse_help_with_other_flags_still_sets_help() {
    let parsed = CliArgs::parse(&args(&["--help", "--version", "--eval", "(+ 1 2)"]))
        .expect("--help with other flags should parse");
    assert!(parsed.help);
    assert!(parsed.version);
}

// ══════════════════════════════════════════════════════════════════
// CliArgs::parse — edge cases and errors
// ══════════════════════════════════════════════════════════════════

#[test]
fn parse_eval_missing_value_is_error() {
    // --eval requires a value argument
    let result = CliArgs::parse(&args(&["--eval"]));
    assert!(result.is_err(), "--eval without value should fail");
}

#[test]
fn parse_load_missing_value_is_error() {
    let result = CliArgs::parse(&args(&["--load"]));
    assert!(result.is_err(), "--load without value should fail");
}

#[test]
fn parse_image_missing_value_is_error() {
    let result = CliArgs::parse(&args(&["--image"]));
    assert!(result.is_err(), "--image without value should fail");
}

#[test]
fn parse_workers_missing_value_is_error() {
    let result = CliArgs::parse(&args(&["--workers"]));
    assert!(result.is_err(), "--workers without value should fail");
}

#[test]
fn parse_heap_size_missing_value_is_error() {
    let result = CliArgs::parse(&args(&["--heap-size"]));
    assert!(result.is_err(), "--heap-size without value should fail");
}

#[test]
fn parse_runtime_tuning_missing_value_is_error() {
    for flag in [
        "--tlab-size",
        "--nursery-size",
        "--stack-size",
        "--gc-log",
        "--log-level",
    ] {
        let result = CliArgs::parse(&args(&[flag]));
        assert!(result.is_err(), "{} without value should fail", flag);
    }
}

#[test]
fn parse_unknown_flag_is_error() {
    let result = CliArgs::parse(&args(&["--frobnicate"]));
    assert!(result.is_err(), "unknown flag should fail");
}

#[test]
fn parse_workers_non_numeric_is_error() {
    let result = CliArgs::parse(&args(&["--workers", "abc"]));
    assert!(result.is_err(), "non-numeric --workers should fail");
}

#[test]
fn parse_double_dash_stops_flag_parsing() {
    // After --, even flag-like strings are CL args, not parsed as flags
    let parsed =
        CliArgs::parse(&args(&["--", "--help", "--version"])).expect("-- should stop flag parsing");
    assert!(!parsed.help);
    assert!(!parsed.version);
    assert_eq!(parsed.cl_args, vec!["--help", "--version"]);
}

// ══════════════════════════════════════════════════════════════════
// CliArgs::parse — conflicting flags
// ══════════════════════════════════════════════════════════════════

#[test]
fn parse_image_and_no_image_conflict_is_error() {
    // --image and --no-image are contradictory
    let result = CliArgs::parse(&args(&["--image", "core.img", "--no-image"]));
    assert!(result.is_err(), "--image + --no-image should conflict");
}

#[test]
fn parse_sandbox_and_no_image_conflict_is_error() {
    // --sandbox with --no-image may be contradictory (sandbox needs a controlled image)
    let result = CliArgs::parse(&args(&["--sandbox", "--no-image"]));
    assert!(result.is_err(), "--sandbox + --no-image should conflict");
}

#[test]
fn parse_no_init_and_bootstrap_conflict_is_error() {
    // --no-init skips init, --bootstrap loads from lib/boot.lisp — conflicting intent
    let result = CliArgs::parse(&args(&["--no-init", "--bootstrap"]));
    assert!(result.is_err(), "--no-init + --bootstrap should conflict");
}

#[test]
fn parse_eval_and_load_conflict_is_error() {
    // --eval and --load both request non-interactive execution — which takes precedence?
    let result = CliArgs::parse(&args(&["--eval", "(+ 1 2)", "--load", "boot.lisp"]));
    assert!(
        result.is_err(),
        "--eval + --load should conflict or have documented precedence"
    );
}

// ══════════════════════════════════════════════════════════════════
// ReplConfig::default
// ══════════════════════════════════════════════════════════════════

#[test]
fn repl_config_default_has_reasonable_history_file() {
    let config = ReplConfig::default();
    // Should contain a recognizable path component
    assert!(
        config.history_file.contains("bliss") || config.history_file.contains("repl"),
        "default history_file should reference bliss or repl, got: {}",
        config.history_file
    );
}

#[test]
fn repl_config_default_has_positive_history_size() {
    let config = ReplConfig::default();
    assert!(
        config.history_size > 0,
        "default history_size should be > 0"
    );
}

#[test]
fn repl_config_default_syntax_highlighting_enabled() {
    let config = ReplConfig::default();
    assert!(
        config.syntax_highlighting,
        "syntax highlighting should be on by default"
    );
}

// ══════════════════════════════════════════════════════════════════
// CLI driver functions — smoke tests
// ══════════════════════════════════════════════════════════════════

#[test]
fn run_with_help_returns_ok() {
    let result = bliss_cli::cli::run(&args(&["--help"]));
    assert!(result.is_ok(), "run(--help) should return Ok");
    assert_eq!(result.unwrap(), 0);
}

#[test]
fn print_help_does_not_panic() {
    bliss_cli::cli::print_help();
}

#[test]
fn help_text_mentions_runtime_flags() {
    let help = bliss_cli::cli::help_text();
    for flag in [
        "--tlab-size",
        "--nursery-size",
        "--stack-size",
        "--gc-log",
        "--jit-dump",
        "--log-level",
    ] {
        assert!(help.contains(flag), "help text should mention {}", flag);
    }
}

#[test]
fn print_version_does_not_panic() {
    bliss_cli::cli::print_version();
}

#[test]
fn run_repl_returns_ok_on_eof() {
    // In test context, stdin is at EOF so run_repl exits immediately with Ok(0).
    let result = bliss_cli::cli::run_repl();
    assert!(result.is_ok(), "run_repl should return Ok on EOF stdin");
    assert_eq!(result.unwrap(), 0);
}

#[test]
fn cli_args_is_debug_and_clone() {
    let parsed = CliArgs::parse(&args(&["--help"])).expect("parse should succeed");
    let dbg = format!("{:?}", parsed);
    assert!(!dbg.is_empty());
    let cloned = parsed.clone();
    assert_eq!(cloned.help, parsed.help);
}
