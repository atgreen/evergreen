// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Build-script inputs for actual native archives, not embedded shared objects.
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const ENVIRONMENT: &str = "EGCL_STATIC_FOREIGN_MANIFEST";

#[derive(Debug)]
struct Library {
    name: String,
    aliases: Vec<String>,
    archives: Vec<PathBuf>,
    symbols: Vec<String>,
}

fn parse(path: &Path, text: &str) -> Result<Vec<Library>, String> {
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut version = false;
    let mut libraries: Vec<Library> = Vec::new();
    for (line, text) in text.lines().enumerate() {
        let text = text.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let bad = |message: &str| format!("{}:{}: {message}", path.display(), line + 1);
        let (key, value) = text
            .split_once('=')
            .ok_or_else(|| bad("expected key = value"))?;
        let (key, value) = (key.trim(), value.trim());
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(bad("empty value or control character"));
        }
        match key {
            "version" if !version && libraries.is_empty() && value == "1" => version = true,
            "library" if version => libraries.push(Library {
                name: value.to_owned(),
                aliases: Vec::new(),
                archives: Vec::new(),
                symbols: Vec::new(),
            }),
            "alias" | "archive" | "symbol" if version && !libraries.is_empty() => {
                let library = libraries.last_mut().unwrap();
                match key {
                    "alias" => library.aliases.push(value.to_owned()),
                    "symbol" => library.symbols.push(value.to_owned()),
                    _ => library.archives.push(base.join(value)),
                }
            }
            _ => {
                return Err(bad(
                    "unknown, duplicate, misplaced or unsupported manifest field",
                ));
            }
        }
    }
    if !version || libraries.is_empty() {
        return Err("static foreign manifest needs version = 1 and at least one library".into());
    }
    let mut names = BTreeSet::new();
    for library in &libraries {
        if library.archives.is_empty() || library.symbols.is_empty() {
            return Err(format!(
                "static library '{}' needs archives and exported symbols",
                library.name
            ));
        }
        for name in std::iter::once(&library.name).chain(&library.aliases) {
            if !names.insert(name) {
                return Err(format!("duplicate static library name/alias '{name}'"));
            }
        }
        let mut symbols = BTreeSet::new();
        for symbol in &library.symbols {
            if !symbols.insert(symbol) {
                return Err(format!(
                    "duplicate static symbol '{symbol}' in '{}'",
                    library.name
                ));
            }
        }
    }
    Ok(libraries)
}

struct Identity(u128);
impl Identity {
    fn new() -> Self {
        Self(0x6c62272e07bb014262b821756295c58d)
    }
    fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u128::from(byte)).wrapping_mul(0x0000000001000000000000000000013b);
        }
    }
    fn field(&mut self, bytes: &[u8]) {
        self.bytes(&(bytes.len() as u64).to_le_bytes());
        self.bytes(bytes);
    }
}

pub struct Configuration {
    pub manifest: String,
    pub identity: Option<String>,
}

/// Snapshot archives into OUT_DIR before hashing/linking, so a build's recorded
/// identity describes its actual linker inputs. Original paths remain Cargo
/// dependencies. No executable reads the source manifest or archives at startup.
pub fn configure(out: &Path) -> Result<Configuration, String> {
    println!("cargo:rerun-if-env-changed={ENVIRONMENT}");
    let Some(path) = std::env::var_os(ENVIRONMENT).filter(|path| !path.is_empty()) else {
        std::fs::write(out.join("static-foreign.rs"),
            "pub(super) fn register_linked_libraries() -> Result<(), egcl_rt::EgclError> { Ok(()) }\n")
            .map_err(|error| error.to_string())?;
        return Ok(Configuration {
            manifest: String::new(),
            identity: None,
        });
    };
    let path = PathBuf::from(path)
        .canonicalize()
        .map_err(|error| format!("static foreign manifest: {error}"))?;
    let manifest = path
        .to_str()
        .ok_or("static foreign manifest path must be UTF-8")?
        .to_owned();
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let libraries = parse(&path, &text)?;
    let mut identity = Identity::new();
    let mut declarations = String::new();
    let mut registration = String::new();
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let mut serial = 0usize;
    println!("cargo:rustc-link-search=native={}", out.display());
    for (library_index, library) in libraries.iter().enumerate() {
        identity.field(library.name.as_bytes());
        identity.field(&(library.aliases.len() as u64).to_le_bytes());
        for alias in &library.aliases {
            identity.field(alias.as_bytes());
        }
        identity.field(&(library.archives.len() as u64).to_le_bytes());
        for (archive_index, archive) in library.archives.iter().enumerate() {
            println!("cargo:rerun-if-changed={}", archive.display());
            let link_name = format!("egcl_foreign_{library_index}_{archive_index}");
            let filename = if msvc {
                format!("{link_name}.lib")
            } else {
                format!("lib{link_name}.a")
            };
            let snapshot = out.join(filename);
            std::fs::copy(archive, &snapshot)
                .map_err(|error| format!("{}: {error}", archive.display()))?;
            let mut file = std::fs::File::open(&snapshot).map_err(|error| error.to_string())?;
            let mut magic = [0u8; 8];
            file.read_exact(&mut magic)
                .map_err(|error| format!("{}: {error}", archive.display()))?;
            if &magic != b"!<arch>\n" {
                return Err(format!(
                    "{}: expected a regular static archive (thin archives are unsupported)",
                    archive.display()
                ));
            }
            identity.field(
                &file
                    .metadata()
                    .map_err(|error| error.to_string())?
                    .len()
                    .to_le_bytes(),
            );
            identity.bytes(&magic);
            let mut buffer = [0u8; 65536];
            loop {
                let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
                if count == 0 {
                    break;
                }
                identity.bytes(&buffer[..count]);
            }
            // Whole-archive retains native initialization/finalization objects
            // even when no exported Lisp entry point references them directly.
            println!("cargo:rustc-link-lib=static:+whole-archive={link_name}");
        }
        identity.field(&(library.symbols.len() as u64).to_le_bytes());
        let mut entries = String::new();
        for symbol in &library.symbols {
            identity.field(symbol.as_bytes());
            // Taking an extern byte's address makes no C prototype assumption
            // and also supports data symbols. The linker resolves link_name.
            // Mutable prevents an immutable Rust declaration from promising
            // that a C data symbol's bytes can never change. We only take a
            // raw address, and never read or write through this declaration.
            writeln!(
                declarations,
                "#[link_name = {symbol:?}] static mut EGCL_STATIC_{serial}: u8;"
            )
            .unwrap();
            write!(
                entries,
                "({symbol:?}, unsafe {{ std::ptr::addr_of!(EGCL_STATIC_{serial}).cast::<()>() }}),"
            )
            .unwrap();
            serial += 1;
        }
        let aliases = library
            .aliases
            .iter()
            .map(|name| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(",");
        writeln!(registration, "unsafe {{ egcl_rt::ffi::register_static_library({:?}, &[{aliases}], &[{entries}])?; }}", library.name).unwrap();
    }
    let code = format!(
        "unsafe extern \"C\" {{\n{declarations}}}\npub(super) fn register_linked_libraries() -> Result<(), egcl_rt::EgclError> {{\n{registration}Ok(())\n}}\n"
    );
    std::fs::write(out.join("static-foreign.rs"), code).map_err(|error| error.to_string())?;
    Ok(Configuration {
        manifest,
        identity: Some(format!("{:032x}", identity.0)),
    })
}

pub fn combine_identity(source: &str, archive: &str) -> String {
    let mut identity = Identity::new();
    identity.field(source.as_bytes());
    identity.field(archive.as_bytes());
    format!("{:032x}", identity.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_preserves_names_spaces_and_relative_archive_paths() {
        let libraries = parse(Path::new("/build inputs/demo.foreign"),
            "# linked inputs\nversion = 1\nlibrary = demo name\nalias = demo.so\narchive = native objects/libdemo.a\nsymbol = demo_add\nlibrary = other\narchive = other.a\nsymbol = other_fn\n").unwrap();
        assert_eq!(libraries.len(), 2);
        assert_eq!(libraries[0].name, "demo name");
        assert_eq!(libraries[0].aliases, ["demo.so"]);
        assert_eq!(
            libraries[0].archives,
            [PathBuf::from("/build inputs/native objects/libdemo.a")]
        );
        assert_eq!(libraries[1].symbols, ["other_fn"]);
    }

    #[test]
    fn manifest_rejects_ambiguous_incomplete_or_misplaced_fields() {
        for text in [
            "library = demo\narchive = demo.a\nsymbol = f",
            "version = 2\nlibrary = demo\narchive = demo.a\nsymbol = f",
            "version = 1\nversion = 1\nlibrary = demo\narchive = demo.a\nsymbol = f",
            "version = 1\narchive = demo.a\nlibrary = demo\nsymbol = f",
            "version = 1\nlibrary = demo\nsymbol = f",
            "version = 1\nlibrary = demo\narchive = demo.a",
            "version = 1\nlibrary = demo\narchive = demo.a\nsymbol = f\nsymbol = f",
            "version = 1\nlibrary = demo\nalias = demo\narchive = demo.a\nsymbol = f",
            "version = 1\nlibrary = demo\narchive = demo.a\nsymbol = f\nlibrary = other\nalias = demo\narchive = other.a\nsymbol = g",
            "version = 1\nlibrary = demo\narchive = demo.a\nsymbol = bad\0name",
        ] {
            assert!(parse(Path::new("demo.foreign"), text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn identity_frames_fields_and_separates_archive_from_source() {
        let mut first = Identity::new();
        first.field(b"ab");
        first.field(b"c");
        let mut second = Identity::new();
        second.field(b"a");
        second.field(b"bc");
        assert_ne!(first.0, second.0);
        assert_ne!(
            combine_identity("source", "archive-a"),
            combine_identity("source", "archive-b")
        );
        assert_ne!(
            combine_identity("source-a", "archive"),
            combine_identity("source-b", "archive")
        );
    }
}
