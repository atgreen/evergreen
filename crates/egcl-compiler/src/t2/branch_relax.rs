// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Translate emission-time positions before publishing any native metadata.
use super::{EmitError, TransferEmission};
use egcl_rt::asm::OffsetMap;

pub(super) fn enabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    !*DISABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_BRANCH_RELAXATION").is_some())
}

pub(super) fn offset(map: Option<&OffsetMap>, old: usize) -> Result<usize, EmitError> {
    match map {
        Some(map) => map.translate(old).ok_or(EmitError::BadBranch),
        None => Ok(old),
    }
}

pub(super) fn offset32(map: Option<&OffsetMap>, old: u32) -> Result<u32, EmitError> {
    u32::try_from(offset(map, old as usize)?).map_err(|_| EmitError::BadBranch)
}

pub(super) fn optional(
    map: Option<&OffsetMap>,
    old: Option<usize>,
) -> Result<Option<usize>, EmitError> {
    old.map(|old| offset(map, old)).transpose()
}

pub(super) fn transfers(
    map: Option<&OffsetMap>,
    emission: &mut TransferEmission,
) -> Result<(), EmitError> {
    for site in &mut emission.sites {
        site.return_offset = offset32(map, site.return_offset)?;
    }
    for pc in &mut emission.deopt_returns {
        *pc = offset32(map, *pc)?;
    }
    for (entry, _) in emission.landings.values_mut() {
        *entry = offset32(map, *entry)?;
    }
    for entries in emission.catch_landings.values_mut() {
        for (entry, _, _) in entries {
            *entry = offset32(map, *entry)?;
        }
    }
    for entries in emission.handler_landings.values_mut() {
        for (entry, _, _, _) in entries {
            *entry = offset32(map, *entry)?;
        }
    }
    Ok(())
}
