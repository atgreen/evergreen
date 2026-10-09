// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::bytecode::BytecodeFunction;
use std::rc::Rc;
use std::sync::{Arc, Weak};

/// A result keyed by the bytecode body's Arc address, including a decline.
pub(super) struct SegmentCacheEntry<C> {
    // Keep the Arc allocation reserved so its address cannot identify a new
    // definition while this entry exists. A weak owner allows declined bodies
    // and their constants to be released without adding GC root ownership.
    _definition: Weak<BytecodeFunction>,
    pub(super) code: Option<Rc<C>>,
}

impl<C> SegmentCacheEntry<C> {
    pub(super) fn new(body: &Arc<BytecodeFunction>, code: Option<Rc<C>>) -> Self {
        Self {
            _definition: Arc::downgrade(body),
            code,
        }
    }
}
