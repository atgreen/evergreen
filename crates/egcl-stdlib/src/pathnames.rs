// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Pathnames and logical pathnames.
//!
//! See spec §5.7.

use egcl_rt::error::EgclError;
use egcl_rt::lock_order::{LockLevel, OrderedMutex};
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{NIL, T, TAG_HEAP_OBJECT, EgclVal};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Once;

// ── Internal data ─────────────────────────────────────────────────

#[derive(Clone, Debug)]
enum DirPart {
    Literal(String),
    Wild,
    WildInferiors,
    Up,
}

#[derive(Clone, Debug)]
struct DirectorySpec {
    absolute: bool,
    parts: Vec<DirPart>,
}

#[derive(Clone, Debug)]
enum ComponentSpec {
    Literal(String),
    Wild,
}

#[derive(Clone, Debug)]
struct ParsedPathname {
    is_logical: bool,
    host_name: Option<String>,
    // Derived from the existing device slot when restoring an image.
    device_name: Option<String>,
    directory: Option<DirectorySpec>,
    name: Option<ComponentSpec>,
    type_field: Option<ComponentSpec>,
}

#[derive(Clone)]
struct PathnameRecord {
    host: EgclVal,
    device: EgclVal,
    directory: EgclVal,
    name: EgclVal,
    type_field: EgclVal,
    version: EgclVal,
    parsed: ParsedPathname,
    namestring: Option<String>,
}

static PATHNAME_STORE: OrderedMutex<Option<HashMap<u64, PathnameRecord>>> =
    OrderedMutex::new(LockLevel::GcWorld, 9, "pathname GC roots", None);
static STRING_REGISTRY: OrderedMutex<Option<HashMap<u64, String>>> =
    OrderedMutex::new(LockLevel::GcWorld, 10, "pathname string registry", None);
static STRING_REVERSE_REGISTRY: OrderedMutex<Option<HashMap<String, EgclVal>>> = OrderedMutex::new(
    LockLevel::GcWorld,
    11,
    "pathname reverse string registry",
    None,
);
static LOGICAL_TRANSLATIONS: OrderedMutex<Option<HashMap<String, EgclVal>>> =
    OrderedMutex::new(LockLevel::GcWorld, 12, "logical pathname GC roots", None);

fn scan_pathname_global_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut store = PATHNAME_STORE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(store) = store.as_mut() {
        for record in store.values_mut() {
            visit(&mut record.host);
            visit(&mut record.device);
            visit(&mut record.directory);
            visit(&mut record.name);
            visit(&mut record.type_field);
            visit(&mut record.version);
        }
    }
    // The string registries are keyed by (or hold) raw value bits of GC-heap
    // strings from make_string_bv. A moving GC invalidates those bits: a stale
    // forward-registry key falsely matches whatever object is later allocated
    // at the recycled address (format/sequences then treat an unrelated value
    // as that string), and a stale reverse-registry value hands out a dangling
    // pointer. Visit each key/value as a root — pinning the string live and
    // letting the relocation pass rewrite the slot — then rebuild the forward
    // map under the post-move bits (bliss-a27). Off-heap pathname keys
    // (make_record_value) are outside the managed heap and pass through
    // untouched.
    let mut registry = STRING_REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(registry) = registry.as_mut() {
        let mut entries: Vec<(EgclVal, String)> = registry
            .drain()
            .map(|(bits, s)| (EgclVal(bits), s))
            .collect();
        for (key, _) in entries.iter_mut() {
            visit(key);
        }
        registry.extend(entries.into_iter().map(|(key, s)| (key.0, s)));
    }
    let mut reverse = STRING_REVERSE_REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(reverse) = reverse.as_mut() {
        for value in reverse.values_mut() {
            visit(value);
        }
    }
    let mut translations = LOGICAL_TRANSLATIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(translations) = translations.as_mut() {
        for value in translations.values_mut() {
            visit(value);
        }
    }
}

fn install_pathname_global_root_scanner() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_pathname_global_roots));
}

fn with_pathname_store<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<u64, PathnameRecord>) -> R,
{
    let mut guard = PATHNAME_STORE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn with_string_registry<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<u64, String>) -> R,
{
    let mut guard = STRING_REGISTRY.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn with_string_reverse_registry<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, EgclVal>) -> R,
{
    let mut guard = STRING_REVERSE_REGISTRY.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Drop redundant heap-string caches before application delivery. Real strings
/// decode from their heap bodies; keeping a cache entry must not keep an unused
/// function's literals in the saved image. Pathname components and logical
/// translations remain roots, and non-heap sentinel entries remain available.
pub fn clear_delivery_string_caches() -> Result<(), EgclError> {
    egcl_rt::gc::with_heap_snapshot(|| {
        // Collect addresses without holding a registry lock while consulting
        // the heap. The snapshot keeps these identities stable throughout.
        let keys = with_string_registry(|registry| registry.keys().copied().collect::<Vec<_>>());
        let heap_keys: std::collections::HashSet<_> = keys
            .into_iter()
            .filter(|bits| egcl_rt::gc::is_in_heap((bits & !egcl_rt::value::TAG_MASK) as usize))
            .collect();
        with_string_registry(|registry| registry.retain(|bits, _| !heap_keys.contains(bits)));
        with_string_reverse_registry(|registry| {
            registry.retain(|_, value| !heap_keys.contains(&value.0))
        });
    })
}

fn with_logical_translations<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, EgclVal>) -> R,
{
    let mut guard = LOGICAL_TRANSLATIONS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

// ── Helpers: keyword / string hashing ─────────────────────────────

fn keyword_hash(s: &str) -> u64 {
    let mut h: u64 = 0x517cc1b727220a95;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & !0b111) | 0b101
}

fn make_string_bv(s: &str) -> EgclVal {
    install_pathname_global_root_scanner();
    let existing = with_string_reverse_registry(|rev| rev.get(s).copied());
    if let Some(bv) = existing {
        return bv;
    }
    // A heap-object tag is a promise that the payload is a readable object
    // pointer.  Older pathname code put an FNV hash behind that tag, forcing
    // every type predicate to consult this side registry before reading an
    // ObjectHeader and making direct compiled type checks unsafe.  Use the
    // ordinary string allocator so pathname components obey the same object
    // representation contract as every other Lisp string.
    let bv = crate::streams::make_lisp_string(s);
    with_string_registry(|reg| {
        reg.insert(bv.0, s.to_string());
    });
    with_string_reverse_registry(|rev| {
        rev.insert(s.to_string(), bv);
    });
    bv
}

fn lookup_string(val: EgclVal) -> Option<String> {
    with_string_registry(|reg| reg.get(&val.0).cloned())
}

/// Read string content, preferring the registry cache before decoding a real
/// heap string (SIMPLE_BASE_STRING or SIMPLE_CHARACTER_STRING).
fn component_string(val: EgclVal) -> Option<String> {
    lookup_string(val).or_else(|| {
        if val.is_string() {
            Some(val.as_string())
        } else {
            None
        }
    })
}

pub fn register_string(val: EgclVal, s: &str) {
    ANY_REGISTERED_STRING.store(true, std::sync::atomic::Ordering::Relaxed);
    install_pathname_global_root_scanner();
    with_string_registry(|reg| {
        reg.insert(val.0, s.to_string());
    });
    with_string_reverse_registry(|rev| {
        rev.entry(s.to_string()).or_insert(val);
    });
}

pub fn registered_string(val: EgclVal) -> Option<String> {
    if !any_registered_string() {
        return None;
    }
    lookup_string(val)
}

/// Has any string sentinel ever been registered? Set on the first
/// `register_string` and never cleared, so `false` is a sound "definitely not
/// registered" and the caller can skip the registry entirely.
fn any_registered_string() -> bool {
    ANY_REGISTERED_STRING.load(std::sync::atomic::Ordering::Relaxed)
}

static ANY_REGISTERED_STRING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Is `val` a registry-backed string sentinel?
///
/// This is the question the sequence type predicates actually ask, and they ask
/// it on EVERY element access: `is_vector` and `is_complex_vector` must not
/// dereference a sentinel, whose bits are a hash rather than a pointer. Going
/// through `registered_string` to answer it locked a global mutex, hashed the
/// value, and CLONED a String — to compute a boolean. On a 20M-iteration loop
/// that made `(aref v 3)` cost 0.239 us/iter against `(car c)`'s 0.026, i.e.
/// ~17x, with lookup_string/hash_one/__lock plainly visible in the profile.
///
/// Answer it without allocating, and without touching the registry at all in
/// the overwhelmingly common case where no sentinel has ever been made.
pub fn is_registered_string(val: EgclVal) -> bool {
    if !any_registered_string() {
        return false;
    }
    with_string_registry(|reg| reg.contains_key(&val.0))
}

fn is_keyword(val: EgclVal, name: &str) -> bool {
    if val.0 == keyword_hash(name) {
        return true;
    }
    // MAKE-PATHNAME and friends pass *live* interpreter keyword symbols (a real
    // TAG_SYMBOL named "KEYWORD:<name>") rather than the stdlib's hash sentinel,
    // so recognize those too — otherwise `:wild`/`:newest` components are treated
    // as opaque literals and wildcard matching (e.g. `*.asd`) silently fails.
    // Guard against NIL/T, which report `is_symbol()` but have no symbol-table
    // index (as_symbol_index would panic). Hash sentinels carry TAG_SYMBOL and
    // are safe: a bogus index simply resolves to no name.
    if val.is_symbol() && val != NIL && val != T {
        if let Some(sym) = egcl_rt::symbols::symbol_name(val.as_symbol_index()) {
            if let Some(bare) = sym.strip_prefix("KEYWORD:") {
                return bare.eq_ignore_ascii_case(name);
            }
        }
    }
    false
}

fn is_wild(val: EgclVal) -> bool {
    is_keyword(val, "WILD")
}

fn alloc_pathname(rec: PathnameRecord) -> EgclVal {
    install_pathname_global_root_scanner();
    // A pathname is a REAL heap object carrying the PATHNAME type_id in its
    // header (bliss-lb6.9), NOT a fake `(id << 3) | TAG_HEAP_OBJECT` sentinel.
    // The old sentinel had the heap-object tag but pointed at a bogus low
    // address, so `is_string()` (and the sequence functions built on it)
    // dereferenced garbage and mistook a pathname for a string — crashing or
    // walking off the end. With a real PATHNAME header, `is_string()` reads a
    // genuine type_id and is false by construction, exactly as SBCL keeps
    // PATHNAME a distinct type disjoint from STRING.
    //
    // The block is allocated off the GC heap (stable address, leaked like the
    // CLOS-instance / stream side storage) and holds only the header; the
    // component record lives in the side-table keyed by the object value.
    let layout = std::alloc::Layout::from_size_align(16, 8).expect("pathname layout");
    let bv = unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        *(ptr as *mut ObjectHeader) = ObjectHeader::new(type_id::PATHNAME, 2);
        EgclVal::from_heap_ptr(ptr)
    };
    with_pathname_store(|store| {
        store.insert(bv.0, rec);
    });
    bv
}

fn get_record(pathname: EgclVal) -> Option<PathnameRecord> {
    with_pathname_store(|store| store.get(&pathname.0).cloned())
}

/// Run `f` over a pathname's record WITHOUT cloning it.
///
/// [`get_record`] clones the whole `PathnameRecord` — six `EgclVal`s, a
/// `ParsedPathname`, and an `Option<String>` namestring — so every accessor that
/// wanted one field paid many heap allocations. Measured with the `alloc-count`
/// feature, that put MAKE-PATHNAME at 105 Rust allocations per call and
/// `asdf:make-plan` at 11.7 million (372 MB of churn), which is most of what an
/// `asdf:load-system` costs (bliss-ou03).
///
/// CONSTRAINT: `f` runs while the global pathname mutex is held. It must not
/// allocate on the GC heap, call back into pathname code, or otherwise re-enter
/// — `with_pathname_store` is a plain `Mutex`, so re-entry deadlocks. Keep these
/// closures to field reads and pure computation over borrowed data.
fn with_record<T>(pathname: EgclVal, f: impl FnOnce(&PathnameRecord) -> T) -> Option<T> {
    with_pathname_store(|store| store.get(&pathname.0).map(f))
}

/// True if `val` is a pathname. Pathnames are real heap objects with the
/// PATHNAME type_id (bliss-lb6.9), so `is_string()` etc. read a genuine header
/// and are already false for them; this membership check identifies pathnames by
/// their component record without dereferencing, and is safe for any value.
pub fn is_pathname(val: EgclVal) -> bool {
    with_pathname_store(|store| store.contains_key(&val.0))
}

/// True if `val` is a *logical* pathname (its parse carries a logical host).
/// Used to wire `(typep x 'logical-pathname)` and `LOGICAL-PATHNAME`. Safe for
/// any value (a non-pathname simply has no record).
pub fn is_logical_pathname(val: EgclVal) -> bool {
    with_pathname_store(|store| {
        store
            .get(&val.0)
            .map(|r| r.parsed.is_logical)
            .unwrap_or(false)
    })
}

/// Coerce a namestring to a LOGICAL pathname (ANSI `LOGICAL-PATHNAME`). The
/// string must name a host — a run of `[A-Za-z0-9-]` terminated by `:` — and use
/// only characters legal in a logical namestring; otherwise a TYPE-ERROR is
/// signalled (ansi logical-pathname.error.2/.10). An existing logical pathname
/// value is returned by the caller before this is reached.
pub fn logical_pathname_from_string(s: &str) -> Result<EgclVal, EgclError> {
    let bad = || EgclError::TypeError {
        datum: make_string_bv(s),
        expected: "logical pathname namestring".to_string(),
    };
    // A logical namestring names a host before the first ':' (and before any
    // ';', '/' or '.').
    let colon = s.find(':').ok_or_else(bad)?;
    if colon == 0 {
        return Err(bad());
    }
    let host = &s[..colon];
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(bad());
    }
    let rest = &s[colon + 1..];
    // Legal logical-namestring body characters: letters, digits, hyphen, and the
    // structural markers ';' '.' '*'. Anything else (e.g. '%') is invalid.
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ';' | '.' | '*'))
    {
        return Err(bad());
    }
    let parsed = parse_logical_namestring(s, None).map_err(|_| bad())?;
    Ok(make_record_value(build_record_from_namestring(
        parsed, None,
    )))
}

fn nil_if_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// User home as a physical namestring, using the native platform convention.
pub fn user_home_namestring() -> Option<String> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").ok()?;
    #[cfg(windows)]
    let home = home.replace('\\', "/");
    Some(home)
}

fn resolve_home_path(input: &str) -> Result<String, EgclError> {
    if let Some(rest) = input.strip_prefix("~/") {
        let home = user_home_namestring()
            .ok_or_else(|| EgclError::FileError("user home directory is not set".to_string()))?;
        return Ok(format!("{}/{}", home.trim_end_matches('/'), rest));
    }
    if input == "~" {
        let home = user_home_namestring()
            .ok_or_else(|| EgclError::FileError("user home directory is not set".to_string()))?;
        return Ok(home);
    }
    Ok(input.to_string())
}

fn split_name_type(token: &str) -> (Option<String>, Option<String>) {
    if token.is_empty() {
        return (None, None);
    }
    if token.starts_with('.') && !token[1..].contains('.') {
        return (Some(token.to_string()), None);
    }
    if let Some(dot) = token.rfind('.') {
        if dot == 0 {
            return (Some(token.to_string()), None);
        }
        let name = token[..dot].to_string();
        let typ = token[dot + 1..].to_string();
        return (Some(name), nil_if_empty(typ));
    }
    (Some(token.to_string()), None)
}

fn canonicalize_dir_parts(parts: &[&str]) -> Result<Vec<DirPart>, EgclError> {
    let mut out = Vec::new();
    for part in parts {
        match *part {
            "" | "." => {}
            ".." => out.push(DirPart::Up),
            "*" => out.push(DirPart::Wild),
            "**" => out.push(DirPart::WildInferiors),
            component => out.push(DirPart::Literal(component.to_string())),
        }
    }
    Ok(out)
}

/// Split a Windows volume prefix without putting it in the directory list.
#[cfg(windows)]
fn windows_path_prefix(s: &str) -> Result<(Option<String>, Option<String>, &str), EgclError> {
    let s = if let Some(rest) = s.strip_prefix("//?/") {
        if rest
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("UNC/"))
        {
            return windows_unc_prefix(&rest[4..]);
        }
        if rest.as_bytes().get(1..3) != Some(b":/") {
            return Err(EgclError::FileError(
                "unsupported Windows device namespace".into(),
            ));
        }
        rest
    } else {
        s
    };
    if let Some(rest) = s.strip_prefix("//") {
        return windows_unc_prefix(rest);
    }
    if s.as_bytes().get(1) == Some(&b':')
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        return Ok((None, Some(s[..1].to_ascii_uppercase()), &s[2..]));
    }
    Ok((None, None, s))
}

#[cfg(windows)]
fn windows_unc_prefix(s: &str) -> Result<(Option<String>, Option<String>, &str), EgclError> {
    let (host, rest) = s
        .split_once('/')
        .ok_or_else(|| EgclError::FileError("UNC pathname requires a server and share".into()))?;
    let end = rest.find('/').unwrap_or(rest.len());
    let share = &rest[..end];
    if host.is_empty() || host == "." || host == "?" || share.is_empty() {
        return Err(EgclError::FileError("invalid UNC server or share".into()));
    }
    Ok((Some(host.into()), Some(share.into()), &rest[end..]))
}

fn parse_physical_namestring(s: &str) -> Result<ParsedPathname, EgclError> {
    if s.is_empty() {
        return Ok(ParsedPathname {
            is_logical: false,
            host_name: None,
            device_name: None,
            directory: None,
            name: None,
            type_field: None,
        });
    }

    let expanded = resolve_home_path(s)?;
    #[cfg(windows)]
    let expanded = expanded.replace('\\', "/");
    #[cfg(windows)]
    let (host_name, device_name, body) = windows_path_prefix(&expanded)?;
    #[cfg(not(windows))]
    let (host_name, device_name, body) = (None, None, expanded.as_str());
    let absolute = body.starts_with('/') || host_name.is_some();
    let trailing_slash = body.ends_with('/');
    let tokens: Vec<&str> = body.split('/').filter(|part| !part.is_empty()).collect();

    let (dir_tokens, final_token) = if trailing_slash || tokens.is_empty() {
        (tokens.as_slice(), None)
    } else {
        (&tokens[..tokens.len() - 1], tokens.last().copied())
    };

    let directory = if absolute || !dir_tokens.is_empty() || trailing_slash {
        Some(DirectorySpec {
            absolute,
            parts: canonicalize_dir_parts(dir_tokens)?,
        })
    } else {
        None
    };

    let (name, type_field) = if let Some(token) = final_token {
        let (name, type_field) = split_name_type(token);
        (
            name.map(|value| {
                if value == "*" {
                    ComponentSpec::Wild
                } else {
                    ComponentSpec::Literal(value)
                }
            }),
            type_field.map(|value| {
                if value == "*" {
                    ComponentSpec::Wild
                } else {
                    ComponentSpec::Literal(value)
                }
            }),
        )
    } else {
        (None, None)
    };

    Ok(ParsedPathname {
        is_logical: false,
        host_name,
        device_name,
        directory,
        name,
        type_field,
    })
}

fn parse_logical_namestring(
    s: &str,
    forced_host: Option<String>,
) -> Result<ParsedPathname, EgclError> {
    let (host_name, rest) = if let Some(host) = forced_host {
        let remainder = s.split_once(':').map(|(_, tail)| tail).unwrap_or(s);
        (host.to_uppercase(), remainder)
    } else if let Some((host, tail)) = s.split_once(':') {
        (host.to_uppercase(), tail)
    } else {
        return Err(EgclError::FileError(format!(
            "invalid logical namestring: {}",
            s
        )));
    };

    let relative = rest.starts_with(';');
    let body = rest.trim_start_matches(';');
    let segments: Vec<&str> = body.split(';').collect();
    let trailing_sep = rest.ends_with(';');

    let (dir_segments, final_segment) = if trailing_sep || segments.is_empty() || body.is_empty() {
        (segments.as_slice(), None)
    } else {
        (&segments[..segments.len() - 1], segments.last().copied())
    };

    let mut dir_parts = Vec::new();
    for segment in dir_segments.iter().copied().filter(|s| !s.is_empty()) {
        let upper = segment.to_uppercase();
        match upper.as_str() {
            "*" => dir_parts.push(DirPart::Wild),
            "**" => dir_parts.push(DirPart::WildInferiors),
            ".." => dir_parts.push(DirPart::Up),
            _ => dir_parts.push(DirPart::Literal(upper)),
        }
    }

    let directory = if dir_parts.is_empty() && !trailing_sep {
        None
    } else {
        Some(DirectorySpec {
            absolute: !relative,
            parts: dir_parts,
        })
    };

    let (name, type_field) = if let Some(segment) = final_segment {
        let upper = segment.to_uppercase();
        let pieces: Vec<&str> = upper.split('.').collect();
        let name_piece = pieces.first().copied().unwrap_or("");
        let type_piece = pieces.get(1).copied().unwrap_or("");
        (
            nil_if_empty(name_piece.to_string()).map(|value| {
                if value == "*" {
                    ComponentSpec::Wild
                } else {
                    ComponentSpec::Literal(value)
                }
            }),
            nil_if_empty(type_piece.to_string()).map(|value| {
                if value == "*" {
                    ComponentSpec::Wild
                } else {
                    ComponentSpec::Literal(value)
                }
            }),
        )
    } else {
        (None, None)
    };

    Ok(ParsedPathname {
        is_logical: true,
        host_name: Some(host_name),
        device_name: None,
        directory,
        name,
        type_field,
    })
}

fn parse_namestring_model(s: &str, host: Option<EgclVal>) -> Result<ParsedPathname, EgclError> {
    let host_string = host.and_then(lookup_string);
    let forced_logical = host_string
        .as_ref()
        .map(|value| !value.is_empty() && !value.starts_with('/'))
        .unwrap_or(false);

    // A Windows drive prefix is physical, not a one-letter logical host.
    #[cfg(windows)]
    if !forced_logical
        && s.as_bytes().get(1) == Some(&b':')
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        return parse_physical_namestring(s);
    }
    #[cfg(windows)]
    if !forced_logical && s.starts_with('\\') {
        return parse_physical_namestring(s);
    }
    if forced_logical || (!s.starts_with('/') && s.contains(':')) {
        parse_logical_namestring(s, host_string)
    } else {
        parse_physical_namestring(s)
    }
}

fn component_from_val(val: EgclVal, uppercase: bool) -> Option<ComponentSpec> {
    if val == NIL {
        return None;
    }
    if is_wild(val) {
        return Some(ComponentSpec::Wild);
    }
    // A NAME/TYPE component computed by string ops (SUBSEQ, SPLIT-NAME-TYPE, …)
    // may never enter the pathname registry, so registry-only lookup would
    // silently drop it and render an empty namestring.
    component_string(val).map(|s| {
        let text = if uppercase { s.to_uppercase() } else { s };
        if text == "*" {
            ComponentSpec::Wild
        } else {
            ComponentSpec::Literal(text)
        }
    })
}

fn directory_from_val(val: EgclVal, uppercase: bool) -> Result<Option<DirectorySpec>, EgclError> {
    if val == NIL {
        return Ok(None);
    }
    if let Some(s) = component_string(val) {
        if s.contains(';') || (!s.starts_with('/') && s.contains(':')) {
            return Ok(parse_logical_namestring(
                &format!("H:{}", s.trim_start_matches("H:")),
                Some("H".to_string()),
            )?
            .directory);
        }
        return Ok(parse_physical_namestring(&s)?.directory);
    }
    if is_wild(val) {
        // ANSI: `(make-pathname :directory :wild)` yields an absolute directory
        // with a single wild component — implementations pick `(:absolute
        // :wild-inferiors)` or `(:absolute :wild)`; make-pathname.5 accepts either.
        return Ok(Some(DirectorySpec {
            absolute: true,
            parts: vec![DirPart::WildInferiors],
        }));
    }
    if uppercase {
        return Err(EgclError::TypeError {
            datum: val,
            expected: "pathname directory".to_string(),
        });
    }
    Err(EgclError::TypeError {
        datum: val,
        expected: "pathname directory".to_string(),
    })
}

fn stringify_component(component: &Option<ComponentSpec>) -> EgclVal {
    match component {
        None => NIL,
        Some(ComponentSpec::Wild) => EgclVal::from_raw(keyword_hash("WILD")),
        Some(ComponentSpec::Literal(text)) => make_string_bv(text),
    }
}

fn stringify_directory(directory: &Option<DirectorySpec>, logical: bool) -> EgclVal {
    match directory {
        None => NIL,
        Some(dir) => make_string_bv(&render_directory(dir, logical)),
    }
}

fn render_directory(directory: &DirectorySpec, logical: bool) -> String {
    if logical {
        let mut out = String::new();
        if !directory.absolute {
            out.push(';');
        }
        for part in &directory.parts {
            match part {
                DirPart::Literal(text) => out.push_str(text),
                DirPart::Wild => out.push('*'),
                DirPart::WildInferiors => out.push_str("**"),
                DirPart::Up => out.push_str(".."),
            }
            out.push(';');
        }
        out
    } else {
        let mut out = String::new();
        if directory.absolute {
            out.push('/');
        }
        for part in &directory.parts {
            match part {
                DirPart::Literal(text) => out.push_str(text),
                DirPart::Wild => out.push('*'),
                DirPart::WildInferiors => out.push_str("**"),
                DirPart::Up => out.push_str(".."),
            }
            out.push('/');
        }
        out
    }
}

fn render_namestring_from_parsed(parsed: &ParsedPathname) -> String {
    if parsed.is_logical {
        let mut out = String::new();
        if let Some(host) = &parsed.host_name {
            out.push_str(host);
            out.push(':');
        }
        if let Some(dir) = &parsed.directory {
            out.push_str(&render_directory(dir, true));
        }
        if let Some(name) = &parsed.name {
            match name {
                ComponentSpec::Literal(text) => out.push_str(text),
                ComponentSpec::Wild => out.push('*'),
            }
        }
        if let Some(type_field) = &parsed.type_field {
            out.push('.');
            match type_field {
                ComponentSpec::Literal(text) => out.push_str(text),
                ComponentSpec::Wild => out.push('*'),
            }
        }
        out
    } else {
        let mut out = String::new();
        #[cfg(windows)]
        if let Some(device) = &parsed.device_name {
            if let Some(host) = &parsed.host_name {
                out.push_str("//");
                out.push_str(host);
                out.push('/');
                out.push_str(device);
            } else {
                out.push_str(device);
                out.push(':');
            }
        }
        if let Some(dir) = &parsed.directory {
            out.push_str(&render_directory(dir, false));
        }
        if let Some(name) = &parsed.name {
            match name {
                ComponentSpec::Literal(text) => out.push_str(text),
                ComponentSpec::Wild => out.push('*'),
            }
        }
        if let Some(type_field) = &parsed.type_field {
            out.push('.');
            match type_field {
                ComponentSpec::Literal(text) => out.push_str(text),
                ComponentSpec::Wild => out.push('*'),
            }
        }
        out
    }
}

fn normalize_record(
    host: EgclVal,
    device: EgclVal,
    directory: EgclVal,
    name: EgclVal,
    type_field: EgclVal,
    version: EgclVal,
) -> Result<PathnameRecord, EgclError> {
    let host_name = component_string(host);
    let is_logical = host_name
        .as_ref()
        .map(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(false);

    #[cfg(windows)]
    let is_logical = is_logical && component_string(device).is_none();
    let device_name = component_string(device).map(|s| {
        if cfg!(windows) && !is_logical && host_name.is_none() {
            s.to_ascii_uppercase()
        } else {
            s
        }
    });
    let directory_spec = directory_from_val(directory, is_logical)?;
    #[cfg(windows)]
    let directory_spec = if !is_logical && host_name.is_some() && device_name.is_some() {
        match directory_spec {
            Some(dir) if !dir.absolute => {
                return Err(EgclError::FileError(
                    "UNC pathname directory must be absolute".into(),
                ));
            }
            Some(dir) => Some(dir),
            None => Some(DirectorySpec {
                absolute: true,
                parts: Vec::new(),
            }),
        }
    } else {
        directory_spec
    };
    let parsed = ParsedPathname {
        device_name,
        is_logical,
        host_name: host_name
            .clone()
            .map(|s| if is_logical { s.to_uppercase() } else { s }),
        directory: directory_spec,
        name: component_from_val(name, is_logical),
        type_field: component_from_val(type_field, is_logical),
    };

    #[cfg(windows)]
    let device = parsed
        .device_name
        .as_deref()
        .map(make_string_bv)
        .unwrap_or(device);

    #[cfg(windows)]
    let directory = stringify_directory(&parsed.directory, is_logical);
    let namestring = Some(render_namestring_from_parsed(&parsed));

    Ok(PathnameRecord {
        host,
        device,
        directory,
        name,
        type_field,
        version,
        parsed,
        namestring,
    })
}

fn build_record_from_namestring(
    parsed: ParsedPathname,
    supplied_host: Option<EgclVal>,
) -> PathnameRecord {
    let host = if let Some(host) = supplied_host.filter(|host| *host != NIL) {
        host
    } else if let Some(host_name) = &parsed.host_name {
        make_string_bv(host_name)
    } else {
        NIL
    };
    let directory = stringify_directory(&parsed.directory, parsed.is_logical);
    let name = stringify_component(&parsed.name);
    let type_field = stringify_component(&parsed.type_field);
    let namestring = Some(render_namestring_from_parsed(&parsed));
    PathnameRecord {
        host,
        device: parsed
            .device_name
            .as_deref()
            .map(make_string_bv)
            .unwrap_or(NIL),
        directory,
        name,
        type_field,
        version: NIL,
        parsed,
        namestring,
    }
}

fn make_record_value(rec: PathnameRecord) -> EgclVal {
    let bv = alloc_pathname(rec.clone());
    if let Some(namestring) = rec.namestring {
        with_string_registry(|reg| {
            reg.insert(bv.0, namestring);
        });
    }
    bv
}

// ── Pathname operations ────────────────────────────────────────────

pub fn parse_namestring(
    thing: EgclVal,
    host: Option<EgclVal>,
    _default_pathname: Option<EgclVal>,
) -> Result<(EgclVal, usize), EgclError> {
    if let Some(existing) = get_record(thing) {
        return Ok((make_record_value(existing), 0));
    }

    if thing.tag() != TAG_HEAP_OBJECT {
        return Err(EgclError::TypeError {
            datum: thing,
            expected: "string or pathname".to_string(),
        });
    }

    // Accept an ordinary heap string not present in the pathname registry —
    // e.g. a namestring passed through a function call or built by FORMAT.
    let s = lookup_string(thing)
        .or_else(|| {
            if thing.is_string() {
                Some(thing.as_string())
            } else {
                None
            }
        })
        .ok_or_else(|| EgclError::TypeError {
            datum: thing,
            expected: "string or pathname".to_string(),
        })?;
    let parsed = parse_namestring_model(&s, host)?;
    let position = s.len();
    let pn = make_record_value(build_record_from_namestring(parsed, host));
    Ok((pn, position))
}

pub fn make_pathname(
    host: EgclVal,
    device: EgclVal,
    directory: EgclVal,
    name: EgclVal,
    type_field: EgclVal,
    version: EgclVal,
) -> Result<EgclVal, EgclError> {
    Ok(make_record_value(normalize_record(
        host, device, directory, name, type_field, version,
    )?))
}

pub fn merge_pathnames(
    pathname: EgclVal,
    default: EgclVal,
    default_version: EgclVal,
) -> Result<EgclVal, EgclError> {
    let primary = get_record(pathname).ok_or_else(|| EgclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let def = get_record(default).ok_or_else(|| EgclError::TypeError {
        datum: default,
        expected: "pathname".to_string(),
    })?;

    let inherit_host = !(cfg!(windows)
        && !primary.parsed.is_logical
        && primary.parsed.device_name.is_some()
        && primary.parsed.host_name.is_none());
    let host_name = primary.parsed.host_name.clone().or_else(|| {
        if inherit_host {
            def.parsed.host_name.clone()
        } else {
            None
        }
    });
    let device_name = primary
        .parsed
        .device_name
        .clone()
        .or_else(|| def.parsed.device_name.clone());
    let same_volume = !cfg!(windows)
        || primary.parsed.is_logical
        || ((primary.parsed.device_name.is_none()
            || primary
                .parsed
                .device_name
                .as_deref()
                .zip(def.parsed.device_name.as_deref())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)))
            && match (&host_name, &def.parsed.host_name) {
                (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                (None, None) => true,
                _ => false,
            });
    let default_dir = if same_volume {
        def.parsed.directory.as_ref()
    } else {
        None
    };
    let merged_dir = match (&primary.parsed.directory, default_dir) {
        (Some(dir), Some(default_dir)) if !dir.absolute => {
            let mut parts = default_dir.parts.clone();
            parts.extend(dir.parts.clone());
            Some(DirectorySpec {
                absolute: default_dir.absolute,
                parts,
            })
        }
        (Some(dir), _) => Some(dir.clone()),
        (None, Some(dir)) => Some(dir.clone()),
        (None, None) => None,
    };

    let name = if primary.name == NIL {
        def.name
    } else {
        primary.name
    };
    let version = if primary.name != NIL {
        if primary.version == NIL {
            default_version
        } else {
            primary.version
        }
    } else if primary.version != NIL {
        primary.version
    } else if def.version != NIL {
        def.version
    } else {
        default_version
    };

    let merged = PathnameRecord {
        host: if primary.host == NIL && inherit_host {
            def.host
        } else {
            primary.host
        },
        device: if primary.device == NIL {
            def.device
        } else {
            primary.device
        },
        directory: stringify_directory(
            &merged_dir,
            primary.parsed.is_logical || def.parsed.is_logical,
        ),
        name,
        type_field: if primary.type_field == NIL {
            def.type_field
        } else {
            primary.type_field
        },
        version,
        parsed: ParsedPathname {
            is_logical: primary.parsed.is_logical || def.parsed.is_logical,
            host_name,
            device_name,
            directory: merged_dir,
            name: primary
                .parsed
                .name
                .clone()
                .or_else(|| def.parsed.name.clone()),
            type_field: primary
                .parsed
                .type_field
                .clone()
                .or_else(|| def.parsed.type_field.clone()),
        },
        namestring: None,
    };

    Ok(make_record_value(merged))
}

pub fn namestring(pathname: EgclVal) -> Result<EgclVal, EgclError> {
    let rec = get_record(pathname).ok_or_else(|| EgclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let rendered = rec
        .namestring
        .unwrap_or_else(|| render_namestring_from_parsed(&rec.parsed));
    Ok(make_string_bv(&rendered))
}

/// The rendered namestring of a record, as borrowed or freshly-built Rust text.
/// Never allocates a `EgclVal`, so — unlike [`namestring`] — it cannot fire a
/// relocating minor GC (bliss-8jt).
fn record_namestring(rec: &PathnameRecord) -> std::borrow::Cow<'_, str> {
    match &rec.namestring {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned(render_namestring_from_parsed(&rec.parsed)),
    }
}

fn pathname_equal_key(rec: &PathnameRecord) -> std::borrow::Cow<'_, str> {
    let key = record_namestring(rec);
    if cfg!(windows) && !rec.parsed.is_logical {
        std::borrow::Cow::Owned(key.to_ascii_lowercase())
    } else {
        key
    }
}

/// CL `EQUAL` on two pathnames, comparing namestrings **without allocating on
/// the GC heap** — the GC-safe replacement for comparing the results of
/// [`namestring`] (which allocates a Lisp string and can relocate the nursery
/// mid-comparison). Non-pathname arguments compare unequal. This is the faithful
/// proxy ASDF relies on for its pervasive pathname-EQUAL caching (bliss-nad,
/// bliss-8jt).
pub fn pathnames_equal(a: EgclVal, b: EgclVal) -> bool {
    // Both records under ONE lock: with_pathname_store is a plain Mutex, so two
    // nested with_record calls would deadlock.
    with_pathname_store(|store| match (store.get(&a.0), store.get(&b.0)) {
        (Some(ra), Some(rb)) => pathname_equal_key(ra) == pathname_equal_key(rb),
        _ => false,
    })
}

/// Run `f` over the exact string [`pathnames_equal`] compares two pathnames by.
/// `None` when `val` is not a pathname.
///
/// This exists so EQUAL, EQUALP and SXHASH cannot disagree about pathnames.
/// They did: `pathnames_equal` compared namestrings while the hash table's
/// `equal_hash` had no pathname case at all and fell through to hashing the raw
/// pointer, so two EQUAL pathnames hashed differently and every EQUAL hash table
/// keyed by a pathname missed — which is exactly what ASDF's pervasive pathname
/// caching relies on (bliss-kssh, and bliss-nad before it for EQUAL itself).
///
/// Takes a closure rather than returning a `String` so the hot EQUAL path keeps
/// comparing a borrowed `Cow` without allocating on the Rust or GC heap.
pub fn with_pathname_equal_key<T>(val: EgclVal, f: impl FnOnce(&str) -> T) -> Option<T> {
    with_record(val, |rec| f(&pathname_equal_key(rec)))
}

pub fn pathname_host(pathname: EgclVal) -> EgclVal {
    with_record(pathname, |r| r.host).unwrap_or(NIL)
}

pub fn pathname_device(pathname: EgclVal) -> EgclVal {
    with_record(pathname, |r| r.device).unwrap_or(NIL)
}

pub fn pathname_directory(pathname: EgclVal) -> EgclVal {
    with_record(pathname, |r| r.directory).unwrap_or(NIL)
}

/// One component of an ANSI `pathname-directory` list.
pub enum PathDirComp {
    Name(String),
    Up,
    Wild,
    WildInferiors,
}

/// The ANSI `pathname-directory` value as `(absolute?, components)`, from which
/// the caller assembles the list `(:absolute|:relative comp…)` — each `comp` a
/// string or one of the keywords `:up` / `:wild` / `:wild-inferiors`. `None` when
/// the pathname has no directory component (`pathname-directory` → NIL). The
/// keywords are built by the caller so they are the interpreter's real keyword
/// symbols. Callers that treat the directory as a namestring (the old behaviour)
/// are wrong: UIOP and ANSI code do list arithmetic on it (bliss-lb6).
pub fn pathname_directory_components(pathname: EgclVal) -> Option<(bool, Vec<PathDirComp>)> {
    let rec = get_record(pathname)?;
    let dir = rec.parsed.directory.as_ref()?;
    let comps = dir
        .parts
        .iter()
        .map(|p| match p {
            DirPart::Literal(s) => PathDirComp::Name(s.clone()),
            DirPart::Up => PathDirComp::Up,
            DirPart::Wild => PathDirComp::Wild,
            DirPart::WildInferiors => PathDirComp::WildInferiors,
        })
        .collect();
    Some((dir.absolute, comps))
}

pub fn pathname_name(pathname: EgclVal) -> EgclVal {
    with_record(pathname, |r| r.name).unwrap_or(NIL)
}

pub fn pathname_type(pathname: EgclVal) -> EgclVal {
    with_record(pathname, |r| r.type_field).unwrap_or(NIL)
}

pub fn pathname_version(pathname: EgclVal) -> EgclVal {
    with_record(pathname, |r| r.version).unwrap_or(NIL)
}

fn match_glob(value: &str, pattern: &str, ignore_case: bool) -> Option<Vec<String>> {
    // ASCII folding preserves byte offsets, so wildcard captures retain the
    // original spelling (including any non-ASCII characters).
    let original = value;
    let folded_value;
    let folded_pattern;
    let (value, pattern) = if ignore_case {
        folded_value = value.to_ascii_lowercase();
        folded_pattern = pattern.to_ascii_lowercase();
        (folded_value.as_str(), folded_pattern.as_str())
    } else {
        (value, pattern)
    };
    if !pattern.contains('*') {
        return if value == pattern {
            Some(Vec::new())
        } else {
            None
        };
    }

    let pieces: Vec<&str> = pattern.split('*').collect();
    let starts_with_star = pattern.starts_with('*');
    let ends_with_star = pattern.ends_with('*');
    let mut captures = Vec::new();
    let mut cursor = 0usize;
    let mut first = true;

    for (idx, piece) in pieces.iter().enumerate() {
        if piece.is_empty() {
            continue;
        }
        if first && !starts_with_star {
            if !value[cursor..].starts_with(piece) {
                return None;
            }
            cursor += piece.len();
            first = false;
            continue;
        }
        let pos = value[cursor..].find(piece)?;
        captures.push(original[cursor..cursor + pos].to_string());
        cursor += pos + piece.len();
        first = false;
        if idx == pieces.len() - 1 && !ends_with_star && cursor != value.len() {
            return None;
        }
    }

    if ends_with_star {
        captures.push(original[cursor..].to_string());
        Some(captures)
    } else if cursor == value.len() {
        Some(captures)
    } else {
        None
    }
}

#[derive(Default, Clone)]
struct MatchCaptures {
    name: Option<String>,
    name_fragments: Vec<String>,
    type_field: Option<String>,
    type_fragments: Vec<String>,
    directory_wilds: Vec<String>,
    directory_inferiors: Vec<String>,
}

fn match_component(
    value: &Option<ComponentSpec>,
    pattern: &Option<ComponentSpec>,
    ignore_case: bool,
) -> Option<(Option<String>, Vec<String>)> {
    match pattern {
        None => {
            if value.is_none() {
                Some((None, Vec::new()))
            } else {
                None
            }
        }
        Some(ComponentSpec::Wild) => Some((
            match value {
                Some(ComponentSpec::Literal(text)) => Some(text.clone()),
                Some(ComponentSpec::Wild) => Some("*".to_string()),
                None => None,
            },
            Vec::new(),
        )),
        Some(ComponentSpec::Literal(pattern_text)) => match value {
            Some(ComponentSpec::Literal(text)) => {
                let fragments = match_glob(text, pattern_text, ignore_case)?;
                Some((Some(text.clone()), fragments))
            }
            Some(ComponentSpec::Wild) => match_glob("*", pattern_text, ignore_case)
                .map(|fragments| (Some("*".to_string()), fragments)),
            None => None,
        },
    }
}

fn match_directory_parts(
    value: &[DirPart],
    pattern: &[DirPart],
    captures: &mut MatchCaptures,
    ignore_case: bool,
) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }
    match &pattern[0] {
        DirPart::Wild => {
            if value.is_empty() {
                return false;
            }
            let capture = match &value[0] {
                DirPart::Literal(text) => text.clone(),
                DirPart::Up => "..".to_string(),
                DirPart::Wild => "*".to_string(),
                DirPart::WildInferiors => "**".to_string(),
            };
            captures.directory_wilds.push(capture);
            if match_directory_parts(&value[1..], &pattern[1..], captures, ignore_case) {
                return true;
            }
            captures.directory_wilds.pop();
            false
        }
        DirPart::WildInferiors => {
            for len in 0..=value.len() {
                let consumed = value[..len]
                    .iter()
                    .map(|part| match part {
                        DirPart::Literal(text) => text.clone(),
                        DirPart::Up => "..".to_string(),
                        DirPart::Wild => "*".to_string(),
                        DirPart::WildInferiors => "**".to_string(),
                    })
                    .collect::<Vec<_>>();
                captures.directory_inferiors.extend(consumed.clone());
                if match_directory_parts(&value[len..], &pattern[1..], captures, ignore_case) {
                    return true;
                }
                for _ in 0..consumed.len() {
                    captures.directory_inferiors.pop();
                }
            }
            false
        }
        DirPart::Literal(expected) => {
            if let Some(DirPart::Literal(actual)) = value.first() {
                (if ignore_case {
                    actual.eq_ignore_ascii_case(expected)
                } else {
                    actual == expected
                }) && match_directory_parts(&value[1..], &pattern[1..], captures, ignore_case)
            } else {
                false
            }
        }
        DirPart::Up => {
            matches!(value.first(), Some(DirPart::Up))
                && match_directory_parts(&value[1..], &pattern[1..], captures, ignore_case)
        }
    }
}

fn pathname_match_with_captures(
    pathname: &PathnameRecord,
    wildcard: &PathnameRecord,
) -> Option<MatchCaptures> {
    // Compare hosts by NAME, not by raw value: two logical pathnames with the
    // same host are separate interned strings (distinct EgclVal bits), so a
    // pointer comparison spuriously failed to match `CLTEST:FOO.LSP` against
    // `CLTEST:*.LSP` (ansi pathname-match-p.7/.8).
    if wildcard.host != NIL {
        let host_eq = match (&pathname.parsed.host_name, &wildcard.parsed.host_name) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => pathname.host == wildcard.host,
        };
        if !host_eq {
            return None;
        }
    }
    if wildcard.device != NIL && !is_wild(wildcard.device) {
        let device_eq = match (&pathname.parsed.device_name, &wildcard.parsed.device_name) {
            (Some(a), Some(b)) if cfg!(windows) && !pathname.parsed.is_logical => {
                a.eq_ignore_ascii_case(b)
            }
            (Some(a), Some(b)) => a == b,
            _ => pathname.device == wildcard.device,
        };
        if !device_eq {
            return None;
        }
    }
    // `:wild` matches any version; `:newest` is likewise permissive (egcl's
    // filesystem model carries no version numbers, and ASDF's `*wild-asd*`
    // pattern uses `:version :newest`, which must still match `foo.asd`).
    if wildcard.version != NIL
        && pathname.version != wildcard.version
        && !is_wild(wildcard.version)
        && !is_keyword(wildcard.version, "NEWEST")
    {
        return None;
    }

    let ignore_case = cfg!(windows) && !pathname.parsed.is_logical && !wildcard.parsed.is_logical;
    let mut captures = MatchCaptures::default();
    match (&pathname.parsed.directory, &wildcard.parsed.directory) {
        (_, None) => {}
        (Some(actual), Some(pattern)) if actual.absolute == pattern.absolute => {
            if !match_directory_parts(&actual.parts, &pattern.parts, &mut captures, ignore_case) {
                return None;
            }
        }
        (None, Some(pattern)) if pattern.parts.is_empty() => {}
        _ => return None,
    }

    let (name_capture, name_fragments) =
        match_component(&pathname.parsed.name, &wildcard.parsed.name, ignore_case)?;
    captures.name = name_capture;
    captures.name_fragments = name_fragments;

    let (type_capture, type_fragments) = match_component(
        &pathname.parsed.type_field,
        &wildcard.parsed.type_field,
        ignore_case,
    )?;
    captures.type_field = type_capture;
    captures.type_fragments = type_fragments;

    Some(captures)
}

pub fn pathname_match_p(pathname: EgclVal, wildcard: EgclVal) -> Result<bool, EgclError> {
    let pn = get_record(pathname).ok_or_else(|| EgclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let wc = get_record(wildcard).ok_or_else(|| EgclError::TypeError {
        datum: wildcard,
        expected: "pathname".to_string(),
    })?;
    Ok(pathname_match_with_captures(&pn, &wc).is_some())
}

fn component_is_wild(component: &Option<ComponentSpec>) -> bool {
    match component {
        Some(ComponentSpec::Wild) => true,
        Some(ComponentSpec::Literal(text)) => text.contains('*'),
        None => false,
    }
}

fn directory_is_wild(directory: &Option<DirectorySpec>) -> bool {
    directory
        .as_ref()
        .map(|dir| {
            dir.parts
                .iter()
                .any(|part| matches!(part, DirPart::Wild | DirPart::WildInferiors))
        })
        .unwrap_or(false)
}

pub fn wild_pathname_p(pathname: EgclVal, field: Option<EgclVal>) -> bool {
    let rec = match get_record(pathname) {
        Some(r) => r,
        None => return false,
    };
    match field {
        // No field, or an explicit NIL field designator, means "any component":
        // `(wild-pathname-p p nil)` is ANSI-equivalent to `(wild-pathname-p p)`.
        None => {
            directory_is_wild(&rec.parsed.directory)
                || component_is_wild(&rec.parsed.name)
                || component_is_wild(&rec.parsed.type_field)
                || is_wild(rec.version)
        }
        Some(field_kw) if field_kw == NIL => {
            directory_is_wild(&rec.parsed.directory)
                || component_is_wild(&rec.parsed.name)
                || component_is_wild(&rec.parsed.type_field)
                || is_wild(rec.version)
        }
        Some(field_kw) if is_keyword(field_kw, "DIRECTORY") => {
            directory_is_wild(&rec.parsed.directory)
        }
        Some(field_kw) if is_keyword(field_kw, "NAME") => component_is_wild(&rec.parsed.name),
        Some(field_kw) if is_keyword(field_kw, "TYPE") => component_is_wild(&rec.parsed.type_field),
        Some(field_kw) if is_keyword(field_kw, "VERSION") => is_wild(rec.version),
        Some(field_kw) if is_keyword(field_kw, "HOST") => false,
        Some(field_kw) if is_keyword(field_kw, "DEVICE") => false,
        _ => false,
    }
}

fn parse_translation_pairs(source: &str) -> Vec<(String, String)> {
    let mut quoted = Vec::new();
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            let mut value = String::new();
            let mut escape = false;
            for inner in chars.by_ref() {
                if escape {
                    value.push(inner);
                    escape = false;
                } else if inner == '\\' {
                    escape = true;
                } else if inner == '"' {
                    break;
                } else {
                    value.push(inner);
                }
            }
            quoted.push(value);
        }
    }
    quoted
        .chunks_exact(2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect()
}

fn apply_component_capture(
    target: &Option<ComponentSpec>,
    capture: Option<&String>,
    fragments: &[String],
) -> Option<ComponentSpec> {
    match target {
        None => None,
        Some(ComponentSpec::Wild) => capture.cloned().map(ComponentSpec::Literal),
        Some(ComponentSpec::Literal(pattern)) if pattern.contains('*') => {
            let mut out = String::new();
            let mut pieces = pattern.split('*');
            if let Some(first) = pieces.next() {
                out.push_str(first);
            }
            for (idx, piece) in pieces.enumerate() {
                if let Some(fragment) = fragments.get(idx) {
                    out.push_str(fragment);
                }
                out.push_str(piece);
            }
            Some(ComponentSpec::Literal(out))
        }
        Some(ComponentSpec::Literal(literal)) => Some(ComponentSpec::Literal(literal.clone())),
    }
}

fn apply_directory_capture(
    target: &Option<DirectorySpec>,
    captures: &MatchCaptures,
) -> Option<DirectorySpec> {
    target.as_ref().map(|dir| {
        let mut wild_idx = 0usize;
        let mut parts = Vec::new();
        for part in &dir.parts {
            match part {
                DirPart::Wild => {
                    if let Some(capture) = captures.directory_wilds.get(wild_idx) {
                        parts.push(DirPart::Literal(capture.clone()));
                    }
                    wild_idx += 1;
                }
                DirPart::WildInferiors => {
                    for capture in &captures.directory_inferiors {
                        parts.push(DirPart::Literal(capture.clone()));
                    }
                }
                DirPart::Literal(text) => parts.push(DirPart::Literal(text.clone())),
                DirPart::Up => parts.push(DirPart::Up),
            }
        }
        DirectorySpec {
            absolute: dir.absolute,
            parts,
        }
    })
}

fn translate_pathname_with_patterns(
    source: &PathnameRecord,
    from_pattern: &PathnameRecord,
    to_pattern: &PathnameRecord,
) -> Result<PathnameRecord, EgclError> {
    let captures = pathname_match_with_captures(source, from_pattern).ok_or_else(|| {
        EgclError::FileError("source pathname does not match translation".to_string())
    })?;
    let parsed = ParsedPathname {
        is_logical: to_pattern.parsed.is_logical,
        host_name: to_pattern.parsed.host_name.clone(),
        device_name: to_pattern.parsed.device_name.clone(),
        directory: apply_directory_capture(&to_pattern.parsed.directory, &captures),
        name: apply_component_capture(
            &to_pattern.parsed.name,
            captures.name.as_ref(),
            &captures.name_fragments,
        ),
        type_field: apply_component_capture(
            &to_pattern.parsed.type_field,
            captures.type_field.as_ref(),
            &captures.type_fragments,
        ),
    };
    Ok(build_record_from_namestring(
        parsed,
        if to_pattern.host == NIL {
            None
        } else {
            Some(to_pattern.host)
        },
    ))
}

/// TRANSLATE-PATHNAME: rewrite SOURCE (which must match FROM-WILDCARD) into the
/// shape of TO-WILDCARD, carrying wild-component captures across.
pub fn translate_pathname(
    source: EgclVal,
    from_wildcard: EgclVal,
    to_wildcard: EgclVal,
) -> Result<EgclVal, EgclError> {
    let get = |v: EgclVal| {
        get_record(v).ok_or(EgclError::TypeError {
            datum: v,
            expected: "pathname".to_string(),
        })
    };
    let translated =
        translate_pathname_with_patterns(&get(source)?, &get(from_wildcard)?, &get(to_wildcard)?)?;
    Ok(make_record_value(translated))
}

pub fn translate_logical_pathname(pathname: EgclVal) -> Result<EgclVal, EgclError> {
    let rec = get_record(pathname).ok_or_else(|| EgclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let host = rec
        .parsed
        .host_name
        .clone()
        .ok_or_else(|| EgclError::TypeError {
            datum: pathname,
            expected: "logical pathname".to_string(),
        })?;
    let translations_val =
        with_logical_translations(|map| map.get(&host).copied()).ok_or_else(|| {
            EgclError::FileError(format!("no translations for logical host {:?}", host))
        })?;
    let translations_src = component_string(translations_val).ok_or_else(|| {
        EgclError::FileError(format!(
            "logical host {:?} has unreadable translations",
            host
        ))
    })?;

    for (from, to) in parse_translation_pairs(&translations_src) {
        let from_host = make_string_bv(&host);
        let from_rec = build_record_from_namestring(
            parse_namestring_model(&from, Some(from_host))?,
            Some(from_host),
        );
        if pathname_match_with_captures(&rec, &from_rec).is_none() {
            continue;
        }
        let to_rec = build_record_from_namestring(parse_namestring_model(&to, None)?, None);
        let translated = translate_pathname_with_patterns(&rec, &from_rec, &to_rec)?;
        if translated.parsed.is_logical {
            let translated_val = make_record_value(translated);
            return translate_logical_pathname(translated_val);
        }
        return Ok(make_record_value(translated));
    }

    Err(EgclError::FileError(format!(
        "no matching translation for logical pathname {:?}",
        host
    )))
}

pub fn set_logical_pathname_translations(
    host: &str,
    translations: EgclVal,
) -> Result<(), EgclError> {
    install_pathname_global_root_scanner();
    with_logical_translations(|map| {
        map.insert(host.to_uppercase(), translations);
    });
    Ok(())
}

pub fn logical_pathname_translations(host: &str) -> Result<EgclVal, EgclError> {
    with_logical_translations(|map| map.get(&host.to_uppercase()).copied()).ok_or_else(|| {
        EgclError::FileError(format!(
            "no logical pathname translations for host {:?}",
            host
        ))
    })
}

fn resolve_relative_path(path_str: &str) -> String {
    let path = Path::new(path_str);
    if path.is_absolute() || path.exists() {
        return path_str.to_string();
    }
    let mut prefix = PathBuf::from("..");
    for _ in 0..5 {
        let candidate = prefix.join(path_str);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
        prefix = prefix.join("..");
    }
    path_str.to_string()
}

pub(crate) fn extract_path_string(val: EgclVal) -> Result<String, EgclError> {
    if let Some(s) = lookup_string(val) {
        return Ok(s);
    }
    if get_record(val).is_some() {
        let ns = namestring(val)?;
        return lookup_string(ns)
            .ok_or_else(|| EgclError::FileError("cannot render pathname".to_string()));
    }
    // Also accept an ordinary heap string (SIMPLE_BASE_STRING) — a namestring
    // that never entered the pathname string registry, e.g. a literal or one
    // built by FORMAT (mirrors parse_namestring). Safe: a pathname is a genuine
    // PATHNAME-typed heap object, so is_string() reads its real header and is
    // false for it.
    if val.is_string() {
        return Ok(val.as_string());
    }
    Err(EgclError::FileError(
        "cannot extract path string from value".to_string(),
    ))
}

fn pathname_from_fs_path(path: &Path) -> Result<EgclVal, EgclError> {
    let canon = path
        .canonicalize()
        .map_err(|e| EgclError::FileError(format!("{}: {}", path.display(), e)))?;
    let mut canon_str = canon.to_string_lossy().to_string();
    // std::fs emits verbatim drive paths (\\?\C:\...) on Windows. Keep
    // ordinary local canonical paths physical when converting back to Lisp.
    #[cfg(windows)]
    if let Some(drive_path) = canon_str.strip_prefix(r"\\?\") {
        if drive_path.as_bytes().get(1) == Some(&b':')
            && drive_path
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
        {
            canon_str = drive_path.to_owned();
        }
    }
    // A directory resolves to a directory pathname (trailing slash) so its final
    // component lands in the directory list and MERGE-PATHNAMES against it keeps
    // that component — e.g. (truename ".") must be ".../egcl/", not ".../egcl"
    // with name "egcl", or ASDF's `(:tree (merge-pathnames "ocicl/" ...))` walks
    // the wrong directory.
    if canon.is_dir() && !canon_str.ends_with('/') {
        canon_str.push('/');
    }
    let parsed = parse_namestring_model(&canon_str, None)?;
    Ok(make_record_value(build_record_from_namestring(
        parsed, None,
    )))
}

fn pathname_from_listed_path(path: &Path) -> Result<EgclVal, EgclError> {
    let mut path_str = path.to_string_lossy().to_string();
    // A directory entry becomes a directory pathname (trailing slash): its final
    // component then lands in the directory list, DIRECTORY-PATHNAME-P is true,
    // and `*/` wildcard matching works — needed by UIOP's SUBDIRECTORIES /
    // DIRECTORY-FILES and ASDF's source-registry tree walk (bliss-lb6.14).
    if path.is_dir() && !path_str.ends_with('/') {
        path_str.push('/');
    }
    let parsed = parse_namestring_model(&path_str, None)?;
    Ok(make_record_value(build_record_from_namestring(
        parsed, None,
    )))
}

pub fn probe_file(pathname: EgclVal) -> Result<Option<EgclVal>, EgclError> {
    let path_str = resolve_relative_path(&extract_path_string(pathname)?);
    let path = Path::new(&path_str);
    if path.exists() {
        pathname_from_fs_path(path).map(Some)
    } else {
        Ok(None)
    }
}

pub fn truename(pathname: EgclVal) -> Result<EgclVal, EgclError> {
    let path_str = resolve_relative_path(&extract_path_string(pathname)?);
    pathname_from_fs_path(Path::new(&path_str))
}

fn wildcard_root(parsed: &ParsedPathname) -> (PathBuf, Vec<DirPart>) {
    let mut prefix = parsed.clone();
    prefix.name = None;
    prefix.type_field = None;
    let mut remaining = Vec::new();
    if let Some(dir) = &mut prefix.directory {
        let first_wild = dir
            .parts
            .iter()
            .position(|part| matches!(part, DirPart::Wild | DirPart::WildInferiors))
            .unwrap_or(dir.parts.len());
        remaining = dir.parts.split_off(first_wild);
    }
    let rendered = render_namestring_from_parsed(&prefix);
    (
        PathBuf::from(if rendered.is_empty() { "." } else { &rendered }),
        remaining,
    )
}

fn collect_candidates(
    root: &Path,
    parts: &[DirPart],
    directory_only: bool,
    visited: &mut HashSet<(PathBuf, usize)>,
    out: &mut Vec<PathBuf>,
) -> Result<(), EgclError> {
    // A suffix length identifies the current position in this one pattern.
    // Multiple ** expansions must not scan the same path and suffix again.
    if !visited.insert((root.to_path_buf(), parts.len())) {
        return Ok(());
    }
    match std::fs::metadata(root) {
        Ok(metadata) if !metadata.is_dir() => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(EgclError::FileError(format!(
                "{}: {}",
                root.display(),
                error
            )));
        }
    }
    match parts.first() {
        Some(DirPart::Literal(name)) => {
            return collect_candidates(&root.join(name), &parts[1..], directory_only, visited, out);
        }
        Some(DirPart::Up) => {
            return collect_candidates(&root.join(".."), &parts[1..], directory_only, visited, out);
        }
        Some(DirPart::WildInferiors) => {
            // ** may consume zero directory levels, or retain itself while
            // descending. An ordinary * consumes exactly one level below.
            collect_candidates(root, &parts[1..], directory_only, visited, out)?;
        }
        None if directory_only => {
            out.push(root.to_path_buf());
            return Ok(());
        }
        _ => {}
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(EgclError::FileError(format!(
                "{}: {}",
                root.display(),
                error
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|e| EgclError::FileError(e.to_string()))?;
        let path = entry.path();
        if parts.is_empty() {
            out.push(path);
        } else if path.is_dir() {
            let remaining = if matches!(parts[0], DirPart::WildInferiors) {
                parts
            } else {
                &parts[1..]
            };
            collect_candidates(&path, remaining, directory_only, visited, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod directory_traversal_tests {
    use super::*;

    #[test]
    fn repeated_wild_inferiors_visit_each_candidate_once() {
        let root = std::env::temp_dir().join(format!(
            "egcl-directory-states-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("one/two")).unwrap();
        std::fs::write(root.join("one/two/distinfo.txt"), "fixture").unwrap();
        let mut candidates = Vec::new();
        let result = collect_candidates(
            &root,
            &[DirPart::WildInferiors, DirPart::WildInferiors],
            false,
            &mut HashSet::new(),
            &mut candidates,
        );
        std::fs::remove_dir_all(&root).unwrap();
        result.unwrap();
        candidates.sort();
        assert_eq!(
            candidates,
            vec![
                root.join("one"),
                root.join("one/two"),
                root.join("one/two/distinfo.txt")
            ]
        );
    }
}

pub fn directory(pathname: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let rec = if let Some(rec) = get_record(pathname) {
        rec
    } else {
        let source = extract_path_string(pathname)?;
        build_record_from_namestring(parse_namestring_model(&source, None)?, None)
    };

    if !wild_pathname_p(pathname, None) && !extract_path_string(pathname)?.contains('*') {
        let path_str = extract_path_string(pathname)?;
        let base = Path::new(&path_str);
        let root = if base.is_dir() {
            base.to_path_buf()
        } else {
            base.parent().unwrap_or(base).to_path_buf()
        };
        let mut result = Vec::new();
        for entry in std::fs::read_dir(&root)
            .map_err(|e| EgclError::FileError(format!("{}: {}", root.display(), e)))?
        {
            let entry = entry.map_err(|e| EgclError::FileError(e.to_string()))?;
            result.push(pathname_from_listed_path(&entry.path())?);
        }
        result.sort_by_key(|bv| lookup_string(namestring(*bv).unwrap()).unwrap_or_default());
        return Ok(result);
    }

    let (root, parts) = wildcard_root(&rec.parsed);
    let mut candidates = Vec::new();
    let directory_only = rec.parsed.name.is_none() && rec.parsed.type_field.is_none();
    collect_candidates(
        &root,
        &parts,
        directory_only,
        &mut HashSet::new(),
        &mut candidates,
    )?;
    candidates.sort();
    candidates.dedup();

    let mut result = Vec::new();
    for candidate in candidates {
        let mut candidate_str = candidate.to_string_lossy().to_string();
        // Mark directory candidates so a `*/` (wild directory) pattern matches
        // them and a `*.asd` (wild name) pattern does not (bliss-lb6.14).
        if candidate.is_dir() && !candidate_str.ends_with('/') {
            candidate_str.push('/');
        }
        let candidate_rec =
            build_record_from_namestring(parse_namestring_model(&candidate_str, None)?, None);
        if pathname_match_with_captures(&candidate_rec, &rec).is_some() {
            result.push(pathname_from_listed_path(&candidate)?);
        }
    }
    result.sort_by_key(|bv| lookup_string(namestring(*bv).unwrap()).unwrap_or_default());
    Ok(result)
}

pub fn ensure_directories_exist(pathname: EgclVal) -> Result<(EgclVal, bool), EgclError> {
    let path_str = extract_path_string(pathname)?;
    let path = Path::new(&path_str);
    // Path::parent discards a final directory even when a namestring ends in
    // a separator. Only file namestrings need that final component removed.
    let directory = if path_str.ends_with(std::path::is_separator) {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let already_exists = directory.is_dir();
    if !already_exists {
        std::fs::create_dir_all(directory)
            .map_err(|e| EgclError::FileError(format!("{}: {}", path_str, e)))?;
    }
    Ok((pathname, !already_exists))
}

pub fn delete_file(pathname: EgclVal) -> Result<(), EgclError> {
    let path_str = extract_path_string(pathname)?;
    std::fs::remove_file(&path_str)
        .map_err(|e| EgclError::FileError(format!("{}: {}", path_str, e)))
}

pub fn rename_file(
    filespec: EgclVal,
    new_name: EgclVal,
) -> Result<(EgclVal, EgclVal, EgclVal), EgclError> {
    let old_path_str = extract_path_string(filespec)?;
    let new_path_str = extract_path_string(new_name)?;
    let old_true = pathname_from_fs_path(Path::new(&old_path_str))?;
    std::fs::rename(&old_path_str, &new_path_str).map_err(|e| {
        EgclError::FileError(format!(
            "rename {} -> {}: {}",
            old_path_str, new_path_str, e
        ))
    })?;
    let new_true = pathname_from_fs_path(Path::new(&new_path_str))?;
    Ok((new_name, old_true, new_true))
}

// ── Core-image serialization of pathnames (bliss-x0f2 off-heap M3) ──────
//
// A pathname VALUE is a 16-byte `std::alloc` block (PATHNAME header) OFF the
// GC heap, with its real data in the off-heap PATHNAME_STORE side table —
// `walk_heap` never visits either, so a %save-core image used to carry only
// stale pointers: printing a restored *DEFAULT-PATHNAME-DEFAULTS* segfaulted
// reading the dead header, and asdf:find-system crashed in parse_namestring.
// These hooks ride the OffHeap section next to the hash-table bodies, with the
// same two-phase contract: `allocate` re-creates the header blocks and returns
// (old, new) pairs for the reloc-map fold; `populate` fills the store once the
// final remap exists. Component slots use the hash-table SlotRec convention —
// a raw tagged word (remapped) or, for an off-heap interned string, the string
// CONTENT, re-created via make_string_bv (which also repopulates the
// STRING_REGISTRY / STRING_REVERSE_REGISTRY caches).

enum PnSlot {
    Raw(u64),
    Str(String),
}

fn pn_put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn pn_put_str(out: &mut Vec<u8>, s: &str) {
    pn_put_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn pn_put_opt_str(out: &mut Vec<u8>, s: &Option<String>) {
    match s {
        Some(s) => {
            out.push(1);
            pn_put_str(out, s);
        }
        None => out.push(0),
    }
}

fn pn_put_slot(out: &mut Vec<u8>, v: EgclVal) {
    let off_heap_string =
        v.is_string() && !egcl_rt::gc::is_in_heap(unsafe { v.as_ptr() } as usize);
    if off_heap_string {
        out.push(1);
        pn_put_str(out, &v.as_string());
    } else {
        out.push(0);
        out.extend_from_slice(&v.0.to_le_bytes());
    }
}

fn pn_get_u32(data: &[u8], off: &mut usize) -> Option<u32> {
    if data.len() < *off + 4 {
        return None;
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Some(v)
}

fn pn_get_u64(data: &[u8], off: &mut usize) -> Option<u64> {
    if data.len() < *off + 8 {
        return None;
    }
    let v = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Some(v)
}

fn pn_get_str(data: &[u8], off: &mut usize) -> Option<String> {
    let len = pn_get_u32(data, off)? as usize;
    if data.len() < *off + len {
        return None;
    }
    let s = String::from_utf8_lossy(&data[*off..*off + len]).into_owned();
    *off += len;
    Some(s)
}

fn pn_get_opt_str(data: &[u8], off: &mut usize) -> Option<Option<String>> {
    if data.len() < *off + 1 {
        return None;
    }
    let tag = data[*off];
    *off += 1;
    if tag == 1 {
        pn_get_str(data, off).map(Some)
    } else {
        Some(None)
    }
}

fn pn_get_slot(data: &[u8], off: &mut usize) -> Option<PnSlot> {
    if data.len() < *off + 1 {
        return None;
    }
    let tag = data[*off];
    *off += 1;
    if tag == 1 {
        pn_get_str(data, off).map(PnSlot::Str)
    } else {
        pn_get_u64(data, off).map(PnSlot::Raw)
    }
}

fn pn_put_parsed(out: &mut Vec<u8>, p: &ParsedPathname) {
    out.push(p.is_logical as u8);
    pn_put_opt_str(out, &p.host_name);
    match &p.directory {
        Some(d) => {
            out.push(1);
            out.push(d.absolute as u8);
            pn_put_u32(out, d.parts.len() as u32);
            for part in &d.parts {
                match part {
                    DirPart::Literal(s) => {
                        out.push(0);
                        pn_put_str(out, s);
                    }
                    DirPart::Wild => out.push(1),
                    DirPart::WildInferiors => out.push(2),
                    DirPart::Up => out.push(3),
                }
            }
        }
        None => out.push(0),
    }
    for comp in [&p.name, &p.type_field] {
        match comp {
            Some(ComponentSpec::Literal(s)) => {
                out.push(1);
                pn_put_str(out, s);
            }
            Some(ComponentSpec::Wild) => out.push(2),
            None => out.push(0),
        }
    }
}

fn pn_get_parsed(data: &[u8], off: &mut usize) -> Option<ParsedPathname> {
    if data.len() < *off + 1 {
        return None;
    }
    let is_logical = data[*off] == 1;
    *off += 1;
    let host_name = pn_get_opt_str(data, off)?;
    if data.len() < *off + 1 {
        return None;
    }
    let has_dir = data[*off] == 1;
    *off += 1;
    let directory = if has_dir {
        if data.len() < *off + 1 {
            return None;
        }
        let absolute = data[*off] == 1;
        *off += 1;
        let n = pn_get_u32(data, off)? as usize;
        let mut parts = Vec::with_capacity(n);
        for _ in 0..n {
            if data.len() < *off + 1 {
                return None;
            }
            let tag = data[*off];
            *off += 1;
            parts.push(match tag {
                0 => DirPart::Literal(pn_get_str(data, off)?),
                1 => DirPart::Wild,
                2 => DirPart::WildInferiors,
                _ => DirPart::Up,
            });
        }
        Some(DirectorySpec { absolute, parts })
    } else {
        None
    };
    let mut comps: [Option<ComponentSpec>; 2] = [None, None];
    for c in comps.iter_mut() {
        if data.len() < *off + 1 {
            return None;
        }
        let tag = data[*off];
        *off += 1;
        *c = match tag {
            1 => Some(ComponentSpec::Literal(pn_get_str(data, off)?)),
            2 => Some(ComponentSpec::Wild),
            _ => None,
        };
    }
    let [name, type_field] = comps;
    Some(ParsedPathname {
        device_name: None,
        is_logical,
        host_name,
        directory,
        name,
        type_field,
    })
}

/// Serialize every live pathname for a core image. Reads raw tagged words and
/// off-heap record data only (no EGCL allocation) — GC-safe post-STW-GC.
pub fn serialize_pathnames() -> Vec<u8> {
    let mut out = Vec::new();
    let records: Vec<(u64, PathnameRecord)> = {
        let mut guard = PATHNAME_STORE.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_mut() {
            Some(store) => store.iter().map(|(k, v)| (*k, v.clone())).collect(),
            None => Vec::new(),
        }
    };
    pn_put_u32(&mut out, records.len() as u32);
    for (raw, rec) in records {
        let header = unsafe { EgclVal(raw).as_ptr() } as u64;
        out.extend_from_slice(&header.to_le_bytes());
        for comp in [
            rec.host,
            rec.device,
            rec.directory,
            rec.name,
            rec.type_field,
            rec.version,
        ] {
            pn_put_slot(&mut out, comp);
        }
        pn_put_opt_str(&mut out, &rec.namestring);
        pn_put_parsed(&mut out, &rec.parsed);
    }
    out
}

/// Pending (new_value, component slots, parsed, namestring) records between the
/// allocate and populate phases of a core restore.
type PendingPathname = (EgclVal, Vec<PnSlot>, ParsedPathname, Option<String>);
thread_local! {
    static PENDING_PATHNAMES: std::cell::RefCell<Vec<PendingPathname>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Phase 1: re-create each pathname's 16-byte header block (off the GC heap)
/// and return (old_header, new_header) pairs for the reloc-map fold. Component
/// remapping and store insertion wait for `populate_pathnames`. GC-safe: only
/// `std::alloc` allocation.
pub fn allocate_pathnames(data: &[u8]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut off = 0usize;
    let Some(n) = pn_get_u32(data, &mut off) else {
        return pairs;
    };
    for _ in 0..n {
        let Some(old_header) = pn_get_u64(data, &mut off) else {
            break;
        };
        let mut slots = Vec::with_capacity(6);
        let mut ok = true;
        for _ in 0..6 {
            match pn_get_slot(data, &mut off) {
                Some(s) => slots.push(s),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            break;
        }
        let Some(namestring) = pn_get_opt_str(data, &mut off) else {
            break;
        };
        let Some(parsed) = pn_get_parsed(data, &mut off) else {
            break;
        };
        let layout = std::alloc::Layout::from_size_align(16, 8).expect("pathname layout");
        let bv = unsafe {
            let ptr = std::alloc::alloc_zeroed(layout);
            *(ptr as *mut ObjectHeader) = ObjectHeader::new(type_id::PATHNAME, 2);
            EgclVal::from_heap_ptr(ptr)
        };
        let new_header = unsafe { bv.as_ptr() } as usize;
        PENDING_PATHNAMES.with(|p| p.borrow_mut().push((bv, slots, parsed, namestring)));
        pairs.push((old_header as usize, new_header));
    }
    pairs
}

// The initial Windows port stored the drive as directory component zero.
// Merged records have no cached namestring, so migrate the parsed form itself.
#[cfg(windows)]
fn restore_windows_volume(parsed: &mut ParsedPathname, namestring: Option<&str>) {
    if parsed.is_logical || parsed.device_name.is_some() {
        return;
    }
    if let Some(dir) = &mut parsed.directory {
        if dir.absolute {
            if let Some(DirPart::Literal(drive)) = dir.parts.first() {
                if drive.len() == 2
                    && drive.as_bytes()[0].is_ascii_alphabetic()
                    && drive.ends_with(':')
                {
                    parsed.device_name = Some(drive[..1].to_ascii_uppercase());
                    dir.parts.remove(0);
                    return;
                }
            }
        }
    }
    if let Some(text) = namestring {
        if let Ok(restored) = parse_physical_namestring(text) {
            *parsed = restored;
        }
    }
}

#[cfg(all(test, windows))]
mod windows_image_tests {
    use super::*;

    #[test]
    fn restores_legacy_merged_drive_without_cached_namestring() {
        // This is the parsed image payload emitted by the initial port for a
        // merged C:/Work/demo.lisp; its outer device slot and cache were NIL.
        let legacy = ParsedPathname {
            is_logical: false,
            host_name: None,
            device_name: None,
            directory: Some(DirectorySpec {
                absolute: true,
                parts: vec![
                    DirPart::Literal("C:".into()),
                    DirPart::Literal("Work".into()),
                ],
            }),
            name: Some(ComponentSpec::Literal("demo".into())),
            type_field: Some(ComponentSpec::Literal("lisp".into())),
        };
        let mut bytes = Vec::new();
        pn_put_parsed(&mut bytes, &legacy);
        let mut parsed = pn_get_parsed(&bytes, &mut 0).unwrap();
        restore_windows_volume(&mut parsed, None);
        assert_eq!(parsed.device_name.as_deref(), Some("C"));
        assert_eq!(parsed.directory.as_ref().unwrap().parts.len(), 1);
        assert_eq!(render_namestring_from_parsed(&parsed), "C:/Work/demo.lisp");
    }
}

/// Phase 2: resolve each pending record's component slots (raw words through
/// `remap`, off-heap strings re-created via `make_string_bv`, which also
/// repopulates the string registries) and insert the records into the
/// PATHNAME_STORE. GC-safe: `make_lisp_string` allocates off the GC heap only.
pub fn populate_pathnames(remap: &dyn Fn(u64) -> u64) {
    install_pathname_global_root_scanner();
    let pending = PENDING_PATHNAMES.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (bv, slots, mut parsed, namestring) in pending {
        let resolve = |slot: &PnSlot| match slot {
            PnSlot::Raw(raw) => EgclVal(remap(*raw)),
            PnSlot::Str(s) => make_string_bv(s),
        };
        let device = resolve(&slots[1]);
        parsed.device_name = component_string(device);
        #[cfg(windows)]
        restore_windows_volume(&mut parsed, namestring.as_deref());
        #[cfg(windows)]
        let directory = stringify_directory(&parsed.directory, parsed.is_logical);
        #[cfg(not(windows))]
        let directory = resolve(&slots[2]);
        let rec = PathnameRecord {
            host: resolve(&slots[0]),
            device: parsed
                .device_name
                .as_deref()
                .map(make_string_bv)
                .unwrap_or(device),
            directory,
            name: resolve(&slots[3]),
            type_field: resolve(&slots[4]),
            version: resolve(&slots[5]),
            parsed,
            namestring,
        };
        with_pathname_store(|store| {
            store.insert(bv.0, rec);
        });
    }
}
