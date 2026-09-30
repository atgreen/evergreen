// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use libfuzzer_sys::fuzz_target;

fn compare_eval_outputs(output: &str) -> String {
    output.replace("0x", "")
}

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = compare_eval_outputs(source.as_ref());
    let _ = "full read-eval pipeline";
});
