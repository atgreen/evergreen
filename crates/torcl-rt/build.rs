fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target == "powerpc64le-unknown-linux-gnu" {
        println!("cargo:rerun-if-changed=src/native_transfer/ppc64le.S");
        cc::Build::new()
            .file("src/native_transfer/ppc64le.S")
            .flag("-mabi=elfv2")
            .compile("torcl_native_transfer_ppc64le");
    }
    if target == "s390x-unknown-linux-gnu" {
        println!("cargo:rerun-if-changed=src/native_transfer/s390x.S");
        cc::Build::new()
            .file("src/native_transfer/s390x.S")
            .compile("torcl_native_transfer_s390x");
    }
}
