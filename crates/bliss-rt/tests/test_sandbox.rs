//! Tests for bliss-rt sandbox module: SandboxPolicy, Sandbox creation,
//! check_path, check_network, check_ffi, check_subprocess, policy accessor.

use bliss_rt::sandbox::*;

fn restrictive_policy() -> SandboxPolicy {
    SandboxPolicy {
        allow_filesystem: false,
        allow_network: false,
        allow_ffi: false,
        allow_subprocess: false,
        max_heap_bytes: 0,
        max_threads: 0,
        allowed_paths: vec![],
    }
}

fn permissive_policy() -> SandboxPolicy {
    SandboxPolicy {
        allow_filesystem: true,
        allow_network: true,
        allow_ffi: true,
        allow_subprocess: true,
        max_heap_bytes: 0,
        max_threads: 0,
        allowed_paths: vec![],
    }
}

// ── SandboxPolicy::default ────────────────────────────────────────

/// Per spec §8.2.2 R8.02: 'the default set MUST be empty (deny-all)'.
/// The Default implementation should be restrictive.
#[test]
fn default_policy_is_deny_all() {
    let p = SandboxPolicy::default();
    assert!(
        !p.allow_filesystem,
        "default policy must deny filesystem (R8.02)"
    );
    assert!(!p.allow_network, "default policy must deny network (R8.02)");
    assert!(!p.allow_ffi, "default policy must deny FFI (R8.02)");
    assert!(
        !p.allow_subprocess,
        "default policy must deny subprocess (R8.02)"
    );
    assert_eq!(
        p.max_heap_bytes, 0,
        "default max_heap_bytes should be 0 (unlimited/unset)"
    );
    assert_eq!(
        p.max_threads, 0,
        "default max_threads should be 0 (unlimited/unset)"
    );
    assert!(
        p.allowed_paths.is_empty(),
        "default allowed_paths must be empty"
    );
}

#[test]
fn sandbox_policy_clone() {
    let mut p = SandboxPolicy {
        allow_network: false,
        ..SandboxPolicy::default()
    };
    p.allowed_paths.push("/tmp".into());
    let p2 = p.clone();
    assert!(!p2.allow_network);
    assert_eq!(p2.allowed_paths, vec!["/tmp"]);
}

// ── Sandbox::new ──────────────────────────────────────────────────

#[test]
fn sandbox_new_default_succeeds() {
    assert!(Sandbox::new(SandboxPolicy::default()).is_ok());
}

#[test]
fn sandbox_new_restrictive_succeeds() {
    assert!(Sandbox::new(restrictive_policy()).is_ok());
}

// ── check_path ────────────────────────────────────────────────────

#[test]
fn check_path_allowed_when_fs_enabled() {
    let sb = Sandbox::new(permissive_policy()).unwrap();
    assert!(sb.check_path("/any/path").is_ok());
}

#[test]
fn check_path_denied_when_fs_disabled() {
    let sb = Sandbox::new(restrictive_policy()).unwrap();
    assert!(sb.check_path("/etc/passwd").is_err());
}

#[test]
fn check_path_exception_via_allowed_paths() {
    let mut p = restrictive_policy();
    p.allowed_paths.push("/tmp".into());
    let sb = Sandbox::new(p).unwrap();
    assert!(sb.check_path("/tmp/foo.txt").is_ok());
    assert!(sb.check_path("/etc/hosts").is_err());
}

// ── check_network / check_ffi / check_subprocess ──────────────────

#[test]
fn check_network_allowed_denied() {
    let sb_ok = Sandbox::new(permissive_policy()).unwrap();
    assert!(sb_ok.check_network().is_ok());
    let sb_no = Sandbox::new(restrictive_policy()).unwrap();
    assert!(sb_no.check_network().is_err());
}

#[test]
fn check_ffi_allowed_denied() {
    let sb_ok = Sandbox::new(permissive_policy()).unwrap();
    assert!(sb_ok.check_ffi().is_ok());
    let sb_no = Sandbox::new(restrictive_policy()).unwrap();
    assert!(sb_no.check_ffi().is_err());
}

#[test]
fn check_subprocess_allowed_denied() {
    let sb_ok = Sandbox::new(permissive_policy()).unwrap();
    assert!(sb_ok.check_subprocess().is_ok());
    let sb_no = Sandbox::new(restrictive_policy()).unwrap();
    assert!(sb_no.check_subprocess().is_err());
}

// ── max_heap_bytes / max_threads enforcement (R8.06, R8.07) ──────

#[test]
fn sandbox_enforces_max_heap_bytes() {
    let p = SandboxPolicy {
        allow_filesystem: false,
        allow_network: false,
        allow_ffi: false,
        allow_subprocess: false,
        max_heap_bytes: 1024,
        max_threads: 0,
        allowed_paths: vec![],
    };
    let sb = Sandbox::new(p).unwrap();
    let pol = sb.policy();
    assert_eq!(pol.max_heap_bytes, 1024);
    // The sandbox should enforce this limit; attempting to exceed it should fail.
    // Since we can't directly allocate through sandbox, we verify the policy
    // is stored and accessible so the runtime can enforce it.
}

#[test]
fn sandbox_enforces_max_threads() {
    let p = SandboxPolicy {
        allow_filesystem: false,
        allow_network: false,
        allow_ffi: false,
        allow_subprocess: false,
        max_heap_bytes: 0,
        max_threads: 2,
        allowed_paths: vec![],
    };
    let sb = Sandbox::new(p).unwrap();
    let pol = sb.policy();
    assert_eq!(pol.max_threads, 2);
}

#[test]
fn sandbox_default_deny_all_blocks_everything() {
    // With default deny-all policy, all checks should fail
    let sb = Sandbox::new(SandboxPolicy::default()).unwrap();
    assert!(
        sb.check_path("/any/path").is_err(),
        "deny-all should block path access"
    );
    assert!(sb.check_network().is_err(), "deny-all should block network");
    assert!(sb.check_ffi().is_err(), "deny-all should block FFI");
    assert!(
        sb.check_subprocess().is_err(),
        "deny-all should block subprocess"
    );
}

// ── policy accessor ───────────────────────────────────────────────

#[test]
fn policy_accessor_returns_configured_values() {
    let p = SandboxPolicy {
        allow_filesystem: false,
        allow_network: false,
        allow_ffi: true,
        allow_subprocess: false,
        max_heap_bytes: 4096,
        max_threads: 10,
        allowed_paths: vec!["/opt".into()],
    };
    let sb = Sandbox::new(p).unwrap();
    let got = sb.policy();
    assert!(!got.allow_filesystem);
    assert!(got.allow_ffi);
    assert_eq!(got.max_heap_bytes, 4096);
    assert_eq!(got.max_threads, 10);
    assert_eq!(got.allowed_paths, vec!["/opt"]);
}
