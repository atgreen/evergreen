//! Unified `-Xlog`-style tag/level logging (bliss-89rd, epic bliss-bfxm).
//!
//! HotSpot's `-Xlog` gives one control surface — `-Xlog:gc=debug,compilation=info`
//! — over what would otherwise be a scatter of independent trace flags. EGCL had
//! exactly that scatter: `EGCL_T1_TRACE`, `EGCL_OSR_DEBUG`, `EGCL_BAIL_TRACE`,
//! `EGCL_BYTECODE_TRACE`, `EGCL_GC_LOG`, … each an ad-hoc `env::var` check with
//! its own output style. This module converges them behind one variable:
//!
//! ```text
//! EGCL_LOG=compile=debug,osr=trace,gc=info,all=warn
//! ```
//!
//! `all` sets the default level for every tag; a per-tag entry overrides it.
//! Levels are `off < error < warn < info < debug < trace`; a message at level L
//! for tag T prints iff L <= the level configured for T.
//!
//! **Back-compatible.** The legacy `EGCL_*` trace variables still work: each is
//! folded into the config as its tag at `trace` level when set, so existing
//! invocations and docs keep functioning while call sites migrate to [`blog!`].

use std::collections::HashMap;
use std::sync::OnceLock;

pub const OFF: u8 = 0;
pub const ERROR: u8 = 1;
pub const WARN: u8 = 2;
pub const INFO: u8 = 3;
pub const DEBUG: u8 = 4;
pub const TRACE: u8 = 5;

fn level_num(s: &str) -> Option<u8> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some(OFF),
        "error" => Some(ERROR),
        "warn" | "warning" => Some(WARN),
        "info" => Some(INFO),
        "debug" => Some(DEBUG),
        "trace" | "on" => Some(TRACE),
        _ => None,
    }
}

/// Legacy `EGCL_*` trace/debug variables and the tag each maps to, so a unified
/// `EGCL_LOG` taxonomy exists while old flags keep working. Presence of the var
/// (any value) turns its tag on at `trace`.
const LEGACY: &[(&str, &str)] = &[
    ("EGCL_BYTECODE_TRACE", "bytecode"),
    ("EGCL_T1_TRACE", "compile"),
    ("EGCL_T2_LOG", "compile"),
    ("EGCL_BAIL_TRACE", "bail"),
    ("EGCL_OSR_DEBUG", "osr"),
    ("EGCL_GC_LOG", "gc"),
    ("EGCL_DEOPT_LOG", "deopt"),
    ("EGCL_LOAD_TRACE", "load"),
];

struct Config {
    default: u8,
    tags: HashMap<String, u8>,
}

fn parse(spec: Option<String>) -> Config {
    let mut cfg = Config {
        default: OFF,
        tags: HashMap::new(),
    };
    if let Some(spec) = spec {
        for entry in spec.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            // "tag=level", or a bare "tag" meaning tag=trace.
            let (tag, level) = match entry.split_once('=') {
                Some((t, l)) => (t.trim(), level_num(l)),
                None => (entry, Some(TRACE)),
            };
            let Some(level) = level else {
                eprintln!(
                    "egcl: EGCL_LOG: unknown level in '{entry}' (want off/error/warn/info/debug/trace)"
                );
                continue;
            };
            if tag.eq_ignore_ascii_case("all") {
                cfg.default = level;
            } else {
                cfg.tags.insert(tag.to_ascii_lowercase(), level);
            }
        }
    }
    // Fold in legacy flags (only raise, never lower, an explicit EGCL_LOG entry).
    for (var, tag) in LEGACY {
        if std::env::var_os(var).is_some() {
            cfg.tags.entry((*tag).to_string()).or_insert(TRACE);
        }
    }
    cfg
}

fn config() -> &'static Config {
    static C: OnceLock<Config> = OnceLock::new();
    C.get_or_init(|| parse(std::env::var("EGCL_LOG").ok()))
}

/// True if a message at `level` for `tag` should be emitted under the current
/// configuration. Cheap after first call (one map lookup); safe to guard hot
/// paths with — but prefer the [`blog!`] macro, which also defers formatting.
#[inline]
pub fn enabled(tag: &str, level: u8) -> bool {
    let c = config();
    let configured = c.tags.get(tag).copied().unwrap_or(c.default);
    level != OFF && level <= configured
}

/// Emit a preformatted log line for `tag` at `level` (no-op if disabled). Most
/// callers want [`blog!`] instead, which avoids formatting when disabled.
pub fn log(tag: &str, level: u8, args: std::fmt::Arguments) {
    if enabled(tag, level) {
        eprintln!("[{tag}] {args}");
    }
}

/// `blog!(tag, level, "fmt", ...)` — log under the unified tag/level scheme,
/// formatting the message only when that tag/level is enabled.
#[macro_export]
macro_rules! blog {
    ($tag:expr, $level:expr, $($arg:tt)*) => {{
        if $crate::log::enabled($tag, $level) {
            eprintln!("[{}] {}", $tag, format_args!($($arg)*));
        }
    }};
}
