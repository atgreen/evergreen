//! Pathnames and logical pathnames.
//!
//! See spec §5.7.

use torcl_rt::error::TorclError;
use torcl_rt::lock_order::{LockLevel, OrderedMutex};
use torcl_rt::object::{ObjectHeader, type_id};
use torcl_rt::value::{NIL, T, TAG_HEAP_OBJECT, TorclVal};

use std::collections::HashMap;
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
    directory: Option<DirectorySpec>,
    name: Option<ComponentSpec>,
    type_field: Option<ComponentSpec>,
}

#[derive(Clone)]
struct PathnameRecord {
    host: TorclVal,
    device: TorclVal,
    directory: TorclVal,
    name: TorclVal,
    type_field: TorclVal,
    version: TorclVal,
    parsed: ParsedPathname,
    namestring: Option<String>,
}

static PATHNAME_STORE: OrderedMutex<Option<HashMap<u64, PathnameRecord>>> =
    OrderedMutex::new(LockLevel::GcWorld, 9, "pathname GC roots", None);
static STRING_REGISTRY: OrderedMutex<Option<HashMap<u64, String>>> =
    OrderedMutex::new(LockLevel::GcWorld, 10, "pathname string registry", None);
static STRING_REVERSE_REGISTRY: OrderedMutex<Option<HashMap<String, TorclVal>>> = OrderedMutex::new(
    LockLevel::GcWorld,
    11,
    "pathname reverse string registry",
    None,
);
static LOGICAL_TRANSLATIONS: OrderedMutex<Option<HashMap<String, TorclVal>>> =
    OrderedMutex::new(LockLevel::GcWorld, 12, "logical pathname GC roots", None);

fn scan_pathname_global_roots(visit: &mut dyn FnMut(*mut TorclVal)) {
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
        let mut entries: Vec<(TorclVal, String)> = registry
            .drain()
            .map(|(bits, s)| (TorclVal(bits), s))
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
    INSTALL.call_once(|| torcl_rt::gc::register_root_scanner(scan_pathname_global_roots));
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
    F: FnOnce(&mut HashMap<String, TorclVal>) -> R,
{
    let mut guard = STRING_REVERSE_REGISTRY.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn with_logical_translations<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, TorclVal>) -> R,
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

fn make_string_bv(s: &str) -> TorclVal {
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

fn lookup_string(val: TorclVal) -> Option<String> {
    with_string_registry(|reg| reg.get(&val.0).cloned())
}

/// Read string content, preferring the registry cache before decoding a real
/// heap string (SIMPLE_BASE_STRING or SIMPLE_CHARACTER_STRING).
fn component_string(val: TorclVal) -> Option<String> {
    lookup_string(val).or_else(|| {
        if val.is_string() {
            Some(val.as_string())
        } else {
            None
        }
    })
}

pub fn register_string(val: TorclVal, s: &str) {
    ANY_REGISTERED_STRING.store(true, std::sync::atomic::Ordering::Relaxed);
    install_pathname_global_root_scanner();
    with_string_registry(|reg| {
        reg.insert(val.0, s.to_string());
    });
    with_string_reverse_registry(|rev| {
        rev.entry(s.to_string()).or_insert(val);
    });
}

pub fn registered_string(val: TorclVal) -> Option<String> {
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
pub fn is_registered_string(val: TorclVal) -> bool {
    if !any_registered_string() {
        return false;
    }
    with_string_registry(|reg| reg.contains_key(&val.0))
}

fn is_keyword(val: TorclVal, name: &str) -> bool {
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
        if let Some(sym) = torcl_rt::symbols::symbol_name(val.as_symbol_index()) {
            if let Some(bare) = sym.strip_prefix("KEYWORD:") {
                return bare.eq_ignore_ascii_case(name);
            }
        }
    }
    false
}

fn is_wild(val: TorclVal) -> bool {
    is_keyword(val, "WILD")
}

fn alloc_pathname(rec: PathnameRecord) -> TorclVal {
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
        TorclVal::from_heap_ptr(ptr)
    };
    with_pathname_store(|store| {
        store.insert(bv.0, rec);
    });
    bv
}

fn get_record(pathname: TorclVal) -> Option<PathnameRecord> {
    with_pathname_store(|store| store.get(&pathname.0).cloned())
}

/// Run `f` over a pathname's record WITHOUT cloning it.
///
/// [`get_record`] clones the whole `PathnameRecord` — six `TorclVal`s, a
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
fn with_record<T>(pathname: TorclVal, f: impl FnOnce(&PathnameRecord) -> T) -> Option<T> {
    with_pathname_store(|store| store.get(&pathname.0).map(f))
}

/// True if `val` is a pathname. Pathnames are real heap objects with the
/// PATHNAME type_id (bliss-lb6.9), so `is_string()` etc. read a genuine header
/// and are already false for them; this membership check identifies pathnames by
/// their component record without dereferencing, and is safe for any value.
pub fn is_pathname(val: TorclVal) -> bool {
    with_pathname_store(|store| store.contains_key(&val.0))
}

/// True if `val` is a *logical* pathname (its parse carries a logical host).
/// Used to wire `(typep x 'logical-pathname)` and `LOGICAL-PATHNAME`. Safe for
/// any value (a non-pathname simply has no record).
pub fn is_logical_pathname(val: TorclVal) -> bool {
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
pub fn logical_pathname_from_string(s: &str) -> Result<TorclVal, TorclError> {
    let bad = || TorclError::TypeError {
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

fn resolve_home_path(input: &str) -> Result<String, TorclError> {
    if let Some(rest) = input.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| TorclError::FileError("HOME is not set".to_string()))?;
        return Ok(format!("{}/{}", home.trim_end_matches('/'), rest));
    }
    if input == "~" {
        let home = std::env::var("HOME")
            .map_err(|_| TorclError::FileError("HOME is not set".to_string()))?;
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

fn canonicalize_dir_parts(parts: &[&str]) -> Result<Vec<DirPart>, TorclError> {
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

fn parse_physical_namestring(s: &str) -> Result<ParsedPathname, TorclError> {
    if s.is_empty() {
        return Ok(ParsedPathname {
            is_logical: false,
            host_name: None,
            directory: None,
            name: None,
            type_field: None,
        });
    }

    let expanded = resolve_home_path(s)?;
    let absolute = expanded.starts_with('/');
    let trailing_slash = expanded.ends_with('/');
    let tokens: Vec<&str> = expanded
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();

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
        host_name: None,
        directory,
        name,
        type_field,
    })
}

fn parse_logical_namestring(
    s: &str,
    forced_host: Option<String>,
) -> Result<ParsedPathname, TorclError> {
    let (host_name, rest) = if let Some(host) = forced_host {
        let remainder = s.split_once(':').map(|(_, tail)| tail).unwrap_or(s);
        (host.to_uppercase(), remainder)
    } else if let Some((host, tail)) = s.split_once(':') {
        (host.to_uppercase(), tail)
    } else {
        return Err(TorclError::FileError(format!(
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
        directory,
        name,
        type_field,
    })
}

fn parse_namestring_model(s: &str, host: Option<TorclVal>) -> Result<ParsedPathname, TorclError> {
    let host_string = host.and_then(lookup_string);
    let forced_logical = host_string
        .as_ref()
        .map(|value| !value.is_empty() && !value.starts_with('/'))
        .unwrap_or(false);

    if forced_logical || (!s.starts_with('/') && s.contains(':')) {
        parse_logical_namestring(s, host_string)
    } else {
        parse_physical_namestring(s)
    }
}

fn component_from_val(val: TorclVal, uppercase: bool) -> Option<ComponentSpec> {
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

fn directory_from_val(val: TorclVal, uppercase: bool) -> Result<Option<DirectorySpec>, TorclError> {
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
        return Err(TorclError::TypeError {
            datum: val,
            expected: "pathname directory".to_string(),
        });
    }
    Err(TorclError::TypeError {
        datum: val,
        expected: "pathname directory".to_string(),
    })
}

fn stringify_component(component: &Option<ComponentSpec>) -> TorclVal {
    match component {
        None => NIL,
        Some(ComponentSpec::Wild) => TorclVal::from_raw(keyword_hash("WILD")),
        Some(ComponentSpec::Literal(text)) => make_string_bv(text),
    }
}

fn stringify_directory(directory: &Option<DirectorySpec>, logical: bool) -> TorclVal {
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
    host: TorclVal,
    device: TorclVal,
    directory: TorclVal,
    name: TorclVal,
    type_field: TorclVal,
    version: TorclVal,
) -> Result<PathnameRecord, TorclError> {
    let host_name = lookup_string(host);
    let is_logical = host_name
        .as_ref()
        .map(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(false);

    let parsed = ParsedPathname {
        is_logical,
        host_name: host_name
            .clone()
            .map(|s| if is_logical { s.to_uppercase() } else { s }),
        directory: directory_from_val(directory, is_logical)?,
        name: component_from_val(name, is_logical),
        type_field: component_from_val(type_field, is_logical),
    };

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
    supplied_host: Option<TorclVal>,
) -> PathnameRecord {
    let host = if let Some(host) = supplied_host {
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
        device: NIL,
        directory,
        name,
        type_field,
        version: NIL,
        parsed,
        namestring,
    }
}

fn make_record_value(rec: PathnameRecord) -> TorclVal {
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
    thing: TorclVal,
    host: Option<TorclVal>,
    _default_pathname: Option<TorclVal>,
) -> Result<(TorclVal, usize), TorclError> {
    if let Some(existing) = get_record(thing) {
        return Ok((make_record_value(existing), 0));
    }

    if thing.tag() != TAG_HEAP_OBJECT {
        return Err(TorclError::TypeError {
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
        .ok_or_else(|| TorclError::TypeError {
            datum: thing,
            expected: "string or pathname".to_string(),
        })?;
    let parsed = parse_namestring_model(&s, host)?;
    let position = s.len();
    let pn = make_record_value(build_record_from_namestring(parsed, host));
    Ok((pn, position))
}

pub fn make_pathname(
    host: TorclVal,
    device: TorclVal,
    directory: TorclVal,
    name: TorclVal,
    type_field: TorclVal,
    version: TorclVal,
) -> Result<TorclVal, TorclError> {
    Ok(make_record_value(normalize_record(
        host, device, directory, name, type_field, version,
    )?))
}

pub fn merge_pathnames(
    pathname: TorclVal,
    default: TorclVal,
    default_version: TorclVal,
) -> Result<TorclVal, TorclError> {
    let primary = get_record(pathname).ok_or_else(|| TorclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let def = get_record(default).ok_or_else(|| TorclError::TypeError {
        datum: default,
        expected: "pathname".to_string(),
    })?;

    let merged_dir = match (&primary.parsed.directory, &def.parsed.directory) {
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
        host: if primary.host == NIL {
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
            host_name: primary
                .parsed
                .host_name
                .clone()
                .or_else(|| def.parsed.host_name.clone()),
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

pub fn namestring(pathname: TorclVal) -> Result<TorclVal, TorclError> {
    let rec = get_record(pathname).ok_or_else(|| TorclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let rendered = rec
        .namestring
        .unwrap_or_else(|| render_namestring_from_parsed(&rec.parsed));
    Ok(make_string_bv(&rendered))
}

/// The rendered namestring of a record, as borrowed or freshly-built Rust text.
/// Never allocates a `TorclVal`, so — unlike [`namestring`] — it cannot fire a
/// relocating minor GC (bliss-8jt).
fn record_namestring(rec: &PathnameRecord) -> std::borrow::Cow<'_, str> {
    match &rec.namestring {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned(render_namestring_from_parsed(&rec.parsed)),
    }
}

/// CL `EQUAL` on two pathnames, comparing namestrings **without allocating on
/// the GC heap** — the GC-safe replacement for comparing the results of
/// [`namestring`] (which allocates a Lisp string and can relocate the nursery
/// mid-comparison). Non-pathname arguments compare unequal. This is the faithful
/// proxy ASDF relies on for its pervasive pathname-EQUAL caching (bliss-nad,
/// bliss-8jt).
pub fn pathnames_equal(a: TorclVal, b: TorclVal) -> bool {
    // Both records under ONE lock: with_pathname_store is a plain Mutex, so two
    // nested with_record calls would deadlock.
    with_pathname_store(|store| match (store.get(&a.0), store.get(&b.0)) {
        (Some(ra), Some(rb)) => record_namestring(ra) == record_namestring(rb),
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
pub fn with_pathname_equal_key<T>(val: TorclVal, f: impl FnOnce(&str) -> T) -> Option<T> {
    with_record(val, |rec| f(&record_namestring(rec)))
}

pub fn pathname_host(pathname: TorclVal) -> TorclVal {
    with_record(pathname, |r| r.host).unwrap_or(NIL)
}

pub fn pathname_device(pathname: TorclVal) -> TorclVal {
    with_record(pathname, |r| r.device).unwrap_or(NIL)
}

pub fn pathname_directory(pathname: TorclVal) -> TorclVal {
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
pub fn pathname_directory_components(pathname: TorclVal) -> Option<(bool, Vec<PathDirComp>)> {
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

pub fn pathname_name(pathname: TorclVal) -> TorclVal {
    with_record(pathname, |r| r.name).unwrap_or(NIL)
}

pub fn pathname_type(pathname: TorclVal) -> TorclVal {
    with_record(pathname, |r| r.type_field).unwrap_or(NIL)
}

pub fn pathname_version(pathname: TorclVal) -> TorclVal {
    with_record(pathname, |r| r.version).unwrap_or(NIL)
}

fn match_glob(value: &str, pattern: &str) -> Option<Vec<String>> {
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
        captures.push(value[cursor..cursor + pos].to_string());
        cursor += pos + piece.len();
        first = false;
        if idx == pieces.len() - 1 && !ends_with_star && cursor != value.len() {
            return None;
        }
    }

    if ends_with_star {
        captures.push(value[cursor..].to_string());
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
                let fragments = match_glob(text, pattern_text)?;
                Some((Some(text.clone()), fragments))
            }
            Some(ComponentSpec::Wild) => {
                match_glob("*", pattern_text).map(|fragments| (Some("*".to_string()), fragments))
            }
            None => None,
        },
    }
}

fn match_directory_parts(
    value: &[DirPart],
    pattern: &[DirPart],
    captures: &mut MatchCaptures,
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
            if match_directory_parts(&value[1..], &pattern[1..], captures) {
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
                if match_directory_parts(&value[len..], &pattern[1..], captures) {
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
                actual == expected && match_directory_parts(&value[1..], &pattern[1..], captures)
            } else {
                false
            }
        }
        DirPart::Up => {
            matches!(value.first(), Some(DirPart::Up))
                && match_directory_parts(&value[1..], &pattern[1..], captures)
        }
    }
}

fn pathname_match_with_captures(
    pathname: &PathnameRecord,
    wildcard: &PathnameRecord,
) -> Option<MatchCaptures> {
    // Compare hosts by NAME, not by raw value: two logical pathnames with the
    // same host are separate interned strings (distinct TorclVal bits), so a
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
    if wildcard.device != NIL && pathname.device != wildcard.device {
        return None;
    }
    // `:wild` matches any version; `:newest` is likewise permissive (torcl's
    // filesystem model carries no version numbers, and ASDF's `*wild-asd*`
    // pattern uses `:version :newest`, which must still match `foo.asd`).
    if wildcard.version != NIL
        && pathname.version != wildcard.version
        && !is_wild(wildcard.version)
        && !is_keyword(wildcard.version, "NEWEST")
    {
        return None;
    }

    let mut captures = MatchCaptures::default();
    match (&pathname.parsed.directory, &wildcard.parsed.directory) {
        (_, None) => {}
        (Some(actual), Some(pattern)) if actual.absolute == pattern.absolute => {
            if !match_directory_parts(&actual.parts, &pattern.parts, &mut captures) {
                return None;
            }
        }
        (None, Some(pattern)) if pattern.parts.is_empty() => {}
        _ => return None,
    }

    let (name_capture, name_fragments) =
        match_component(&pathname.parsed.name, &wildcard.parsed.name)?;
    captures.name = name_capture;
    captures.name_fragments = name_fragments;

    let (type_capture, type_fragments) =
        match_component(&pathname.parsed.type_field, &wildcard.parsed.type_field)?;
    captures.type_field = type_capture;
    captures.type_fragments = type_fragments;

    Some(captures)
}

pub fn pathname_match_p(pathname: TorclVal, wildcard: TorclVal) -> Result<bool, TorclError> {
    let pn = get_record(pathname).ok_or_else(|| TorclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let wc = get_record(wildcard).ok_or_else(|| TorclError::TypeError {
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

pub fn wild_pathname_p(pathname: TorclVal, field: Option<TorclVal>) -> bool {
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
) -> Result<PathnameRecord, TorclError> {
    let captures = pathname_match_with_captures(source, from_pattern).ok_or_else(|| {
        TorclError::FileError("source pathname does not match translation".to_string())
    })?;
    let parsed = ParsedPathname {
        is_logical: to_pattern.parsed.is_logical,
        host_name: to_pattern.parsed.host_name.clone(),
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
    source: TorclVal,
    from_wildcard: TorclVal,
    to_wildcard: TorclVal,
) -> Result<TorclVal, TorclError> {
    let get = |v: TorclVal| {
        get_record(v).ok_or(TorclError::TypeError {
            datum: v,
            expected: "pathname".to_string(),
        })
    };
    let translated =
        translate_pathname_with_patterns(&get(source)?, &get(from_wildcard)?, &get(to_wildcard)?)?;
    Ok(make_record_value(translated))
}

pub fn translate_logical_pathname(pathname: TorclVal) -> Result<TorclVal, TorclError> {
    let rec = get_record(pathname).ok_or_else(|| TorclError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let host = rec
        .parsed
        .host_name
        .clone()
        .ok_or_else(|| TorclError::TypeError {
            datum: pathname,
            expected: "logical pathname".to_string(),
        })?;
    let translations_val =
        with_logical_translations(|map| map.get(&host).copied()).ok_or_else(|| {
            TorclError::FileError(format!("no translations for logical host {:?}", host))
        })?;
    let translations_src = lookup_string(translations_val).ok_or_else(|| {
        TorclError::FileError(format!(
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

    Err(TorclError::FileError(format!(
        "no matching translation for logical pathname {:?}",
        host
    )))
}

pub fn set_logical_pathname_translations(
    host: &str,
    translations: TorclVal,
) -> Result<(), TorclError> {
    install_pathname_global_root_scanner();
    with_logical_translations(|map| {
        map.insert(host.to_uppercase(), translations);
    });
    Ok(())
}

pub fn logical_pathname_translations(host: &str) -> Result<TorclVal, TorclError> {
    with_logical_translations(|map| map.get(&host.to_uppercase()).copied()).ok_or_else(|| {
        TorclError::FileError(format!(
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

pub(crate) fn extract_path_string(val: TorclVal) -> Result<String, TorclError> {
    if let Some(s) = lookup_string(val) {
        return Ok(s);
    }
    if get_record(val).is_some() {
        let ns = namestring(val)?;
        return lookup_string(ns)
            .ok_or_else(|| TorclError::FileError("cannot render pathname".to_string()));
    }
    // Also accept an ordinary heap string (SIMPLE_BASE_STRING) — a namestring
    // that never entered the pathname string registry, e.g. a literal or one
    // built by FORMAT (mirrors parse_namestring). Safe: a pathname is a genuine
    // PATHNAME-typed heap object, so is_string() reads its real header and is
    // false for it.
    if val.is_string() {
        return Ok(val.as_string());
    }
    Err(TorclError::FileError(
        "cannot extract path string from value".to_string(),
    ))
}

fn pathname_from_fs_path(path: &Path) -> Result<TorclVal, TorclError> {
    let canon = path
        .canonicalize()
        .map_err(|e| TorclError::FileError(format!("{}: {}", path.display(), e)))?;
    let mut canon_str = canon.to_string_lossy().to_string();
    // A directory resolves to a directory pathname (trailing slash) so its final
    // component lands in the directory list and MERGE-PATHNAMES against it keeps
    // that component — e.g. (truename ".") must be ".../torcl/", not ".../torcl"
    // with name "torcl", or ASDF's `(:tree (merge-pathnames "ocicl/" ...))` walks
    // the wrong directory.
    if canon.is_dir() && !canon_str.ends_with('/') {
        canon_str.push('/');
    }
    let parsed = parse_namestring_model(&canon_str, None)?;
    Ok(make_record_value(build_record_from_namestring(
        parsed, None,
    )))
}

fn pathname_from_listed_path(path: &Path) -> Result<TorclVal, TorclError> {
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

pub fn probe_file(pathname: TorclVal) -> Result<Option<TorclVal>, TorclError> {
    let path_str = resolve_relative_path(&extract_path_string(pathname)?);
    let path = Path::new(&path_str);
    if path.exists() {
        pathname_from_fs_path(path).map(Some)
    } else {
        Ok(None)
    }
}

pub fn truename(pathname: TorclVal) -> Result<TorclVal, TorclError> {
    let path_str = resolve_relative_path(&extract_path_string(pathname)?);
    pathname_from_fs_path(Path::new(&path_str))
}

fn wildcard_root(parsed: &ParsedPathname) -> PathBuf {
    let mut root = if parsed
        .directory
        .as_ref()
        .map(|d| d.absolute)
        .unwrap_or(false)
    {
        PathBuf::from("/")
    } else {
        PathBuf::from(".")
    };
    if let Some(dir) = &parsed.directory {
        for part in &dir.parts {
            match part {
                DirPart::Literal(text) => root.push(text),
                DirPart::Up => root.push(".."),
                DirPart::Wild | DirPart::WildInferiors => break,
            }
        }
    }
    root
}

fn collect_candidates(
    root: &Path,
    recursive: bool,
    out: &mut Vec<PathBuf>,
) -> Result<(), TorclError> {
    let entries = std::fs::read_dir(root)
        .map_err(|e| TorclError::FileError(format!("{}: {}", root.display(), e)))?;
    for entry in entries {
        let entry = entry.map_err(|e| TorclError::FileError(e.to_string()))?;
        let path = entry.path();
        out.push(path.clone());
        if recursive && path.is_dir() {
            collect_candidates(&path, true, out)?;
        }
    }
    Ok(())
}

pub fn directory(pathname: TorclVal) -> Result<Vec<TorclVal>, TorclError> {
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
            .map_err(|e| TorclError::FileError(format!("{}: {}", root.display(), e)))?
        {
            let entry = entry.map_err(|e| TorclError::FileError(e.to_string()))?;
            result.push(pathname_from_listed_path(&entry.path())?);
        }
        result.sort_by_key(|bv| lookup_string(namestring(*bv).unwrap()).unwrap_or_default());
        return Ok(result);
    }

    let recursive = rec
        .parsed
        .directory
        .as_ref()
        .map(|dir| {
            dir.parts
                .iter()
                .any(|part| matches!(part, DirPart::WildInferiors))
        })
        .unwrap_or(false);
    let root = wildcard_root(&rec.parsed);
    let mut candidates = Vec::new();
    collect_candidates(&root, recursive, &mut candidates)?;

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

pub fn ensure_directories_exist(pathname: TorclVal) -> Result<(TorclVal, bool), TorclError> {
    let path_str = extract_path_string(pathname)?;
    let path = Path::new(&path_str);
    let parent = path.parent().unwrap_or(path);
    let already_exists = parent.is_dir();
    if !already_exists {
        std::fs::create_dir_all(parent)
            .map_err(|e| TorclError::FileError(format!("{}: {}", path_str, e)))?;
    }
    Ok((pathname, !already_exists))
}

pub fn delete_file(pathname: TorclVal) -> Result<(), TorclError> {
    let path_str = extract_path_string(pathname)?;
    std::fs::remove_file(&path_str)
        .map_err(|e| TorclError::FileError(format!("{}: {}", path_str, e)))
}

pub fn rename_file(
    filespec: TorclVal,
    new_name: TorclVal,
) -> Result<(TorclVal, TorclVal, TorclVal), TorclError> {
    let old_path_str = extract_path_string(filespec)?;
    let new_path_str = extract_path_string(new_name)?;
    let old_true = pathname_from_fs_path(Path::new(&old_path_str))?;
    std::fs::rename(&old_path_str, &new_path_str).map_err(|e| {
        TorclError::FileError(format!(
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

fn pn_put_slot(out: &mut Vec<u8>, v: TorclVal) {
    let off_heap_string =
        v.is_string() && !torcl_rt::gc::is_in_heap(unsafe { v.as_ptr() } as usize);
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
        is_logical,
        host_name,
        directory,
        name,
        type_field,
    })
}

/// Serialize every live pathname for a core image. Reads raw tagged words and
/// off-heap record data only (no TorCL allocation) — GC-safe post-STW-GC.
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
        let header = unsafe { TorclVal(raw).as_ptr() } as u64;
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
type PendingPathname = (TorclVal, Vec<PnSlot>, ParsedPathname, Option<String>);
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
            TorclVal::from_heap_ptr(ptr)
        };
        let new_header = unsafe { bv.as_ptr() } as usize;
        PENDING_PATHNAMES.with(|p| p.borrow_mut().push((bv, slots, parsed, namestring)));
        pairs.push((old_header as usize, new_header));
    }
    pairs
}

/// Phase 2: resolve each pending record's component slots (raw words through
/// `remap`, off-heap strings re-created via `make_string_bv`, which also
/// repopulates the string registries) and insert the records into the
/// PATHNAME_STORE. GC-safe: `make_lisp_string` allocates off the GC heap only.
pub fn populate_pathnames(remap: &dyn Fn(u64) -> u64) {
    install_pathname_global_root_scanner();
    let pending = PENDING_PATHNAMES.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (bv, slots, parsed, namestring) in pending {
        let resolve = |slot: &PnSlot| match slot {
            PnSlot::Raw(raw) => TorclVal(remap(*raw)),
            PnSlot::Str(s) => make_string_bv(s),
        };
        let rec = PathnameRecord {
            host: resolve(&slots[0]),
            device: resolve(&slots[1]),
            directory: resolve(&slots[2]),
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
