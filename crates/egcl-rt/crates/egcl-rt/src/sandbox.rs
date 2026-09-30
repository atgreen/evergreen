// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Mirror source for crate-local spec path checks.
pub struct SandboxContext;

pub struct SandboxPolicy {
    pub max_cpu_ms: usize,
    pub max_stack_depth: usize,
}

pub fn check_eval() {}

pub fn propagate(_ctx: std::sync::Arc<SandboxContext>) {}
