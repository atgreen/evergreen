//! Explicit library lifetime and global symbol lookup for the CFFI backend.
#![cfg(target_os = "linux")]
use std::process::Command;
use torcl_rt::ffi::{
    close_foreign_library, foreign_symbol, foreign_symbol_global, load_foreign_library,
};

#[test]
fn library_close_runs_destructors_and_invalidates_only_its_handle() {
    let dir = std::env::temp_dir().join(format!("torcl-library-lifetime-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("library.c");
    let library = dir.join("library.so");
    std::fs::write(
        &source,
        r#"
        static int *closed;
        void torcl_fixture_set_closed(int *p) { closed = p; }
        int torcl_fixture_library_answer(void) { return 42; }
        __attribute__((destructor)) static void finish(void) { if (closed) *closed = 99; }
    "#,
    )
    .unwrap();
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap()
            .success()
    );
    let first = load_foreign_library(library.to_str().unwrap()).unwrap();
    let mut marker = 0i32;
    unsafe {
        let install: extern "C" fn(*mut i32) =
            std::mem::transmute(foreign_symbol(first, "torcl_fixture_set_closed").unwrap());
        install(&mut marker);
        let answer: extern "C" fn() -> i32 =
            std::mem::transmute(foreign_symbol_global("torcl_fixture_library_answer").unwrap());
        assert_eq!(answer(), 42);
        close_foreign_library(first).unwrap();
        assert_eq!(marker, 99, "explicit close must run C destructors");
        assert!(foreign_symbol(first, "torcl_fixture_library_answer").is_err());
        assert!(foreign_symbol_global("torcl_fixture_library_answer").is_err());
        assert!(close_foreign_library(first).is_err());
        assert!(close_foreign_library(std::ptr::null_mut()).is_err());
    }
    let second = load_foreign_library(library.to_str().unwrap()).unwrap();
    assert_ne!(
        first, second,
        "a closed token must not be reused for a new library"
    );
    unsafe {
        assert!(foreign_symbol(first, "torcl_fixture_library_answer").is_err());
        assert!(foreign_symbol(second, "torcl_fixture_library_answer").is_ok());
        close_foreign_library(second).unwrap();
    }
    let strlen = unsafe { foreign_symbol_global("strlen") }.unwrap();
    let strlen: extern "C" fn(*const u8) -> usize = unsafe { std::mem::transmute(strlen) };
    assert_eq!(strlen(c"hello".as_ptr().cast()), 5);
    assert!(unsafe { foreign_symbol_global("bad\0name") }.is_err());
}
