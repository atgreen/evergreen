//! Lisp-aware statistical profiler (bliss-sc4t). A `perf`/flamegraph of egcl
//! shows only the interpreter's Rust frames while code tree-walks or runs
//! bytecode — the actual Lisp functions are invisible. This samples a **shadow
//! stack** of the Lisp call chain instead, so a profile shows the real
//! functions, mixed across tiers (a stack can be `FIB [T2]` over `MAP [T0]` over
//! `TOPLEVEL [treewalk]`).
//!
//! **Safe under the moving GC.** Unlike an async-signal sampler that walks native
//! stacks at an arbitrary PC (unsound when a collection may be relocating), the
//! mutator maintains a per-thread shadow stack (push on Lisp entry, pop on exit
//! via an RAII guard that fires on every exit path, including `?` and non-local
//! transfers) and **samples itself**: a background timer sets a flag, and the
//! mutator snapshots its own shadow stack at the next call / loop back-edge —
//! points where it is at a consistent state and holds no `EgclVal` mid-flight.
//! The shadow stack holds only integers, so nothing here allocates a `EgclVal`
//! or touches GC state.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Execution tier a shadow frame was entered at (low 3 bits of a packed frame).
pub const TREEWALK: u8 = 0;
pub const T0: u8 = 1;
pub const T1: u8 = 2;
pub const T2: u8 = 3;

fn tier_name(t: u8) -> &'static str {
    match t {
        TREEWALK => "treewalk",
        T0 => "T0",
        T1 => "T1",
        T2 => "T2",
        _ => "?",
    }
}

static ON: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);
/// name_id → function name (index is the id).
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static NAME_IDS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
/// Each recorded sample is a bottom→top stack of packed frames.
static SAMPLES: Mutex<Vec<Box<[u32]>>> = Mutex::new(Vec::new());

/// The active Lisp call chain, bottom→top; each entry is `(name_id<<3)|tier`.
static SHADOW: egcl_rt::execution_local::ExecutionLocal<std::cell::RefCell<Vec<u32>>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::RefCell::new(Vec::new()))
};
thread_local! {
    /// Cache of symbol id → interned name id, so a compiled call needn't
    /// re-resolve + re-intern the name every invocation.
    static SYM_NAME_ID: std::cell::RefCell<HashMap<u32, u32>> =
        std::cell::RefCell::new(HashMap::new());
}

/// True while a recording is in progress. The hot-path gate — a relaxed load.
#[inline]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn intern(name: &str) -> u32 {
    let mut ids = NAME_IDS.lock().unwrap();
    let ids = ids.get_or_insert_with(HashMap::new);
    if let Some(&id) = ids.get(name) {
        return id;
    }
    let mut names = NAMES.lock().unwrap();
    let id = names.len() as u32;
    names.push(name.to_string());
    ids.insert(name.to_string(), id);
    id
}

fn name_id_for_sym(sym: u32) -> u32 {
    if let Some(id) = SYM_NAME_ID.with(|c| c.borrow().get(&sym).copied()) {
        return id;
    }
    let name = egcl_rt::symbols::symbol_name(sym).unwrap_or_else(|| format!("#<sym {sym}>"));
    let id = intern(&name);
    SYM_NAME_ID.with(|c| c.borrow_mut().insert(sym, id));
    id
}

/// RAII shadow-stack frame: pushes on creation (when recording), pops on drop —
/// so the stack stays balanced across normal return, `?`, and non-local exit.
#[must_use]
pub struct Frame(bool);

impl Frame {
    fn push(id: u32, tier: u8) -> Frame {
        SHADOW.with(|s| s.borrow_mut().push((id << 3) | u32::from(tier)));
        Frame(true)
    }
    /// Enter a Lisp function by symbol id (a compiled call).
    #[inline]
    pub fn sym(sym: u32, tier: u8) -> Frame {
        if !enabled() {
            return Frame(false);
        }
        Frame::push(name_id_for_sym(sym), tier)
    }
    /// Enter a Lisp function by name (a tree-walked call).
    #[inline]
    pub fn name(name: &str, tier: u8) -> Frame {
        if !enabled() {
            return Frame(false);
        }
        Frame::push(intern(name), tier)
    }
}

impl Drop for Frame {
    #[inline]
    fn drop(&mut self) {
        if self.0 {
            SHADOW.with(|s| {
                s.borrow_mut().pop();
            });
        }
    }
}

/// If a sample is due, snapshot this thread's shadow stack. Call at consistent
/// points — call boundaries and loop back-edges. Near-free otherwise.
#[inline]
pub fn maybe_sample() {
    if !enabled() || !PENDING.swap(false, Ordering::Relaxed) {
        return;
    }
    SHADOW.with(|s| {
        let s = s.borrow();
        if !s.is_empty() {
            if let Ok(mut samples) = SAMPLES.lock() {
                samples.push(s.clone().into_boxed_slice());
            }
        }
    });
}

/// Sample when due, combining the tree-walked prefix (the shadow stack) with the
/// bytecode interpreter's live activation chain `bytecode_syms` (bottom→top,
/// tier T0). The bytecode interpreter inlines callee activations onto its own
/// stack rather than making a Rust call, so per-call guards can't see them —
/// this reads that stack directly at a call/loop-edge sample point. The iterator
/// is consumed only when a sample is actually taken.
#[inline]
pub fn maybe_sample_stack(bytecode_syms: impl Iterator<Item = u32>) {
    if !enabled() || !PENDING.swap(false, Ordering::Relaxed) {
        return;
    }
    let mut stack: Vec<u32> = SHADOW.with(|s| s.borrow().clone());
    for sym in bytecode_syms {
        stack.push((name_id_for_sym(sym) << 3) | u32::from(T0));
    }
    if !stack.is_empty() {
        if let Ok(mut samples) = SAMPLES.lock() {
            samples.push(stack.into_boxed_slice());
        }
    }
}

/// Begin recording at `hz` samples/second (clamped to a sane range). Clears any
/// prior recording. A background thread raises the sample flag on a timer.
pub fn start(hz: u32) {
    let hz = hz.clamp(1, 100_000);
    if let Ok(mut s) = SAMPLES.lock() {
        s.clear();
    }
    if ON.swap(true, Ordering::Relaxed) {
        return; // already running
    }
    let period = std::time::Duration::from_nanos(1_000_000_000 / u64::from(hz));
    std::thread::spawn(move || {
        while ON.load(Ordering::Relaxed) {
            std::thread::sleep(period);
            PENDING.store(true, Ordering::Relaxed);
        }
    });
}

/// Stop recording (the sampler thread exits on its next tick).
pub fn stop() {
    ON.store(false, Ordering::Relaxed);
}

/// `EGCL_SPROF=<hz>` begins recording at startup (default 1000 Hz if the value
/// is empty/unparsable), so a `--load foo.lisp` run is profiled without editing
/// the program.
pub fn init_from_env() {
    if let Ok(v) = std::env::var("EGCL_SPROF") {
        let hz = v.trim().parse::<u32>().unwrap_or(1000).max(1);
        start(hz);
    }
}

/// If `EGCL_SPROF=…` was set, stop and write folded stacks to
/// `EGCL_SPROF_OUT` (default `egcl-sprof.folded`) as the process exits — feed
/// it to flamegraph.pl or speedscope. Called from `main`.
pub fn dump_on_exit() {
    if std::env::var_os("EGCL_SPROF").is_none() {
        return;
    }
    stop();
    let path =
        std::env::var("EGCL_SPROF_OUT").unwrap_or_else(|_| "egcl-sprof.folded".to_string());
    let folded = fold();
    match std::fs::write(&path, &folded) {
        Ok(()) => eprintln!(
            "egcl: wrote {} sample(s) to {path} (folded stacks — flamegraph.pl / speedscope)",
            sample_count()
        ),
        Err(e) => eprintln!("egcl: could not write EGCL_SPROF_OUT to {path}: {e}"),
    }
}

/// Number of samples collected.
pub fn sample_count() -> usize {
    SAMPLES.lock().map(|s| s.len()).unwrap_or(0)
}

/// Fold the samples into Brendan-Gregg collapsed-stack format:
/// `frameA;frameB;frameC <count>` per line, ready for flamegraph.pl or
/// speedscope. Each frame is `NAME [tier]`.
pub fn fold() -> String {
    let names = NAMES.lock().unwrap();
    let label = |packed: u32| -> String {
        let id = (packed >> 3) as usize;
        let tier = (packed & 7) as u8;
        let n = names.get(id).map(String::as_str).unwrap_or("?");
        format!("{n} [{}]", tier_name(tier))
    };
    let mut counts: HashMap<String, u64> = HashMap::new();
    if let Ok(samples) = SAMPLES.lock() {
        for stack in samples.iter() {
            let folded = stack
                .iter()
                .map(|&p| label(p))
                .collect::<Vec<_>>()
                .join(";");
            *counts.entry(folded).or_insert(0) += 1;
        }
    }
    let mut lines: Vec<(String, u64)> = counts.into_iter().collect();
    lines.sort_by(|a, b| b.1.cmp(&a.1));
    let mut out = String::new();
    for (stack, n) in lines {
        out.push_str(&stack);
        out.push(' ');
        out.push_str(&n.to_string());
        out.push('\n');
    }
    out
}
