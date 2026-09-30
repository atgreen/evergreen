//! `egcl` — EGCL Common Lisp command-line interface.
//!
//! Entry point, CLI argument parsing, image loading, and the
//! interactive REPL driver.

pub mod cli;
mod runtime_contract;

pub use cli::{CliArgs, ReplConfig, help_text, print_help, print_version, run, run_repl};

/// Convenience imports for embedding the EGCL CLI driver in tests or tools.
pub mod prelude {
    pub use crate::{CliArgs, ReplConfig, run, run_repl};
}
