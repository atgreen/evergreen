// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Opt-in collection diagnostics. No Lisp/Rust heap allocations while logging.
//! Durations include diagnostic clock reads and any other enabled GC tracing.
//! EGCL_GC_PHASE_LOG is a cached presence flag: even `=0` enables it. Successful
//! minor bodies emit eight disjoint intervals; failed bodies emit no complete
//! phase line. Lazy nursery indexing is charged to the phase that triggers it.
//! The body total excludes the line's formatting/write; wrapper timing includes
//! that overhead. Minors drained inside a major have no separate STW wrapper.

use std::sync::OnceLock;
use std::time::Instant;

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_GC_PHASE_LOG").is_some())
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    SetupIndex,
    RootsMark,
    OldScan,
    YoungMark,
    Evacuate,
    HeapRelocate,
    RootsRelocate,
    ReclaimRearm,
}

pub(super) struct MinorPhaseLog {
    enabled: bool,
    start: Instant,
    previous_ns: u64,
    phases: [u64; 8],
}

fn ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

impl MinorPhaseLog {
    pub(super) fn new(start: Instant) -> Self {
        Self {
            enabled: enabled(),
            start,
            previous_ns: 0,
            phases: [0; 8],
        }
    }

    pub(super) fn mark(&mut self, phase: Phase) {
        if self.enabled {
            let now = ns(self.start);
            self.phases[phase as usize] = now.saturating_sub(self.previous_ns);
            self.previous_ns = now;
        }
    }

    pub(super) fn finish(self, minor: u64, dirty: bool, indexed: u64, pause_us: u64) {
        if !self.enabled {
            return;
        }
        let mut line = StackLine::new(b"[gc-phases] minor");
        line.field(b"id", minor);
        for (label, value) in [
            b"setup_index_ns".as_slice(),
            b"roots_mark_ns",
            b"old_scan_ns",
            b"young_mark_ns",
            b"evacuate_ns",
            b"heap_relocate_ns",
            b"roots_relocate_ns",
            b"reclaim_rearm_ns",
        ]
        .into_iter()
        .zip(self.phases)
        {
            line.field(label, value);
        }
        line.field(b"body_total_ns", self.previous_ns);
        line.field(b"phase_sum_ns", self.phases.iter().copied().sum());
        line.field(b"recorded_pause_us", pause_us);
        line.field(b"dirty_scan", u64::from(dirty));
        line.field(b"nursery_indexed", indexed);
        line.write();
    }
}

/// The wrapper's body interval includes its diagnostic output. Entry is STW
/// rendezvous/TLAB retirement; exit is world resumption. Collector admission
/// acquired by callers before this wrapper is not measured here.
pub(super) fn log_stw(
    minor: u64,
    start: Instant,
    entered: Option<Instant>,
    finished: Option<Instant>,
    ok: bool,
) {
    let end = Instant::now();
    let duration = |from: Instant, to: Instant| {
        to.saturating_duration_since(from)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    };
    let mut line = StackLine::new(b"[gc-phases] minor_stw");
    line.field(b"id", minor);
    line.field(b"ok", u64::from(ok));
    line.field(b"total_ns", duration(start, end));
    if let (Some(entered), Some(finished)) = (entered, finished) {
        line.field(b"enter_ns", duration(start, entered));
        line.field(b"body_with_log_ns", duration(entered, finished));
        line.field(b"exit_ns", duration(finished, end));
    }
    line.write();
}

/// Both fixed schemas fit comfortably even with twenty decimal digits in
/// every numeric field. One syscall per line avoids per-label write overhead.
struct StackLine {
    bytes: [u8; 1024],
    len: usize,
}
impl StackLine {
    fn new(prefix: &[u8]) -> Self {
        let mut line = Self {
            bytes: [0; 1024],
            len: 0,
        };
        line.append(prefix);
        line
    }
    fn append(&mut self, bytes: &[u8]) {
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }
    fn field(&mut self, label: &[u8], mut value: u64) {
        self.append(b" ");
        self.append(label);
        self.append(b"=");
        let mut digits = [0u8; 20];
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
    fn write(mut self) {
        self.append(b"\n");
        crate::syscall::dbg_write(&self.bytes[..self.len]);
    }
}
