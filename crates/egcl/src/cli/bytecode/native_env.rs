// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bytecode::BytecodeFunction;
use egcl_rt::symbols::ResolvedSymbolName;

/// Stable metadata addressed by native code, owned from emission until the
/// last activation/direct caller releases that exact installed code version.
/// Contains no Lisp heap pointers and needs no GC root registration.
#[derive(Default)]
pub(super) struct NativeEnvNames(Vec<ResolvedSymbolName>);

impl NativeEnvNames {
    pub(super) fn new(body: &BytecodeFunction) -> Box<Self> {
        Box::new(Self(
            body.names
                .iter()
                .map(|name| ResolvedSymbolName::new(name))
                .collect(),
        ))
    }

    pub(super) fn get(&self, index: u64) -> Option<&ResolvedSymbolName> {
        self.0.get(usize::try_from(index).ok()?)
    }
}
