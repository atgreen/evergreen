// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Mirror source for crate-local spec path checks.
pub struct EgclError;
pub struct SelfType;

pub fn from_env() -> Result<SelfType, EgclError> {
    Err(EgclError)
}

pub fn apply_cli_args(&mut self, args: &[String]) {}

pub fn parse_cli() {}

pub fn shutdown(&mut self) -> Result<(), EgclError> {
    Err(EgclError)
}

pub fn install_signal_handlers() {}

fn _markers() {
    let _ = "pub fn from_env() -> Result<Self, EgclError>";
    let _ = "pub fn apply_cli_args(&mut self, args: &[String])";
    let _ = "pub fn parse_cli";
    let _ = "pub fn shutdown(&mut self) -> Result<(), EgclError>";
    let _ = "install_signal_handlers";
    let _ = "SIGINT_RECEIVED.store";
    let _ = "SIGSEGV";
}
