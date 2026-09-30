//! The elf_loader FFI backend (bliss-bca.5): load a shared library at runtime
//! and call into it — with no `dlopen`/`ld.so`, so it works in a fully static
//! binary. Runs on the default build; the `c-ffi` build uses dlopen instead.
#![cfg(not(feature = "c-ffi"))]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Compile a tiny shared library with the system C compiler. Returns None (and
/// the test soft-skips) if no `cc` is available.
fn build_test_so(dir: &Path, src: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let c = dir.join("t.c");
    std::fs::write(&c, src).ok()?;
    let so = dir.join("libt.so");
    let ok = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(&c)
        .arg("-o")
        .arg(&so)
        .status()
        .ok()?
        .success();
    ok.then_some(so)
}

#[test]
fn loads_a_library_and_calls_a_function() {
    let dir = std::env::temp_dir().join(format!("egcl-ffi-{}", std::process::id()));
    // A self-contained export plus one that imports a host libc symbol and runs
    // a constructor — exercising host-symbol resolution and DT_INIT_ARRAY.
    let src = "
        typedef unsigned long size_t;
        extern void *memcpy(void *, const void *, size_t);
        static int ctor = 0;
        __attribute__((constructor)) static void init(void) { ctor = 5; }
        int square(int x) { return x * x; }
        int viahost(int x) { int y = x + ctor, z; memcpy(&z, &y, sizeof z); return z; }
    ";
    let Some(so) = build_test_so(&dir, src) else {
        eprintln!("cc unavailable — skipping elf_loader FFI test");
        return;
    };
    let so = so.to_str().unwrap();

    let lib = egcl_rt::ffi::load_foreign_library(so).expect("load_foreign_library");

    let square = unsafe { egcl_rt::ffi::foreign_symbol(lib, "square") }.expect("square");
    let square: extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(square) };
    assert_eq!(square(6), 36);

    // Imports host memcpy and depends on its constructor having run (ctor == 5).
    let viahost = unsafe { egcl_rt::ffi::foreign_symbol(lib, "viahost") }.expect("viahost");
    let viahost: extern "C" fn(i32) -> i32 = unsafe { std::mem::transmute(viahost) };
    assert_eq!(
        viahost(10),
        15,
        "host memcpy + constructor should give x + 5"
    );

    // A missing symbol and a bad path both error cleanly.
    assert!(unsafe { egcl_rt::ffi::foreign_symbol(lib, "does_not_exist") }.is_err());
    assert!(egcl_rt::ffi::load_foreign_library("/no/such/library.so").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}
