// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `egcl` — Evergreen Common Lisp command-line interface.
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

/// Run one embedded root without process-exit semantics. Descendants may still
/// be active on return: the embedding supervisor must drain them separately.
/// Only one embedded root may start during this process lifetime.
pub fn run_embedded(args: &[String]) -> Result<i32, egcl_rt::EgclError> {
    egcl_rt::host::begin_embedded_root()?;
    match cli::run(args) {
        Err(egcl_rt::EgclError::Shutdown) => Ok(egcl_rt::host::exit_code().unwrap_or(0)),
        result => result,
    }
}
