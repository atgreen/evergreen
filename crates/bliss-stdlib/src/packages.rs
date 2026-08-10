//! Package system and bootstrap.
//!
//! Manages the CL package registry, package operations, and
//! the standard package layout. See spec §5.1.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Package registry ───────────────────────────────────────────────

/// Global package registry. Thread-safe (RwLock-protected).
pub struct PackageRegistry {
    _private: (),
}

impl PackageRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        unimplemented!("PackageRegistry::new")
    }

    /// Initialize with the standard packages (CL, CL-USER, KEYWORD, BLISS, etc.).
    pub fn init_standard_packages(&mut self) -> Result<(), BlissError> {
        unimplemented!("PackageRegistry::init_standard_packages")
    }

    /// Find a package by name or nickname. O(1) amortised (R5.05).
    pub fn find_package(&self, name: &str) -> Option<BlissVal> {
        unimplemented!("PackageRegistry::find_package")
    }

    /// Create a new package.
    pub fn make_package(
        &mut self,
        name: &str,
        nicknames: &[&str],
        use_list: &[&str],
    ) -> Result<BlissVal, BlissError> {
        unimplemented!("PackageRegistry::make_package")
    }

    /// Delete a package.
    pub fn delete_package(&mut self, name: &str) -> Result<(), BlissError> {
        unimplemented!("PackageRegistry::delete_package")
    }

    /// List all packages.
    pub fn list_all_packages(&self) -> Vec<BlissVal> {
        unimplemented!("PackageRegistry::list_all_packages")
    }
}

// ── Package operations ─────────────────────────────────────────────

/// Intern a symbol in a package.
pub fn intern(name: &str, package: BlissVal) -> Result<(BlissVal, InternStatus), BlissError> {
    unimplemented!("intern")
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
    unimplemented!("find_symbol")
}

/// Export symbols from a package.
pub fn export(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    unimplemented!("export")
}

/// Unexport symbols from a package.
pub fn unexport(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    unimplemented!("unexport")
}

/// Unintern a symbol from a package.
pub fn unintern(symbol: BlissVal, package: BlissVal) -> Result<bool, BlissError> {
    unimplemented!("unintern")
}

/// Use a package (add to use-list).
pub fn use_package(packages: &[BlissVal], target: BlissVal) -> Result<(), BlissError> {
    unimplemented!("use_package")
}

/// Unuse a package.
pub fn unuse_package(packages: &[BlissVal], target: BlissVal) -> Result<(), BlissError> {
    unimplemented!("unuse_package")
}

/// Import symbols into a package.
pub fn import(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    unimplemented!("import")
}

/// Shadowing import.
pub fn shadowing_import(symbols: &[BlissVal], package: BlissVal) -> Result<(), BlissError> {
    unimplemented!("shadowing_import")
}

/// Shadow symbols.
pub fn shadow(names: &[&str], package: BlissVal) -> Result<(), BlissError> {
    unimplemented!("shadow")
}
