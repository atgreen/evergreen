//! Security sandbox — resource limits and access control.
//!
//! See §8 of the spec.

use crate::error::BlissError;

/// Sandbox policy configuration.
#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    /// Whether filesystem access is allowed.
    pub allow_filesystem: bool,
    /// Whether network access is allowed.
    pub allow_network: bool,
    /// Whether FFI calls are allowed.
    pub allow_ffi: bool,
    /// Whether subprocess creation is allowed.
    pub allow_subprocess: bool,
    /// Maximum heap size (0 = unlimited).
    pub max_heap_bytes: usize,
    /// Maximum number of green threads (0 = unlimited).
    pub max_threads: usize,
    /// Allowed filesystem paths (when allow_filesystem is false, these are exceptions).
    pub allowed_paths: Vec<String>,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        unimplemented!("SandboxPolicy::default")
    }
}

/// The active sandbox, enforcing resource limits.
pub struct Sandbox {
    _private: (),
}

impl Sandbox {
    /// Create a new sandbox with the given policy.
    pub fn new(policy: SandboxPolicy) -> Result<Self, BlissError> {
        unimplemented!("Sandbox::new")
    }

    /// Check if a filesystem path is accessible under the current policy.
    pub fn check_path(&self, path: &str) -> Result<(), BlissError> {
        unimplemented!("Sandbox::check_path")
    }

    /// Check if network access is allowed.
    pub fn check_network(&self) -> Result<(), BlissError> {
        unimplemented!("Sandbox::check_network")
    }

    /// Check if FFI calls are allowed.
    pub fn check_ffi(&self) -> Result<(), BlissError> {
        unimplemented!("Sandbox::check_ffi")
    }

    /// Check if subprocess creation is allowed.
    pub fn check_subprocess(&self) -> Result<(), BlissError> {
        unimplemented!("Sandbox::check_subprocess")
    }

    /// Get the current policy.
    pub fn policy(&self) -> &SandboxPolicy {
        unimplemented!("Sandbox::policy")
    }
}
