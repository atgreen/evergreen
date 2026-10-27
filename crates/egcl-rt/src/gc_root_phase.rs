// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Opt-in root attribution, independent of the eight minor phase intervals.
//! EGCL_GC_ROOT_PHASE_LOG is a cached presence flag (`=0` also enables it).
//! Each interval includes the original visitor's work, excludes its own log
//! write, and uses no per-slot clock/counter wrapper. Enclosing GC timings
//! include these diagnostic writes: use a separate uninstrumented performance
//! run. `pass` is the existing root-scan epoch, not a collection identifier;
//! minor stack/symbol intervals preceding external enumeration retain the
//! preceding epoch. `minor` is the attempted minor number (0 for generic
//! external scanners, also used by major/verification/snapshot callers).
//!
//! Address mapping: find egcl_rt::gc::root_scanners in `nm -anC` for this exact
//! unstripped binary. Subtract its link-time address from `anchor` to obtain
//! the ASLR slide, then subtract that slide from `scanner` and resolve with
//! `addr2line -Cfipe BINARY ADDRESS`. Keep the binary hash with diagnostic logs.

use std::sync::OnceLock;
use std::time::Instant;

pub(super) fn start() -> Option<Instant> {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    ENABLED
        .get_or_init(|| std::env::var_os("EGCL_GC_ROOT_PHASE_LOG").is_some())
        .then(Instant::now)
}

pub(super) fn finish(start: Option<Instant>, kind: &[u8], minor: u64, scanner: usize) {
    let Some(start) = start else { return };
    let elapsed = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    let mut line = StackLine {
        bytes: [0; 512],
        len: 0,
    };
    line.append(b"[gc-root-phases] ");
    line.append(kind);
    line.field(b"pass", super::root_scan_pass());
    line.field(b"minor", minor);
    line.field(b"scanner", scanner as u64);
    line.field(b"anchor", super::root_scanners as *const () as usize as u64);
    line.field(b"elapsed_ns", elapsed);
    line.append(b"\n");
    crate::syscall::dbg_write(&line.bytes[..line.len]);
}

// Schema labels are fixed internal literals. The longest schema is under 256
// bytes even with twenty decimal digits in every numeric field.
struct StackLine {
    bytes: [u8; 512],
    len: usize,
}
impl StackLine {
    fn append(&mut self, bytes: &[u8]) {
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }
    fn field(&mut self, label: &[u8], mut value: u64) {
        self.append(b" ");
        self.append(label);
        self.append(b"=");
        let mut digits = [0; 20];
        let mut cursor = digits.len();
        loop {
            cursor -= 1;
            digits[cursor] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.append(&digits[cursor..]);
    }
}

#[cfg(test)]
mod tests {
    use super::StackLine;

    #[test]
    fn fixed_schema_formats_maximum_counters_without_truncation() {
        let mut line = StackLine {
            bytes: [0; 512],
            len: 0,
        };
        line.append(b"[gc-root-phases] minor_relocate_cl_stack");
        for label in [
            b"pass".as_slice(),
            b"minor",
            b"scanner",
            b"anchor",
            b"elapsed_ns",
        ] {
            line.field(label, u64::MAX);
        }
        line.append(b"\n");
        let text = std::str::from_utf8(&line.bytes[..line.len]).unwrap();
        assert_eq!(text.matches("=18446744073709551615").count(), 5);
        assert!(text.starts_with("[gc-root-phases] minor_relocate_cl_stack pass="));
        assert!(text.ends_with("elapsed_ns=18446744073709551615\n"));
        assert!(line.len < 256);
    }
}
