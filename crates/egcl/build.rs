#[path = "src/runtime_contract.rs"]
mod runtime_contract;
use std::{env, fs, path::Path, process::Command};

// Deterministic content identity, not a security signature. Include runtime
// sources and Lisp bootstrap, independent of checkout path and build profile.
fn fingerprint(root: &Path) -> String {
    fn visit(path: &Path, paths: &mut Vec<std::path::PathBuf>) {
        println!("cargo:rerun-if-changed={}", path.display());
        for entry in fs::read_dir(path).expect("read runtime sources") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|n| n == "target" || n == ".git")
                {
                    continue;
                }
                visit(&path, paths);
            } else if path
                .extension()
                .is_some_and(|e| e == "rs" || e == "toml" || e == "lisp" || e == "lock")
            {
                paths.push(path);
            }
        }
    }
    let mut paths = vec![
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
        root.join(".cargo/config.toml"),
    ];
    for name in [
        "egcl",
        "egcl-rt",
        "egcl-compiler",
        "egcl-stdlib",
        "egcl-delivery-macros",
    ] {
        let package = root.join("crates").join(name);
        paths.push(package.join("Cargo.toml"));
        if package.join("build.rs").is_file() {
            paths.push(package.join("build.rs"));
        }
        visit(&package.join("src"), &mut paths);
    }
    visit(&root.join("lib"), &mut paths);
    paths.sort();
    let mut hash = 0x6c62272e07bb014262b821756295c58du128;
    for path in paths {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for byte in name
            .bytes()
            .chain([0])
            .chain(fs::read(path).unwrap())
            .chain([0])
        {
            hash = (hash ^ u128::from(byte)).wrapping_mul(0x0000000001000000000000000000013b);
        }
    }
    format!("{hash:032x}")
}

fn configure_runtime() {
    println!("cargo:rerun-if-env-changed=EGCL_RUNTIME_MANIFEST");
    println!("cargo:rustc-check-cfg=cfg(egcl_no_t1)");
    println!("cargo:rustc-check-cfg=cfg(egcl_no_t2)");
    println!("cargo:rustc-check-cfg=cfg(egcl_specialized_builtins)");
    println!("cargo:rustc-check-cfg=cfg(egcl_builtin, values(any()))");
    println!("cargo:rustc-check-cfg=cfg(egcl_no_disassembly)");
    println!("cargo:rustc-check-cfg=cfg(egcl_no_dynamic_code)");
    println!("cargo:rustc-check-cfg=cfg(egcl_no_tree_walker)");
    println!("cargo:rustc-check-cfg=cfg(egcl_specialized_runtime)");
    let manifest = env::var_os("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).parent().unwrap().parent().unwrap();
    let rustc = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("-vV")
        .output()
        .expect("query rustc identity");
    assert!(rustc.status.success(), "query rustc identity failed");
    let mut full = runtime_contract::Contract {
        source: fingerprint(root),
        target: env::var("TARGET").unwrap(),
        toolchain: String::from_utf8(rustc.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>()
            .join(";"),
        features: [
            ("CARGO_FEATURE_THREAD_CACHE_ALLOC", "thread-cache-alloc"),
            ("CARGO_FEATURE_ALLOC_COUNT", "alloc-count"),
            ("CARGO_FEATURE_PYTHON", "python"),
        ]
        .into_iter()
        .filter(|(var, _)| env::var_os(var).is_some())
        .map(|(_, name)| name.to_owned())
        .collect(),
        rustflags: env::var("CARGO_ENCODED_RUSTFLAGS")
            .unwrap_or_default()
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect(),
        builtins: None,
        max_tier: runtime_contract::NativeTier::T2,
        capabilities: runtime_contract::CAPABILITIES
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    };
    let selected = if let Some(path) = env::var_os("EGCL_RUNTIME_MANIFEST") {
        println!("cargo:rerun-if-changed={}", Path::new(&path).display());
        let selected = runtime_contract::Contract::parse(
            &fs::read_to_string(path).expect("read runtime manifest"),
        )
        .expect("parse runtime manifest");
        // Dependency features are resolved by Cargo, not exposed to this build
        // script. The executable adds its dependencies' actual features and delivery
        // checks that complete contract before using the artifact.
        full.features.extend(
            selected
                .features
                .iter()
                .filter(|name| name.contains('/'))
                .cloned(),
        );
        full.accepts(&selected)
            .expect("native runtime build is incompatible with delivery driver");
        println!("cargo:rustc-cfg=egcl_specialized_runtime");
        selected
    } else {
        full
    };
    if !selected.capabilities.contains("disassembly") {
        println!("cargo:rustc-cfg=egcl_no_disassembly");
    }
    if !selected.capabilities.contains("dynamic-code") {
        println!("cargo:rustc-cfg=egcl_no_dynamic_code");
    }
    if !selected.capabilities.contains("tree-walker") {
        println!("cargo:rustc-cfg=egcl_no_tree_walker");
    }
    if selected.max_tier < runtime_contract::NativeTier::T1 {
        println!("cargo:rustc-cfg=egcl_no_t1");
    }
    if selected.max_tier < runtime_contract::NativeTier::T2 {
        println!("cargo:rustc-cfg=egcl_no_t2");
    }
    if let Some(builtins) = &selected.builtins {
        println!("cargo:rustc-cfg=egcl_specialized_builtins");
        for name in builtins {
            println!("cargo:rustc-cfg=egcl_builtin={name:?}");
        }
    }
    // Keep the shared builtin dependency catalog exercised by the build too.
    assert!(runtime_contract::opens_code_world("EVAL"));
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("runtime-contract.txt"),
        selected.encode(),
    )
    .unwrap();
    println!("cargo:rustc-env=EGCL_RUNTIME_SOURCE={}", root.display());
    // The exact target triple as a compile-time constant for the banner and
    // --version. It is already computed above for the runtime contract; exposing it
    // this way costs nothing at runtime, where reading it back out of the embedded
    // contract would mean parsing the contract on every startup.
    println!("cargo:rustc-env=EGCL_TARGET={}", selected.target);
}

fn main() {
    configure_runtime();
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl")
    {
        // Only the CLI supplies the wrapper. Library consumers and test
        // executables keep their normal libc; no global workspace linker flag.
        println!("cargo:rustc-link-arg-bin=egcl=-Wl,--wrap=memcpy");
    }
}
