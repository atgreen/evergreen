// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Security sandbox — resource limits and access control.
//!
//! See §8 of the spec.

use crate::error::EgclError;

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
    /// Per spec §8.2.2 R8.02: default set MUST be empty (deny-all).
    fn default() -> Self {
        SandboxPolicy {
            allow_filesystem: false,
            allow_network: false,
            allow_ffi: false,
            allow_subprocess: false,
            max_heap_bytes: 0,
            max_threads: 0,
            allowed_paths: Vec::new(),
        }
    }
}

/// The active sandbox, enforcing resource limits.
pub struct Sandbox {
    policy: SandboxPolicy,
}

impl Sandbox {
    /// Create a new sandbox with the given policy.
    pub fn new(policy: SandboxPolicy) -> Result<Self, EgclError> {
        Ok(Sandbox { policy })
    }

    /// Check if a filesystem path is accessible under the current policy.
    pub fn check_path(&self, path: &str) -> Result<(), EgclError> {
        if self.policy.allow_filesystem {
            return Ok(());
        }
        // Check if path falls under any allowed_paths exception
        for allowed in &self.policy.allowed_paths {
            if path.starts_with(allowed.as_str()) {
                return Ok(());
            }
        }
        Err(EgclError::SandboxViolation(format!(
            "filesystem access denied: {}",
            path
        )))
    }

    /// Check if network access is allowed.
    pub fn check_network(&self) -> Result<(), EgclError> {
        if self.policy.allow_network {
            Ok(())
        } else {
            Err(EgclError::SandboxViolation("network access denied".into()))
        }
    }

    /// Check if FFI calls are allowed.
    pub fn check_ffi(&self) -> Result<(), EgclError> {
        if self.policy.allow_ffi {
            Ok(())
        } else {
            Err(EgclError::SandboxViolation("FFI access denied".into()))
        }
    }

    /// Check if subprocess creation is allowed.
    pub fn check_subprocess(&self) -> Result<(), EgclError> {
        if self.policy.allow_subprocess {
            Ok(())
        } else {
            Err(EgclError::SandboxViolation(
                "subprocess creation denied".into(),
            ))
        }
    }

    /// Check whether a heap allocation of `bytes` is allowed under the
    /// max_heap_bytes policy limit.
    ///
    /// `current_usage` is the current total heap usage in bytes.
    /// Returns `Ok(())` if unlimited (0) or within budget, `Err` otherwise.
    pub fn check_heap_alloc(&self, current_usage: usize, bytes: usize) -> Result<(), EgclError> {
        if self.policy.max_heap_bytes == 0 {
            // 0 means unlimited
            return Ok(());
        }
        let new_total = current_usage.saturating_add(bytes);
        if new_total > self.policy.max_heap_bytes {
            Err(EgclError::SandboxViolation(format!(
                "heap allocation denied: {} + {} = {} exceeds limit of {} bytes",
                current_usage, bytes, new_total, self.policy.max_heap_bytes
            )))
        } else {
            Ok(())
        }
    }

    /// Check whether creating another thread is allowed under the
    /// max_threads policy limit.
    ///
    /// `current_count` is the current number of active threads.
    /// Returns `Ok(())` if unlimited (0) or within budget, `Err` otherwise.
    pub fn check_thread_create(&self, current_count: usize) -> Result<(), EgclError> {
        if self.policy.max_threads == 0 {
            // 0 means unlimited
            return Ok(());
        }
        if current_count >= self.policy.max_threads {
            Err(EgclError::SandboxViolation(format!(
                "thread creation denied: {} threads already active, limit is {}",
                current_count, self.policy.max_threads
            )))
        } else {
            Ok(())
        }
    }

    /// Get the current policy.
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }
}
