// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Mirror source for crate-local spec path checks.
use std::sync::atomic::Ordering;

fn markers() {
    let _ = Ordering::Acquire;
    let _ = Ordering::Release;
    let _ = "compare_exchange";
    let _ = "yield_requested";
    let _ = "binding";
    let _ = "100 000";
}
