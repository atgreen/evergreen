// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#[test]
fn foreign_runtime_inhibition_prevents_creating_an_image() {
    let path = std::env::temp_dir().join(format!("egcl-inhibited-{}.bimg", std::process::id()));
    egcl_rt::image::inhibit_saving().unwrap();
    egcl_rt::image::inhibit_saving().unwrap();
    let error = egcl_rt::image::save_image(
        path.to_str().unwrap(),
        &egcl_rt::image::SaveImageOptions {
            executable: false,
            compression: egcl_rt::image::ImageCompression::None,
            purify: false,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("foreign runtime"), "{error}");
    assert!(!path.exists());
}
