fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl")
    {
        // Only the CLI supplies the wrapper. Library consumers and test
        // executables keep their normal libc; no global workspace linker flag.
        println!("cargo:rustc-link-arg-bin=bliss-cli=-Wl,--wrap=memcpy");
    }
}
