//! Package system and bootstrap.
//!
//! Manages the CL package registry, package operations, and
//! the standard package layout. See spec §5.1.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

// ── Internal package data ─────────────────────────────────────────

/// Counter for generating unique package IDs (globally unique across threads).
static NEXT_PACKAGE_ID: AtomicI64 = AtomicI64::new(1);

/// Counter for generating unique symbol values (globally unique across threads).
static NEXT_SYMBOL_ID: AtomicI64 = AtomicI64::new(1);

thread_local! {
    static CURRENT_STORE: RefCell<Option<Arc<RegistryStore>>> = const { RefCell::new(None) };
}

fn swap_current_store(
    new_store: Option<Arc<RegistryStore>>,
) -> Option<Arc<RegistryStore>> {
    CURRENT_STORE.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), new_store))
}

struct RegistryStore {
    state: RwLock<PackageStore>,
}

struct PackageStore {
    packages: HashMap<i64, Arc<RwLock<Package>>>,
    /// Map from global name/nickname → package id for fast lookup.
    name_index: HashMap<String, i64>,
}

impl PackageStore {
    fn new() -> Self {
        Self {
            packages: HashMap::new(),
            name_index: HashMap::new(),
        }
    }
}

#[derive(Clone)]
struct Package {
    name: String,
    nicknames: Vec<String>,
    local_nicknames: HashMap<String, i64>,
    internal_symbols: HashMap<String, BlissVal>,
    external_symbols: HashMap<String, BlissVal>,
    shadowing_symbols: HashSet<String>,
    use_list: Vec<BlissVal>,
}

impl Package {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            nicknames: Vec::new(),
            local_nicknames: HashMap::new(),
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

fn no_active_registry_error() -> BlissError {
    BlissError::PackageError("No active PackageRegistry on this thread".to_string())
}

fn lock_poisoned_error(context: &str) -> BlissError {
    BlissError::PackageError(format!("Package registry lock poisoned during {context}"))
}

fn current_store() -> Result<Arc<RegistryStore>, BlissError> {
    CURRENT_STORE.with(|cell| {
        cell.borrow()
            .as_ref()
            .cloned()
            .ok_or_else(no_active_registry_error)
    })
}

fn lookup_package_arc(
    store: &PackageStore,
    package: BlissVal,
) -> Result<Arc<RwLock<Package>>, BlissError> {
    store
        .packages
        .get(&pkg_id(package))
        .cloned()
        .ok_or_else(|| BlissError::PackageError("Package not found".to_string()))
}

fn find_symbol_name_in_store(store: &PackageStore, sym: BlissVal) -> Result<Option<String>, BlissError> {
    for package in store.packages.values() {
        let package = package
            .read()
            .map_err(|_| lock_poisoned_error("symbol lookup"))?;
        for (name, &val) in &package.internal_symbols {
            if val == sym {
                return Ok(Some(name.clone()));
            }
        }
        for (name, &val) in &package.external_symbols {
            if val == sym {
                return Ok(Some(name.clone()));
            }
        }
    }
    Ok(None)
}

// ── Package registry ───────────────────────────────────────────────

/// Package registry that owns its package store.
pub struct PackageRegistry {
    store: Arc<RegistryStore>,
    previous_store: Option<Arc<RegistryStore>>,
}

/// Scoped activation guard for the thread-local package registry.
pub struct ActivePackageRegistryGuard {
    previous_store: Option<Arc<RegistryStore>>,
}

impl Clone for PackageRegistry {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            previous_store: None,
        }
    }
}

impl PackageRegistry {
    /// Create a new empty registry and install it as the current store for this thread.
    pub fn new() -> Self {
        let store = Arc::new(RegistryStore {
            state: RwLock::new(PackageStore::new()),
        });
        let previous_store = swap_current_store(Some(Arc::clone(&store)));
        Self {
            store,
            previous_store,
        }
    }

    /// Install this registry as the current thread-local registry until the returned
    /// guard is dropped.
    pub fn activate(&self) -> ActivePackageRegistryGuard {
        let previous_store = swap_current_store(Some(Arc::clone(&self.store)));
        ActivePackageRegistryGuard { previous_store }
    }

    /// Initialize with the standard packages (CL, CL-USER, KEYWORD, BLISS, etc.).
    pub fn init_standard_packages(&mut self) -> Result<(), BlissError> {
        if self.bootstrap_initialized()? {
            return Ok(());
        }

        let keyword = self.ensure_package("KEYWORD", &[], &[])?;
        let common_lisp = self.ensure_package("COMMON-LISP", &["CL"], &[])?;
        let bliss_internal = self.ensure_package("BLISS-INTERNAL", &["BI"], &["CL"])?;
        let bliss_ext = self.ensure_package("BLISS-EXT", &[], &["CL"])?;
        let common_lisp_user =
            self.ensure_package("COMMON-LISP-USER", &["CL-USER"], &["CL", "BLISS-EXT"])?;

        self.ensure_package_alias("BI", bliss_internal)?;
        self.ensure_package_alias("CL", common_lisp)?;
        self.ensure_package_alias("CL-USER", common_lisp_user)?;
        self.ensure_package_alias("COMMON-LISP", common_lisp)?;
        self.ensure_package_alias("COMMON-LISP-USER", common_lisp_user)?;
        self.ensure_package_alias("KEYWORD", keyword)?;
        self.ensure_package_alias("BLISS-INTERNAL", bliss_internal)?;
        self.ensure_package_alias("BLISS-EXT", bliss_ext)?;
        Ok(())
    }

    /// Find a package by global name or nickname.
    pub fn find_package(&self, name: &str) -> Option<BlissVal> {
        let store = self.store.state.read().ok()?;
        store
            .name_index
            .get(name)
            .copied()
            .map(BlissVal::from_fixnum)
    }

    /// Resolve a package designator relative to another package, honoring package-local nicknames first.
    pub fn find_package_from(&self, package: BlissVal, name: &str) -> Option<BlissVal> {
        let store = self.store.state.read().ok()?;
        let package = store.packages.get(&pkg_id(package))?.clone();
        let package = package.read().ok()?;
        if let Some(&id) = package.local_nicknames.get(name) {
            return Some(BlissVal::from_fixnum(id));
        }
        store
            .name_index
            .get(name)
            .copied()
            .map(BlissVal::from_fixnum)
    }

    /// Create a new package.
    pub fn make_package(
        &mut self,
        name: &str,
        nicknames: &[&str],
        use_list: &[&str],
    ) -> Result<BlissVal, BlissError> {
        let mut store = self
            .store
            .state
            .write()
            .map_err(|_| lock_poisoned_error("package creation"))?;

        if store.name_index.contains_key(name) {
            return Err(BlissError::PackageError(format!(
                "Package named {:?} already exists",
                name
            )));
        }
        for nick in nicknames {
            if store.name_index.contains_key(*nick) {
                return Err(BlissError::PackageError(format!(
                    "Nickname {:?} conflicts with an existing package",
                    nick
                )));
            }
        }

        let mut resolved_uses = Vec::new();
        for use_name in use_list {
            match store.name_index.get(*use_name) {
                Some(&id) => resolved_uses.push(BlissVal::from_fixnum(id)),
                None => {
                    return Err(BlissError::PackageError(format!(
                        "Package {:?} not found for use-list",
                        use_name
                    )));
                }
            }
        }

        let (id, handle) = alloc_package_id();
        let mut pkg = Package::new(name);
        pkg.nicknames = nicknames.iter().map(|s| s.to_string()).collect();
        pkg.use_list = resolved_uses;

        store
            .packages
            .insert(id, Arc::new(RwLock::new(pkg)));
        store.name_index.insert(name.to_string(), id);
        for nick in nicknames {
            store.name_index.insert((*nick).to_string(), id);
        }

        Ok(handle)
    }

    pub fn add_package_local_nickname(
        &self,
        package: BlissVal,
        local_nickname: &str,
        actual_package: BlissVal,
    ) -> Result<(), BlissError> {
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("local nickname add"))?;
        let owner = lookup_package_arc(&store, package)?;
        lookup_package_arc(&store, actual_package)?;
        drop(store);

        let mut owner = owner
            .write()
            .map_err(|_| lock_poisoned_error("local nickname add"))?;
        match owner.local_nicknames.get(local_nickname).copied() {
            Some(existing) if existing != pkg_id(actual_package) => Err(BlissError::PackageError(
                format!("Local nickname {:?} already points elsewhere", local_nickname),
            )),
            _ => {
                owner
                    .local_nicknames
                    .insert(local_nickname.to_string(), pkg_id(actual_package));
                Ok(())
            }
        }
    }

    pub fn remove_package_local_nickname(
        &self,
        package: BlissVal,
        local_nickname: &str,
    ) -> Result<Option<BlissVal>, BlissError> {
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("local nickname removal"))?;
        let owner = lookup_package_arc(&store, package)?;
        drop(store);
        let mut owner = owner
            .write()
            .map_err(|_| lock_poisoned_error("local nickname removal"))?;
        Ok(owner
            .local_nicknames
            .remove(local_nickname)
            .map(BlissVal::from_fixnum))
    }

    pub fn package_local_nicknames(
        &self,
        package: BlissVal,
    ) -> Result<HashMap<String, BlissVal>, BlissError> {
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("local nickname read"))?;
        let package = lookup_package_arc(&store, package)?;
        let package = package
            .read()
            .map_err(|_| lock_poisoned_error("local nickname read"))?;
        Ok(package
            .local_nicknames
            .iter()
            .map(|(name, &id)| (name.clone(), BlissVal::from_fixnum(id)))
            .collect())
    }

    pub fn package_locally_nicknamed_by_list(
        &self,
        package: BlissVal,
    ) -> Result<Vec<BlissVal>, BlissError> {
        let target = pkg_id(package);
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("local nickname reverse lookup"))?;
        let mut out = Vec::new();
        for (&id, pkg) in &store.packages {
            let pkg = pkg
                .read()
                .map_err(|_| lock_poisoned_error("local nickname reverse lookup"))?;
            if pkg.local_nicknames.values().any(|&other| other == target) {
                out.push(BlissVal::from_fixnum(id));
            }
        }
        Ok(out)
    }

    fn ensure_package(
        &mut self,
        name: &str,
        nicknames: &[&str],
        use_list: &[&str],
    ) -> Result<BlissVal, BlissError> {
        if let Some(existing) = self.find_package(name) {
            self.ensure_package_aliases(existing, name, nicknames)?;
            self.ensure_package_uses(existing, use_list)?;
            return Ok(existing);
        }

        for &nickname in nicknames {
            if let Some(existing) = self.find_package(nickname) {
                self.ensure_package_aliases(existing, name, nicknames)?;
                self.ensure_package_uses(existing, use_list)?;
                return Ok(existing);
            }
        }

        self.make_package(name, nicknames, use_list)
    }

    fn bootstrap_initialized(&self) -> Result<bool, BlissError> {
        let Some(common_lisp) = self.find_package("COMMON-LISP") else {
            return Ok(false);
        };
        let Some(common_lisp_user) = self.find_package("COMMON-LISP-USER") else {
            return Ok(false);
        };
        let Some(bliss_internal) = self.find_package("BLISS-INTERNAL") else {
            return Ok(false);
        };
        let Some(bliss_ext) = self.find_package("BLISS-EXT") else {
            return Ok(false);
        };
        if self.find_package("KEYWORD").is_none() {
            return Ok(false);
        }

        Ok(self.find_package("CL") == Some(common_lisp)
            && self.find_package("CL-USER") == Some(common_lisp_user)
            && self.find_package("BI") == Some(bliss_internal)
            && self.package_uses_package(bliss_internal, common_lisp)?
            && self.package_uses_package(bliss_ext, common_lisp)?
            && self.package_uses_package(common_lisp_user, common_lisp)?
            && self.package_uses_package(common_lisp_user, bliss_ext)?)
    }

    fn ensure_package_aliases(
        &mut self,
        package: BlissVal,
        canonical_name: &str,
        nicknames: &[&str],
    ) -> Result<(), BlissError> {
        self.ensure_package_alias(canonical_name, package)?;
        for &nickname in nicknames {
            self.ensure_package_alias(nickname, package)?;
        }
        Ok(())
    }

    fn ensure_package_uses(
        &mut self,
        package: BlissVal,
        use_list: &[&str],
    ) -> Result<(), BlissError> {
        let resolved = use_list
            .iter()
            .map(|name| {
                self.find_package(name).ok_or_else(|| {
                    BlissError::PackageError(format!("Package {:?} not found for use-list", name))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        use_package(&resolved, package)
    }

    fn package_uses_package(
        &self,
        package: BlissVal,
        used_package: BlissVal,
    ) -> Result<bool, BlissError> {
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("package use check"))?;
        let package = lookup_package_arc(&store, package)?;
        let package = package
            .read()
            .map_err(|_| lock_poisoned_error("package use check"))?;
        let used_id = pkg_id(used_package);
        Ok(package.use_list.iter().any(|handle| pkg_id(*handle) == used_id))
    }

    fn ensure_package_alias(&mut self, alias: &str, package: BlissVal) -> Result<(), BlissError> {
        let id = pkg_id(package);
        let mut store = self
            .store
            .state
            .write()
            .map_err(|_| lock_poisoned_error("package aliasing"))?;
        match store.name_index.get(alias).copied() {
            Some(existing) if existing == id => Ok(()),
            Some(_) => Err(BlissError::PackageError(format!(
                "Package alias {:?} conflicts with an existing package",
                alias
            ))),
            None => {
                store.name_index.insert(alias.to_string(), id);
                if let Some(pkg) = store.packages.get(&id) {
                    let mut pkg = pkg
                        .write()
                        .map_err(|_| lock_poisoned_error("package aliasing"))?;
                    if alias != pkg.name && !pkg.nicknames.iter().any(|nick| nick == alias) {
                        pkg.nicknames.push(alias.to_string());
                    }
                }
                Ok(())
            }
        }
    }

    /// Delete a package.
    pub fn delete_package(&mut self, name: &str) -> Result<(), BlissError> {
        let mut store = self
            .store
            .state
            .write()
            .map_err(|_| lock_poisoned_error("package deletion"))?;
        let id = store
            .name_index
            .get(name)
            .copied()
            .ok_or_else(|| BlissError::PackageError(format!("Package {:?} not found", name)))?;

        let pkg = store
            .packages
            .remove(&id)
            .ok_or_else(|| BlissError::PackageError("Package not found".to_string()))?;
        let pkg = pkg
            .read()
            .map_err(|_| lock_poisoned_error("package deletion"))?;
        store.name_index.remove(&pkg.name);
        for nick in &pkg.nicknames {
            store.name_index.remove(nick);
        }
        drop(pkg);

        for package in store.packages.values() {
            let mut package = package
                .write()
                .map_err(|_| lock_poisoned_error("package deletion"))?;
            package.use_list.retain(|handle| pkg_id(*handle) != id);
            package.local_nicknames.retain(|_, other| *other != id);
        }
        Ok(())
    }

    /// List all packages.
    pub fn list_all_packages(&self) -> Vec<BlissVal> {
        self.store
            .state
            .read()
            .map(|store| {
                store
                    .packages
                    .keys()
                    .copied()
                    .map(BlissVal::from_fixnum)
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for PackageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PackageRegistry {
    fn drop(&mut self) {
        CURRENT_STORE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &self.store))
            {
                *slot = self.previous_store.take();
            }
        });
    }
}

impl Drop for ActivePackageRegistryGuard {
    fn drop(&mut self) {
        let previous_store = self.previous_store.take();
        let _ = swap_current_store(previous_store);
    }
}

// ── Package operations ─────────────────────────────────────────────

/// Intern a symbol in a package.
pub fn intern(name: &str, package: BlissVal) -> Result<(BlissVal, InternStatus), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("intern"))?;
    let package_handle = lookup_package_arc(&registry, package)?;
    let package_guard = package_handle
        .write()
        .map_err(|_| lock_poisoned_error("intern"))?;

    if let Some(&sym) = package_guard.internal_symbols.get(name) {
        return Ok((sym, InternStatus::Internal));
    }
    if let Some(&sym) = package_guard.external_symbols.get(name) {
        return Ok((sym, InternStatus::External));
    }

    let visible_packages = package_guard.use_list.clone();
    drop(package_guard);
    for used_pkg_handle in visible_packages {
        if let Some(used_pkg) = registry.packages.get(&pkg_id(used_pkg_handle)) {
            let used_pkg = used_pkg
                .read()
                .map_err(|_| lock_poisoned_error("intern"))?;
            if let Some(&sym) = used_pkg.external_symbols.get(name) {
                return Ok((sym, InternStatus::Inherited));
            }
        }
    }

    let package_handle = lookup_package_arc(&registry, package)?;
    let mut package = package_handle
        .write()
        .map_err(|_| lock_poisoned_error("intern"))?;
    if let Some(&sym) = package.internal_symbols.get(name) {
        return Ok((sym, InternStatus::Internal));
    }
    if let Some(&sym) = package.external_symbols.get(name) {
        return Ok((sym, InternStatus::External));
    }
    let sym = alloc_symbol();
    package.internal_symbols.insert(name.to_string(), sym);
    Ok((sym, InternStatus::New))
}

/// Status returned by `intern`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternStatus {
    Internal,
    External,
    Inherited,
    New,
}

/// Find a symbol without interning it.
pub fn find_symbol(
    name: &str,
    package: BlissVal,
) -> Result<Option<(BlissVal, InternStatus)>, BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("find-symbol"))?;
    let package = lookup_package_arc(&registry, package)?;
    let package = package
        .read()
        .map_err(|_| lock_poisoned_error("find-symbol"))?;

    if let Some(&sym) = package.internal_symbols.get(name) {
        return Ok(Some((sym, InternStatus::Internal)));
    }
    if let Some(&sym) = package.external_symbols.get(name) {
        return Ok(Some((sym, InternStatus::External)));
    }

    for used_pkg_handle in &package.use_list {
        if let Some(used_pkg) = registry.packages.get(&pkg_id(*used_pkg_handle)) {
            let used_pkg = used_pkg
                .read()
                .map_err(|_| lock_poisoned_error("find-symbol"))?;
            if let Some(&sym) = used_pkg.external_symbols.get(name) {
                return Ok(Some((sym, InternStatus::Inherited)));
            }
        }
    }

    Ok(None)
}

/// Export symbols from a package.
pub fn export(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("export"))?;
    let package = lookup_package_arc(&registry, package)?;
    let mut package = package
        .write()
        .map_err(|_| lock_poisoned_error("export"))?;

    for &sym in symbols {
        let sym_name = package
            .internal_symbols
            .iter()
            .find(|(_, v)| **v == sym)
            .map(|(k, _)| k.clone());

        if let Some(name) = sym_name {
            package.internal_symbols.remove(&name);
            package.external_symbols.insert(name, sym);
        } else if !package.external_symbols.values().any(|&v| v == sym) {
            return Err(BlissError::PackageError(
                "Symbol not accessible in package".to_string(),
            ));
        }
    }
    Ok(())
}

/// Unexport symbols from a package.
pub fn unexport(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("unexport"))?;
    let package = lookup_package_arc(&registry, package)?;
    let mut package = package
        .write()
        .map_err(|_| lock_poisoned_error("unexport"))?;

    for &sym in symbols {
        let sym_name = package
            .external_symbols
            .iter()
            .find(|(_, v)| **v == sym)
            .map(|(k, _)| k.clone());
        if let Some(name) = sym_name {
            package.external_symbols.remove(&name);
            package.internal_symbols.insert(name, sym);
        }
    }
    Ok(())
}

/// Unintern a symbol from a package.
pub fn unintern(symbol: BlissVal, package: BlissVal) -> Result<bool, BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("unintern"))?;
    let package = lookup_package_arc(&registry, package)?;
    let mut package = package
        .write()
        .map_err(|_| lock_poisoned_error("unintern"))?;

    let removed_internal = package
        .internal_symbols
        .iter()
        .find(|(_, v)| **v == symbol)
        .map(|(k, _)| k.clone());
    if let Some(name) = removed_internal {
        package.internal_symbols.remove(&name);
        package.shadowing_symbols.remove(&name);
        return Ok(true);
    }

    let removed_external = package
        .external_symbols
        .iter()
        .find(|(_, v)| **v == symbol)
        .map(|(k, _)| k.clone());
    if let Some(name) = removed_external {
        package.external_symbols.remove(&name);
        package.shadowing_symbols.remove(&name);
        return Ok(true);
    }

    Ok(false)
}

/// Use a package (add to use-list).
pub fn use_package(packages: &[BlissVal], target: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("use-package"))?;
    for &pkg_handle in packages {
        lookup_package_arc(&registry, pkg_handle)?;
    }
    let target = lookup_package_arc(&registry, target)?;
    let mut target = target
        .write()
        .map_err(|_| lock_poisoned_error("use-package"))?;
    for &pkg_handle in packages {
        if !target.use_list.contains(&pkg_handle) {
            target.use_list.push(pkg_handle);
        }
    }
    Ok(())
}

/// Unuse a package.
pub fn unuse_package(packages: &[BlissVal], target: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("unuse-package"))?;
    let target = lookup_package_arc(&registry, target)?;
    let mut target = target
        .write()
        .map_err(|_| lock_poisoned_error("unuse-package"))?;
    for &pkg_handle in packages {
        target.use_list.retain(|&x| x != pkg_handle);
    }
    Ok(())
}

/// Import symbols into a package.
pub fn import(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("import"))?;
    let target = lookup_package_arc(&registry, package)?;
    let resolved = symbols
        .iter()
        .map(|&sym| {
            find_symbol_name_in_store(&registry, sym)?.ok_or_else(|| {
                BlissError::PackageError("Symbol not found in any package".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut target = target
        .write()
        .map_err(|_| lock_poisoned_error("import"))?;

    for (&sym, name) in symbols.iter().zip(resolved) {
        if let Some(&existing) = target.internal_symbols.get(&name) {
            if existing != sym {
                return Err(BlissError::PackageError(format!(
                    "Name conflict for symbol {:?}",
                    name
                )));
            }
            continue;
        }
        if let Some(&existing) = target.external_symbols.get(&name) {
            if existing != sym {
                return Err(BlissError::PackageError(format!(
                    "Name conflict for symbol {:?}",
                    name
                )));
            }
            continue;
        }
        target.internal_symbols.insert(name, sym);
    }
    Ok(())
}

/// Shadowing import.
pub fn shadowing_import(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("shadowing-import"))?;
    let target = lookup_package_arc(&registry, package)?;
    let resolved = symbols
        .iter()
        .map(|&sym| {
            find_symbol_name_in_store(&registry, sym)?.ok_or_else(|| {
                BlissError::PackageError("Symbol not found in any package".to_string())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut target = target
        .write()
        .map_err(|_| lock_poisoned_error("shadowing-import"))?;

    for (&sym, name) in symbols.iter().zip(resolved) {
        target.internal_symbols.remove(&name);
        target.external_symbols.remove(&name);
        target.internal_symbols.insert(name.clone(), sym);
        target.shadowing_symbols.insert(name);
    }
    Ok(())
}

/// Shadow symbols.
pub fn shadow(names: &[&str], package: BlissVal) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("shadow"))?;
    let target = lookup_package_arc(&registry, package)?;
    let mut target = target
        .write()
        .map_err(|_| lock_poisoned_error("shadow"))?;

    for &name in names {
        if !target.internal_symbols.contains_key(name) && !target.external_symbols.contains_key(name)
        {
            let sym = alloc_symbol();
            target.internal_symbols.insert(name.to_string(), sym);
        }
        target.shadowing_symbols.insert(name.to_string());
    }
    Ok(())
}
