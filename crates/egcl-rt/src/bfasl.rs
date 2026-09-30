// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! EGCL FASL (`.bfasl`) portable compiled-artifact container (bliss-lb6.6).
//!
//! See spec §6.11. This module implements the *container* — the header, the
//! versioned + checksummed section framing, and the loader verification rules
//! (R6.60–R6.65) — independent of what the sections contain. Higher layers build
//! sections (compiled functions, constant pool, source maps, …) and hand them
//! here; the loader verifies and yields the sections back.
//!
//! Format (little-endian):
//! ```text
//!   0  6  magic          b"BFASL\0"
//!   6  2  format_version u16 (major<<8 | minor)
//!   8  1  flags          u8
//!   9  1  platform       u8  (0 = portable/arch-neutral)
//!  10  2  target_abi     u16 (0 when portable)
//!  12  8  content_hash   u64 (source + dependency identities; cache key)
//!  20  4  section_count  u32
//!  24  …  sections[]     { kind:u16, length:u32, bytes }
//!   …  4  checksum       u32 (FNV-1a over all preceding bytes)
//! ```

/// Magic prefix. A valid `.bfasl` starts with these 6 bytes.
pub const BFASL_MAGIC: [u8; 6] = *b"BFASL\0";
/// Current format version: `major<<8 | minor`.
pub const BFASL_VERSION: u16 = 0x0100; // 1.0

const HEADER_LEN: usize = 24;
const CHECKSUM_LEN: usize = 4;

/// Section kind identifiers (spec §6.11.2). Unknown kinds are skippable.
pub mod section {
    pub const FUNCTIONS: u16 = 1;
    pub const CONSTANT_POOL: u16 = 2;
    pub const SYMBOLS: u16 = 3;
    pub const PACKAGES: u16 = 4;
    pub const SOURCE_MAP: u16 = 5;
    pub const DEBUG: u16 = 6;
    pub const STACKMAPS: u16 = 7;
    pub const RELOCATIONS: u16 = 8;
    pub const DEPENDENCIES: u16 = 9;
    pub const CACHED_T1: u16 = 10;
    pub const TOPLEVEL_FORMS: u16 = 11;
    pub const BYTECODE_UNIT: u16 = 12;
}

/// Why a `.bfasl` failed to load (R6.63). Every variant is a clean rejection,
/// never a crash or silent partial read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BfaslError {
    /// The magic prefix did not match — not a `.bfasl`.
    BadMagic,
    /// Incompatible `format_version` (differing major, or newer minor). R6.64.
    VersionMismatch { found: u16, expected: u16 },
    /// The file ended before a declared header field or section.
    Truncated,
    /// The trailing checksum did not verify.
    BadChecksum,
}

impl std::fmt::Display for BfaslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BfaslError::BadMagic => write!(f, "not a .bfasl file (bad magic)"),
            BfaslError::VersionMismatch { found, expected } => write!(
                f,
                ".bfasl version mismatch: file {:#06x}, runtime {:#06x}",
                found, expected
            ),
            BfaslError::Truncated => write!(f, "truncated .bfasl file"),
            BfaslError::BadChecksum => write!(f, ".bfasl checksum verification failed"),
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A 64-bit content hash of a unit's source text (and, appended, dependency
/// identities) for cache invalidation (R6.70).
pub fn content_hash(source: &[u8]) -> u64 {
    fnv1a(source)
}

/// Builds a `.bfasl` byte image from sections (spec §6.11.2).
#[derive(Default)]
pub struct BfaslBuilder {
    flags: u8,
    platform: u8,
    target_abi: u16,
    content_hash: u64,
    sections: Vec<(u16, Vec<u8>)>,
}

impl BfaslBuilder {
    /// A new builder for a portable (`platform = 0`) unit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the cache-invalidation content hash (R6.61/R6.70).
    pub fn content_hash(mut self, h: u64) -> Self {
        self.content_hash = h;
        self
    }

    /// Append a section. Order is preserved; the loader indexes by kind.
    pub fn section(mut self, kind: u16, bytes: Vec<u8>) -> Self {
        self.sections.push((kind, bytes));
        self
    }

    /// Serialize to the on-disk `.bfasl` byte image.
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + CHECKSUM_LEN);
        out.extend_from_slice(&BFASL_MAGIC);
        out.extend_from_slice(&BFASL_VERSION.to_le_bytes());
        out.push(self.flags);
        out.push(self.platform);
        out.extend_from_slice(&self.target_abi.to_le_bytes());
        out.extend_from_slice(&self.content_hash.to_le_bytes());
        out.extend_from_slice(&(self.sections.len() as u32).to_le_bytes());
        for (kind, bytes) in &self.sections {
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        let ck = fnv1a(&out) as u32;
        out.extend_from_slice(&ck.to_le_bytes());
        out
    }
}

/// A verified, parsed `.bfasl` (header + sections). Produced by [`load`].
#[derive(Debug)]
pub struct Bfasl {
    pub version: u16,
    pub flags: u8,
    pub platform: u8,
    pub target_abi: u16,
    pub content_hash: u64,
    sections: Vec<(u16, Vec<u8>)>,
}

impl Bfasl {
    /// The first section of `kind`, if present.
    pub fn section(&self, kind: u16) -> Option<&[u8]> {
        self.sections
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, b)| b.as_slice())
    }

    /// All sections, in file order (`kind`, `bytes`).
    pub fn sections(&self) -> impl Iterator<Item = (u16, &[u8])> {
        self.sections.iter().map(|(k, b)| (*k, b.as_slice()))
    }
}

fn take<'a>(data: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], BfaslError> {
    let end = pos
        .checked_add(n)
        .filter(|&e| e <= data.len())
        .ok_or(BfaslError::Truncated)?;
    let s = &data[*pos..end];
    *pos = end;
    Ok(s)
}

/// Verify and parse a `.bfasl` image (R6.63/R6.64): checks magic, version
/// compatibility (same major, minor ≤ ours), and the trailing checksum before
/// materializing any section, then returns the parsed header + sections.
pub fn load(data: &[u8]) -> Result<Bfasl, BfaslError> {
    if data.len() < HEADER_LEN + CHECKSUM_LEN {
        return Err(BfaslError::Truncated);
    }
    if data[..6] != BFASL_MAGIC {
        return Err(BfaslError::BadMagic);
    }
    let version = u16::from_le_bytes([data[6], data[7]]);
    // Same major required; minor must not be newer than ours (R6.64).
    if (version >> 8) != (BFASL_VERSION >> 8) || (version & 0xff) > (BFASL_VERSION & 0xff) {
        return Err(BfaslError::VersionMismatch {
            found: version,
            expected: BFASL_VERSION,
        });
    }
    // Verify the checksum over everything before the trailing 4 bytes.
    let ck_off = data.len() - CHECKSUM_LEN;
    let stored = u32::from_le_bytes(data[ck_off..].try_into().unwrap());
    if (fnv1a(&data[..ck_off]) as u32) != stored {
        return Err(BfaslError::BadChecksum);
    }

    let mut pos = 8usize;
    let flags = take(data, &mut pos, 1)?[0];
    let platform = take(data, &mut pos, 1)?[0];
    let target_abi = u16::from_le_bytes(take(data, &mut pos, 2)?.try_into().unwrap());
    let content_hash = u64::from_le_bytes(take(data, &mut pos, 8)?.try_into().unwrap());
    let section_count = u32::from_le_bytes(take(data, &mut pos, 4)?.try_into().unwrap()) as usize;

    let mut sections = Vec::with_capacity(section_count);
    for _ in 0..section_count {
        // Do not read into the checksum trailer.
        if pos + 6 > ck_off {
            return Err(BfaslError::Truncated);
        }
        let kind = u16::from_le_bytes(take(data, &mut pos, 2)?.try_into().unwrap());
        let len = u32::from_le_bytes(take(data, &mut pos, 4)?.try_into().unwrap()) as usize;
        if pos + len > ck_off {
            return Err(BfaslError::Truncated);
        }
        let bytes = take(data, &mut pos, len)?.to_vec();
        sections.push((kind, bytes));
    }

    Ok(Bfasl {
        version,
        flags,
        platform,
        target_abi,
        content_hash,
        sections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_sections_and_header() {
        let hash = content_hash(b"(defun f (x) (+ x 1))");
        let img = BfaslBuilder::new()
            .content_hash(hash)
            .section(section::TOPLEVEL_FORMS, b"(defun f (x) (+ x 1))".to_vec())
            .section(section::SOURCE_MAP, b"F\0demo.lisp\0".to_vec())
            .build();

        let f = load(&img).expect("valid .bfasl loads");
        assert_eq!(f.version, BFASL_VERSION);
        assert_eq!(f.platform, 0, "portable");
        assert_eq!(f.content_hash, hash);
        assert_eq!(
            f.section(section::TOPLEVEL_FORMS).unwrap(),
            b"(defun f (x) (+ x 1))"
        );
        // Debug/source metadata survives a fresh parse (R6.68).
        assert_eq!(f.section(section::SOURCE_MAP).unwrap(), b"F\0demo.lisp\0");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut img = BfaslBuilder::new().build();
        img[0] = b'X';
        assert_eq!(load(&img).unwrap_err(), BfaslError::BadMagic);
    }

    #[test]
    fn rejects_version_mismatch() {
        // A newer major must be rejected (R6.64).
        let mut img = BfaslBuilder::new().build();
        let bad = (BFASL_VERSION + 0x0100).to_le_bytes(); // bump major
        img[6] = bad[0];
        img[7] = bad[1];
        // Re-checksum so we exercise the version path, not the checksum path.
        let ck_off = img.len() - CHECKSUM_LEN;
        let ck = fnv1a(&img[..ck_off]) as u32;
        img[ck_off..].copy_from_slice(&ck.to_le_bytes());
        match load(&img) {
            Err(BfaslError::VersionMismatch { .. }) => {}
            other => panic!("expected version mismatch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_tampered_checksum() {
        let mut img = BfaslBuilder::new()
            .section(section::TOPLEVEL_FORMS, b"nil".to_vec())
            .build();
        // Flip a payload byte without fixing the checksum.
        let i = HEADER_LEN + 8; // somewhere in the section bytes
        img[i] ^= 0xff;
        assert_eq!(load(&img).unwrap_err(), BfaslError::BadChecksum);
    }

    #[test]
    fn rejects_truncated() {
        let img = BfaslBuilder::new().build();
        assert_eq!(load(&img[..HEADER_LEN]).unwrap_err(), BfaslError::Truncated);
    }

    #[test]
    fn unknown_section_kinds_are_preserved_and_skippable() {
        let img = BfaslBuilder::new()
            .section(0xBEEF, b"future".to_vec())
            .section(section::TOPLEVEL_FORMS, b"ok".to_vec())
            .build();
        let f = load(&img).expect("loads despite unknown section");
        assert_eq!(f.section(0xBEEF).unwrap(), b"future");
        assert_eq!(f.section(section::TOPLEVEL_FORMS).unwrap(), b"ok");
    }

    #[test]
    fn bytecode_unit_section_round_trips() {
        let img = BfaslBuilder::new()
            .section(section::BYTECODE_UNIT, b"BBU\0payload".to_vec())
            .section(section::TOPLEVEL_FORMS, b"ok".to_vec())
            .build();
        let f = load(&img).expect("loads bfasl with bytecode unit");
        assert_eq!(f.section(section::BYTECODE_UNIT).unwrap(), b"BBU\0payload");
    }
}
