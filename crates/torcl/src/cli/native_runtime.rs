//! Capability selection and matching-runtime builds for saved-image delivery.
use crate::runtime_contract::Contract;
pub(super) fn contract() -> Contract {
    let mut contract = Contract::parse(include_str!(concat!(
        env!("OUT_DIR"),
        "/runtime-contract.txt"
    )))
    .expect("build.rs generated a valid runtime contract");
    contract.features.retain(|name| !name.contains('/'));
    contract.features.extend(
        torcl_rt::image::RUNTIME_BUILD_FEATURES
            .iter()
            .chain(torcl_stdlib::RUNTIME_BUILD_FEATURES)
            .map(|name| (*name).to_owned()),
    );
    contract
}

/// Build in a separate cache: never overwrite the driver or its test binary.
/// Cargo owns locking and dependency reuse. Each selection has its own manifest.
pub(super) fn build(
    source: Option<&str>,
    selected: &Contract,
) -> Result<Vec<u8>, torcl_rt::error::TorclError> {
    use std::{fs, path::Path, process::Command};
    let fail = |message: String| {
        torcl_rt::error::TorclError::ProgramError(format!("native delivery: {message}"))
    };
    let source = Path::new(source.unwrap_or(env!("TORCL_RUNTIME_SOURCE")))
        .canonicalize()
        .map_err(|e| {
            fail(format!(
                "runtime source unavailable; use --runtime-source: {e}"
            ))
        })?;
    // Separate artifacts for *all* contract inputs, including dependency
    // features and compiler flags. Concurrent deliveries cannot exchange them.
    let key = selected
        .encode()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
    let key = format!("{key:016x}");
    let cache = source.join("target/delivery").join(&selected.source);
    fs::create_dir_all(&cache).map_err(|e| fail(e.to_string()))?;
    let manifest = cache.join(format!("{key}.runtime"));
    let encoded = selected.encode();
    if fs::read_to_string(&manifest).ok().as_deref() != Some(&encoded) {
        super::delivery::write_atomic(&manifest, encoded.as_bytes(), false)
            .map_err(|e| fail(e.to_string()))?;
    }
    let flags: Vec<u8> = selected
        .rustflags
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let flags = String::from_utf8(flags).map_err(|e| fail(e.to_string()))?;
    let output = Command::new("cargo")
        .current_dir(&source)
        .arg("--config")
        .arg("build.jobs=2")
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "torcl",
            "--bin",
            "torcl",
            "--target",
            &selected.target,
            "--manifest-path",
        ])
        .arg(source.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(cache.join(&key))
        .arg("--no-default-features")
        .arg("--features")
        .arg(
            selected
                .features
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        )
        .env_remove("RUSTFLAGS")
        .env("CARGO_ENCODED_RUSTFLAGS", flags)
        .env("TORCL_RUNTIME_MANIFEST", &manifest)
        .output()
        .map_err(|e| fail(format!("start Cargo: {e}")))?;
    if !output.status.success() {
        return Err(fail(format!(
            "Cargo build failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let name = if selected.target.contains("windows") {
        "torcl.exe"
    } else {
        "torcl"
    };
    let binary = cache
        .join(&key)
        .join(&selected.target)
        .join("release")
        .join(name);
    let info = Command::new(&binary)
        .arg("--runtime-info")
        .output()
        .map_err(|e| fail(format!("cannot execute matching-target runtime: {e}")))?;
    if !info.status.success() {
        return Err(fail("built runtime --runtime-info failed".into()));
    }
    let actual =
        Contract::parse(std::str::from_utf8(&info.stdout).map_err(|e| fail(e.to_string()))?)
            .map_err(fail)?;
    if &actual != selected {
        return Err(fail(
            "built runtime contract differs from requested manifest".into(),
        ));
    }
    fs::read(binary).map_err(|e| fail(e.to_string()))
}

thread_local! {
    static SAVE_CONTRACT: std::cell::RefCell<Option<Contract>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn with_save_contract<T>(selected: &Contract, save: impl FnOnce() -> T) -> T {
    struct Reset(Option<Contract>);
    impl Drop for Reset {
        fn drop(&mut self) {
            SAVE_CONTRACT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(SAVE_CONTRACT.with(|slot| slot.replace(Some(selected.clone()))));
    save()
}

pub(super) fn register_image_hooks() {
    torcl_rt::image::set_runtime_contract_hooks(
        || {
            SAVE_CONTRACT.with(|slot| {
                slot.borrow()
                    .clone()
                    .unwrap_or_else(contract)
                    .encode()
                    .into_bytes()
            })
        },
        |bytes| {
            let fail = |e| torcl_rt::TorclError::InvalidImage(e);
            let Some(bytes) = bytes else {
                return if cfg!(torcl_specialized_runtime) {
                    Err(fail(
                        "specialized runtime requires native image requirements".into(),
                    ))
                } else {
                    Ok(())
                };
            };
            let required =
                Contract::parse(std::str::from_utf8(bytes).map_err(|e| fail(e.to_string()))?)
                    .map_err(fail)?;
            contract().accepts(&required).map_err(fail)
        },
    );
}
