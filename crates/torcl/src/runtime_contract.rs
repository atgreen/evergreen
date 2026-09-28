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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeTier {
    T0,
    T1,
    T2,
}

impl NativeTier {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "t0" => Ok(Self::T0),
            "t1" => Ok(Self::T1),
            "t2" => Ok(Self::T2),
            _ => Err("max-tier must be t0, t1, or t2".into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::T0 => "t0",
            Self::T1 => "t1",
            Self::T2 => "t2",
        }
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
    pub max_tier: NativeTier,
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
                "max-tier",
            ]
            .contains(&key)
                || fields.insert(key, value).is_some()
            {
                return Err(format!(
                    "unknown or duplicate runtime contract field: {key}"
                ));
            }
        }
        if fields.remove("schema") != Some("3") {
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
        let max_tier = NativeTier::parse(
            fields
                .remove("max-tier")
                .ok_or("missing runtime max-tier")?,
        )?;
        Ok(Self {
            source,
            target,
            toolchain,
            features: feature_set,
            rustflags,
            capabilities,
            builtins,
            max_tier,
        })
    }
    pub fn encode(&self) -> String {
        format!(
            "schema=3\nsource={}\ntarget={}\ntoolchain={}\nfeatures={}\nrustflags={}\ncapabilities={}\nbuiltins={}\nmax-tier={}\n",
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
            ),
            self.max_tier.name()
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
        if self.max_tier < required.max_tier {
            return Err("image requires unavailable native compilation tiers".into());
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
            "schema=3\nsource=test\ntarget=test\ntoolchain=test\nfeatures=\nrustflags=\ncapabilities={capabilities}\nbuiltins={builtins}\nmax-tier=t2\n"
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

    #[test]
    fn native_tier_requirements_follow_available_compilers() {
        let full = contract("*", "").unwrap();
        for (available_index, available) in ["t0", "t1", "t2"].iter().enumerate() {
            let runtime = Contract::parse(
                &full
                    .encode()
                    .replace("max-tier=t2", &format!("max-tier={available}")),
            )
            .unwrap();
            assert_eq!(Contract::parse(&runtime.encode()).unwrap(), runtime);
            for (required_index, required) in ["t0", "t1", "t2"].iter().enumerate() {
                let image = Contract::parse(
                    &full
                        .encode()
                        .replace("max-tier=t2", &format!("max-tier={required}")),
                )
                .unwrap();
                assert_eq!(
                    runtime.accepts(&image).is_ok(),
                    available_index >= required_index
                );
            }
        }
        for invalid in ["", "t3", "native", "T0"] {
            assert!(
                Contract::parse(
                    &full
                        .encode()
                        .replace("max-tier=t2", &format!("max-tier={invalid}"))
                )
                .is_err()
            );
        }
    }
}
