// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    // One capability gate for the scheduler and its integration tests.
    println!("cargo:rustc-check-cfg=cfg(egcl_unix_fibers)");
    println!("cargo:rustc-check-cfg=cfg(egcl_fibers)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let unix = std::env::var("CARGO_CFG_TARGET_FAMILY")
        .unwrap_or_default()
        .split(',')
        .any(|family| family == "unix");
    let pointers_64 = std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() == Ok("64");
    let unix_fibers = unix
        && pointers_64
        && (matches!(arch.as_str(), "x86_64" | "aarch64")
            || matches!(
                target.as_str(),
                "powerpc64le-unknown-linux-gnu"
                    | "powerpc64le-unknown-linux-musl"
                    | "s390x-unknown-linux-gnu"
                    | "s390x-unknown-linux-musl"
            ));
    if unix_fibers {
        println!("cargo:rustc-cfg=egcl_unix_fibers");
    }
    if unix_fibers || (arch == "x86_64" && target.contains("windows")) {
        println!("cargo:rustc-cfg=egcl_fibers");
    }
    if matches!(
        target.as_str(),
        "powerpc64le-unknown-linux-gnu" | "powerpc64le-unknown-linux-musl"
    ) {
        println!("cargo:rerun-if-changed=src/native_transfer/ppc64le.S");
        println!("cargo:rerun-if-changed=src/context/ppc64le.S");
        cc::Build::new()
            .file("src/native_transfer/ppc64le.S")
            .file("src/context/ppc64le.S")
            .flag("-mabi=elfv2")
            .compile("egcl_native_transfer_ppc64le");
    }
    if matches!(
        target.as_str(),
        "s390x-unknown-linux-gnu" | "s390x-unknown-linux-musl"
    ) {
        println!("cargo:rerun-if-changed=src/native_transfer/s390x.S");
        println!("cargo:rerun-if-changed=src/context/s390x.S");
        cc::Build::new()
            .file("src/native_transfer/s390x.S")
            .file("src/context/s390x.S")
            .compile("egcl_native_transfer_s390x");
    }
}
