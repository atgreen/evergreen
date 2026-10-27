// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic, OpKind};
use std::collections::HashSet;

#[test]
fn relaxed_layout_preserves_exact_native_metadata() {
    const CHILD: &str = "EGCL_TEST_BRANCH_LAYOUT_CHILD";
    const TEST: &str = "t2::emit::shared_deopt::tests::branch_relax_tests::relaxed_layout_preserves_exact_native_metadata";
    let Ok(mode) = std::env::var(CHILD) else {
        let records: Vec<_> = ["on", "off"]
            .into_iter()
            .map(|mode| {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                    .env(CHILD, mode)
                    .env_remove("EGCL_DISABLE_SHARED_DEOPT_RECOVERY")
                    .env_remove("EGCL_DISABLE_UNUSED_DEOPT_STUB_ELISION");
                if mode == "off" {
                    command.env("EGCL_DISABLE_BRANCH_RELAXATION", "1");
                } else {
                    command.env_remove("EGCL_DISABLE_BRANCH_RELAXATION");
                }
                let output = command.output().unwrap();
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success(),
                    "{stdout}\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                stdout
                    .lines()
                    .find_map(|line| {
                        line.split_once("BRANCH-LAYOUT ")
                            .map(|(_, record)| record.to_owned())
                    })
                    .expect("child must execute the exact test and publish its checked artifact")
            })
            .collect();
        let on: Vec<_> = records[0].split('|').collect();
        let off: Vec<_> = records[1].split('|').collect();
        assert!(
            on[0].parse::<usize>().unwrap() < off[0].parse::<usize>().unwrap(),
            "{records:?}"
        );
        assert!(
            on[1].parse::<usize>().unwrap() > 0,
            "an executable prefix must contain a genuine rel8 branch"
        );
        assert_eq!(
            on[2], off[2],
            "logical origins, stack adjustments and precise value maps must survive layout"
        );
        assert!(
            on[3].parse::<usize>().unwrap() < off[3].parse::<usize>().unwrap(),
            "the first recovery CALL must move earlier after prefix shortening"
        );
        return;
    };
    assert!(matches!(mode.as_str(), "on" | "off"));
    let artifact = ordinary(&guarded_function(32));
    let calls = artifact.native_calls.as_ref().unwrap();
    let first_deopt = calls
        .iter()
        .filter(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
        .map(|site| site.return_offset)
        .min()
        .unwrap();
    // Stop before embedded recovery tables. Every decoded byte here is code,
    // including the first unique recovery CALL, rather than recipe data.
    let mut boundaries = HashSet::new();
    let mut short_branches = 0;
    for instruction in Decoder::new(64, &artifact.code[..first_deopt], DecoderOptions::NONE) {
        assert!(!instruction.is_invalid());
        boundaries.insert(instruction.ip() as usize);
        if instruction.len() == 2
            && instruction.op_count() > 0
            && matches!(
                instruction.op0_kind(),
                OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
            )
            && instruction.mnemonic() != Mnemonic::Call
        {
            short_branches += 1;
        }
    }
    boundaries.insert(first_deopt);
    for entry in [
        artifact.frame_entry,
        artifact.ordinary_entry,
        artifact.ordinary_bound_entry,
        artifact.native_recursive_entry,
        Some(artifact.compiled_entry),
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            entry < first_deopt && boundaries.contains(&entry),
            "entry {entry} must name executable instruction start"
        );
    }
    assert!(
        artifact
            .bcp_offsets
            .iter()
            .any(|&offset| offset != u32::MAX),
        "fixture must publish actual bytecode position metadata"
    );
    for &offset in artifact
        .bcp_offsets
        .iter()
        .filter(|&&offset| offset != u32::MAX)
    {
        assert!(
            (offset as usize) < first_deopt && boundaries.contains(&(offset as usize)),
            "BCP offset {offset} must name body instruction start"
        );
    }
    let mut unique = HashSet::new();
    let mut logical = Vec::new();
    for site in calls.iter() {
        assert!(unique.insert(site.return_offset), "CALL PCs must be unique");
        // Decode candidate CALL encodings ending at this exact PC. A preceding
        // byte pattern elsewhere in the buffer is not an attachment point.
        assert!(
            (2..=7).any(
                |size| site.return_offset.checked_sub(size).is_some_and(|start| {
                    let mut decoder = Decoder::new(
                        64,
                        &artifact.code[start..site.return_offset],
                        DecoderOptions::NONE,
                    );
                    let instruction = decoder.decode();
                    !instruction.is_invalid()
                        && instruction.mnemonic() == Mnemonic::Call
                        && instruction.len() == size
                        && !decoder.can_decode()
                })
            ),
            "no CALL ends at recorded PC {}",
            site.return_offset
        );
        let values = calls
            .value_map(site.return_offset)
            .expect("every CALL retains exact value-map attachment");
        logical.push(format!(
            "{:?}:{:?}:{}:{values:?}",
            site.origin, site.stack_base, site.stack_adjust
        ));
    }
    assert_eq!(
        calls
            .iter()
            .filter(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
            .count(),
        32
    );
    // Cold stub iteration follows an existing HashMap, so compare logical
    // coverage independently of physical stub order in these fresh processes.
    logical.sort();
    println!(
        "BRANCH-LAYOUT {}|{}|{}|{}",
        artifact.code.len(),
        short_branches,
        logical.join(";"),
        first_deopt
    );
}
