// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native Lisp threads share their creator's library namespace.
use egcl_rt::thread::NativeThreadId;
use egcl_rt::{EgclError, EgclVal};

pub fn make_thread(entry: EgclVal, name: Option<String>) -> Result<NativeThreadId, EgclError> {
    let packages = crate::packages::PackageContext::capture()?;
    egcl_rt::thread::make_thread_named_with_setup(entry, name, move || packages.activate())
}
