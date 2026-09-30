// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! JFR-style unified event stream — CLI symbolication + report layer
//! (bliss-ai8n / bliss-u3h0).
//!
//! The engine **core** (ring buffer, record path, event/reason definitions)
//! lives in [`egcl_rt::events`] so the GC collector — which cannot depend on
//! this crate — can emit GC events into the same stream. This module re-exports
//! that core unchanged (so existing `super::events::record(...)` call sites in
//! `bytecode.rs` keep compiling) and adds the pieces that need the symbol
//! registry: turning symbol ids into names and formatting a dump.

pub use egcl_rt::events::*;

/// Read `EGCL_EVENTS` once at startup: `EGCL_EVENTS=1` begins recording
/// immediately (so `egcl --load foo.lisp` can be profiled without editing
/// the program). Any other value / unset leaves recording off.
pub fn init_from_env() {
    if std::env::var("EGCL_EVENTS").ok().as_deref() == Some("1") {
        set_enabled(true);
    }
    // EGCL_EVENTS_STREAM=<path>: open an unbounded NDJSON event stream for the
    // egcl-jitrec recorder (record-then-explore, for large programs).
    if let Ok(path) = std::env::var("EGCL_EVENTS_STREAM") {
        if !path.is_empty() {
            open_stream(&path);
        }
    }
}

/// Emit a `bcp→native-offset` map as a JSON array field, or nothing if empty.
fn json_map_field(s: &mut String, key: &str, map: &[u32]) {
    if map.is_empty() {
        return;
    }
    s.push_str(&format!(",\"{key}\":["));
    for (i, off) in map.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&off.to_string());
    }
    s.push(']');
}

/// Finalize the NDJSON stream (egcl-jitrec): after the workload has run and all
/// events streamed live, append the bounded metadata — a `sym` record (id→name)
/// for every symbol seen in an event, and an `fn` record (source + T0/T1/T2
/// disassembly + bcp maps) for every compiled function touched — then close it.
/// Called once as the process exits.
pub fn finalize_stream() {
    if !streaming() {
        return;
    }
    let syms = seen_syms();
    for &sym in &syms {
        let name = egcl_rt::symbols::registry_key(sym).unwrap_or_else(|| format!("#<sym {sym}>"));
        stream_line(&format!(
            "{{\"t\":\"sym\",\"id\":{sym},\"name\":\"{}\"}}",
            json_escape(&name)
        ));
    }
    for &sym in &syms {
        if let Some(td) = super::bytecode::tier_disasm(sym) {
            let mut s = format!(
                "{{\"t\":\"fn\",\"sym\":{sym},\"name\":\"{}\"",
                json_escape(&sym_label(sym))
            );
            if let Some(src) = super::bytecode::source_text(sym) {
                s.push_str(&format!(",\"src\":\"{}\"", json_escape(&src)));
            }
            s.push_str(&format!(",\"t0\":\"{}\"", json_escape(&td.t0)));
            if let Some(t1) = td.t1 {
                s.push_str(&format!(",\"t1\":\"{}\"", json_escape(&t1)));
            }
            if let Some(t2) = td.t2 {
                s.push_str(&format!(",\"t2\":\"{}\"", json_escape(&t2)));
            }
            json_map_field(&mut s, "t1map", &td.t1_map);
            json_map_field(&mut s, "t2map", &td.t2_map);
            s.push('}');
            stream_line(&s);
        }
    }
    close_stream();
}

/// If `EGCL_EVENTS_DUMP=<path>` is set, write the recorded stream as JSON to
/// that path. Called once as the process winds down (from `main`) so any
/// workload — `--load`, `--eval`, a REPL session — can be captured without
/// appending a dump form to it. Errors are reported but never fatal.
/// scripts/event-viewer.sh relies on this to build the HTML viewer.
/// Report direct-builtin-call counts when `EGCL_DIRECT_BUILTIN_STATS` is set
/// (bliss-x5y.27). Diagnostic only: it says whether emitted call sites actually
/// took the direct path, which is otherwise invisible.
pub fn maybe_report_direct_builtin_stats() {
    if std::env::var_os("EGCL_DIRECT_BUILTIN_STATS").is_none() {
        return;
    }
    let hits = super::bytecode::DIRECT_BUILTIN_HITS.load(std::sync::atomic::Ordering::Relaxed);
    let fallbacks =
        super::bytecode::DIRECT_BUILTIN_FALLBACKS.load(std::sync::atomic::Ordering::Relaxed);
    let t0 = super::bytecode::DIRECT_BUILTIN_T0.load(std::sync::atomic::Ordering::Relaxed);
    let t0_fb =
        super::bytecode::DIRECT_BUILTIN_T0_FALLBACKS.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "[direct-builtin] native: direct={hits} fallback={fallbacks} | interpreter: direct={t0} fallback={t0_fb}"
    );
    for line in super::direct_builtin_report() {
        eprintln!("[direct-builtin]   {line}");
    }
}

pub fn maybe_dump_on_exit() {
    let Ok(path) = std::env::var("EGCL_EVENTS_DUMP") else {
        return;
    };
    if path.is_empty() {
        return;
    }
    if let Err(e) = std::fs::write(&path, to_json()) {
        eprintln!("egcl: could not write EGCL_EVENTS_DUMP to {path}: {e}");
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
    egcl_rt::symbols::registry_key(sym).unwrap_or_else(|| format!("#<sym {sym}>"))
}

/// A human-readable, time-ordered dump of the recorded stream — one line per
/// event plus a summary header/footer. Suitable for a REPL builtin or `--load`
/// profiling run. Times are milliseconds since process start.
pub fn report_lines() -> Vec<String> {
    let events = snapshot();
    let mut out = Vec::with_capacity(events.len() + 4);
    let dropped = dropped();
    out.push(format!(
        "; egcl event stream — {} event(s){}",
        events.len(),
        if dropped > 0 {
            format!(" ({dropped} dropped: ring full)")
        } else {
            String::new()
        }
    ));
    let (mut compiles, mut deopts, mut osrs, mut minors, mut majors) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
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

/// Minimal JSON string escaper — names come from the symbol registry (Lisp
/// symbol names can contain quotes/backslashes), so escape defensively.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Machine-readable export of the whole stream as a JSON object
/// `{"events":[...]}`, one entry per event with its symbolicated name and
/// kind-specific fields. Consumed by the JITWatch-style HTML viewer
/// (tools/event-viewer, scripts/event-viewer.sh). Hand-rolled (no serde dep) —
/// the shape is small and fixed.
pub fn to_json() -> String {
    let events = snapshot();
    let mut s = String::from("{\"events\":[");
    for (i, ev) in events.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"seq\":{},\"ns\":{},\"kind\":\"{}\"",
            ev.seq,
            ev.nanos,
            kind_tag(ev.kind)
        ));
        match ev.kind {
            EventKind::Compile => {
                s.push_str(&format!(
                    ",\"name\":\"{}\",\"tier\":{}",
                    json_escape(&sym_label(ev.sym)),
                    ev.arg0
                ));
            }
            EventKind::Deopt => {
                s.push_str(&format!(
                    ",\"name\":\"{}\",\"reason\":\"{}\",\"n\":{}",
                    json_escape(&sym_label(ev.sym)),
                    deopt_reason_name(ev.arg0),
                    ev.arg1
                ));
            }
            EventKind::Osr => {
                s.push_str(&format!(
                    ",\"name\":\"{}\",\"bcp\":{},\"n\":{}",
                    json_escape(&sym_label(ev.sym)),
                    ev.arg0,
                    ev.arg1
                ));
            }
            EventKind::GcMinor => {
                s.push_str(&format!(
                    ",\"pause_us\":{},\"promoted\":{}",
                    ev.arg0, ev.arg1
                ));
            }
            EventKind::GcMajor => {
                s.push_str(&format!(
                    ",\"pause_us\":{},\"regions_freed\":{}",
                    ev.arg0, ev.arg1
                ));
            }
        }
        s.push('}');
    }
    s.push(']');

    // Per-function TriView: the T0 / T1 / T2 representations of every function
    // the JIT touched, so the viewer can show the whole tier progression —
    // including tiers that have since been uninstalled (e.g. a function that
    // deoptimised back to T0). T0 is the annotated bytecode; T1/T2 are the native
    // x86-64 captured at compile time (bliss-kkd0).
    use std::collections::BTreeSet;
    let syms: BTreeSet<u32> = snapshot()
        .iter()
        .map(|e| e.sym)
        .filter(|&s| s != NO_SYM)
        .collect();
    s.push_str(",\"functions\":{");
    let mut first = true;
    for sym in syms {
        if let Some(td) = super::bytecode::tier_disasm(sym) {
            if !first {
                s.push(',');
            }
            first = false;
            s.push_str(&format!("\"{}\":{{", json_escape(&sym_label(sym))));
            if let Some(src) = super::bytecode::source_text(sym) {
                s.push_str(&format!("\"src\":\"{}\",", json_escape(&src)));
            }
            s.push_str(&format!("\"t0\":\"{}\"", json_escape(&td.t0)));
            if let Some(t1) = td.t1 {
                s.push_str(&format!(",\"t1\":\"{}\"", json_escape(&t1)));
            }
            if let Some(t2) = td.t2 {
                s.push_str(&format!(",\"t2\":\"{}\"", json_escape(&t2)));
            }
            // bcp→native-offset maps for the viewer's linked selection.
            json_map_field(&mut s, "t1map", &td.t1_map);
            json_map_field(&mut s, "t2map", &td.t2_map);
            s.push('}');
        }
    }
    s.push('}');

    s.push_str(",\"dropped\":");
    s.push_str(&dropped().to_string());
    s.push('}');
    s
}

/// Per-function aggregate counts (the JFR "flat" lens, complementing the
/// time-ordered [`report_lines`] timeline).
#[derive(Default, Clone, Copy)]
struct PerFn {
    t1: u64,
    t2: u64,
    deopts: u64,
    osr: u64,
}

/// A **flat aggregated** dump of the stream: engine-wide totals (with the GC
/// pause budget and a wall-clock span) plus a per-function table of tier
/// promotions / deopts / OSR entries, hottest first. This is the analysis lens
/// for "where did the JIT spend its effort", distinct from the raw timeline.
pub fn summary_lines() -> Vec<String> {
    use std::collections::HashMap;
    let events = snapshot();
    let mut out = Vec::new();

    // Engine-wide roll-up.
    let (mut compiles, mut deopts, mut osrs, mut minors, mut majors) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut minor_us, mut major_us, mut promoted) = (0u64, 0u64, 0u64);
    let mut per_fn: HashMap<u32, PerFn> = HashMap::new();
    let (mut first_ns, mut last_ns) = (i64::MAX, i64::MIN);
    for ev in &events {
        first_ns = first_ns.min(ev.nanos);
        last_ns = last_ns.max(ev.nanos);
        match ev.kind {
            EventKind::Compile => {
                compiles += 1;
                let e = per_fn.entry(ev.sym).or_default();
                if ev.arg0 >= 2 {
                    e.t2 += 1;
                } else {
                    e.t1 += 1;
                }
            }
            EventKind::Deopt => {
                deopts += 1;
                per_fn.entry(ev.sym).or_default().deopts += 1;
            }
            EventKind::Osr => {
                osrs += 1;
                per_fn.entry(ev.sym).or_default().osr += 1;
            }
            EventKind::GcMinor => {
                minors += 1;
                minor_us += ev.arg0;
                promoted += ev.arg1;
            }
            EventKind::GcMajor => {
                majors += 1;
                major_us += ev.arg0;
            }
        }
    }

    let span_ms = if events.is_empty() {
        0.0
    } else {
        (last_ns - first_ns) as f64 / 1_000_000.0
    };
    out.push(format!(
        "; egcl event stream — flat summary ({} events over {:.3}ms)",
        events.len(),
        span_ms
    ));
    out.push(format!(
        "  Compiles: {compiles}   Deopts: {deopts}   OSR entries: {osrs}"
    ));
    out.push(format!(
        "  GC: {minors} minor ({:.3}ms), {majors} major ({:.3}ms); {promoted} bytes promoted",
        minor_us as f64 / 1000.0,
        major_us as f64 / 1000.0,
    ));

    // Per-function table, most JIT activity first.
    let mut rows: Vec<(u32, PerFn)> = per_fn
        .into_iter()
        .filter(|(sym, _)| *sym != NO_SYM)
        .collect();
    rows.sort_by_key(|(_, p)| std::cmp::Reverse(p.t1 + p.t2 + p.deopts + p.osr));
    if !rows.is_empty() {
        out.push("  Per-function (T1/T2 compiles, deopts, osr):".to_string());
        for (sym, p) in rows {
            out.push(format!(
                "    T1={} T2={} deopt={} osr={}   {}",
                p.t1,
                p.t2,
                p.deopts,
                p.osr,
                sym_label(sym)
            ));
        }
    }
    out
}
