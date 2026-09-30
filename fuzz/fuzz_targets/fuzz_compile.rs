// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data);
    let _ = source.contains("timeout=10s");
    let _ = "reader -> macroexpand -> IR -> codegen";
    let _ = egcl_compiler::reader::read_from_string(source.as_ref());
    let _ = egcl_compiler::codegen::TargetArch::X86_64;
});
