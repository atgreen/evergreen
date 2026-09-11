//! JFR-style unified event stream — CLI symbolication + report layer
//! (bliss-ai8n / bliss-u3h0).
//!
//! The engine **core** (ring buffer, record path, event/reason definitions)
//! lives in [`bliss_rt::events`] so the GC collector — which cannot depend on
//! this crate — can emit GC events into the same stream. This module re-exports
//! that core unchanged (so existing `super::events::record(...)` call sites in
//! `bytecode.rs` keep compiling) and adds the pieces that need the symbol
//! registry: turning symbol ids into names and formatting a dump.

pub use bliss_rt::events::*;

/// Read `BLISS_EVENTS` once at startup: `BLISS_EVENTS=1` begins recording
/// immediately (so `bliss-cli --load foo.lisp` can be profiled without editing
/// the program). Any other value / unset leaves recording off.
pub fn init_from_env() {
    if std::env::var("BLISS_EVENTS").ok().as_deref() == Some("1") {
        set_enabled(true);
    }
}

/// Resolve a symbol id to a printable function name (dump-time symbolication).
fn sym_label(sym: u32) -> String {
    // NO_SYM is the sentinel for an anonymous activation — a top-level form or
    // gensym lambda with no FnMeta (see maybe_osr), or a non-function event
    // (GC) — none of which has a registry name.
    if sym == NO_SYM {
        return "<anonymous/top-level>".to_string();
    }
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
    let (mut compiles, mut deopts, mut osrs, mut minors, mut majors) = (0u64, 0u64, 0u64, 0u64, 0u64);
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
            EventKind::Osr => {
                osrs += 1;
                out.push(format!(
                    "{:>10.3}ms  #{:<6} OSR      @bcp {:<6} (#{}) {}",
                    ms,
                    ev.seq,
                    ev.arg0,
                    ev.arg1,
                    sym_label(ev.sym)
                ));
            }
            EventKind::GcMinor => {
                minors += 1;
                out.push(format!(
                    "{:>10.3}ms  #{:<6} GC-MINOR {}us, {} bytes promoted",
                    ms, ev.seq, ev.arg0, ev.arg1
                ));
            }
            EventKind::GcMajor => {
                majors += 1;
                out.push(format!(
                    "{:>10.3}ms  #{:<6} GC-MAJOR {}us, {} regions freed",
                    ms, ev.seq, ev.arg0, ev.arg1
                ));
            }
        }
    }
    out.push(format!(
        "; summary: {compiles} compile, {deopts} deopt, {osrs} osr, {minors} gc-minor, {majors} gc-major"
    ));
    out
}
