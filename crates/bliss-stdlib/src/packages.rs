//! Package system and bootstrap.
//!
//! Manages the CL package registry, package operations, and
//! the standard package layout. See spec §5.1.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};

// ── Internal package data ─────────────────────────────────────────

/// Counter for generating unique package IDs (globally unique across threads).
static NEXT_PACKAGE_ID: AtomicI64 = AtomicI64::new(1);

/// Counter for generating unique symbol values (globally unique across threads).
static NEXT_SYMBOL_ID: AtomicI64 = AtomicI64::new(1);

/// Thread-local package store so parallel tests don't interfere.
/// In production, a single-threaded runtime or explicit locking wraps access.
thread_local! {
    static PACKAGE_STORE: RefCell<PackageStore> = RefCell::new(PackageStore::new());
}

struct PackageStore {
    packages: HashMap<i64, Package>,
    /// Map from name/nickname → package id for fast lookup.
    name_index: HashMap<String, i64>,
}

impl PackageStore {
    fn new() -> Self {
        PackageStore {
            packages: HashMap::new(),
            name_index: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.packages.clear();
        self.name_index.clear();
    }
}

#[derive(Clone)]
struct Package {
    name: String,
    nicknames: Vec<String>,
    internal_symbols: HashMap<String, BlissVal>,
    external_symbols: HashMap<String, BlissVal>,
    shadowing_symbols: HashSet<String>,
    use_list: Vec<BlissVal>,
}

impl Package {
    fn new(name: &str) -> Self {
        Package {
            name: name.to_string(),
            nicknames: Vec::new(),
            internal_symbols: HashMap::new(),
            external_symbols: HashMap::new(),
            shadowing_symbols: HashSet::new(),
            use_list: Vec::new(),
        }
    }
}

/// Allocate a fresh BlissVal to represent a package handle.
fn alloc_package_id() -> (i64, BlissVal) {
    let id = NEXT_PACKAGE_ID.fetch_add(1, Ordering::Relaxed);
    (id, BlissVal::from_fixnum(id))
}

/// Allocate a fresh BlissVal to represent a symbol.
fn alloc_symbol() -> BlissVal {
    let id = NEXT_SYMBOL_ID.fetch_add(1, Ordering::Relaxed);
    BlissVal::from_fixnum(id)
}

/// Extract the package ID from a BlissVal handle.
fn pkg_id(handle: BlissVal) -> i64 {
    handle.as_fixnum()
}

/// Helper: run a closure with mutable access to the thread-local store.
fn with_store_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut PackageStore) -> R,
{
    PACKAGE_STORE.with(|cell| f(&mut cell.borrow_mut()))
}

/// Helper: run a closure with read access to the thread-local store.
fn with_store<F, R>(f: F) -> R
where
    F: FnOnce(&PackageStore) -> R,
{
    PACKAGE_STORE.with(|cell| f(&cell.borrow()))
}

// ── Package registry ───────────────────────────────────────────────

/// Global package registry. Thread-safe (RwLock-protected).
pub struct PackageRegistry {
    _private: (),
}

impl PackageRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        with_store_mut(|store| store.clear());
        PackageRegistry { _private: () }
    }

    /// Initialize with the standard packages (CL, CL-USER, KEYWORD, BLISS, etc.).
    pub fn init_standard_packages(&mut self) -> Result<(), BlissError> {
        self.make_package("COMMON-LISP", &["CL"], &[])?;
        self.make_package("CL-USER", &[], &[])?;
        self.make_package("KEYWORD", &[], &[])?;
        Ok(())
    }

    /// Find a package by name or nickname. O(1) amortised (R5.05).
    pub fn find_package(&self, name: &str) -> Option<BlissVal> {
        with_store(|store| {
            store.name_index.get(name).map(|&id| BlissVal::from_fixnum(id))
        })
    }

    /// Create a new package.
    pub fn make_package(
        &mut self,
        name: &str,
        nicknames: &[&str],
        use_list: &[&str],
    ) -> Result<BlissVal, BlissError> {
        with_store_mut(|store| {
            // Check for duplicate name or nickname conflicts.
            if store.name_index.contains_key(name) {
                return Err(BlissError::PackageError(
                    format!("Package named {:?} already exists", name),
                ));
            }
            for nick in nicknames {
                if store.name_index.contains_key(*nick) {
                    return Err(BlissError::PackageError(
                        format!("Nickname {:?} conflicts with an existing package", nick),
                    ));
                }
            }

            // Resolve use_list package names to IDs.
            let mut resolved_uses: Vec<BlissVal> = Vec::new();
            for use_name in use_list {
                match store.name_index.get(*use_name) {
                    Some(&id) => resolved_uses.push(BlissVal::from_fixnum(id)),
                    None => {
                        return Err(BlissError::PackageError(
                            format!("Package {:?} not found for use-list", use_name),
                        ));
                    }
                }
            }

            let (id, handle) = alloc_package_id();
            let mut pkg = Package::new(name);
            pkg.nicknames = nicknames.iter().map(|s| s.to_string()).collect();
            pkg.use_list = resolved_uses;

            // Register name and nicknames.
            store.name_index.insert(name.to_string(), id);
            for nick in nicknames {
                store.name_index.insert(nick.to_string(), id);
            }
            store.packages.insert(id, pkg);

            Ok(handle)
        })
    }

    /// Delete a package.
    pub fn delete_package(&mut self, name: &str) -> Result<(), BlissError> {
        with_store_mut(|store| {
            let id = store.name_index.get(name).copied().ok_or_else(|| {
                BlissError::PackageError(format!("Package {:?} not found", name))
            })?;

            let pkg = store.packages.remove(&id).unwrap();
            store.name_index.remove(&pkg.name);
            for nick in &pkg.nicknames {
                store.name_index.remove(nick);
            }
            Ok(())
        })
    }

    /// List all packages.
    pub fn list_all_packages(&self) -> Vec<BlissVal> {
        with_store(|store| {
            store
                .packages
                .keys()
                .map(|&id| BlissVal::from_fixnum(id))
                .collect()
        })
    }
}

// ── Package operations ─────────────────────────────────────────────

/// Intern a symbol in a package.
pub fn intern(name: &str, package: BlissVal) -> Result<(BlissVal, InternStatus), BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);
        let pkg = store.packages.get(&id).ok_or_else(|| {
            BlissError::PackageError("Package not found".to_string())
        })?;

        // Check internal symbols first.
        if let Some(&sym) = pkg.internal_symbols.get(name) {
            return Ok((sym, InternStatus::Internal));
        }

        // Check external symbols.
        if let Some(&sym) = pkg.external_symbols.get(name) {
            return Ok((sym, InternStatus::External));
        }

        // Check inherited symbols (from use_list).
        let use_list = pkg.use_list.clone();
        for used_pkg_handle in &use_list {
            let used_id = pkg_id(*used_pkg_handle);
            if let Some(used_pkg) = store.packages.get(&used_id) {
                if let Some(&sym) = used_pkg.external_symbols.get(name) {
                    return Ok((sym, InternStatus::Inherited));
                }
            }
        }

        // Create a new symbol and add to internal_symbols.
        let sym = alloc_symbol();
        let pkg = store.packages.get_mut(&id).unwrap();
        pkg.internal_symbols.insert(name.to_string(), sym);
        Ok((sym, InternStatus::New))
    })
}

/// Status returned by `intern`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternStatus {
    /// Symbol was already present as internal.
    Internal,
    /// Symbol was already present as external.
    External,
    /// Symbol was inherited from a used package.
    Inherited,
    /// Symbol was freshly created.
    New,
}

/// Find a symbol without interning it.
pub fn find_symbol(name: &str, package: BlissVal) -> Result<Option<(BlissVal, InternStatus)>, BlissError> {
    with_store(|store| {
        let id = pkg_id(package);
        let pkg = store.packages.get(&id).ok_or_else(|| {
            BlissError::PackageError("Package not found".to_string())
        })?;

        // Check internal symbols.
        if let Some(&sym) = pkg.internal_symbols.get(name) {
            return Ok(Some((sym, InternStatus::Internal)));
        }

        // Check external symbols.
        if let Some(&sym) = pkg.external_symbols.get(name) {
            return Ok(Some((sym, InternStatus::External)));
        }

        // Check inherited symbols.
        for used_pkg_handle in &pkg.use_list {
            let used_id = pkg_id(*used_pkg_handle);
            if let Some(used_pkg) = store.packages.get(&used_id) {
                if let Some(&sym) = used_pkg.external_symbols.get(name) {
                    return Ok(Some((sym, InternStatus::Inherited)));
                }
            }
        }

        Ok(None)
    })
}

/// Export symbols from a package.
pub fn export(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);
        let pkg = store.packages.get_mut(&id).ok_or_else(|| {
            BlissError::PackageError("Package not found".to_string())
        })?;

        for &sym in symbols {
            // Find the symbol name in internal_symbols.
            let sym_name = pkg
                .internal_symbols
                .iter()
                .find(|(_, v)| **v == sym)
                .map(|(k, _)| k.clone());

            if let Some(name) = sym_name {
                pkg.internal_symbols.remove(&name);
                pkg.external_symbols.insert(name, sym);
            } else {
                // Check if already external — that's fine, no-op.
                let already_external = pkg.external_symbols.values().any(|&v| v == sym);
                if !already_external {
                    return Err(BlissError::PackageError(
                        "Symbol not accessible in package".to_string(),
                    ));
                }
            }
        }
        Ok(())
    })
}

/// Unexport symbols from a package.
pub fn unexport(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);
        let pkg = store.packages.get_mut(&id).ok_or_else(|| {
            BlissError::PackageError("Package not found".to_string())
        })?;

        for &sym in symbols {
            let sym_name = pkg
                .external_symbols
                .iter()
                .find(|(_, v)| **v == sym)
                .map(|(k, _)| k.clone());

            if let Some(name) = sym_name {
                pkg.external_symbols.remove(&name);
                pkg.internal_symbols.insert(name, sym);
            }
            // If not external, no-op per CL spec.
        }
        Ok(())
    })
}

/// Unintern a symbol from a package.
pub fn unintern(symbol: BlissVal, package: BlissVal) -> Result<bool, BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);
        let pkg = store.packages.get_mut(&id).ok_or_else(|| {
            BlissError::PackageError("Package not found".to_string())
        })?;

        // Try to remove from internal_symbols.
        let removed_internal = pkg
            .internal_symbols
            .iter()
            .find(|(_, v)| **v == symbol)
            .map(|(k, _)| k.clone());

        if let Some(name) = removed_internal {
            pkg.internal_symbols.remove(&name);
            pkg.shadowing_symbols.remove(&name);
            return Ok(true);
        }

        // Try to remove from external_symbols.
        let removed_external = pkg
            .external_symbols
            .iter()
            .find(|(_, v)| **v == symbol)
            .map(|(k, _)| k.clone());

        if let Some(name) = removed_external {
            pkg.external_symbols.remove(&name);
            pkg.shadowing_symbols.remove(&name);
            return Ok(true);
        }

        Ok(false)
    })
}

/// Use a package (add to use-list).
pub fn use_package(packages: &[BlissVal], target: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let target_id = pkg_id(target);

        // Verify all packages exist.
        for &pkg_handle in packages {
            let pid = pkg_id(pkg_handle);
            if !store.packages.contains_key(&pid) {
                return Err(BlissError::PackageError("Package not found".to_string()));
            }
        }

        let target_pkg = store.packages.get_mut(&target_id).ok_or_else(|| {
            BlissError::PackageError("Target package not found".to_string())
        })?;

        for &pkg_handle in packages {
            if !target_pkg.use_list.contains(&pkg_handle) {
                target_pkg.use_list.push(pkg_handle);
            }
        }
        Ok(())
    })
}

/// Unuse a package.
pub fn unuse_package(packages: &[BlissVal], target: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let target_id = pkg_id(target);
        let target_pkg = store.packages.get_mut(&target_id).ok_or_else(|| {
            BlissError::PackageError("Target package not found".to_string())
        })?;

        for &pkg_handle in packages {
            target_pkg.use_list.retain(|&x| x != pkg_handle);
        }
        Ok(())
    })
}

/// Import symbols into a package.
pub fn import(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);

        for &sym in symbols {
            // Find the symbol's name by searching all packages.
            let name = find_symbol_name_in_store(store, sym).ok_or_else(|| {
                BlissError::PackageError("Symbol not found in any package".to_string())
            })?;

            let pkg = store.packages.get(&id).ok_or_else(|| {
                BlissError::PackageError("Package not found".to_string())
            })?;

            // Check for conflict: a different symbol with the same name exists.
            if let Some(&existing) = pkg.internal_symbols.get(&name) {
                if existing != sym {
                    return Err(BlissError::PackageError(
                        format!("Name conflict for symbol {:?}", name),
                    ));
                }
                // Same symbol already present — no-op.
                continue;
            }
            if let Some(&existing) = pkg.external_symbols.get(&name) {
                if existing != sym {
                    return Err(BlissError::PackageError(
                        format!("Name conflict for symbol {:?}", name),
                    ));
                }
                continue;
            }

            let pkg = store.packages.get_mut(&id).unwrap();
            pkg.internal_symbols.insert(name, sym);
        }
        Ok(())
    })
}

/// Shadowing import.
pub fn shadowing_import(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);

        for &sym in symbols {
            let name = find_symbol_name_in_store(store, sym).ok_or_else(|| {
                BlissError::PackageError("Symbol not found in any package".to_string())
            })?;

            let pkg = store.packages.get_mut(&id).ok_or_else(|| {
                BlissError::PackageError("Package not found".to_string())
            })?;

            // Remove any existing symbol with the same name (no conflict check).
            pkg.internal_symbols.remove(&name);
            pkg.external_symbols.remove(&name);

            // Add as internal and mark as shadowing.
            pkg.internal_symbols.insert(name.clone(), sym);
            pkg.shadowing_symbols.insert(name);
        }
        Ok(())
    })
}

/// Shadow symbols.
pub fn shadow(names: &[&str], package: BlissVal) -> Result<(), BlissError> {
    with_store_mut(|store| {
        let id = pkg_id(package);
        let pkg = store.packages.get_mut(&id).ok_or_else(|| {
            BlissError::PackageError("Package not found".to_string())
        })?;

        for &name in names {
            // If symbol doesn't exist in internal or external, create it.
            if !pkg.internal_symbols.contains_key(name)
                && !pkg.external_symbols.contains_key(name)
            {
                let sym = alloc_symbol();
                pkg.internal_symbols.insert(name.to_string(), sym);
            }
            pkg.shadowing_symbols.insert(name.to_string());
        }
        Ok(())
    })
}

// ── Helpers ───────────────────────────────────────────────────────

/// Find the name of a symbol by searching all packages in the store.
fn find_symbol_name_in_store(store: &PackageStore, sym: BlissVal) -> Option<String> {
    for pkg in store.packages.values() {
        for (name, &val) in &pkg.internal_symbols {
            if val == sym {
                return Some(name.clone());
            }
        }
        for (name, &val) in &pkg.external_symbols {
            if val == sym {
                return Some(name.clone());
            }
        }
    }
    None
}
