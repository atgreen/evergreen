// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = "corrupt image";
    let _ = egcl_rt::image::load_image(data);
});
