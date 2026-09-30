// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let control = String::from_utf8_lossy(data);
    let _ = egcl_stdlib::format::format_to_string(control.as_ref(), &[]);
});
