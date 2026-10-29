// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(target_os = "ios")]
use egcl_rt::EgclError;
use egcl_stdlib::process::{ProcessCommand, launch_program, run_program};

#[test]
fn subprocess_apis_refuse_shell_and_direct_argv_before_spawning() {
    for spec in [
        ProcessCommand::Shell("exit 0".into()),
        ProcessCommand::Argv(vec!["/definitely/not/an/ios/executable".into()]),
    ] {
        assert!(
            matches!(run_program(spec), Err(EgclError::FileError(message))
            if message.contains("unsupported on iOS"))
        );
    }
    for spec in [
        ProcessCommand::Shell("exit 0".into()),
        ProcessCommand::Argv(vec!["/definitely/not/an/ios/executable".into()]),
    ] {
        assert!(
            matches!(launch_program(spec), Err(EgclError::FileError(message))
            if message.contains("unsupported on iOS"))
        );
    }
}
