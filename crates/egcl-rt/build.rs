// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

fn main() {
    println!("cargo:rustc-check-cfg=cfg(egcl_msan)");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_SANITIZE");
    if std::env::var("CARGO_CFG_SANITIZE")
        .unwrap_or_default()
        .split(',')
        .any(|sanitizer| sanitizer == "memory")
    {
        println!("cargo:rustc-cfg=egcl_msan");
    }
    let target = std::env::var("TARGET").unwrap_or_default();
    // One capability gate for the scheduler and its integration tests.
    println!("cargo:rustc-check-cfg=cfg(egcl_unix_fibers)");
    println!("cargo:rustc-check-cfg=cfg(egcl_fibers)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let unix = std::env::var("CARGO_CFG_TARGET_FAMILY")
        .unwrap_or_default()
        .split(',')
        .any(|family| family == "unix");
    // Instrument the x86-64 Unix stack-switch backend used by sanitizer CI.
    println!("cargo:rustc-check-cfg=cfg(egcl_tsan)");
    if arch == "x86_64"
        && unix
        && std::env::var("CARGO_CFG_SANITIZE")
            .unwrap_or_default()
            .split(',')
            .any(|sanitizer| sanitizer == "thread")
    {
        println!("cargo:rustc-cfg=egcl_tsan");
    }
    let pointers_64 = std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").as_deref() == Ok("64");
    let unix_fibers = unix
        && pointers_64
        && (matches!(arch.as_str(), "x86_64" | "aarch64" | "riscv64")
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
    // glibc Linux targets whose foreign libraries go through the system
    // dlopen (the c-ffi feature, or an arch the static ELF loader lacks) load
    // embedded libraries through a sealed memfd (bliss-rhsy4.2). Only glibc:
    // musl matches an already-loaded DT_NEEDED by the file name it was opened
    // with, which for a memfd is just its descriptor number, not by SONAME.
    println!("cargo:rustc-check-cfg=cfg(egcl_dlopen_embedded)");
    let c_ffi = std::env::var_os("CARGO_FEATURE_C_FFI").is_some();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
        && (c_ffi || !matches!(arch.as_str(), "x86_64" | "aarch64"))
    {
        println!("cargo:rustc-cfg=egcl_dlopen_embedded");
    }
    println!("cargo:rustc-check-cfg=cfg(egcl_ios_simulator)");
    if target == "aarch64-apple-ios-sim" {
        println!("cargo:rustc-cfg=egcl_ios_simulator");
    }
    if arch == "aarch64"
        && matches!(
            std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
            Ok("macos" | "ios")
        )
    {
        println!("cargo:rerun-if-changed=src/syscall/macos_context.c");
        cc::Build::new()
            .file("src/syscall/macos_context.c")
            .compile("egcl_macos_context");
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
