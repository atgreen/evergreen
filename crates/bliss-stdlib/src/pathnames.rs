//! Pathnames and logical pathnames.
//!
//! See spec §5.7.

use bliss_rt::error::BlissError;
use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, TAG_HEAP_OBJECT};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    host: BlissVal,
    device: BlissVal,
    directory: BlissVal,
    name: BlissVal,
    type_field: BlissVal,
    version: BlissVal,
    parsed: ParsedPathname,
    namestring: Option<String>,
}

static PATHNAME_STORE: Mutex<Option<HashMap<u64, PathnameRecord>>> = Mutex::new(None);
static STRING_REGISTRY: Mutex<Option<HashMap<u64, String>>> = Mutex::new(None);
static STRING_REVERSE_REGISTRY: Mutex<Option<HashMap<String, BlissVal>>> = Mutex::new(None);
static LOGICAL_TRANSLATIONS: Mutex<Option<HashMap<String, BlissVal>>> = Mutex::new(None);

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
    F: FnOnce(&mut HashMap<String, BlissVal>) -> R,
{
    let mut guard = STRING_REVERSE_REGISTRY.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

fn with_logical_translations<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, BlissVal>) -> R,
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

fn string_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & !0b111) | 0b010
}

fn make_string_bv(s: &str) -> BlissVal {
    let existing = with_string_reverse_registry(|rev| rev.get(s).copied());
    if let Some(bv) = existing {
        return bv;
    }
    let bv = BlissVal::from_raw(string_hash(s));
    with_string_registry(|reg| {
        reg.insert(bv.0, s.to_string());
    });
    with_string_reverse_registry(|rev| {
        rev.insert(s.to_string(), bv);
    });
    bv
}

fn lookup_string(val: BlissVal) -> Option<String> {
    with_string_registry(|reg| reg.get(&val.0).cloned())
}

pub fn register_string(val: BlissVal, s: &str) {
    with_string_registry(|reg| {
        reg.insert(val.0, s.to_string());
    });
    with_string_reverse_registry(|rev| {
        rev.entry(s.to_string()).or_insert(val);
    });
}

pub fn registered_string(val: BlissVal) -> Option<String> {
    lookup_string(val)
}

fn is_keyword(val: BlissVal, name: &str) -> bool {
    val.0 == keyword_hash(name)
}

fn is_wild(val: BlissVal) -> bool {
    is_keyword(val, "WILD")
}

fn alloc_pathname(rec: PathnameRecord) -> BlissVal {
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
        BlissVal::from_heap_ptr(ptr)
    };
    with_pathname_store(|store| {
        store.insert(bv.0, rec);
    });
    bv
}

fn get_record(pathname: BlissVal) -> Option<PathnameRecord> {
    with_pathname_store(|store| store.get(&pathname.0).cloned())
}

/// True if `val` is a pathname. Pathnames are real heap objects with the
/// PATHNAME type_id (bliss-lb6.9), so `is_string()` etc. read a genuine header
/// and are already false for them; this membership check identifies pathnames by
/// their component record without dereferencing, and is safe for any value.
pub fn is_pathname(val: BlissVal) -> bool {
    with_pathname_store(|store| store.contains_key(&val.0))
}

fn nil_if_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn resolve_home_path(input: &str) -> Result<String, BlissError> {
    if let Some(rest) = input.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .map_err(|_| BlissError::FileError("HOME is not set".to_string()))?;
        return Ok(format!("{}/{}", home.trim_end_matches('/'), rest));
    }
    if input == "~" {
        let home = std::env::var("HOME")
            .map_err(|_| BlissError::FileError("HOME is not set".to_string()))?;
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

fn canonicalize_dir_parts(parts: &[&str]) -> Result<Vec<DirPart>, BlissError> {
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

fn parse_physical_namestring(s: &str) -> Result<ParsedPathname, BlissError> {
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
) -> Result<ParsedPathname, BlissError> {
    let (host_name, rest) = if let Some(host) = forced_host {
        let remainder = s.split_once(':').map(|(_, tail)| tail).unwrap_or(s);
        (host.to_uppercase(), remainder)
    } else if let Some((host, tail)) = s.split_once(':') {
        (host.to_uppercase(), tail)
    } else {
        return Err(BlissError::FileError(format!(
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

fn parse_namestring_model(s: &str, host: Option<BlissVal>) -> Result<ParsedPathname, BlissError> {
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

fn component_from_val(val: BlissVal, uppercase: bool) -> Option<ComponentSpec> {
    if val == NIL {
        return None;
    }
    if is_wild(val) {
        return Some(ComponentSpec::Wild);
    }
    lookup_string(val).map(|s| {
        let text = if uppercase { s.to_uppercase() } else { s };
        if text == "*" {
            ComponentSpec::Wild
        } else {
            ComponentSpec::Literal(text)
        }
    })
}

fn directory_from_val(val: BlissVal, uppercase: bool) -> Result<Option<DirectorySpec>, BlissError> {
    if val == NIL {
        return Ok(None);
    }
    if let Some(s) = lookup_string(val) {
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
        return Ok(Some(DirectorySpec {
            absolute: false,
            parts: vec![DirPart::Wild],
        }));
    }
    if uppercase {
        return Err(BlissError::TypeError {
            datum: val,
            expected: "pathname directory".to_string(),
        });
    }
    Err(BlissError::TypeError {
        datum: val,
        expected: "pathname directory".to_string(),
    })
}

fn stringify_component(component: &Option<ComponentSpec>) -> BlissVal {
    match component {
        None => NIL,
        Some(ComponentSpec::Wild) => BlissVal::from_raw(keyword_hash("WILD")),
        Some(ComponentSpec::Literal(text)) => make_string_bv(text),
    }
}

fn stringify_directory(directory: &Option<DirectorySpec>, logical: bool) -> BlissVal {
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
    host: BlissVal,
    device: BlissVal,
    directory: BlissVal,
    name: BlissVal,
    type_field: BlissVal,
    version: BlissVal,
) -> Result<PathnameRecord, BlissError> {
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
    supplied_host: Option<BlissVal>,
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

fn make_record_value(rec: PathnameRecord) -> BlissVal {
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
    thing: BlissVal,
    host: Option<BlissVal>,
    _default_pathname: Option<BlissVal>,
) -> Result<(BlissVal, usize), BlissError> {
    if let Some(existing) = get_record(thing) {
        return Ok((make_record_value(existing), 0));
    }

    if thing.tag() != TAG_HEAP_OBJECT {
        return Err(BlissError::TypeError {
            datum: thing,
            expected: "string or pathname".to_string(),
        });
    }

    // Accept both registry-backed string sentinels and ordinary heap strings
    // (SIMPLE_BASE_STRING) — e.g. a namestring passed through a function call or
    // built by FORMAT, which is not in the pathname string registry (bliss-lb6).
    let s = lookup_string(thing)
        .or_else(|| {
            if thing.is_string() {
                Some(thing.as_string())
            } else {
                None
            }
        })
        .ok_or_else(|| BlissError::TypeError {
            datum: thing,
            expected: "string or pathname".to_string(),
        })?;
    let parsed = parse_namestring_model(&s, host)?;
    let position = s.len();
    let pn = make_record_value(build_record_from_namestring(parsed, host));
    Ok((pn, position))
}

pub fn make_pathname(
    host: BlissVal,
    device: BlissVal,
    directory: BlissVal,
    name: BlissVal,
    type_field: BlissVal,
    version: BlissVal,
) -> Result<BlissVal, BlissError> {
    Ok(make_record_value(normalize_record(
        host, device, directory, name, type_field, version,
    )?))
}

pub fn merge_pathnames(
    pathname: BlissVal,
    default: BlissVal,
    default_version: BlissVal,
) -> Result<BlissVal, BlissError> {
    let primary = get_record(pathname).ok_or_else(|| BlissError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let def = get_record(default).ok_or_else(|| BlissError::TypeError {
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

pub fn namestring(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    let rec = get_record(pathname).ok_or_else(|| BlissError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let rendered = rec
        .namestring
        .unwrap_or_else(|| render_namestring_from_parsed(&rec.parsed));
    Ok(make_string_bv(&rendered))
}

pub fn pathname_host(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.host)
}

pub fn pathname_device(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.device)
}

pub fn pathname_directory(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.directory)
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
pub fn pathname_directory_components(pathname: BlissVal) -> Option<(bool, Vec<PathDirComp>)> {
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

pub fn pathname_name(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.name)
}

pub fn pathname_type(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.type_field)
}

pub fn pathname_version(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.version)
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
    if wildcard.host != NIL && pathname.host != wildcard.host {
        return None;
    }
    if wildcard.device != NIL && pathname.device != wildcard.device {
        return None;
    }
    if wildcard.version != NIL && pathname.version != wildcard.version && !is_wild(wildcard.version)
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

pub fn pathname_match_p(pathname: BlissVal, wildcard: BlissVal) -> Result<bool, BlissError> {
    let pn = get_record(pathname).ok_or_else(|| BlissError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let wc = get_record(wildcard).ok_or_else(|| BlissError::TypeError {
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

pub fn wild_pathname_p(pathname: BlissVal, field: Option<BlissVal>) -> bool {
    let rec = match get_record(pathname) {
        Some(r) => r,
        None => return false,
    };
    match field {
        None => {
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
) -> Result<PathnameRecord, BlissError> {
    let captures = pathname_match_with_captures(source, from_pattern).ok_or_else(|| {
        BlissError::FileError("source pathname does not match translation".to_string())
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

pub fn translate_logical_pathname(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    let rec = get_record(pathname).ok_or_else(|| BlissError::TypeError {
        datum: pathname,
        expected: "pathname".to_string(),
    })?;
    let host = rec
        .parsed
        .host_name
        .clone()
        .ok_or_else(|| BlissError::TypeError {
            datum: pathname,
            expected: "logical pathname".to_string(),
        })?;
    let translations_val =
        with_logical_translations(|map| map.get(&host).copied()).ok_or_else(|| {
            BlissError::FileError(format!("no translations for logical host {:?}", host))
        })?;
    let translations_src = lookup_string(translations_val).ok_or_else(|| {
        BlissError::FileError(format!(
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

    Err(BlissError::FileError(format!(
        "no matching translation for logical pathname {:?}",
        host
    )))
}

pub fn set_logical_pathname_translations(
    host: &str,
    translations: BlissVal,
) -> Result<(), BlissError> {
    with_logical_translations(|map| {
        map.insert(host.to_uppercase(), translations);
    });
    Ok(())
}

pub fn logical_pathname_translations(host: &str) -> Result<BlissVal, BlissError> {
    with_logical_translations(|map| map.get(&host.to_uppercase()).copied()).ok_or_else(|| {
        BlissError::FileError(format!(
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

pub(crate) fn extract_path_string(val: BlissVal) -> Result<String, BlissError> {
    if let Some(s) = lookup_string(val) {
        return Ok(s);
    }
    if get_record(val).is_some() {
        let ns = namestring(val)?;
        return lookup_string(ns)
            .ok_or_else(|| BlissError::FileError("cannot render pathname".to_string()));
    }
    Err(BlissError::FileError(
        "cannot extract path string from value".to_string(),
    ))
}

fn pathname_from_fs_path(path: &Path) -> Result<BlissVal, BlissError> {
    let canon = path
        .canonicalize()
        .map_err(|e| BlissError::FileError(format!("{}: {}", path.display(), e)))?;
    let canon_str = canon.to_string_lossy().to_string();
    let parsed = parse_namestring_model(&canon_str, None)?;
    Ok(make_record_value(build_record_from_namestring(
        parsed, None,
    )))
}

fn pathname_from_listed_path(path: &Path) -> Result<BlissVal, BlissError> {
    let path_str = path.to_string_lossy().to_string();
    let parsed = parse_namestring_model(&path_str, None)?;
    Ok(make_record_value(build_record_from_namestring(
        parsed, None,
    )))
}

pub fn probe_file(pathname: BlissVal) -> Result<Option<BlissVal>, BlissError> {
    let path_str = resolve_relative_path(&extract_path_string(pathname)?);
    let path = Path::new(&path_str);
    if path.exists() {
        pathname_from_fs_path(path).map(Some)
    } else {
        Ok(None)
    }
}

pub fn truename(pathname: BlissVal) -> Result<BlissVal, BlissError> {
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
) -> Result<(), BlissError> {
    let entries = std::fs::read_dir(root)
        .map_err(|e| BlissError::FileError(format!("{}: {}", root.display(), e)))?;
    for entry in entries {
        let entry = entry.map_err(|e| BlissError::FileError(e.to_string()))?;
        let path = entry.path();
        out.push(path.clone());
        if recursive && path.is_dir() {
            collect_candidates(&path, true, out)?;
        }
    }
    Ok(())
}

pub fn directory(pathname: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
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
            .map_err(|e| BlissError::FileError(format!("{}: {}", root.display(), e)))?
        {
            let entry = entry.map_err(|e| BlissError::FileError(e.to_string()))?;
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
        let candidate_str = candidate.to_string_lossy().to_string();
        let candidate_rec =
            build_record_from_namestring(parse_namestring_model(&candidate_str, None)?, None);
        if pathname_match_with_captures(&candidate_rec, &rec).is_some() {
            result.push(pathname_from_listed_path(&candidate)?);
        }
    }
    result.sort_by_key(|bv| lookup_string(namestring(*bv).unwrap()).unwrap_or_default());
    Ok(result)
}

pub fn ensure_directories_exist(pathname: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    let path_str = extract_path_string(pathname)?;
    let path = Path::new(&path_str);
    let parent = path.parent().unwrap_or(path);
    let already_exists = parent.is_dir();
    if !already_exists {
        std::fs::create_dir_all(parent)
            .map_err(|e| BlissError::FileError(format!("{}: {}", path_str, e)))?;
    }
    Ok((pathname, !already_exists))
}

pub fn delete_file(pathname: BlissVal) -> Result<(), BlissError> {
    let path_str = extract_path_string(pathname)?;
    std::fs::remove_file(&path_str)
        .map_err(|e| BlissError::FileError(format!("{}: {}", path_str, e)))
}

pub fn rename_file(
    filespec: BlissVal,
    new_name: BlissVal,
) -> Result<(BlissVal, BlissVal, BlissVal), BlissError> {
    let old_path_str = extract_path_string(filespec)?;
    let new_path_str = extract_path_string(new_name)?;
    let old_true = pathname_from_fs_path(Path::new(&old_path_str))?;
    std::fs::rename(&old_path_str, &new_path_str).map_err(|e| {
        BlissError::FileError(format!(
            "rename {} -> {}: {}",
            old_path_str, new_path_str, e
        ))
    })?;
    let new_true = pathname_from_fs_path(Path::new(&new_path_str))?;
    Ok((new_name, old_true, new_true))
}
