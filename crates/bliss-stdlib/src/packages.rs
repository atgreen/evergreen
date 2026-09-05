//! Package system and bootstrap.
//!
//! Manages the CL package registry, package operations, and
//! the standard package layout. See spec §5.1.

use bliss_rt::error::BlissError;
use bliss_rt::lock_order::{LockLevel, OrderedRwLock};
use bliss_rt::value::BlissVal;

use std::cell::RefCell;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::hashtable::{
    HashTest, MakeHashTableOptions, gethash, hash_table_entries, make_hash_table, remhash,
    set_gethash,
};
use crate::streams::make_lisp_string;

// Package/symbol tables are keyed by internal package ids and interned symbol
// names (never adversarial input), and `package_name`/`find-symbol` are among
// the hottest functions in an asdf:load-system profile. The std default
// `RandomState` (SipHash) both dominated that profile and made iteration order
// nondeterministic per process. Use a small FxHash-style multiplicative hasher:
// far cheaper, and deterministic (a fixed seed) — which also removes the
// run-to-run nondeterminism that made babel loads flaky (bliss-nad).
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, i: u64) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            self.add(u64::from_le_bytes(bytes[..8].try_into().unwrap()));
            bytes = &bytes[8..];
        }
        for &b in bytes {
            self.add(b as u64);
        }
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.add(i as u64);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

type FxBuild = BuildHasherDefault<FxHasher>;
type HashMap<K, V> = std::collections::HashMap<K, V, FxBuild>;
type HashSet<T> = std::collections::HashSet<T, FxBuild>;

// ── Internal package data ─────────────────────────────────────────

thread_local! {
    static CURRENT_STORE: RefCell<Option<Arc<RegistryStore>>> = const { RefCell::new(None) };
}

fn swap_current_store(new_store: Option<Arc<RegistryStore>>) -> Option<Arc<RegistryStore>> {
    CURRENT_STORE.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), new_store))
}

struct RegistryStore {
    state: OrderedRwLock<PackageStore>,
}

struct PackageStore {
    packages: HashMap<u64, Arc<OrderedRwLock<Package>>>,
}

impl PackageStore {
    fn new() -> Self {
        Self {
            packages: HashMap::default(),
        }
    }
}

#[derive(Clone)]
struct Package {
    object: BlissVal,
    name: String,
    nicknames: Vec<String>,
    local_nicknames: HashMap<String, BlissVal>,
    internal_symbols: PackageSymbolTable,
    external_symbols: PackageSymbolTable,
    shadowing_symbols: HashSet<String>,
    use_list: Vec<BlissVal>,
}

impl Package {
    fn new(name: &str, object: BlissVal) -> Result<Self, BlissError> {
        let (internal, external) = match bliss_rt::packages::symbol_tables(object) {
            Some((internal, external))
                if crate::hashtable::hash_table_p(internal)
                    && crate::hashtable::hash_table_p(external) =>
            {
                (internal, external)
            }
            _ => (make_package_symbol_table()?, make_package_symbol_table()?),
        };
        bliss_rt::packages::set_symbol_tables(object, internal, external)?;
        Ok(Self {
            object,
            name: name.to_string(),
            nicknames: Vec::new(),
            local_nicknames: HashMap::default(),
            internal_symbols: PackageSymbolTable(internal),
            external_symbols: PackageSymbolTable(external),
            shadowing_symbols: HashSet::default(),
            use_list: Vec::new(),
        })
    }
}

#[derive(Clone, Copy)]
struct PackageSymbolTable(BlissVal);

fn make_package_symbol_table() -> Result<BlissVal, BlissError> {
    make_hash_table(&MakeHashTableOptions {
        test: HashTest::Equal,
        synchronized: true,
        ..MakeHashTableOptions::default()
    })
}

impl PackageSymbolTable {
    fn get(self, name: &str) -> Option<BlissVal> {
        gethash(make_lisp_string(name), self.0, bliss_rt::value::NIL)
            .ok()
            .and_then(|(value, present)| present.then_some(value))
    }

    fn contains_key(self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn insert(self, name: &str, symbol: BlissVal) {
        set_gethash(make_lisp_string(name), self.0, symbol)
            .expect("package symbol table must remain a valid hash table");
    }

    fn remove(self, name: &str) -> Option<BlissVal> {
        let old = self.get(name);
        if old.is_some() {
            remhash(make_lisp_string(name), self.0)
                .expect("package symbol table must remain a valid hash table");
        }
        old
    }

    fn entries(self) -> Vec<(String, BlissVal)> {
        hash_table_entries(self.0)
            .expect("package symbol table must remain a valid hash table")
            .into_iter()
            .map(|(name, symbol)| (name.as_string(), symbol))
            .collect()
    }

    fn values(self) -> Vec<BlissVal> {
        self.entries()
            .into_iter()
            .map(|(_, symbol)| symbol)
            .collect()
    }
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
fn pkg_id(handle: BlissVal) -> u64 {
    handle.0
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
) -> Result<Arc<OrderedRwLock<Package>>, BlissError> {
    store
        .packages
        .get(&pkg_id(package))
        .cloned()
        .ok_or_else(|| BlissError::PackageError("Package not found".to_string()))
}

fn lookup_named_package(store: &PackageStore, name: &str) -> Option<BlissVal> {
    let package = bliss_rt::packages::find(name)?;
    store
        .packages
        .contains_key(&pkg_id(package))
        .then_some(package)
}

fn find_symbol_name_in_store(
    store: &PackageStore,
    sym: BlissVal,
) -> Result<Option<String>, BlissError> {
    for package in store.packages.values() {
        let package = package
            .read()
            .map_err(|_| lock_poisoned_error("symbol lookup"))?;
        for (name, val) in package.internal_symbols.entries() {
            if val == sym {
                return Ok(Some(name));
            }
        }
        for (name, val) in package.external_symbols.entries() {
            if val == sym {
                return Ok(Some(name));
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
            state: OrderedRwLock::new(
                LockLevel::PackageRegistry,
                2,
                "stdlib package registry",
                PackageStore::new(),
            ),
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

        // NOTE: the ANSI special operators are NOT seeded here.
        // seed_ansi_special_operators interns symbols on the shared GC heap,
        // and this initializer doubles as a heap-free store fixture for the
        // package unit tests, which run in parallel — concurrent interning
        // from test threads segfaults. The host runtime seeds after creating
        // its registry (cli.rs seed_standard_packages_registry).
        Ok(())
    }

    /// Find a package by global name or nickname.
    pub fn find_package(&self, name: &str) -> Option<BlissVal> {
        let store = self.store.state.read().ok()?;
        lookup_named_package(&store, name)
    }

    /// Resolve a package designator relative to another package, honoring package-local nicknames first.
    pub fn find_package_from(&self, package: BlissVal, name: &str) -> Option<BlissVal> {
        let store = self.store.state.read().ok()?;
        let package = store.packages.get(&pkg_id(package))?.clone();
        let package = package.read().ok()?;
        if let Some(&actual) = package.local_nicknames.get(name) {
            return Some(actual);
        }
        drop(package);
        lookup_named_package(&store, name)
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

        if lookup_named_package(&store, name).is_some() {
            return Err(BlissError::PackageError(format!(
                "Package named {:?} already exists",
                name
            )));
        }
        for nick in nicknames {
            if lookup_named_package(&store, nick).is_some() {
                return Err(BlissError::PackageError(format!(
                    "Nickname {:?} conflicts with an existing package",
                    nick
                )));
            }
        }

        let mut resolved_uses = Vec::new();
        for use_name in use_list {
            match lookup_named_package(&store, use_name) {
                Some(package) => resolved_uses.push(package),
                None => {
                    return Err(BlissError::PackageError(format!(
                        "Package {:?} not found for use-list",
                        use_name
                    )));
                }
            }
        }

        let handle = bliss_rt::packages::find_or_create(name);
        let id = pkg_id(handle);
        let mut pkg = Package::new(name, handle)?;
        pkg.nicknames = nicknames.iter().map(|s| s.to_string()).collect();
        pkg.use_list = resolved_uses;

        store.packages.insert(
            id,
            Arc::new(OrderedRwLock::new(
                LockLevel::Package,
                id.max(1),
                "package object",
                pkg,
            )),
        );
        for nick in nicknames {
            bliss_rt::packages::add_nickname(name, nick);
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
            Some(existing) if existing != actual_package => Err(BlissError::PackageError(format!(
                "Local nickname {:?} already points elsewhere",
                local_nickname
            ))),
            _ => {
                owner
                    .local_nicknames
                    .insert(local_nickname.to_string(), actual_package);
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
        Ok(owner.local_nicknames.remove(local_nickname))
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
            .map(|(name, &package)| (name.clone(), package))
            .collect())
    }

    pub fn package_locally_nicknamed_by_list(
        &self,
        package: BlissVal,
    ) -> Result<Vec<BlissVal>, BlissError> {
        let target = package;
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("local nickname reverse lookup"))?;
        let mut out = Vec::new();
        for pkg in store.packages.values() {
            let pkg = pkg
                .read()
                .map_err(|_| lock_poisoned_error("local nickname reverse lookup"))?;
            if pkg.local_nicknames.values().any(|&other| other == target) {
                out.push(pkg.object);
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
        Ok(package.use_list.contains(&used_package))
    }

    fn ensure_package_alias(&mut self, alias: &str, package: BlissVal) -> Result<(), BlissError> {
        let id = pkg_id(package);
        let store = self
            .store
            .state
            .read()
            .map_err(|_| lock_poisoned_error("package aliasing"))?;
        match lookup_named_package(&store, alias) {
            Some(existing) if existing == package => Ok(()),
            Some(_) => Err(BlissError::PackageError(format!(
                "Package alias {:?} conflicts with an existing package",
                alias
            ))),
            None => {
                let primary = bliss_rt::packages::package_name(package).unwrap_or_default();
                bliss_rt::packages::add_nickname(&primary, alias);
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
        let handle = lookup_named_package(&store, name)
            .ok_or_else(|| BlissError::PackageError(format!("Package {:?} not found", name)))?;
        let id = pkg_id(handle);

        let pkg = store
            .packages
            .remove(&id)
            .ok_or_else(|| BlissError::PackageError("Package not found".to_string()))?;
        let pkg = pkg
            .read()
            .map_err(|_| lock_poisoned_error("package deletion"))?;
        let deleted = pkg.object;
        drop(pkg);
        bliss_rt::packages::unregister(deleted);

        for package in store.packages.values() {
            let mut package = package
                .write()
                .map_err(|_| lock_poisoned_error("package deletion"))?;
            package.use_list.retain(|handle| *handle != deleted);
            package.local_nicknames.retain(|_, other| *other != deleted);
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
                    .values()
                    .filter_map(|package| package.read().ok().map(|package| package.object))
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
        // Use `try_with`: at thread/process teardown the CURRENT_STORE
        // thread-local may already be destroyed (a PackageRegistry lives *in*
        // CURRENT_STORE, so its drop can run during that same TLS's
        // destruction). `with` would panic there — `cannot access a Thread
        // Local Storage value during or after destruction` — and a panic in a
        // drop glue on a worker thread aborts the whole process (surfaced when
        // spawning native threads, bliss-q9i1). There is nothing to restore once
        // the TLS is gone, so silently skip. (Normal, non-teardown drops still
        // restore the previous store.)
        let _ = CURRENT_STORE.try_with(|cell| {
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

    if let Some(sym) = package_guard.internal_symbols.get(name) {
        return Ok((sym, InternStatus::Internal));
    }
    if let Some(sym) = package_guard.external_symbols.get(name) {
        return Ok((sym, InternStatus::External));
    }

    let visible_packages = package_guard.use_list.clone();
    drop(package_guard);
    for used_pkg_handle in visible_packages {
        if let Some(used_pkg) = registry.packages.get(&pkg_id(used_pkg_handle)) {
            let used_pkg = used_pkg.read().map_err(|_| lock_poisoned_error("intern"))?;
            if let Some(sym) = used_pkg.external_symbols.get(name) {
                return Ok((sym, InternStatus::Inherited));
            }
        }
    }

    let package_handle = lookup_package_arc(&registry, package)?;
    let package_name = {
        let package = package_handle
            .read()
            .map_err(|_| lock_poisoned_error("intern"))?;
        if let Some(sym) = package.internal_symbols.get(name) {
            return Ok((sym, InternStatus::Internal));
        }
        if let Some(sym) = package.external_symbols.get(name) {
            return Ok((sym, InternStatus::External));
        }
        package.name.clone()
    };
    // Symbol/package registry access has a lower global rank than a package
    // object lock, so allocate before reacquiring the package for insertion.
    let sym = alloc_symbol(&package_name, name);
    let package = package_handle
        .write()
        .map_err(|_| lock_poisoned_error("intern"))?;
    if let Some(existing) = package.internal_symbols.get(name) {
        return Ok((existing, InternStatus::Internal));
    }
    if let Some(existing) = package.external_symbols.get(name) {
        return Ok((existing, InternStatus::External));
    }
    package.internal_symbols.insert(name, sym);
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

    if let Some(sym) = package.internal_symbols.get(name) {
        return Ok(Some((sym, InternStatus::Internal)));
    }
    if let Some(sym) = package.external_symbols.get(name) {
        return Ok(Some((sym, InternStatus::External)));
    }

    // Never nest per-package locks just to traverse a use-list. Snapshot the
    // handles under the target lock, then acquire each used package separately;
    // true multi-package operations use ascending package-id order.
    let use_list = package.use_list.clone();
    drop(package);
    for used_pkg_handle in use_list {
        if let Some(used_pkg) = registry.packages.get(&pkg_id(used_pkg_handle)) {
            let used_pkg = used_pkg
                .read()
                .map_err(|_| lock_poisoned_error("find-symbol"))?;
            if let Some(sym) = used_pkg.external_symbols.get(name) {
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
    let package = package.write().map_err(|_| lock_poisoned_error("export"))?;

    for &sym in symbols {
        let sym_name = package
            .internal_symbols
            .entries()
            .into_iter()
            .find(|(_, value)| *value == sym)
            .map(|(name, _)| name);

        if let Some(name) = sym_name {
            package.internal_symbols.remove(&name);
            package.external_symbols.insert(&name, sym);
        } else if !package
            .external_symbols
            .values()
            .into_iter()
            .any(|v| v == sym)
        {
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
    let package = package
        .write()
        .map_err(|_| lock_poisoned_error("unexport"))?;

    for &sym in symbols {
        let sym_name = package
            .external_symbols
            .entries()
            .into_iter()
            .find(|(_, value)| *value == sym)
            .map(|(name, _)| name);
        if let Some(name) = sym_name {
            package.external_symbols.remove(&name);
            package.internal_symbols.insert(&name, sym);
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
        .entries()
        .into_iter()
        .find(|(_, value)| *value == symbol)
        .map(|(name, _)| name);
    if let Some(name) = removed_internal {
        package.internal_symbols.remove(&name);
        package.shadowing_symbols.remove(&name);
        return Ok(true);
    }

    let removed_external = package
        .external_symbols
        .entries()
        .into_iter()
        .find(|(_, value)| *value == symbol)
        .map(|(name, _)| name);
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
    let target = target.write().map_err(|_| lock_poisoned_error("import"))?;

    for (&sym, name) in symbols.iter().zip(resolved) {
        if let Some(existing) = target.internal_symbols.get(&name) {
            if existing != sym {
                return Err(BlissError::PackageError(format!(
                    "Name conflict for symbol {:?}",
                    name
                )));
            }
            continue;
        }
        if let Some(existing) = target.external_symbols.get(&name) {
            if existing != sym {
                return Err(BlissError::PackageError(format!(
                    "Name conflict for symbol {:?}",
                    name
                )));
            }
            continue;
        }
        target.internal_symbols.insert(&name, sym);
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
        target.internal_symbols.insert(&name, sym);
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
    let (package_name, missing) = {
        let target = target.read().map_err(|_| lock_poisoned_error("shadow"))?;
        let missing = names
            .iter()
            .copied()
            .filter(|name| {
                !target.internal_symbols.contains_key(name)
                    && !target.external_symbols.contains_key(name)
            })
            .collect::<Vec<_>>();
        (target.name.clone(), missing)
    };
    // Symbol allocation consults the lower-ranked runtime package registry.
    // Prepare candidates before reacquiring the package object, then recheck
    // under the write lock to tolerate concurrent interning.
    let candidates = missing
        .into_iter()
        .map(|name| (name, alloc_symbol(&package_name, name)))
        .collect::<Vec<_>>();
    let mut target = target.write().map_err(|_| lock_poisoned_error("shadow"))?;

    for (name, sym) in candidates {
        if !target.internal_symbols.contains_key(name)
            && !target.external_symbols.contains_key(name)
        {
            target.internal_symbols.insert(name, sym);
        }
    }
    for &name in names {
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
    lookup_named_package(&g, name)
}

/// True if `value` is a live package handle in the active registry. Cheap
/// discriminator for PACKAGEP / TYPEP 'PACKAGE and the printer.
pub fn is_package(value: BlissVal) -> bool {
    if !bliss_rt::types::packagep(value) {
        return false;
    }
    current_store()
        .ok()
        .and_then(|store| {
            let g = store.state.read().ok()?;
            Some(g.packages.contains_key(&pkg_id(value)))
        })
        .unwrap_or(false)
}

/// The canonical name of a package handle.
/// True if `package`'s canonical name is exactly `candidate`.
///
/// The reader asks this once per reachable package per symbol token while
/// walking a use-graph, and [`package_name`] answers it by CLONING the name —
/// so the comparison cost was one heap allocation per package per token. This
/// compares in place instead.
pub fn package_name_eq(package: BlissVal, candidate: &str) -> bool {
    if !bliss_rt::types::packagep(package) {
        return false;
    }
    let Ok(store) = current_store() else {
        return false;
    };
    let Ok(g) = store.state.read() else {
        return false;
    };
    let Some(pkg) = g.packages.get(&pkg_id(package)) else {
        return false;
    };
    pkg.read().map(|p| p.name == candidate).unwrap_or(false)
}

pub fn package_name(package: BlissVal) -> Option<String> {
    if !bliss_rt::types::packagep(package) {
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
                    .values()
                    .filter_map(|package| package.read().ok().map(|package| package.object))
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
        let mut out = pkg.internal_symbols.values();
        out.extend(pkg.external_symbols.values());
        Some(out)
    })()
    .unwrap_or_default()
}

/// The package's shadowing symbols (present symbols that shadow inherited
/// same-named ones), as required by PACKAGE-SHADOWING-SYMBOLS. A shadowing
/// entry is a bare name; resolve it to the package's present symbol.
pub fn package_shadowing_symbols(package: BlissVal) -> Vec<BlissVal> {
    (|| -> Option<Vec<BlissVal>> {
        let store = current_store().ok()?;
        let g = store.state.read().ok()?;
        let pkg = g.packages.get(&pkg_id(package))?;
        let pkg = pkg.read().ok()?;
        let mut out = Vec::new();
        for name in &pkg.shadowing_symbols {
            if let Some(sym) = pkg
                .internal_symbols
                .get(name.as_str())
                .or_else(|| pkg.external_symbols.get(name.as_str()))
            {
                out.push(sym);
            }
        }
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
        Some(pkg.read().ok()?.external_symbols.values())
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
        let mut out = pkg.internal_symbols.values();
        out.extend(pkg.external_symbols.values());
        for used in &pkg.use_list {
            if let Some(u) = g.packages.get(&pkg_id(*used)) {
                if let Ok(u) = u.read() {
                    out.extend(u.external_symbols.values());
                }
            }
        }
        Some(out)
    })()
    .unwrap_or_default()
}

/// Home `sym` as a *present* INTERNAL symbol of `package` under `bare_name`,
/// unless a symbol is already present there (internal or external) under that
/// name — in which case the existing homing wins and this is a no-op. Unlike
/// [`intern`], this homes an ALREADY-interned symbol without minting a new one,
/// so the caller (the reader) keeps the symbol's identity. Used to register a
/// freshly-read bare symbol in the current package so FIND-SYMBOL reports
/// `:INTERNAL` rather than a fabricated `:INHERITED` (bliss-v15i).
pub fn intern_present(
    package: BlissVal,
    bare_name: &str,
    sym: BlissVal,
) -> Result<(), BlissError> {
    let store = current_store()?;
    let registry = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("intern_present"))?;
    let handle = lookup_package_arc(&registry, package)?;
    let package = handle
        .write()
        .map_err(|_| lock_poisoned_error("intern_present"))?;
    if package.internal_symbols.get(bare_name).is_some()
        || package.external_symbols.get(bare_name).is_some()
    {
        return Ok(());
    }
    package.internal_symbols.insert(bare_name, sym);
    Ok(())
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
    let g = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("add_symbol"))?;
    let pkg = lookup_package_arc(&g, package)?;
    drop(g);
    let pkg = pkg.write().map_err(|_| lock_poisoned_error("add_symbol"))?;
    if external {
        pkg.internal_symbols.remove(bare_name);
        pkg.external_symbols.insert(bare_name, sym);
    } else if !pkg.external_symbols.contains_key(bare_name) {
        pkg.internal_symbols.insert(bare_name, sym);
    }
    Ok(())
}

/// Seed the 25 ANSI special operators (CLHS 3.1.2.1.2.1) PRESENT and EXTERNAL
/// in the active registry's COMMON-LISP package, with each symbol's heap
/// home-package cell pointing at CL. They are compiler-handled by name and
/// previously had no package-table entry at all, so (find-symbol "EVAL-WHEN"
/// :cl) was NIL until something happened to intern the name, and the "genuine
/// CL symbol" checks (the stays-inherited guard, COMMON-LISP ownership) could
/// not see them. This is step (a) of the bliss-xmxf plan: with the operator
/// surface complete in the CL table, the reader can intern bare names into the
/// CURRENT package (step b) without breaking operator dispatch by full name
/// ("undefined function: UIOP/PACKAGE:EVAL-WHEN"). Idempotent; callers seed
/// after the COMMON-LISP package exists.
pub fn seed_ansi_special_operators() -> Result<(), BlissError> {
    const ANSI_SPECIAL_OPERATORS: [&str; 25] = [
        "BLOCK",
        "CATCH",
        "EVAL-WHEN",
        "FLET",
        "FUNCTION",
        "GO",
        "IF",
        "LABELS",
        "LET",
        "LET*",
        "LOAD-TIME-VALUE",
        "LOCALLY",
        "MACROLET",
        "MULTIPLE-VALUE-CALL",
        "MULTIPLE-VALUE-PROG1",
        "PROGN",
        "PROGV",
        "QUOTE",
        "RETURN-FROM",
        "SETQ",
        "SYMBOL-MACROLET",
        "TAGBODY",
        "THE",
        "THROW",
        "UNWIND-PROTECT",
    ];
    let Some(common_lisp) = find_package("COMMON-LISP") else {
        return Ok(());
    };
    for name in ANSI_SPECIAL_OPERATORS {
        let sym = BlissVal::from_symbol_index(bliss_rt::symbols::intern(name));
        add_symbol(common_lisp, name, sym, true)?;
        bliss_rt::symbols::set_symbol_package(sym.as_symbol_index(), common_lisp);
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
    if lookup_named_package(&g, name).is_some() {
        return Err(BlissError::PackageError(format!(
            "Package named {:?} already exists",
            name
        )));
    }
    for nick in nicknames {
        if lookup_named_package(&g, nick).is_some() {
            return Err(BlissError::PackageError(format!(
                "Nickname {:?} conflicts with an existing package",
                nick
            )));
        }
    }
    let mut resolved_uses = Vec::new();
    for use_name in use_list {
        match lookup_named_package(&g, use_name) {
            Some(package) => resolved_uses.push(package),
            None => {
                return Err(BlissError::PackageError(format!(
                    "Package {:?} not found for use-list",
                    use_name
                )));
            }
        }
    }
    let handle = bliss_rt::packages::find_or_create(name);
    let id = pkg_id(handle);
    let mut pkg = Package::new(name, handle)?;
    pkg.nicknames = nicknames.iter().map(|s| s.to_string()).collect();
    pkg.use_list = resolved_uses;
    g.packages.insert(
        id,
        Arc::new(OrderedRwLock::new(
            LockLevel::Package,
            id.max(1),
            "package object",
            pkg,
        )),
    );
    for nick in nicknames {
        bliss_rt::packages::add_nickname(name, nick);
    }
    Ok(handle)
}

/// Add `nickname` to `package` (idempotent). Fails if the nickname already names
/// a *different* package.
pub fn add_nickname(package: BlissVal, nickname: &str) -> Result<(), BlissError> {
    let store = current_store()?;
    let g = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("add_nickname"))?;
    let id = pkg_id(package);
    match lookup_named_package(&g, nickname) {
        Some(existing) if existing == package => return Ok(()),
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
    let canonical_name = {
        let mut pkg = pkg
            .write()
            .map_err(|_| lock_poisoned_error("add_nickname"))?;
        if !pkg.nicknames.iter().any(|n| n == nickname) {
            pkg.nicknames.push(nickname.to_string());
        }
        pkg.name.clone()
    };
    // Do not recursively enter the package registry through package_name while
    // its read guard is live. The canonical name is already available from the
    // package guard above; release both guards before updating the runtime's
    // global name index.
    drop(g);
    bliss_rt::packages::add_nickname(&canonical_name, nickname);
    Ok(())
}

/// Delete a package by name from the active registry.
pub fn delete_package(name: &str) -> Result<(), BlissError> {
    let store = current_store()?;
    let mut g = store
        .state
        .write()
        .map_err(|_| lock_poisoned_error("delete_package"))?;
    let Some(package) = lookup_named_package(&g, name) else {
        return Ok(());
    };
    let id = pkg_id(package);
    if let Some(pkg) = g.packages.remove(&id) {
        if let Ok(pkg) = pkg.read() {
            drop(pkg);
            bliss_rt::packages::unregister(package);
        }
    }
    for pkg in g.packages.values() {
        if let Ok(mut pkg) = pkg.write() {
            pkg.use_list.retain(|handle| *handle != package);
            pkg.local_nicknames.retain(|_, other| *other != package);
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
    let g = store
        .state
        .read()
        .map_err(|_| lock_poisoned_error("rename_package"))?;
    let id = pkg_id(package);
    if let Some(existing) = lookup_named_package(&g, new_name) {
        if existing != package {
            return Err(BlissError::PackageError(format!(
                "Package named {:?} already exists",
                new_name
            )));
        }
    }
    for nick in new_nicknames {
        if let Some(existing) = lookup_named_package(&g, nick) {
            if existing != package {
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
        let mut pkg = pkg_arc
            .write()
            .map_err(|_| lock_poisoned_error("rename_package"))?;
        pkg.name = new_name.to_string();
        pkg.nicknames = new_nicknames.iter().map(|s| s.to_string()).collect();
    }
    bliss_rt::packages::rename(package, new_name, new_nicknames)?;
    Ok(package)
}

/// Add `used` to `target`'s use-list by *name* (both must already exist).
pub fn use_package_by_name(target: BlissVal, used_name: &str) -> Result<(), BlissError> {
    let used = find_package(used_name).ok_or_else(|| {
        BlissError::PackageError(format!("Package {:?} not found for use-list", used_name))
    })?;
    use_package(&[used], target)
}
