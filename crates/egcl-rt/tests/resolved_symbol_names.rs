// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::symbols::{self, ResolvedSymbolName};

// Separate process: image restoration deliberately replaces the whole registry.
#[test]
fn resolved_names_follow_registry_restoration() {
    let first = "RESOLVED-RESTORE-FIRST";
    let second = "RESOLVED-RESTORE-SECOND";
    let first_index = symbols::intern(first);
    let second_index = symbols::intern(second);
    let original = ResolvedSymbolName::new(first);
    assert_eq!(original.index(), Some(first_index));
    let objects = symbols::serialize_objects();
    let mut reordered = 2_u32.to_le_bytes().to_vec();
    for name in [second, first] {
        reordered.extend_from_slice(&(name.len() as u32).to_le_bytes());
        reordered.extend_from_slice(name.as_bytes());
    }
    symbols::restore(&reordered).unwrap();
    assert_eq!(original.index(), Some(1));
    let restored = ResolvedSymbolName::new(first);
    assert_eq!(restored.index(), Some(1));
    symbols::restore_objects(&objects).unwrap();
    assert_eq!(restored.index(), Some(first_index));
    assert_eq!(original.index(), Some(first_index));
    assert_eq!(symbols::find_index(second), Some(second_index));
}
