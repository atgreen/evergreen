//! Global package registry with heap-resident PACKAGE objects (§1.12, D1.20).
//!
//! One runtime registry for Lisp packages (bliss-jtc.6 Stage D): every package
//! is a real [`PackageData`] object on the GC heap, reachable by name (or
//! nickname) through this registry, so the reader, interpreter, and stdlib agree
//! on package identity instead of each keeping their own name tables.
//!
//! Packages are **pinned and immortal**, like interned symbols — a package lives
//! for the life of the process, so the object references cached here stay valid.
//!
//! The package object owns its internal/external symbol-table references.  The
//! tables themselves are supplied by the standard-library package layer (which
//! owns the CL hash-table implementation), while the runtime keeps the object
//! cells visible to GC, images, the interpreter, and compiled code.

use crate::error::BlissError;
use crate::lock_order::{LockLevel, OrderedRwLock};
use crate::object::{ObjectHeader, PackageData, type_id};
use crate::value::{BlissVal, NIL};
use std::collections::HashMap;

/// `PackageData` payload size: name + internal + external + use_list + nicknames
/// (5×8) + lock pointer (8) = 48 bytes past the header.
const PACKAGE_BODY_SIZE: usize = 48;

/// Standard packages seeded on first use, as `(name, &[nicknames])`. Mirrors the
/// set the reader previously hard-coded so `exists` answers identically.
const STANDARD_PACKAGES: &[(&str, &[&str])] = &[
    ("COMMON-LISP", &["CL"]),
    ("COMMON-LISP-USER", &["CL-USER"]),
    ("KEYWORD", &[]),
    ("BLISS", &[]),
];

fn header_size() -> usize {
    std::mem::size_of::<ObjectHeader>()
}

struct PackageRegistry {
    /// Name or nickname (uppercased) → the package object. Multiple keys may map
    /// to the same package (a package plus its nicknames).
    name_to_package: HashMap<String, BlissVal>,
    /// Every distinct package object, for future GC rooting of their cells.
    packages: Vec<BlissVal>,
}

static REGISTRY: OrderedRwLock<Option<PackageRegistry>> = OrderedRwLock::new(
    LockLevel::PackageRegistry,
    3,
    "runtime package registry",
    None,
);

/// Allocate a pinned `SIMPLE_BASE_STRING` (matches the reader/symbol encoding).
fn alloc_pinned_name(s: &str) -> BlissVal {
    let bytes = s.as_bytes();
    let body = crate::gc::alloc_pinned_typed(8 + bytes.len(), type_id::SIMPLE_BASE_STRING)
        .expect("OOM allocating package name string");
    // SAFETY: `body` points past a freshly written header.
    unsafe {
        *(body as *mut u64) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), body.add(8), bytes.len());
        let v = BlissVal::from_heap_ptr(body.sub(header_size()));
        v
    }
}

/// Allocate a pinned PACKAGE object named `name`, all other cells empty.
fn alloc_pinned_package(name: &str) -> BlissVal {
    let name_str = alloc_pinned_name(name);
    crate::rooted!(name_str = name_str);
    let body = crate::gc::alloc_pinned_typed(PACKAGE_BODY_SIZE, type_id::PACKAGE)
        .expect("OOM allocating package object");
    // SAFETY: `body` is a fresh PACKAGE body; the header precedes it and its
    // start coincides with `PackageData`'s first field.
    unsafe {
        let header = body.sub(header_size());
        let pkg = header as *mut PackageData;
        (*pkg).name = *name_str;
        (*pkg).internal_symbols = NIL;
        (*pkg).external_symbols = NIL;
        (*pkg).use_list = NIL;
        (*pkg).nicknames = NIL;
        (*pkg).lock = std::ptr::null_mut();
        let v = BlissVal::from_heap_ptr(header);
        v
    }
}

fn with_registry_mut<R>(f: impl FnOnce(&mut PackageRegistry) -> R) -> R {
    let mut guard = REGISTRY.write().expect("package registry poisoned");
    let reg = guard.get_or_insert_with(|| PackageRegistry {
        name_to_package: HashMap::new(),
        packages: Vec::new(),
    });
    if reg.packages.is_empty() {
        // Seed the standard packages on first use (outside the borrow below via a
        // direct insert, since alloc happens before we hold nothing else).
        for (name, nicks) in STANDARD_PACKAGES {
            let pkg = alloc_pinned_package(name);
            reg.packages.push(pkg);
            reg.name_to_package.insert((*name).to_string(), pkg);
            for nick in *nicks {
                reg.name_to_package.insert((*nick).to_string(), pkg);
            }
        }
    }
    f(reg)
}

/// The package named (or nicknamed) `name`, if registered. Case-insensitive.
/// Run `f` against the registry under a SHARED lock, or return `None` if the
/// registry still needs its one-time seeding (which requires the write lock).
fn with_registry<R>(f: impl FnOnce(&PackageRegistry) -> R) -> Option<R> {
    let guard = REGISTRY.read().expect("package registry poisoned");
    match guard.as_ref() {
        Some(reg) if !reg.packages.is_empty() => Some(f(reg)),
        _ => None,
    }
}

pub fn find(name: &str) -> Option<BlissVal> {
    // Two costs mattered here: this was ~8% of an ASDF load, called once per
    // package per symbol token.
    //
    //  * it took the registry's WRITE lock for a read-only lookup, serialising
    //    every package resolution on it;
    //  * it allocated an uppercased copy of the name just to hash it, even
    //    though registry keys are stored uppercase and essentially every caller
    //    already passes a canonical name.
    let lookup = |reg: &PackageRegistry| -> Option<BlissVal> {
        if name.is_ascii() && !name.bytes().any(|b| b.is_ascii_lowercase()) {
            reg.name_to_package.get(name).copied()
        } else {
            reg.name_to_package.get(&name.to_uppercase()).copied()
        }
    };
    // Shared-lock fast path; fall back to the write path only to seed.
    match with_registry(lookup) {
        Some(found) => found,
        None => with_registry_mut(|reg| lookup(reg)),
    }
}

/// Whether a package with this name or nickname exists. Case-insensitive.
pub fn exists(name: &str) -> bool {
    find(name).is_some()
}

/// Ensure a package named `name` exists, creating it if necessary, and return it.
pub fn find_or_create(name: &str) -> BlissVal {
    let key = name.to_uppercase();
    with_registry_mut(|reg| {
        if let Some(&pkg) = reg.name_to_package.get(&key) {
            return pkg;
        }
        let pkg = alloc_pinned_package(&key);
        reg.packages.push(pkg);
        reg.name_to_package.insert(key, pkg);
        pkg
    })
}

/// Register `name` as a package (creating it if new). Retained for the reader's
/// `register-package` entrypoint.
pub fn register(name: &str) {
    let _ = find_or_create(name);
}

/// Add `nickname` as another name for the package currently named `name`.
pub fn add_nickname(name: &str, nickname: &str) {
    let pkg = find_or_create(name);
    let key = nickname.to_uppercase();
    with_registry_mut(|reg| {
        // Reader/package prepasses may have provisionally created a package
        // under the nickname before DEFPACKAGE establishes the real alias.
        // The package layer has already performed the CL conflict check, so
        // publishing the authoritative alias must replace that placeholder.
        reg.name_to_package.insert(key, pkg);
    });
}

/// Replace a package's primary name and global nicknames while preserving its
/// heap identity. Returns an error if a requested name belongs to another
/// package.
pub fn rename(pkg: BlissVal, new_name: &str, new_nicknames: &[&str]) -> Result<(), BlissError> {
    if !crate::types::packagep(pkg) {
        return Err(BlissError::TypeError {
            datum: pkg,
            expected: "PACKAGE".into(),
        });
    }
    let primary = new_name.to_uppercase();
    let nicknames: Vec<String> = new_nicknames
        .iter()
        .map(|name| name.to_uppercase())
        .collect();
    with_registry_mut(|reg| {
        for name in std::iter::once(&primary).chain(nicknames.iter()) {
            if reg
                .name_to_package
                .get(name)
                .is_some_and(|other| *other != pkg)
            {
                return Err(BlissError::PackageError(format!(
                    "Package name {name:?} is already in use"
                )));
            }
        }
        reg.name_to_package.retain(|_, package| *package != pkg);
        reg.name_to_package.insert(primary.clone(), pkg);
        for nickname in &nicknames {
            reg.name_to_package.insert(nickname.clone(), pkg);
        }
        let name = alloc_pinned_name(&primary);
        // SAFETY: validated PACKAGE object with fixed pinned layout.
        unsafe {
            (*(pkg.as_ptr() as *mut PackageData)).name = name;
        }
        Ok(())
    })
}

/// Remove all global names for a package. The pinned object remains allocated
/// so existing references stay valid, but subsequent name lookup cannot find it.
pub fn unregister(pkg: BlissVal) {
    with_registry_mut(|reg| {
        reg.name_to_package.retain(|_, package| *package != pkg);
    });
}

/// The primary name of a package object, read from its heap name cell.
pub fn package_name(pkg: BlissVal) -> Option<String> {
    if !pkg.is_heap_object() {
        return None;
    }
    // SAFETY: a registered package is a pinned live PACKAGE object.
    unsafe {
        let data = pkg.as_ptr() as *const PackageData;
        Some((*data).name.as_string())
    }
}

/// Return the package's internal and external symbol-table objects.
pub fn symbol_tables(pkg: BlissVal) -> Option<(BlissVal, BlissVal)> {
    if !crate::types::packagep(pkg) {
        return None;
    }
    // SAFETY: PACKAGE values use the fixed D1.20 layout and packages are pinned.
    unsafe {
        let data = pkg.as_ptr() as *const PackageData;
        Some(((*data).internal_symbols, (*data).external_symbols))
    }
}

/// Install the authoritative internal/external symbol tables in a package.
///
/// PACKAGE objects are pinned, so publishing the two tagged references while
/// holding the stdlib package lock is sufficient.  The GC traces both cells.
pub fn set_symbol_tables(
    pkg: BlissVal,
    internal_symbols: BlissVal,
    external_symbols: BlissVal,
) -> Result<(), BlissError> {
    if !crate::types::packagep(pkg) {
        return Err(BlissError::TypeError {
            datum: pkg,
            expected: "PACKAGE".into(),
        });
    }
    // SAFETY: PACKAGE values use the fixed D1.20 layout and packages are pinned.
    unsafe {
        let data = pkg.as_ptr() as *mut PackageData;
        (*data).internal_symbols = internal_symbols;
        (*data).external_symbols = external_symbols;
    }
    Ok(())
}

// ── Image serialization (bliss-jtc.6 Stage F) ───────────────────────────────

/// Serialize the package registry: each package's primary name followed by its
/// nicknames, length-prefixed. Restoring recreates the packages and nicknames so
/// a saved image resolves the same package names after reload. (Symbol
/// membership lives with the symbols; see `symbols::serialize`.)
pub fn serialize() -> Vec<u8> {
    let mut buf = Vec::new();
    with_registry_mut(|reg| {
        buf.extend_from_slice(&(reg.packages.len() as u32).to_le_bytes());
        for &pkg in &reg.packages {
            let primary = package_name(pkg).unwrap_or_default();
            // Nicknames: every registry key that maps to this package except the
            // primary name.
            let nicks: Vec<String> = reg
                .name_to_package
                .iter()
                .filter(|entry| *entry.1 == pkg && *entry.0 != primary)
                .map(|entry| entry.0.clone())
                .collect();
            encode_str(&mut buf, &primary);
            buf.extend_from_slice(&(nicks.len() as u32).to_le_bytes());
            for n in &nicks {
                encode_str(&mut buf, n);
            }
        }
    });
    buf
}

/// Restore a package registry serialized by [`serialize`], recreating each
/// package and its nicknames. Additive over the standard packages seeded on
/// first use.
pub fn restore(data: &[u8]) -> Result<(), BlissError> {
    let mut pos = 0usize;
    let count = read_u32(data, &mut pos)? as usize;
    for _ in 0..count {
        let primary = read_str(data, &mut pos)?;
        let _ = find_or_create(&primary);
        let nick_count = read_u32(data, &mut pos)? as usize;
        for _ in 0..nick_count {
            let nick = read_str(data, &mut pos)?;
            add_nickname(&primary, &nick);
        }
    }
    Ok(())
}

fn encode_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, BlissError> {
    let end = pos
        .checked_add(4)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| BlissError::Internal("truncated package image section".into()))?;
    let v = u32::from_le_bytes(data[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(v)
}

fn read_str(data: &[u8], pos: &mut usize) -> Result<String, BlissError> {
    let len = read_u32(data, pos)? as usize;
    let end = pos
        .checked_add(len)
        .filter(|&e| e <= data.len())
        .ok_or_else(|| BlissError::Internal("truncated package image string".into()))?;
    let s = String::from_utf8_lossy(&data[*pos..end]).into_owned();
    *pos = end;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::type_id;
    use std::sync::{Mutex, OnceLock};

    fn heap_test_lock() -> &'static Mutex<()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn standard_packages_exist_by_name_and_nickname() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        assert!(exists("COMMON-LISP"));
        assert!(exists("cl")); // nickname, case-insensitive
        assert!(exists("KEYWORD"));
        assert!(exists("CL-USER"));
        assert!(!exists("NO-SUCH-PACKAGE"));
        // A nickname resolves to the same object as the primary name.
        assert_eq!(find("CL"), find("COMMON-LISP"));
    }

    #[test]
    fn find_or_create_is_idempotent_and_names_round_trip() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let a = find_or_create("MY-PKG");
        let b = find_or_create("my-pkg");
        assert_eq!(a, b, "same name (case-insensitive) yields the same package");
        assert_eq!(package_name(a).as_deref(), Some("MY-PKG"));
    }

    #[test]
    fn package_object_lives_on_the_gc_heap() {
        let _g = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        find_or_create("HEAP-CHECK-PKG");
        let mut saw = false;
        crate::walk_heap(|_p, tid, _s| {
            if tid == type_id::PACKAGE {
                saw = true;
            }
            true
        })
        .expect("walk_heap");
        assert!(saw, "packages must be PACKAGE objects on the GC heap");
    }
}
