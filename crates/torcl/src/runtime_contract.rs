//! Native delivery compatibility contract, shared by build.rs and the CLI.
use std::collections::{BTreeMap, BTreeSet};

pub const CAPABILITIES: &[&str] = &["disassembly", "dynamic-code"];

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
            ]
            .contains(&key)
                || fields.insert(key, value).is_some()
            {
                return Err(format!(
                    "unknown or duplicate runtime contract field: {key}"
                ));
            }
        }
        if fields.remove("schema") != Some("1") {
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
        Ok(Self {
            source,
            target,
            toolchain,
            features: feature_set,
            rustflags,
            capabilities,
        })
    }
    pub fn encode(&self) -> String {
        format!(
            "schema=1\nsource={}\ntarget={}\ntoolchain={}\nfeatures={}\nrustflags={}\ncapabilities={}\n",
            self.source,
            self.target,
            self.toolchain,
            self.features.iter().cloned().collect::<Vec<_>>().join(","),
            self.rustflags,
            self.capabilities
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(",")
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
        Ok(())
    }
}
