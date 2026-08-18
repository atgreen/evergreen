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
///
/// Based at `1 << 40` so package handle ids occupy a reserved high range that
/// cannot collide with the low-range CLOS class/generic-function/method and
/// macro meta-handles (which count up from small values). Discrimination is by
/// registry membership, but keeping the ranges disjoint means a stray id is
/// never *ambiguously* claimable by two subsystems (bliss-bhs Stage 4 / dx6).
const PACKAGE_ID_BASE: i64 = 1 << 40;
static NEXT_PACKAGE_ID: AtomicI64 = AtomicI64::new(PACKAGE_ID_BASE);

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
    // Opaque metaobject handle, not a fixnum: keeps package handles off the
    // fixnum tag so a plain integer (or a same-id symbol handle) can't collide
    // with a package in the registry. See bliss-dx6.1.
    (id, BlissVal::from_meta_handle(id))
}

/// The shared heap-resident symbol for `bare_name` in package `pkg_name`
/// (bliss-jtc.6 Stage D). The registry key is package-qualified so the same name
/// in two packages yields *distinct* symbols (CL per-package identity), and the
/// symbol's home-package cell is set to the shared PACKAGE object — retiring the
/// former anonymous fixnum handles.
fn alloc_symbol(pkg_name: &str, bare_name: &str) -> BlissVal {
    let idx = bliss_rt::symbols::intern(&format!("{pkg_name}::{bare_name}"));
    bliss_rt::symbols::set_symbol_package(idx, bliss_rt::packages::find_or_create(pkg_name));
    BlissVal::from_symbol_index(idx)
}

/// Extract the package ID from a BlissVal handle.
fn pkg_id(handle: BlissVal) -> i64 {
    handle.as_meta_handle_id()
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
            .map(BlissVal::from_meta_handle)
    }

    /// Resolve a package designator relative to another package, honoring package-local nicknames first.
    pub fn find_package_from(&self, package: BlissVal, name: &str) -> Option<BlissVal> {
        let store = self.store.state.read().ok()?;
        let package = store.packages.get(&pkg_id(package))?.clone();
        let package = package.read().ok()?;
        if let Some(&id) = package.local_nicknames.get(name) {
            return Some(BlissVal::from_meta_handle(id));
        }
        store
            .name_index
            .get(name)
            .copied()
            .map(BlissVal::from_meta_handle)
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
                Some(&id) => resolved_uses.push(BlissVal::from_meta_handle(id)),
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
            .map(BlissVal::from_meta_handle))
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
            .map(|(name, &id)| (name.clone(), BlissVal::from_meta_handle(id)))
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
                out.push(BlissVal::from_meta_handle(id));
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
                    .map(BlissVal::from_meta_handle)
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
    let sym = alloc_symbol(&package.name, name);
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
            let sym = alloc_symbol(&target.name, name);
            target.internal_symbols.insert(name.to_string(), sym);
        }
        target.shadowing_symbols.insert(name.to_string());
    }
    Ok(())
}

// ── Free-function API over the active thread-local registry ──────────
//
// The interpreter (crates/bliss) keeps exactly one PackageRegistry alive and
// active for the session (see `PackageRegistry::new`/`activate`) and drives all
// package operations through these `current_store()` free functions, so it never
// has to hold a registry handle or reason about the activation guard's Drop.
// They mirror the same-named `PackageRegistry` methods but resolve the store
// from the thread-local each call.

/// Find a package by global name or nickname in the active registry.
pub fn find_package(name: &str) -> Option<BlissVal> {
    let store = current_store().ok()?;
    let g = store.state.read().ok()?;
    g.name_index
        .get(name)
        .copied()
        .map(BlissVal::from_meta_handle)
}

/// True if `value` is a live package handle in the active registry. Cheap
/// discriminator for PACKAGEP / TYPEP 'PACKAGE and the printer.
pub fn is_package(value: BlissVal) -> bool {
    if !value.is_meta_handle() {
        return false;
    }
    current_store()
        .ok()
        .and_then(|store| {
            let g = store.state.read().ok()?;
            Some(g.packages.contains_key(&value.as_meta_handle_id()))
        })
        .unwrap_or(false)
}

/// The canonical name of a package handle.
pub fn package_name(package: BlissVal) -> Option<String> {
    if !package.is_meta_handle() {
        return None;
    }
    let store = current_store().ok()?;
    let g = store.state.read().ok()?;
    let pkg = g.packages.get(&pkg_id(package))?;
    let name = pkg.read().ok()?.name.clone();
    Some(name)
}

/// The nicknames of a package handle (excludes the canonical name).
pub fn package_nicknames(package: BlissVal) -> Vec<String> {
    (|| -> Option<Vec<String>> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        Some(pkg.read().ok()?.nicknames.clone())
    })()
    .unwrap_or_default()
}

/// The use-list of a package handle, as package handles.
pub fn package_use_list(package: BlissVal) -> Vec<BlissVal> {
    (|| -> Option<Vec<BlissVal>> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        Some(pkg.read().ok()?.use_list.clone())
    })()
    .unwrap_or_default()
}

/// List all packages in the active registry.
pub fn list_all_packages() -> Vec<BlissVal> {
    current_store()
        .ok()
        .and_then(|store| {
            let g = store.state.read().ok()?;
            Some(
                g.packages
                    .keys()
                    .copied()
                    .map(BlissVal::from_meta_handle)
                    .collect(),
            )
        })
        .unwrap_or_default()
}

/// Symbols *present* in the package (its own internal + external).
pub fn present_symbols(package: BlissVal) -> Vec<BlissVal> {
    (|| -> Option<Vec<BlissVal>> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        let pkg = pkg.read().ok()?;
        let mut out: Vec<BlissVal> = pkg.internal_symbols.values().copied().collect();
        out.extend(pkg.external_symbols.values().copied());
        Some(out)
    })()
    .unwrap_or_default()
}

/// The package's own *external* (exported) symbols. For DO-EXTERNAL-SYMBOLS.
pub fn external_symbols_of(package: BlissVal) -> Vec<BlissVal> {
    (|| -> Option<Vec<BlissVal>> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        Some(pkg.read().ok()?.external_symbols.values().copied().collect())
    })()
    .unwrap_or_default()
}

/// Symbols *accessible* in the package: present here plus inherited externals
/// from each used package (one level). For DO-SYMBOLS.
pub fn accessible_symbols(package: BlissVal) -> Vec<BlissVal> {
    (|| -> Option<Vec<BlissVal>> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        let pkg = pkg.read().ok()?;
        let mut out: Vec<BlissVal> = pkg.internal_symbols.values().copied().collect();
        out.extend(pkg.external_symbols.values().copied());
        for used in &pkg.use_list {
            if let Some(u) = g.packages.get(&pkg_id(*used)) {
                if let Ok(u) = u.read() {
                    out.extend(u.external_symbols.values().copied());
                }
            }
        }
        Some(out)
    })()
    .unwrap_or_default()
}

/// The symbol *present* under `bare_name` in `package` (its own internal or
/// external map only — NOT inherited from used packages). `None` if absent.
pub fn find_present_symbol(package: BlissVal, bare_name: &str) -> Option<BlissVal> {
    let store = current_store().ok()?;
    let g = store.state.read().ok()?;
    let pkg = g.packages.get(&pkg_id(package))?;
    let pkg = pkg.read().ok()?;
    pkg.internal_symbols
        .get(bare_name)
        .or_else(|| pkg.external_symbols.get(bare_name))
        .copied()
}

/// True if `name` is externally accessible (present as an external, or the name
/// of an inherited symbol) in `package`. For the `:EXTERNAL` status of
/// FIND-SYMBOL and export bookkeeping.
pub fn is_external_symbol(package: BlissVal, bare_name: &str) -> bool {
    (|| -> Option<bool> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        Some(pkg.read().ok()?.external_symbols.contains_key(bare_name))
    })()
    .unwrap_or(false)
}

/// Insert an *already-interned* symbol into a package's index (used to record a
/// symbol the interpreter allocated through its own naming scheme, and to seed
/// standard-package symbols). `external` selects the external vs internal map.
/// Idempotent; promoting an internal symbol to external is allowed.
pub fn add_symbol(
    package: BlissVal,
    bare_name: &str,
    sym: BlissVal,
    external: bool,
) -> Result<(), BlissError> {
    let store = current_store()?;
    let g = store.state.read().map_err(|_| lock_poisoned_error("add_symbol"))?;
    let pkg = lookup_package_arc(&g, package)?;
    drop(g);
    let mut pkg = pkg.write().map_err(|_| lock_poisoned_error("add_symbol"))?;
    if external {
        pkg.internal_symbols.remove(bare_name);
        pkg.external_symbols.insert(bare_name.to_string(), sym);
    } else if !pkg.external_symbols.contains_key(bare_name) {
        pkg.internal_symbols.insert(bare_name.to_string(), sym);
    }
    Ok(())
}

/// Create a package in the active registry (see `PackageRegistry::make_package`).
pub fn make_package(
    name: &str,
    nicknames: &[&str],
    use_list: &[&str],
) -> Result<BlissVal, BlissError> {
    let store = current_store()?;
    let mut g = store
        .state
        .write()
        .map_err(|_| lock_poisoned_error("make_package"))?;
    if g.name_index.contains_key(name) {
        return Err(BlissError::PackageError(format!(
            "Package named {:?} already exists",
            name
        )));
    }
    for nick in nicknames {
        if g.name_index.contains_key(*nick) {
            return Err(BlissError::PackageError(format!(
                "Nickname {:?} conflicts with an existing package",
                nick
            )));
        }
    }
    let mut resolved_uses = Vec::new();
    for use_name in use_list {
        match g.name_index.get(*use_name) {
            Some(&id) => resolved_uses.push(BlissVal::from_meta_handle(id)),
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
    g.packages.insert(id, Arc::new(RwLock::new(pkg)));
    g.name_index.insert(name.to_string(), id);
    for nick in nicknames {
        g.name_index.insert((*nick).to_string(), id);
    }
    Ok(handle)
}

/// Add `nickname` to `package` (idempotent). Fails if the nickname already names
/// a *different* package.
pub fn add_nickname(package: BlissVal, nickname: &str) -> Result<(), BlissError> {
    let store = current_store()?;
    let mut g = store
        .state
        .write()
        .map_err(|_| lock_poisoned_error("add_nickname"))?;
    let id = pkg_id(package);
    match g.name_index.get(nickname).copied() {
        Some(existing) if existing == id => return Ok(()),
        Some(_) => {
            return Err(BlissError::PackageError(format!(
                "Nickname {:?} conflicts with an existing package",
                nickname
            )));
        }
        None => {}
    }
    let pkg = g
        .packages
        .get(&id)
        .cloned()
        .ok_or_else(|| BlissError::PackageError("Package not found".to_string()))?;
    {
        let mut pkg = pkg.write().map_err(|_| lock_poisoned_error("add_nickname"))?;
        if !pkg.nicknames.iter().any(|n| n == nickname) {
            pkg.nicknames.push(nickname.to_string());
        }
    }
    g.name_index.insert(nickname.to_string(), id);
    Ok(())
}

/// Delete a package by name from the active registry.
pub fn delete_package(name: &str) -> Result<(), BlissError> {
    let store = current_store()?;
    let mut g = store
        .state
        .write()
        .map_err(|_| lock_poisoned_error("delete_package"))?;
    let Some(id) = g.name_index.get(name).copied() else {
        return Ok(());
    };
    if let Some(pkg) = g.packages.remove(&id) {
        if let Ok(pkg) = pkg.read() {
            let names: Vec<String> = std::iter::once(pkg.name.clone())
                .chain(pkg.nicknames.iter().cloned())
                .collect();
            drop(pkg);
            for n in names {
                g.name_index.remove(&n);
            }
        }
    }
    for pkg in g.packages.values() {
        if let Ok(mut pkg) = pkg.write() {
            pkg.use_list.retain(|handle| pkg_id(*handle) != id);
            pkg.local_nicknames.retain(|_, other| *other != id);
        }
    }
    Ok(())
}

/// Rename a package: its old name and old nicknames are removed and replaced by
/// `new_name` + `new_nicknames` (CL RENAME-PACKAGE semantics). Returns the handle.
pub fn rename_package(
    package: BlissVal,
    new_name: &str,
    new_nicknames: &[&str],
) -> Result<BlissVal, BlissError> {
    let store = current_store()?;
    let mut g = store
        .state
        .write()
        .map_err(|_| lock_poisoned_error("rename_package"))?;
    let id = pkg_id(package);
    if let Some(&existing) = g.name_index.get(new_name) {
        if existing != id {
            return Err(BlissError::PackageError(format!(
                "Package named {:?} already exists",
                new_name
            )));
        }
    }
    for nick in new_nicknames {
        if let Some(&existing) = g.name_index.get(*nick) {
            if existing != id {
                return Err(BlissError::PackageError(format!(
                    "Nickname {:?} conflicts with an existing package",
                    nick
                )));
            }
        }
    }
    let pkg_arc = g
        .packages
        .get(&id)
        .cloned()
        .ok_or_else(|| BlissError::PackageError("Package not found".to_string()))?;
    {
        let pkg = pkg_arc.read().map_err(|_| lock_poisoned_error("rename_package"))?;
        g.name_index.remove(&pkg.name);
        for nick in &pkg.nicknames {
            g.name_index.remove(nick);
        }
    }
    {
        let mut pkg = pkg_arc
            .write()
            .map_err(|_| lock_poisoned_error("rename_package"))?;
        pkg.name = new_name.to_string();
        pkg.nicknames = new_nicknames.iter().map(|s| s.to_string()).collect();
    }
    g.name_index.insert(new_name.to_string(), id);
    for nick in new_nicknames {
        g.name_index.insert((*nick).to_string(), id);
    }
    Ok(BlissVal::from_meta_handle(id))
}

/// Add `used` to `target`'s use-list by *name* (both must already exist).
pub fn use_package_by_name(target: BlissVal, used_name: &str) -> Result<(), BlissError> {
    let used = find_package(used_name).ok_or_else(|| {
        BlissError::PackageError(format!("Package {:?} not found for use-list", used_name))
    })?;
    use_package(&[used], target)
}
