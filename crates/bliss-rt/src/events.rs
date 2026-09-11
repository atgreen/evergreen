//! JFR-style unified event stream — engine core (bliss-ai8n / bliss-u3h0).
//!
//! A low-overhead ring buffer of **typed, `Copy` events** — no heap value, no
//! allocation on the record path — so it is inherently GC-safe: the moving
//! collector never traces these records, and [`record`] holds the buffer lock
//! only long enough to push a plain struct (nothing inside can trigger a GC).
//! Recording is **opt-in** (off by default: a single relaxed atomic load gates
//! the hot path), so a non-recording process pays only that load and a
//! predicted-not-taken branch.
//!
//! This mirrors HotSpot's JFR model: the engine already *emits* the raw signals
//! piecemeal (tier promotions, deopt-with-reason, OSR entries, GC pauses); this
//! converts them into one coherent, analyzable stream. Events carry plain
//! integers (a symbol id, a couple of args, a monotonic sequence + timestamp)
//! and are symbolicated to names only at dump time, exactly like a `.jfr` file.
//!
//! The **core** lives here in `bliss-rt` because GC events originate in the
//! collector, which cannot depend on the higher `bliss` crate. The `bliss` CLI
//! re-exports this core and adds the symbolication / report layer (which needs
//! the symbol registry).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// The kind of engine event. A small `Copy` enum so an [`Event`] is trivially
/// copyable and the record path allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A function was promoted to native code. `arg0` = tier (1 or 2).
    Compile,
    /// A native speculation guard failed and execution fell back to the
    /// interpreter. `arg0` = a `DEOPT_*` reason code; `arg1` = the running
    /// per-function deopt count.
    Deopt,
    /// A hot loop entered native code mid-run via on-stack replacement. `arg0` =
    /// the bytecode position of the loop header; `arg1` = the running per-
    /// function OSR entry count.
    Osr,
    /// A minor (nursery) collection completed. `arg0` = pause microseconds;
    /// `arg1` = bytes promoted to the old generation. `sym` is unused
    /// (`u32::MAX`).
    GcMinor,
    /// A major (full-heap) collection completed. `arg0` = pause microseconds;
    /// `arg1` = regions freed. `sym` is unused (`u32::MAX`).
    GcMajor,
}

/// A single recorded event. All fields are plain integers so the struct is
/// `Copy` and recording never touches the GC-managed heap.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    /// Monotonic recording sequence number (record order, gap-free).
    pub seq: u64,
    /// Monotonic nanoseconds since process start.
    pub nanos: i64,
    pub kind: EventKind,
    /// Symbol id of the function the event concerns (`u32::MAX` = none).
    pub sym: u32,
    pub arg0: u64,
    pub arg1: u64,
}

// Deopt reason codes — stable small integers, symbolicated at dump time.
/// Ordinary speculative guard failure; the fast path stays installed.
pub const DEOPT_GUARD: u64 = 0;
/// Deopt threshold hit and a *supported* numeric phase change: the stale
/// specialization is retired and replaced with generic T1 while a new T2 is
/// queued.
pub const DEOPT_PHASE_CHANGE: u64 = 1;
/// Deopt threshold hit on an unsupported domain: speculation is blacklisted and
/// the function falls back to T0.
pub const DEOPT_BLACKLIST: u64 = 2;

/// `sym` sentinel for events not tied to a function (e.g. GC events).
pub const NO_SYM: u32 = u32::MAX;

/// Human-readable name for a deopt reason code (dump-time symbolication).
pub fn deopt_reason_name(code: u64) -> &'static str {
    match code {
        DEOPT_GUARD => "guard",
        DEOPT_PHASE_CHANGE => "phase-change",
        DEOPT_BLACKLIST => "blacklist",
        _ => "unknown",
    }
}

/// Ring-buffer capacity. Bounded so a long-running recording cannot grow without
/// limit; the oldest events are dropped first (like a JFR ring recording).
const CAP: usize = 1 << 16; // 65_536 events

static ENABLED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Count of events dropped because the ring was full (reported so a truncated
/// stream never silently reads as complete).
static DROPPED: AtomicU64 = AtomicU64::new(0);
static BUFFER: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());

fn origin() -> Instant {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// True if the event stream is currently recording. The hot-path gate — a
/// single relaxed load — so callers can wrap [`record`] unconditionally.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Start or stop recording. Turning recording *on* does not clear existing
/// events (call [`reset`] for that), matching a JFR recording you can pause.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Drop all recorded events and reset the sequence + dropped counters.
pub fn reset() {
    SEQ.store(0, Ordering::Relaxed);
    DROPPED.store(0, Ordering::Relaxed);
    if let Ok(mut b) = BUFFER.lock() {
        b.clear();
    }
}

/// Record one event. The [`enabled`] gate keeps this near-free when recording is
/// off; the actual push is `#[cold]` and out of line.
#[inline]
pub fn record(kind: EventKind, sym: u32, arg0: u64, arg1: u64) {
    if !enabled() {
        return;
    }
    record_slow(kind, sym, arg0, arg1);
}

#[cold]
fn record_slow(kind: EventKind, sym: u32, arg0: u64, arg1: u64) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = origin().elapsed().as_nanos() as i64;
    let ev = Event {
        seq,
        nanos,
        kind,
        sym,
        arg0,
        arg1,
    };
    // Lock only to push a `Copy` struct; no heap-value allocation happens here,
    // so no GC can fire while the lock is held.
    if let Ok(mut b) = BUFFER.lock() {
        if b.len() == CAP {
            b.pop_front();
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        b.push_back(ev);
    }
}

/// A copy of the currently-buffered events, oldest first.
pub fn snapshot() -> Vec<Event> {
    BUFFER
        .lock()
        .map(|b| b.iter().copied().collect())
        .unwrap_or_default()
}

/// Number of events currently buffered.
pub fn len() -> usize {
    BUFFER.lock().map(|b| b.len()).unwrap_or(0)
}

/// Number of events dropped because the ring filled (0 if never truncated).
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}
