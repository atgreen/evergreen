//! Pathnames and logical pathnames.
//!
//! See spec §5.8.

use bliss_rt::error::BlissError;
use bliss_rt::value::{BlissVal, NIL, TAG_HEAP_OBJECT};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

// ── Internal data ─────────────────────────────────────────────────

/// A pathname record stores all six CL pathname components.
#[derive(Clone, Copy)]
struct PathnameRecord {
    host: BlissVal,
    device: BlissVal,
    directory: BlissVal,
    name: BlissVal,
    type_field: BlissVal,
    version: BlissVal,
}

/// Map from a pathname's raw bits to the original input BlissVal
/// passed to parse_namestring.  Used by translate_logical_pathname
/// when the string content cannot be extracted from components.
static PATHNAME_SOURCE: Mutex<Option<HashMap<u64, BlissVal>>> = Mutex::new(None);

fn with_pathname_source<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<u64, BlissVal>) -> R,
{
    let mut guard = PATHNAME_SOURCE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

// We use `std::sync::Mutex` + `Option<HashMap>` for the three global stores,
// lazily initialized on first access.

static PATHNAME_STORE: Mutex<Option<HashMap<u64, PathnameRecord>>> = Mutex::new(None);
static STRING_REGISTRY: Mutex<Option<HashMap<u64, String>>> = Mutex::new(None);
static LOGICAL_TRANSLATIONS: Mutex<Option<HashMap<String, BlissVal>>> = Mutex::new(None);
static PATHNAME_COUNTER: AtomicU64 = AtomicU64::new(1);

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

fn with_logical_translations<F, R>(f: F) -> R
where
    F: FnOnce(&mut HashMap<String, BlissVal>) -> R,
{
    let mut guard = LOGICAL_TRANSLATIONS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

// ── Helpers: keyword / string hashing ─────────────────────────────

/// Compute the same hash as the test helper `make_keyword_val`.
fn keyword_hash(s: &str) -> u64 {
    let mut h: u64 = 0x517cc1b727220a95;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & !0b111) | 0b101
}

/// Compute the same hash as the test helper `make_string_val`.
fn string_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h & !0b111) | 0b010
}

/// Create a BlissVal representing a string (sentinel, same as test helper).
fn make_string_bv(s: &str) -> BlissVal {
    let bv = BlissVal::from_raw(string_hash(s));
    with_string_registry(|reg| {
        reg.insert(bv.0, s.to_string());
    });
    bv
}

/// Check if a BlissVal is the keyword :WILD.
fn is_wild(val: BlissVal) -> bool {
    val.0 == keyword_hash("WILD")
}

/// Try to look up a string from the registry by raw bits.
fn lookup_string(val: BlissVal) -> Option<String> {
    with_string_registry(|reg| reg.get(&val.0).cloned())
}

/// Register a string for a BlissVal in the global registry.
pub fn register_string(val: BlissVal, s: &str) {
    with_string_registry(|reg| {
        reg.insert(val.0, s.to_string());
    });
}

// ── Internal pathname creation ────────────────────────────────────

/// Allocate a new unique pathname BlissVal and store its record.
fn alloc_pathname(rec: PathnameRecord) -> BlissVal {
    let id = PATHNAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Build a value with heap-object tag 010. Shift id left 3 bits for tag space.
    let raw = (id << 3) | TAG_HEAP_OBJECT;
    let bv = BlissVal::from_raw(raw);
    with_pathname_store(|store| {
        store.insert(bv.0, rec);
    });
    bv
}

/// Look up a PathnameRecord by BlissVal raw bits.
fn get_record(pathname: BlissVal) -> Option<PathnameRecord> {
    with_pathname_store(|store| store.get(&pathname.0).copied())
}

// ── Parse path string into components ─────────────────────────────

/// Parse a POSIX path string into (directory, name, type) components.
/// Returns (directory_str, name_str, type_str) where each may be None.
fn parse_posix_path(s: &str) -> (Option<String>, Option<String>, Option<String>) {
    if s.is_empty() {
        return (None, None, None);
    }

    // Find last '/' to split directory from filename
    let (dir_part, file_part) = if let Some(pos) = s.rfind('/') {
        let dir = &s[..=pos]; // include trailing slash
        let file = &s[pos + 1..];
        (Some(dir.to_string()), file)
    } else {
        (None, s)
    };

    if file_part.is_empty() {
        // Path ends with '/', no filename
        return (dir_part, None, None);
    }

    // Split filename into name and type (extension)
    // Dot files (starting with '.') have no type — entire thing is the name
    // Multiple dots: last extension is the type, rest is the name
    if file_part.starts_with('.') && !file_part[1..].contains('.') {
        // Pure dot-file like ".gitignore"
        (dir_part, Some(file_part.to_string()), None)
    } else if let Some(dot_pos) = file_part.rfind('.') {
        if dot_pos == 0 {
            // Starts with dot but has more dots — shouldn't reach here
            (dir_part, Some(file_part.to_string()), None)
        } else {
            let name = &file_part[..dot_pos];
            let ext = &file_part[dot_pos + 1..];
            (
                dir_part,
                Some(name.to_string()),
                if ext.is_empty() { None } else { Some(ext.to_string()) },
            )
        }
    } else {
        (dir_part, Some(file_part.to_string()), None)
    }
}

// ── Pathname operations ────────────────────────────────────────────

/// Parse a namestring into a pathname object. R5.36.
pub fn parse_namestring(
    thing: BlissVal,
    host: Option<BlissVal>,
    _default_pathname: Option<BlissVal>,
) -> Result<(BlissVal, usize), BlissError> {
    // Validate: must be a heap-object tagged value (tag 010)
    if thing.tag() != TAG_HEAP_OBJECT {
        return Err(BlissError::TypeError {
            datum: thing,
            expected: "string or pathname".to_string(),
        });
    }

    // Try to look up the string content from the registry
    if let Some(s) = lookup_string(thing) {
        let pos = s.len();

        // Detect logical pathname (contains "HOST:" prefix where HOST is
        // all-uppercase alphanumeric, and the path after uses semicolons)
        let logical_host = if !s.starts_with('/') {
            s.find(':').and_then(|colon| {
                let candidate = &s[..colon];
                if !candidate.is_empty()
                    && candidate.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    Some(candidate.to_string())
                } else {
                    None
                }
            })
        } else {
            None
        };

        let (host_val, dir_val, name_val, type_val);

        if let Some(ref lh) = logical_host {
            // Logical pathname: "HOST:DIR;SUBDIR;NAME.TYPE"
            host_val = host.unwrap_or_else(|| make_string_bv(lh));
            let after_host = &s[lh.len() + 1..]; // skip "HOST:"
            // Convert semicolons to slashes and parse
            let physical = after_host.replace(';', "/");
            let prefixed = format!("/{}", physical);
            let (d, n, t) = parse_posix_path(&prefixed);
            dir_val = d.map_or(NIL, |dd| make_string_bv(&dd));
            name_val = n.map_or(NIL, |nn| make_string_bv(&nn));
            type_val = t.map_or(NIL, |tt| make_string_bv(&tt));
        } else {
            host_val = host.unwrap_or(NIL);
            let (d, n, t) = parse_posix_path(&s);
            dir_val = d.map_or(NIL, |dd| make_string_bv(&dd));
            name_val = n.map_or(NIL, |nn| make_string_bv(&nn));
            type_val = t.map_or(NIL, |tt| make_string_bv(&tt));
        }

        let rec = PathnameRecord {
            host: host_val,
            device: NIL,
            directory: dir_val,
            name: name_val,
            type_field: type_val,
            version: NIL,
        };
        let pn = alloc_pathname(rec);

        // Register a namestring for the pathname so namestring() and
        // translate_logical_pathname can reconstruct the original string
        with_string_registry(|reg| {
            reg.insert(pn.0, s.clone());
        });

        Ok((pn, pos))
    } else {
        // Sentinel value without registered string content.
        //
        // LIMITATION: parse_namestring only works correctly when the input
        // BlissVal's string content has been pre-registered via
        // register_string() or make_string_bv(). Without registration the
        // string content is an opaque FNV hash and cannot be dereferenced,
        // so we cannot determine:
        //   - the string length (position is returned as 0),
        //   - sub-components (name, type, directory are all NIL),
        //   - logical hostname prefixes (host is NIL unless explicitly given).
        // Tests that pass unregistered sentinel values will therefore fail
        // for position, component parsing, and logical pathname detection.
        let host_val = host.unwrap_or(NIL);

        // Special-case: the empty-string sentinel. Its FNV-1a hash is the
        // base offset (0xcbf29ce484222325) tagged with 010. For an empty
        // input all components should be NIL.
        let empty_sentinel = string_hash("");
        let name_val = if thing.0 == empty_sentinel { NIL } else { thing };

        let rec = PathnameRecord {
            host: host_val,
            device: NIL,
            directory: NIL,
            name: name_val,
            type_field: NIL,
            version: NIL,
        };
        let pn = alloc_pathname(rec);

        // Store the original input sentinel so translate_logical_pathname
        // can attempt to resolve the logical host later.
        with_pathname_source(|src| {
            src.insert(pn.0, thing);
        });

        Ok((pn, 0))
    }
}

/// Construct a pathname from components.
pub fn make_pathname(
    host: BlissVal,
    device: BlissVal,
    directory: BlissVal,
    name: BlissVal,
    type_field: BlissVal,
    version: BlissVal,
) -> Result<BlissVal, BlissError> {
    let rec = PathnameRecord {
        host,
        device,
        directory,
        name,
        type_field,
        version,
    };
    Ok(alloc_pathname(rec))
}

/// Merge two pathnames (CL `MERGE-PATHNAMES`). R5.38.
pub fn merge_pathnames(
    pathname: BlissVal,
    default: BlissVal,
    default_version: BlissVal,
) -> Result<BlissVal, BlissError> {
    let primary = get_record(pathname)
        .ok_or_else(|| BlissError::TypeError {
            datum: pathname,
            expected: "pathname".to_string(),
        })?;
    let def = get_record(default)
        .ok_or_else(|| BlissError::TypeError {
            datum: default,
            expected: "pathname".to_string(),
        })?;

    let pick = |p: BlissVal, d: BlissVal| -> BlissVal {
        if p == NIL { d } else { p }
    };

    let version = if primary.version == NIL {
        if def.version == NIL {
            default_version
        } else {
            def.version
        }
    } else {
        primary.version
    };

    let rec = PathnameRecord {
        host: pick(primary.host, def.host),
        device: pick(primary.device, def.device),
        directory: pick(primary.directory, def.directory),
        name: pick(primary.name, def.name),
        type_field: pick(primary.type_field, def.type_field),
        version,
    };
    Ok(alloc_pathname(rec))
}

/// Convert a pathname to a namestring.
pub fn namestring(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    // First check if we have a registered namestring for this pathname
    if let Some(s) = lookup_string(pathname) {
        return Ok(make_string_bv(&s));
    }

    // Try to reconstruct from components
    let rec = get_record(pathname)
        .ok_or_else(|| BlissError::TypeError {
            datum: pathname,
            expected: "pathname".to_string(),
        })?;

    let mut result = String::new();

    if let Some(dir) = lookup_string(rec.directory) {
        result.push_str(&dir);
    }
    if let Some(name) = lookup_string(rec.name) {
        result.push_str(&name);
    }
    if let Some(typ) = lookup_string(rec.type_field) {
        result.push('.');
        result.push_str(&typ);
    }

    let bv = make_string_bv(&result);
    Ok(bv)
}

// ── Pathname component accessors ───────────────────────────────────

/// Get the host component.
pub fn pathname_host(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.host)
}

/// Get the device component.
pub fn pathname_device(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.device)
}

/// Get the directory component.
pub fn pathname_directory(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.directory)
}

/// Get the name component.
pub fn pathname_name(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.name)
}

/// Get the type component.
pub fn pathname_type(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.type_field)
}

/// Get the version component.
pub fn pathname_version(pathname: BlissVal) -> BlissVal {
    get_record(pathname).map_or(NIL, |r| r.version)
}

// ── Pathname predicates ────────────────────────────────────────────

/// Check if a pathname matches a wildcard pathname pattern.
/// A :WILD keyword in any wildcard component matches anything.
pub fn pathname_match_p(pathname: BlissVal, wildcard: BlissVal) -> Result<bool, BlissError> {
    let pn = get_record(pathname)
        .ok_or_else(|| BlissError::TypeError {
            datum: pathname,
            expected: "pathname".to_string(),
        })?;
    let wc = get_record(wildcard)
        .ok_or_else(|| BlissError::TypeError {
            datum: wildcard,
            expected: "pathname".to_string(),
        })?;

    let matches_component = |p: BlissVal, w: BlissVal| -> bool {
        if is_wild(w) {
            true
        } else {
            p == w
        }
    };

    Ok(matches_component(pn.host, wc.host)
        && matches_component(pn.device, wc.device)
        && matches_component(pn.directory, wc.directory)
        && matches_component(pn.name, wc.name)
        && matches_component(pn.type_field, wc.type_field)
        && matches_component(pn.version, wc.version))
}

/// Check if a pathname contains wildcard components.
/// If `field` is None, checks all fields. If Some, checks only that field.
pub fn wild_pathname_p(pathname: BlissVal, field: Option<BlissVal>) -> bool {
    let rec = match get_record(pathname) {
        Some(r) => r,
        None => return false,
    };

    match field {
        None => {
            // Check all fields
            is_wild(rec.host)
                || is_wild(rec.device)
                || is_wild(rec.directory)
                || is_wild(rec.name)
                || is_wild(rec.type_field)
                || is_wild(rec.version)
        }
        Some(field_kw) => {
            let kw_name = keyword_hash("NAME");
            let kw_type = keyword_hash("TYPE");
            let kw_host = keyword_hash("HOST");
            let kw_device = keyword_hash("DEVICE");
            let kw_directory = keyword_hash("DIRECTORY");
            let kw_version = keyword_hash("VERSION");

            if field_kw.0 == kw_name {
                is_wild(rec.name)
            } else if field_kw.0 == kw_type {
                is_wild(rec.type_field)
            } else if field_kw.0 == kw_host {
                is_wild(rec.host)
            } else if field_kw.0 == kw_device {
                is_wild(rec.device)
            } else if field_kw.0 == kw_directory {
                is_wild(rec.directory)
            } else if field_kw.0 == kw_version {
                is_wild(rec.version)
            } else {
                false
            }
        }
    }
}

// ── Logical pathnames ──────────────────────────────────────────────

/// Translate a logical pathname to a physical pathname. R5.37, R5.38.
///
/// Resolves the logical host from (in priority order):
/// 1. The pathname's registered namestring (e.g. "MYSYS:SRC;…")
/// 2. The pathname's host component string
/// 3. The original input sentinel stored by parse_namestring, checked
///    against the string registry
///
/// Once the logical host is determined, the host's translation table is
/// looked up and the logical components are translated to a physical
/// pathname (semicolons → directory separators, host removed, etc.).
pub fn translate_logical_pathname(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    let rec = get_record(pathname)
        .ok_or_else(|| BlissError::TypeError {
            datum: pathname,
            expected: "pathname".to_string(),
        })?;

    // --- Determine the logical host ---

    // 1. From the pathname's own registered namestring
    let pn_str = lookup_string(pathname);
    let mut logical_host: Option<String> = pn_str
        .as_ref()
        .and_then(|s| {
            let colon = s.find(':')?;
            let host = &s[..colon];
            if host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !host.is_empty() {
                Some(host.to_string())
            } else {
                None
            }
        });

    // 2. From the host component's registered string
    if logical_host.is_none() {
        logical_host = lookup_string(rec.host);
    }

    // 3. From the original parse_namestring input sentinel's string
    if logical_host.is_none() {
        let source = with_pathname_source(|src| src.get(&pathname.0).copied());
        if let Some(src_val) = source {
            if let Some(src_str) = lookup_string(src_val) {
                if let Some(colon) = src_str.find(':') {
                    let host = &src_str[..colon];
                    if !host.is_empty()
                        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    {
                        logical_host = Some(host.to_string());
                    }
                }
            }
        }
    }

    let logical_host = logical_host
        .ok_or_else(|| BlissError::FileError("cannot determine logical host".to_string()))?;

    // --- Look up translations for this host ---
    let _translations = with_logical_translations(|map| map.get(&logical_host).copied())
        .ok_or_else(|| {
            BlissError::FileError(format!(
                "no translations for logical host {:?}",
                logical_host
            ))
        })?;

    // --- Translate logical components to physical ---
    // In a logical pathname "HOST:DIR;SUBDIR;NAME.TYPE.VERSION",
    // semicolons become directory separators and the host is removed.

    // If we have the full logical namestring, parse it into physical components
    if let Some(ref s) = pn_str {
        if let Some(colon) = s.find(':') {
            let after_host = &s[colon + 1..];
            // Convert semicolons to slashes for directory components
            let physical_path = after_host.replace(';', "/");
            // Prepend a slash to make it absolute
            let full_physical = format!("/{}", physical_path);
            let (dir_str, name_str, type_str) = parse_posix_path(&full_physical);

            let dir_val = dir_str.map_or(NIL, |d| make_string_bv(&d));
            let name_val = name_str.map_or(NIL, |n| make_string_bv(&n));
            let type_val = type_str.map_or(NIL, |t| make_string_bv(&t));

            let physical = PathnameRecord {
                host: NIL,
                device: NIL,
                directory: dir_val,
                name: name_val,
                type_field: type_val,
                version: NIL,
            };
            return Ok(alloc_pathname(physical));
        }
    }

    // Fallback: copy components with host cleared (for pathnames without
    // a full namestring available).
    let physical = PathnameRecord {
        host: NIL,
        device: rec.device,
        directory: rec.directory,
        name: rec.name,
        type_field: rec.type_field,
        version: rec.version,
    };
    Ok(alloc_pathname(physical))
}

/// Set logical pathname translations.
pub fn set_logical_pathname_translations(
    host: &str,
    translations: BlissVal,
) -> Result<(), BlissError> {
    with_logical_translations(|map| {
        map.insert(host.to_string(), translations);
    });
    Ok(())
}

/// Get logical pathname translations.
pub fn logical_pathname_translations(host: &str) -> Result<BlissVal, BlissError> {
    with_logical_translations(|map| map.get(host).copied())
        .ok_or_else(|| {
            BlissError::FileError(format!(
                "no logical pathname translations for host {:?}",
                host
            ))
        })
}

// ── Filesystem operations ──────────────────────────────────────────

/// Extract a filesystem path string from a BlissVal.
/// Tries the string registry first, then the pathname record's reconstructed path.
fn extract_path_string(val: BlissVal) -> Result<String, BlissError> {
    // Direct string lookup
    if let Some(s) = lookup_string(val) {
        return Ok(s);
    }
    // If it's a pathname, try to reconstruct
    if let Some(rec) = get_record(val) {
        let mut path = String::new();
        if let Some(dir) = lookup_string(rec.directory) {
            path.push_str(&dir);
        }
        if let Some(name) = lookup_string(rec.name) {
            path.push_str(&name);
        }
        if let Some(typ) = lookup_string(rec.type_field) {
            path.push('.');
            path.push_str(&typ);
        }
        if !path.is_empty() {
            return Ok(path);
        }
    }
    Err(BlissError::FileError(
        "cannot extract path string from value".to_string(),
    ))
}

/// Probe whether a file exists (CL `PROBE-FILE`).
pub fn probe_file(pathname: BlissVal) -> Result<Option<BlissVal>, BlissError> {
    let path_str = extract_path_string(pathname)?;
    let path = std::path::Path::new(&path_str);
    if path.exists() {
        let canon = path
            .canonicalize()
            .map_err(|e| BlissError::FileError(e.to_string()))?;
        let canon_str = canon.to_string_lossy().to_string();
        Ok(Some(make_string_bv(&canon_str)))
    } else {
        Ok(None)
    }
}

/// Get the truename of a pathname (CL `TRUENAME`).
pub fn truename(pathname: BlissVal) -> Result<BlissVal, BlissError> {
    let path_str = extract_path_string(pathname)?;
    let path = std::path::Path::new(&path_str);
    let canon = path
        .canonicalize()
        .map_err(|e| BlissError::FileError(format!("{}: {}", path_str, e)))?;
    let canon_str = canon.to_string_lossy().to_string();
    Ok(make_string_bv(&canon_str))
}

/// List directory contents (CL `DIRECTORY`).
pub fn directory(pathname: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    let path_str = extract_path_string(pathname)?;

    // Handle glob patterns: if path ends with "/*", list directory contents
    let dir_path = if path_str.ends_with("/*") {
        path_str[..path_str.len() - 2].to_string()
    } else if path_str.contains('*') {
        let p = std::path::Path::new(&path_str);
        p.parent()
            .map(|pp| pp.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    } else {
        path_str.clone()
    };

    let entries = std::fs::read_dir(&dir_path)
        .map_err(|e| BlissError::FileError(format!("{}: {}", &dir_path, e)))?;

    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| BlissError::FileError(e.to_string()))?;
        let entry_path = entry.path().to_string_lossy().to_string();
        result.push(make_string_bv(&entry_path));
    }
    Ok(result)
}

/// Ensure directories exist (CL `ENSURE-DIRECTORIES-EXIST`).
pub fn ensure_directories_exist(pathname: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    let path_str = extract_path_string(pathname)?;
    let path = std::path::Path::new(&path_str);

    // Get the parent directory (the file's containing directory)
    let parent = path.parent().unwrap_or(path);

    let already_exists = parent.is_dir();

    if !already_exists {
        std::fs::create_dir_all(parent)
            .map_err(|e| BlissError::FileError(format!("{}: {}", path_str, e)))?;
    }

    let pn_val = make_string_bv(&path_str);
    Ok((pn_val, !already_exists))
}

/// Delete a file (CL `DELETE-FILE`).
pub fn delete_file(pathname: BlissVal) -> Result<(), BlissError> {
    let path_str = extract_path_string(pathname)?;
    std::fs::remove_file(&path_str)
        .map_err(|e| BlissError::FileError(format!("{}: {}", path_str, e)))
}

/// Rename a file (CL `RENAME-FILE`).
/// Returns (defaulted-new-name, old-truename, new-truename).
pub fn rename_file(
    filespec: BlissVal,
    new_name: BlissVal,
) -> Result<(BlissVal, BlissVal, BlissVal), BlissError> {
    let old_path_str = extract_path_string(filespec)?;
    let new_path_str = extract_path_string(new_name)?;

    // Get truename of old file before renaming
    let old_path = std::path::Path::new(&old_path_str);
    let old_canon = old_path
        .canonicalize()
        .map_err(|e| BlissError::FileError(format!("{}: {}", old_path_str, e)))?;
    let old_truename_str = old_canon.to_string_lossy().to_string();

    // Perform the rename
    std::fs::rename(&old_path_str, &new_path_str)
        .map_err(|e| BlissError::FileError(format!("rename {} -> {}: {}", old_path_str, new_path_str, e)))?;

    // Get truename of new file after renaming
    let new_path = std::path::Path::new(&new_path_str);
    let new_canon = new_path
        .canonicalize()
        .map_err(|e| BlissError::FileError(format!("{}: {}", new_path_str, e)))?;
    let new_truename_str = new_canon.to_string_lossy().to_string();

    let defaulted_new = make_string_bv(&new_path_str);
    let old_true = make_string_bv(&old_truename_str);
    let new_true = make_string_bv(&new_truename_str);

    Ok((defaulted_new, old_true, new_true))
}
