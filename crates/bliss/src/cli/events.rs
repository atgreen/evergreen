//! JFR-style unified event stream (bliss-ai8n, epic bliss-bfxm).
//!
//! A low-overhead ring buffer of **typed, `Copy` events** — no `BlissVal`, no
//! allocation on the record path — so it is inherently GC-safe: the moving
//! collector never sees these records, and `record` holds the buffer lock only
//! long enough to push a plain struct (it never allocates a `BlissVal` nor calls
//! anything that can trigger a minor GC). Recording is **opt-in** (off by
//! default: a single relaxed atomic load gates the hot path), so a process that
//! is not recording pays only that load and a predicted-not-taken branch.
//!
//! This mirrors HotSpot's JFR model, which fits bliss because the engine already
//! *emits* the raw signals piecemeal (tier promotions, deopt-with-reason, …);
//! this converts them into one coherent, analyzable stream. Events carry plain
//! integers (symbol id, tier, reason code, a monotonic sequence + timestamp) and
//! are symbolicated to names only at dump time, exactly like a `.jfr` recording.
//!
//! First slice records **compile** (T1/T2 promotion) and **deopt** (with reason)
//! events. OSR / allocation / GC events are a planned follow-up (bliss child).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// The kind of engine event. Kept as a small `Copy` enum so an `Event` is
/// trivially copyable and the record path allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A function was promoted to native code. `arg0` = tier (1 or 2).
    Compile,
    /// A native speculation guard failed and execution fell back to the
    /// interpreter. `arg0` = a `DEOPT_*` reason code; `arg1` = the running
    /// per-function deopt count.
    Deopt,
}

/// A single recorded event. All fields are plain integers so the struct is
/// `Copy` and recording never touches the GC-managed heap.
#[derive(Clone, Copy, Debug)]
pub struct Event {
    /// Monotonic recording sequence number (record order, gap-free).
    pub seq: u64,
    /// Monotonic nanoseconds since process start (same origin as
    /// `get-internal-real-time`).
    pub nanos: i64,
    pub kind: EventKind,
    /// Symbol id of the function the event concerns (resolve with
    /// `bliss_rt::symbols::symbol_name`).
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

/// True if the event stream is currently recording. This is the hot-path gate —
/// a single relaxed load — so callers can wrap `record` unconditionally.
#[inline]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Start or stop recording. Turning recording *on* does not clear existing
/// events (call [`reset`] for that), matching a JFR recording you can pause.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Read `BLISS_EVENTS` once at startup: `BLISS_EVENTS=1` begins recording
/// immediately (so `bliss-cli --load foo.lisp` can be profiled without editing
/// the program). Any other value / unset leaves recording off.
pub fn init_from_env() {
    if std::env::var("BLISS_EVENTS").ok().as_deref() == Some("1") {
        set_enabled(true);
    }
}

/// Drop all recorded events and reset the sequence + dropped counters.
pub fn reset() {
    SEQ.store(0, Ordering::Relaxed);
    DROPPED.store(0, Ordering::Relaxed);
    if let Ok(mut b) = BUFFER.lock() {
        b.clear();
    }
}

/// Record one event. The `enabled()` gate keeps this near-free when recording is
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
    let nanos = bliss_stdlib::time::get_real_time_nanos();
    let ev = Event {
        seq,
        nanos,
        kind,
        sym,
        arg0,
        arg1,
    };
    // Lock only to push a `Copy` struct; no BlissVal allocation happens here, so
    // no minor GC can fire while the lock is held.
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

/// Resolve a symbol id to a printable function name (dump-time symbolication),
/// falling back to `#<sym N>` when the id no longer resolves.
fn sym_label(sym: u32) -> String {
    // registry_key is the same resolver PROFILE-REPORT uses for these engine
    // sym ids, so names line up across the profiling reports.
    bliss_rt::symbols::registry_key(sym).unwrap_or_else(|| format!("#<sym {sym}>"))
}

/// A human-readable, time-ordered dump of the recorded stream — one line per
/// event plus a summary header/footer. Suitable for a REPL builtin or `--load`
/// profiling run. Times are milliseconds since process start.
pub fn report_lines() -> Vec<String> {
    let events = snapshot();
    let mut out = Vec::with_capacity(events.len() + 4);
    let dropped = dropped();
    out.push(format!(
        "; bliss event stream — {} event(s){}",
        events.len(),
        if dropped > 0 {
            format!(" ({dropped} dropped: ring full)")
        } else {
            String::new()
        }
    ));
    let (mut compiles, mut deopts) = (0u64, 0u64);
    for ev in &events {
        let ms = ev.nanos as f64 / 1_000_000.0;
        match ev.kind {
            EventKind::Compile => {
                compiles += 1;
                out.push(format!(
                    "{:>10.3}ms  #{:<6} COMPILE  T{}  {}",
                    ms,
                    ev.seq,
                    ev.arg0,
                    sym_label(ev.sym)
                ));
            }
            EventKind::Deopt => {
                deopts += 1;
                out.push(format!(
                    "{:>10.3}ms  #{:<6} DEOPT    {:<12} (#{}) {}",
                    ms,
                    ev.seq,
                    deopt_reason_name(ev.arg0),
                    ev.arg1,
                    sym_label(ev.sym)
                ));
            }
        }
    }
    out.push(format!(
        "; summary: {compiles} compile, {deopts} deopt"
    ));
    out
}
