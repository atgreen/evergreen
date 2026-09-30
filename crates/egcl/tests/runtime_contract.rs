// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#[path = "../src/runtime_contract.rs"]
mod runtime_contract;
use runtime_contract::Contract;
#[test]
fn strict_contract_and_subset_validation() {
    let full = Contract::parse("schema=3\nsource=abc\ntarget=x86_64-test\ntoolchain=rustc-test\nfeatures=thread-cache-alloc\nrustflags=\ncapabilities=disassembly\nbuiltins=*\nmax-tier=t2\n").unwrap();
    let small = Contract::parse(
        "schema=3\nsource=abc\ntarget=x86_64-test\ntoolchain=rustc-test\nfeatures=thread-cache-alloc\nrustflags=\ncapabilities=\nbuiltins=*\nmax-tier=t2\n",
    )
    .unwrap();
    assert!(full.accepts(&small).is_ok());
    assert!(small.accepts(&full).is_err());
    assert_eq!(Contract::parse(&full.encode()).unwrap(), full);
    for bad in [
        full.encode().replace("schema=3", "schema=99"),
        full.encode() + "source=abc\n",
        full.encode().replace("disassembly", "unknown"),
    ] {
        assert!(Contract::parse(&bad).is_err());
    }
    let mut wrong = small.clone();
    wrong.source = "other".into();
    assert!(full.accepts(&wrong).is_err());
    wrong = small.clone();
    wrong.target = "other".into();
    assert!(full.accepts(&wrong).is_err());
    wrong = small.clone();
    wrong.toolchain = "other".into();
    assert!(full.accepts(&wrong).is_err());
    wrong = small.clone();
    wrong.features.insert("egcl-rt/python".into());
    assert!(full.accepts(&wrong).is_err());
    wrong = small.clone();
    wrong.rustflags = "00".into();
    assert!(full.accepts(&wrong).is_err());
}

#[test]
fn arbitrary_evaluation_closes_over_every_capability() {
    use runtime_contract::{CAPABILITIES, close_capabilities, opens_code_world};
    let mut capabilities = std::collections::BTreeSet::from(["dynamic-code".to_owned()]);
    close_capabilities(&mut capabilities);
    assert!(CAPABILITIES.iter().all(|name| capabilities.contains(*name)));
    for name in [
        "EVAL",
        "LOAD",
        "REQUIRE",
        "COMPILE",
        "COMPILE-FILE",
        "READ",
        "READ-FROM-STRING",
    ] {
        assert!(opens_code_world(name));
    }
    assert!(!opens_code_world("DISASSEMBLE"));
    let bad = "schema=3\nsource=abc\ntarget=test\ntoolchain=test\nfeatures=\nrustflags=\ncapabilities=dynamic-code\nbuiltins=*\nmax-tier=t2\n";
    assert!(Contract::parse(bad).is_err());
}

#[test]
fn runtime_info_reports_the_actual_build_features() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--runtime-info")
        .output()
        .unwrap();
    assert!(output.status.success());
    let contract = Contract::parse(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert_eq!(
        contract.features.contains("python"),
        cfg!(feature = "python")
    );
    for feature in egcl_rt::image::RUNTIME_BUILD_FEATURES
        .iter()
        .chain(egcl_stdlib::RUNTIME_BUILD_FEATURES)
    {
        assert!(contract.features.contains(*feature), "{contract:?}");
    }
}
