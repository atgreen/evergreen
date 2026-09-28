//! Native delivery compatibility contract, shared by build.rs and the CLI.
use std::collections::{BTreeMap, BTreeSet};

pub const CAPABILITIES: &[&str] = &["disassembly", "dynamic-code", "tree-walker"];

/// These public operations can introduce arbitrary code (including reader #.).
/// Their graph edge reaches every native capability. Internal EvalHost executes
/// a saved constant form whose references are traced separately.
pub fn opens_code_world(name: &str) -> bool {
    matches!(
        name,
        "EVAL"
            | "LOAD"
            | "REQUIRE"
            | "COMPILE"
            | "COMPILE-FILE"
            | "READ"
            | "READ-PRESERVING-WHITESPACE"
            | "READ-FROM-STRING"
            | "READ-DELIMITED-LIST"
    )
}

pub fn close_capabilities(capabilities: &mut BTreeSet<String>) {
    if capabilities.contains("dynamic-code") {
        capabilities.extend(CAPABILITIES.iter().map(|name| (*name).to_owned()));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contract {
    pub source: String,
    pub target: String,
    pub toolchain: String,
    pub features: BTreeSet<String>,
    pub rustflags: String,
    pub capabilities: BTreeSet<String>,
    /// None means the complete builtin set; Some(empty) means no dispatch arms.
    pub builtins: Option<BTreeSet<String>>,
}
impl Contract {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            let (key, value) = line
                .split_once('=')
                .ok_or("invalid runtime contract line")?;
            if ![
                "schema",
                "source",
                "target",
                "toolchain",
                "features",
                "rustflags",
                "capabilities",
                "builtins",
            ]
            .contains(&key)
                || fields.insert(key, value).is_some()
            {
                return Err(format!(
                    "unknown or duplicate runtime contract field: {key}"
                ));
            }
        }
        if fields.remove("schema") != Some("2") {
            return Err("unsupported runtime contract schema".into());
        }
        let mut required = |key| {
            fields
                .remove(key)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("missing runtime contract {key}"))
        };
        let source = required("source")?;
        let target = required("target")?;
        let toolchain = required("toolchain")?;
        let features = fields
            .remove("features")
            .ok_or("missing runtime features")?;
        let mut feature_set = BTreeSet::new();
        if !features.is_empty() {
            for feature in features.split(',') {
                if ![
                    "thread-cache-alloc",
                    "alloc-count",
                    "python",
                    "torcl-stdlib/python",
                    "torcl-rt/c-ffi",
                    "torcl-rt/python",
                ]
                .contains(&feature)
                    || !feature_set.insert(feature.to_owned())
                {
                    return Err(format!("unknown or duplicate build feature: {feature}"));
                }
            }
        }
        let rustflags = fields
            .remove("rustflags")
            .ok_or("missing runtime rustflags")?
            .to_owned();
        if rustflags.len() % 2 != 0 || !rustflags.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid encoded runtime rustflags".into());
        }
        let raw = fields
            .remove("capabilities")
            .ok_or("missing runtime capabilities")?;
        let mut capabilities = BTreeSet::new();
        for name in raw.split(',').filter(|v| !v.is_empty()) {
            if !CAPABILITIES.contains(&name) || !capabilities.insert(name.to_owned()) {
                return Err(format!("unknown or duplicate runtime capability: {name}"));
            }
        }
        if !raw.is_empty() && raw.split(',').any(str::is_empty) {
            return Err("empty runtime capability".into());
        }
        let mut closed = capabilities.clone();
        close_capabilities(&mut closed);
        if closed != capabilities {
            return Err("dynamic-code requires every native capability".into());
        }
        let raw = fields
            .remove("builtins")
            .ok_or("missing runtime builtins")?;
        let builtins = if raw == "*" {
            None
        } else {
            let mut names = BTreeSet::new();
            if !raw.is_empty() {
                for encoded in raw.split(',') {
                    if encoded.is_empty()
                        || encoded.len() % 2 != 0
                        || !encoded.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        return Err("invalid encoded builtin name".into());
                    }
                    let bytes = encoded
                        .as_bytes()
                        .chunks_exact(2)
                        .map(|pair| {
                            u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
                        })
                        .collect();
                    let name = String::from_utf8(bytes).map_err(|_| "invalid builtin UTF-8")?;
                    if name.chars().any(char::is_control) || !names.insert(name) {
                        return Err("invalid or duplicate builtin name".into());
                    }
                }
            }
            Some(names)
        };
        if capabilities.contains("dynamic-code") && builtins.is_some() {
            return Err("dynamic-code requires every native builtin".into());
        }
        Ok(Self {
            source,
            target,
            toolchain,
            features: feature_set,
            rustflags,
            capabilities,
            builtins,
        })
    }
    pub fn encode(&self) -> String {
        format!(
            "schema=2\nsource={}\ntarget={}\ntoolchain={}\nfeatures={}\nrustflags={}\ncapabilities={}\nbuiltins={}\n",
            self.source,
            self.target,
            self.toolchain,
            self.features.iter().cloned().collect::<Vec<_>>().join(","),
            self.rustflags,
            self.capabilities
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
            self.builtins.as_ref().map_or_else(
                || "*".to_owned(),
                |names| {
                    names
                        .iter()
                        .map(|name| name.bytes().map(|b| format!("{b:02x}")).collect::<String>())
                        .collect::<Vec<_>>()
                        .join(",")
                }
            )
        )
    }
    pub fn accepts(&self, required: &Self) -> Result<(), String> {
        for (name, actual, expected) in [
            ("source", &self.source, &required.source),
            ("target", &self.target, &required.target),
            ("toolchain", &self.toolchain, &required.toolchain),
        ] {
            if actual != expected {
                return Err(format!("runtime {name} mismatch: {actual} != {expected}"));
            }
        }
        if self.features != required.features || self.rustflags != required.rustflags {
            return Err("runtime build features or compiler flags mismatch".into());
        }
        if !required.capabilities.is_subset(&self.capabilities) {
            return Err("image requires unavailable native runtime capabilities".into());
        }
        if let Some(available) = &self.builtins {
            if required
                .builtins
                .as_ref()
                .is_none_or(|needed| !needed.is_subset(available))
            {
                return Err("image requires unavailable native builtins".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod builtin_contract_tests {
    use super::Contract;

    fn contract(builtins: &str, capabilities: &str) -> Result<Contract, String> {
        Contract::parse(&format!(
            "schema=2\nsource=test\ntarget=test\ntoolchain=test\nfeatures=\nrustflags=\ncapabilities={capabilities}\nbuiltins={builtins}\n"
        ))
    }

    #[test]
    fn builtin_requirements_are_checked_when_restoring_an_image() {
        // Names are UTF-8 hex: '*' is multiplication, not the full-set marker.
        let full = contract("*", "").unwrap();
        let small = contract("2a,434152", "").unwrap();
        let car = contract("434152", "").unwrap();
        let empty = contract("", "").unwrap();
        assert!(full.accepts(&small).is_ok());
        assert!(small.accepts(&car).is_ok());
        assert!(car.accepts(&small).is_err());
        assert!(small.accepts(&full).is_err());
        assert!(small.accepts(&empty).is_ok());
        assert!(empty.accepts(&car).is_err());
        assert_eq!(Contract::parse(&small.encode()).unwrap(), small);
    }

    #[test]
    fn dynamic_code_requires_all_native_builtins() {
        let capabilities = "disassembly,dynamic-code,tree-walker";
        assert!(contract("*", capabilities).is_ok());
        assert!(contract("434152", capabilities).is_err());
        assert!(contract("", capabilities).is_err());
    }

    #[test]
    fn malformed_builtin_sets_are_rejected() {
        for names in [
            "CAR",
            "0",
            "ff",
            "00",
            "434152,434152",
            ",434152",
            "434152,",
        ] {
            assert!(contract(names, "").is_err(), "accepted {names}");
        }
    }
}
