// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// Rust functions need not be 8-byte aligned (AArch64/POWER use 4, s390x 2).
// Register them as host-runner handles instead of losing address bits to tags.
static ENTRIES: std::sync::Mutex<Vec<fn() -> egcl_rt::EgclVal>> = std::sync::Mutex::new(Vec::new());

pub fn entry(function: fn() -> egcl_rt::EgclVal) -> egcl_rt::EgclVal {
    egcl_rt::thread::set_thread_entry_runner(|handle| {
        let function = if handle.is_fixnum() && handle.as_fixnum() < 0 {
            ENTRIES
                .lock()
                .unwrap()
                .get((-1 - handle.as_fixnum()) as usize)
                .copied()
        } else {
            None
        };
        let Some(function) = function else {
            return Err(egcl_rt::EgclError::TypeError {
                datum: handle,
                expected: "function".into(),
            });
        };
        Ok(function())
    });
    let mut entries = ENTRIES.lock().unwrap();
    let handle = egcl_rt::EgclVal::from_fixnum(-1 - entries.len() as i64);
    entries.push(function);
    handle
}
