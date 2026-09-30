// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target == "powerpc64le-unknown-linux-gnu" {
        println!("cargo:rerun-if-changed=src/native_transfer/ppc64le.S");
        cc::Build::new()
            .file("src/native_transfer/ppc64le.S")
            .flag("-mabi=elfv2")
            .compile("egcl_native_transfer_ppc64le");
    }
    if target == "s390x-unknown-linux-gnu" {
        println!("cargo:rerun-if-changed=src/native_transfer/s390x.S");
        cc::Build::new()
            .file("src/native_transfer/s390x.S")
            .compile("egcl_native_transfer_s390x");
    }
}
