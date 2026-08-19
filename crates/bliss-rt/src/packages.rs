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
//! Staging: this foundation creates the package objects and the name→package
//! map (retiring the reader's name-set). The `internal_symbols`/`external_symbols`
//! cells are `NIL` until the per-package symbol maps are migrated onto them; the
//! interpreter still tracks package membership in its own maps meanwhile.

use crate::error::BlissError;
use crate::object::{type_id, ObjectHeader, PackageData};
use crate::value::{BlissVal, NIL};
use std::collections::HashMap;
use std::sync::RwLock;

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

static REGISTRY: RwLock<Option<PackageRegistry>> = RwLock::new(None);

/// Allocate a pinned `SIMPLE_BASE_STRING` (matches the reader/symbol encoding).
fn alloc_pinned_name(s: &str) -> BlissVal {
    let bytes = s.as_bytes();
    let body = crate::gc::alloc_typed(8 + bytes.len(), type_id::SIMPLE_BASE_STRING)
        .expect("OOM allocating package name string");
    // SAFETY: `body` points past a freshly written header.
    unsafe {
        *(body as *mut u64) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), body.add(8), bytes.len());
        let v = BlissVal::from_heap_ptr(body.sub(header_size()));
        crate::gc::pin(v);
        v
    }
}

/// Allocate a pinned PACKAGE object named `name`, all other cells empty.
fn alloc_pinned_package(name: &str) -> BlissVal {
    let name_str = alloc_pinned_name(name);
    let roots = crate::gc::ShadowRootScope::new();
    let name_str = roots.root(name_str);
    let body = crate::gc::alloc_typed(PACKAGE_BODY_SIZE, type_id::PACKAGE)
        .expect("OOM allocating package object");
    // SAFETY: `body` is a fresh PACKAGE body; the header precedes it and its
    // start coincides with `PackageData`'s first field.
    unsafe {
        let header = body.sub(header_size());
        let pkg = header as *mut PackageData;
        (*pkg).name = name_str.get();
        (*pkg).internal_symbols = NIL;
        (*pkg).external_symbols = NIL;
        (*pkg).use_list = NIL;
        (*pkg).nicknames = NIL;
        (*pkg).lock = std::ptr::null_mut();
        let v = BlissVal::from_heap_ptr(header);
        crate::gc::pin(v);
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
pub fn find(name: &str) -> Option<BlissVal> {
    let key = name.to_uppercase();
    with_registry_mut(|reg| reg.name_to_package.get(&key).copied())
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
        reg.name_to_package.entry(key).or_insert(pkg);
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
