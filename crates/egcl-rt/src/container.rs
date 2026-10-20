// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The zip archive a saved executable carries after the runtime binary
//! (bliss-rhsy4.9).
//!
//! A zip is read from its end: the end-of-central-directory record locates a
//! directory of members, and readers accept arbitrary bytes before the first
//! member. So `[runtime][zip]` still runs as the runtime, and is also a zip
//! that `unzip`, `zip`, `7z` and Python's `zipfile` list and edit without
//! touching the runtime in front. That is what lets a user replace an
//! embedded LGPL library with their own build.
//!
//! Members:
//!
//! - `egcl/manifest`: UTF-8 text, one record per line, tab-separated. Line one
//!   is `egcl-container 1`. `application` marks an application executable;
//!   `library MEMBER NAME...` gives the names a library answers to beyond its
//!   member's file name. Unknown records are ignored.
//! - `egcl/core`: the EGCLIMG core, stored (uncompressed) so it is mapped from
//!   the file in place.
//! - `lib/FILE`: embedded shared libraries, stored or deflated. One added by
//!   hand without a manifest record answers to its file name.
//!
//! A writer stores every member, pads the core to a 4 KiB file offset and
//! libraries to 16 KiB (the zipalign convention, in the local header's extra
//! field), and records no timestamps, so a save is reproducible. Archives
//! larger than 4 GiB (ZIP64) and encrypted members are refused.

use crate::error::EgclError;

pub const CORE: &str = "egcl/core";
pub const MANIFEST: &str = "egcl/manifest";
pub const LIBRARY_DIR: &str = "lib/";

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const END_SIGNATURE: u32 = 0x0605_4b50;
const END_SIZE: usize = 22;
const LOCAL_SIZE: u64 = 30;
pub const STORED: u16 = 0;
pub const DEFLATED: u16 = 8;
/// Android zipalign's extra-field id for alignment padding.
const ALIGNMENT_EXTRA: u16 = 0xd935;
/// 1980-01-01 00:00, the earliest DOS date.
const DOS_DATE: u16 = 0x0021;
const MANIFEST_HEADER: &str = "egcl-container 1";

fn invalid(what: impl std::fmt::Display) -> EgclError {
    EgclError::InvalidImage(format!("executable archive: {what}"))
}

/// Random access to the bytes holding an archive.
pub trait ReadAt {
    fn size(&self) -> std::io::Result<u64>;
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
}

impl ReadAt for [u8] {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.len() as u64)
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| std::io::ErrorKind::UnexpectedEof)?;
        let bytes = start
            .checked_add(buf.len())
            .and_then(|end| self.get(start..end))
            .ok_or(std::io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

impl ReadAt for std::fs::File {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    #[cfg(unix)]
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }

    #[cfg(not(unix))]
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = self;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }
}

/// One member, located in the underlying file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub size: u64,
    /// Absolute offset of the member's (possibly compressed) data.
    pub data_offset: u64,
}

#[derive(Debug)]
pub struct Archive {
    /// Offset of the archive's first byte: everything before it is the
    /// runtime binary.
    pub start: u64,
    pub members: Vec<Member>,
}

impl Archive {
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    /// The core member, which must be stored so it can be mapped in place.
    pub fn core(&self) -> Result<Option<&Member>, EgclError> {
        match self.member(CORE) {
            Some(m) if m.method != STORED || m.compressed_size != m.size => Err(invalid(format!(
                "{CORE} is compressed; it must be stored (zip -0) so it can be mapped"
            ))),
            core => Ok(core),
        }
    }

    pub fn manifest(&self, source: &(impl ReadAt + ?Sized)) -> Result<Manifest, EgclError> {
        match self.member(MANIFEST) {
            None => Ok(Manifest::default()),
            Some(m) => {
                let bytes = read_member(source, m)?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| invalid(format!("{MANIFEST} is not UTF-8")))?;
                Manifest::parse(&text)
            }
        }
    }

    /// The members under `lib/`, in archive order.
    pub fn libraries(&self) -> impl Iterator<Item = &Member> {
        self.members
            .iter()
            .filter(|m| m.name.starts_with(LIBRARY_DIR) && !m.name.ends_with('/'))
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// The archive at the end of `source`, or `None` when it does not end with
/// one. Bytes before the archive are allowed, as are archives whose recorded
/// offsets are relative to its own start rather than the file's (a zip
/// concatenated after the fact); `unzip` makes the same correction.
pub fn read(source: &(impl ReadAt + ?Sized)) -> Result<Option<Archive>, EgclError> {
    let io = |e: std::io::Error| invalid(e);
    let len = source.size().map_err(io)?;
    if len < END_SIZE as u64 {
        return Ok(None);
    }
    // The end record is the last 22 bytes plus a comment of up to 64 KiB.
    // Archives this runtime writes have no comment, so try the short read
    // first: a saved executable's startup then reads 22 bytes, not 64 KiB.
    let mut tail = vec![0; END_SIZE];
    source
        .read_exact_at(&mut tail, len - END_SIZE as u64)
        .map_err(io)?;
    if u32_at(&tail, 0) != END_SIGNATURE || u16_at(&tail, 20) != 0 {
        let tail_len = len.min((END_SIZE + u16::MAX as usize) as u64) as usize;
        tail = vec![0; tail_len];
        source
            .read_exact_at(&mut tail, len - tail_len as u64)
            .map_err(io)?;
    }
    let tail_len = tail.len();
    let tail_start = len - tail_len as u64;
    let Some(at) = (0..=tail_len - END_SIZE).rev().find(|&at| {
        u32_at(&tail, at) == END_SIGNATURE
            && at + END_SIZE + u16_at(&tail, at + 20) as usize == tail_len
    }) else {
        return Ok(None);
    };
    let end = &tail[at..at + END_SIZE];
    if u16_at(end, 4) != 0 || u16_at(end, 6) != 0 {
        return Err(invalid("multi-disk archives are not supported"));
    }
    let count = u16_at(end, 10);
    let (cd_size, cd_offset) = (u32_at(end, 12) as u64, u32_at(end, 16) as u64);
    if count == u16::MAX || cd_size == u32::MAX as u64 || cd_offset == u32::MAX as u64 {
        return Err(invalid("ZIP64 archives are not supported"));
    }
    let end_offset = tail_start + at as u64;
    let cd_start = end_offset
        .checked_sub(cd_size)
        .ok_or_else(|| invalid("central directory overruns the file"))?;
    let shift = cd_start
        .checked_sub(cd_offset)
        .ok_or_else(|| invalid("central directory offset is past its position"))?;

    let mut directory = vec![0; cd_size as usize];
    source.read_exact_at(&mut directory, cd_start).map_err(io)?;
    let mut members = Vec::with_capacity(count as usize);
    let mut start = cd_start;
    let mut pos = 0usize;
    for _ in 0..count {
        let header = directory
            .get(pos..pos + 46)
            .ok_or_else(|| invalid("truncated central directory"))?;
        if u32_at(header, 0) != CENTRAL_SIGNATURE {
            return Err(invalid("bad central directory entry"));
        }
        let flags = u16_at(header, 8);
        let method = u16_at(header, 10);
        let crc32 = u32_at(header, 16);
        let compressed_size = u32_at(header, 20) as u64;
        let size = u32_at(header, 24) as u64;
        let name_len = u16_at(header, 28) as usize;
        let skip = name_len + u16_at(header, 30) as usize + u16_at(header, 32) as usize;
        let local = (u32_at(header, 42) as u64)
            .checked_add(shift)
            .ok_or_else(|| invalid("member offset overflow"))?;
        let name = directory
            .get(pos + 46..pos + 46 + name_len)
            .ok_or_else(|| invalid("truncated central directory"))?;
        let name =
            String::from_utf8(name.to_vec()).map_err(|_| invalid("member name is not UTF-8"))?;
        pos += 46 + skip;
        if flags & 1 != 0 {
            return Err(invalid(format!("{name} is encrypted")));
        }
        if compressed_size == u32::MAX as u64 || size == u32::MAX as u64 {
            return Err(invalid("ZIP64 archives are not supported"));
        }
        // The data follows the LOCAL header, whose extra field may differ
        // from the central one (alignment padding lives only there).
        let mut local_header = [0u8; LOCAL_SIZE as usize];
        source.read_exact_at(&mut local_header, local).map_err(io)?;
        if u32_at(&local_header, 0) != LOCAL_SIGNATURE {
            return Err(invalid(format!("{name}: bad local header")));
        }
        let data_offset = local
            + LOCAL_SIZE
            + u16_at(&local_header, 26) as u64
            + u16_at(&local_header, 28) as u64;
        if data_offset + compressed_size > cd_start {
            return Err(invalid(format!("{name} overruns the central directory")));
        }
        start = start.min(local);
        members.push(Member {
            name,
            method,
            crc32,
            compressed_size,
            size,
            data_offset,
        });
    }
    Ok(Some(Archive { start, members }))
}

/// A member's uncompressed bytes, checked against its CRC-32.
pub fn read_member(source: &(impl ReadAt + ?Sized), member: &Member) -> Result<Vec<u8>, EgclError> {
    let too_large = || {
        invalid(format!(
            "{} is too large for this address space",
            member.name
        ))
    };
    let mut raw = vec![0; usize::try_from(member.compressed_size).map_err(|_| too_large())?];
    source
        .read_exact_at(&mut raw, member.data_offset)
        .map_err(|e| invalid(format!("{}: {e}", member.name)))?;
    let bytes = match member.method {
        STORED => raw,
        DEFLATED => inflate(&raw, usize::try_from(member.size).map_err(|_| too_large())?)
            .map_err(|e| invalid(format!("{}: {e}", member.name)))?,
        method => {
            return Err(invalid(format!(
                "{} uses compression method {method}; only stored and deflated members are supported",
                member.name
            )));
        }
    };
    if bytes.len() as u64 != member.size || crc32(&bytes) != member.crc32 {
        return Err(invalid(format!("{} failed its CRC-32 check", member.name)));
    }
    Ok(bytes)
}

// ── Manifest ────────────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub application: bool,
    /// Member name, then the extra names that library answers to.
    pub libraries: Vec<(String, Vec<String>)>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, EgclError> {
        let mut lines = text.lines();
        if lines.next() != Some(MANIFEST_HEADER) {
            return Err(invalid(format!(
                "{MANIFEST} does not start with '{MANIFEST_HEADER}'"
            )));
        }
        let mut manifest = Manifest::default();
        for line in lines {
            let mut fields = line.split('\t');
            match fields.next() {
                Some("application") => manifest.application = true,
                Some("library") => {
                    if let Some(member) = fields.next() {
                        manifest.libraries.push((
                            member.to_owned(),
                            fields
                                .filter(|f| !f.is_empty())
                                .map(str::to_owned)
                                .collect(),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(manifest)
    }

    pub fn render(&self) -> Result<String, EgclError> {
        let mut text = format!("{MANIFEST_HEADER}\n");
        if self.application {
            text.push_str("application\n");
        }
        for (member, names) in &self.libraries {
            text.push_str("library");
            for field in std::iter::once(member).chain(names) {
                if field.contains(['\t', '\n', '\r']) {
                    return Err(EgclError::FfiError(format!(
                        "embedded library name {field:?} contains a tab or line break"
                    )));
                }
                text.push('\t');
                text.push_str(field);
            }
            text.push('\n');
        }
        Ok(text)
    }

    pub fn names_for(&self, member: &str) -> &[String] {
        self.libraries
            .iter()
            .find(|(m, _)| m == member)
            .map_or(&[], |(_, names)| names)
    }
}

// ── Writing ─────────────────────────────────────────────────────────

/// Builds `[prefix][archive]`, every member stored.
pub struct Writer {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl Writer {
    pub fn new(prefix: Vec<u8>) -> Writer {
        Writer {
            out: prefix,
            central: Vec::new(),
            count: 0,
        }
    }

    /// Append a stored member whose data starts at a multiple of `align`
    /// bytes from the start of the whole file. `mode` is its Unix mode.
    pub fn add(&mut self, name: &str, data: &[u8], align: u64, mode: u32) -> Result<(), EgclError> {
        let too_large = || invalid("archive would need ZIP64, which this runtime does not write");
        let size = u32::try_from(data.len()).map_err(|_| too_large())?;
        let offset = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let name_len = u16::try_from(name.len()).map_err(|_| invalid("member name too long"))?;
        self.count = self.count.checked_add(1).ok_or_else(too_large)?;
        let crc = crc32(data);

        let mut extra = Vec::new();
        if align > 1 {
            let unpadded = offset as u64 + LOCAL_SIZE + name.len() as u64 + 6;
            let pad = (align - unpadded % align) % align;
            extra.extend_from_slice(&ALIGNMENT_EXTRA.to_le_bytes());
            extra.extend_from_slice(&(2 + pad as u16).to_le_bytes());
            extra.extend_from_slice(&(align.min(u16::MAX as u64) as u16).to_le_bytes());
            extra.resize(extra.len() + pad as usize, 0);
        }
        // Version 1.0 (stored), UTF-8 names, no timestamp.
        let common = |out: &mut Vec<u8>| {
            out.extend_from_slice(&10u16.to_le_bytes());
            out.extend_from_slice(&0x0800u16.to_le_bytes());
            out.extend_from_slice(&STORED.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&DOS_DATE.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&name_len.to_le_bytes());
        };
        self.out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
        common(&mut self.out);
        self.out
            .extend_from_slice(&(extra.len() as u16).to_le_bytes());
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(&extra);
        debug_assert!(align <= 1 || self.out.len() as u64 % align == 0);
        self.out.extend_from_slice(data);

        self.central
            .extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
        // Made by Unix, so `unzip` restores the mode.
        self.central.extend_from_slice(&0x031eu16.to_le_bytes());
        common(&mut self.central);
        self.central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attributes
        self.central
            .extend_from_slice(&((0o100000 | mode) << 16).to_le_bytes());
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<u8>, EgclError> {
        let too_large = || invalid("archive would need ZIP64, which this runtime does not write");
        let cd_offset = u32::try_from(self.out.len()).map_err(|_| too_large())?;
        let cd_size = u32::try_from(self.central.len()).map_err(|_| too_large())?;
        self.out.extend_from_slice(&self.central);
        self.out.extend_from_slice(&END_SIGNATURE.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]); // disk numbers
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&cd_size.to_le_bytes());
        self.out.extend_from_slice(&cd_offset.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        Ok(self.out)
    }
}

/// `[prefix][archive]` holding `core` and every embedded foreign library.
/// With an empty prefix this is a stand-alone core file.
pub fn build(prefix: Vec<u8>, core: &[u8], application: bool) -> Result<Vec<u8>, EgclError> {
    let libraries = crate::ffi::embedded::saved_libraries()?;
    let manifest = Manifest {
        application,
        libraries: libraries
            .iter()
            .map(|library| (library.member.clone(), library.names.clone()))
            .collect(),
    };
    let mut writer = Writer::new(prefix);
    writer.add(MANIFEST, manifest.render()?.as_bytes(), 1, 0o644)?;
    writer.add(CORE, core, 4096, 0o644)?;
    for library in &libraries {
        writer.add(&library.member, &library.bytes, 16384, 0o755)?;
    }
    writer.finish()
}

// ── CRC-32 (IEEE 802.3, as zip uses) ────────────────────────────────

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |c, &b| {
        CRC_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8)
    })
}

// ── Inflate (RFC 1951) ──────────────────────────────────────────────
//
// Zip tools deflate members by default, so `zip app.zip lib/libfoo.so.1`
// produces one. Decoding follows zlib's reference `puff`: canonical Huffman
// codes decoded a bit at a time, which is plenty for libraries read once.

struct Bits<'a> {
    input: &'a [u8],
    pos: usize,
    buffer: u64,
    count: u32,
}

impl Bits<'_> {
    fn take(&mut self, need: u32) -> Result<u32, String> {
        while self.count < need {
            let byte = *self
                .input
                .get(self.pos)
                .ok_or("deflate stream ends early")?;
            self.pos += 1;
            self.buffer |= (byte as u64) << self.count;
            self.count += 8;
        }
        let value = (self.buffer & ((1u64 << need) - 1)) as u32;
        self.buffer >>= need;
        self.count -= need;
        Ok(value)
    }
}

struct Huffman {
    /// Number of codes of each length 0..=15.
    count: [u16; 16],
    /// Symbols ordered by code.
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, String> {
        let mut count = [0u16; 16];
        for &len in lengths {
            count[len as usize] += 1;
        }
        let mut left = 1i32;
        for &codes in &count[1..] {
            left = (left << 1) - codes as i32;
            if left < 0 {
                return Err("over-subscribed Huffman code".into());
            }
        }
        let mut offsets = [0u16; 16];
        for len in 1..15 {
            offsets[len + 1] = offsets[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbol[offsets[len as usize] as usize] = sym as u16;
                offsets[len as usize] += 1;
            }
        }
        Ok(Huffman { count, symbol })
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return Ok(self.symbol[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("invalid Huffman code".into())
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Decode a raw deflate stream that must produce exactly `size` bytes.
pub fn inflate(input: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let mut bits = Bits {
        input,
        pos: 0,
        buffer: 0,
        count: 0,
    };
    let mut out = Vec::with_capacity(size);
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                // Stored: realign to a byte, then LEN and its complement.
                bits.buffer = 0;
                bits.count = 0;
                let header = input
                    .get(bits.pos..bits.pos + 4)
                    .ok_or("deflate stream ends early")?;
                let len = u16_at(header, 0);
                if len != !u16_at(header, 2) {
                    return Err("stored block length check failed".into());
                }
                bits.pos += 4;
                let block = input
                    .get(bits.pos..bits.pos + len as usize)
                    .ok_or("deflate stream ends early")?;
                if out.len() + block.len() > size {
                    return Err("inflates past its recorded size".into());
                }
                out.extend_from_slice(block);
                bits.pos += len as usize;
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let literal = Huffman::new(&lengths)?;
                let distance = Huffman::new(&[5; 30])?;
                codes(&mut bits, &literal, &distance, &mut out, size)?;
            }
            2 => {
                let literals = bits.take(5)? as usize + 257;
                let distances = bits.take(5)? as usize + 1;
                let code_lengths = bits.take(4)? as usize + 4;
                if literals > 286 || distances > 30 {
                    return Err("bad dynamic block counts".into());
                }
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut lengths = [0u8; 19];
                for &i in &ORDER[..code_lengths] {
                    lengths[i] = bits.take(3)? as u8;
                }
                let lencode = Huffman::new(&lengths)?;
                let mut lengths = vec![0u8; literals + distances];
                let mut i = 0;
                while i < lengths.len() {
                    let symbol = lencode.decode(&mut bits)?;
                    let (value, repeat) = match symbol {
                        0..=15 => (symbol as u8, 1),
                        16 => (
                            *lengths[..i]
                                .last()
                                .ok_or("repeat with no previous length")?,
                            3 + bits.take(2)? as usize,
                        ),
                        17 => (0, 3 + bits.take(3)? as usize),
                        _ => (0, 11 + bits.take(7)? as usize),
                    };
                    if i + repeat > lengths.len() {
                        return Err("too many code lengths".into());
                    }
                    lengths[i..i + repeat].fill(value);
                    i += repeat;
                }
                if lengths[256] == 0 {
                    return Err("no end-of-block code".into());
                }
                let literal = Huffman::new(&lengths[..literals])?;
                let distance = Huffman::new(&lengths[literals..])?;
                codes(&mut bits, &literal, &distance, &mut out, size)?;
            }
            _ => return Err("invalid deflate block type".into()),
        }
        if last == 1 {
            break;
        }
    }
    if out.len() != size {
        return Err("inflates to a different size than recorded".into());
    }
    Ok(out)
}

fn codes(
    bits: &mut Bits,
    literal: &Huffman,
    distance: &Huffman,
    out: &mut Vec<u8>,
    size: usize,
) -> Result<(), String> {
    loop {
        let symbol = literal.decode(bits)? as usize;
        if symbol < 256 {
            if out.len() == size {
                return Err("inflates past its recorded size".into());
            }
            out.push(symbol as u8);
            continue;
        }
        if symbol == 256 {
            return Ok(());
        }
        let symbol = symbol - 257;
        if symbol >= 29 {
            return Err("invalid length code".into());
        }
        let len = LENGTH_BASE[symbol] as usize + bits.take(LENGTH_EXTRA[symbol] as u32)? as usize;
        let symbol = distance.decode(bits)? as usize;
        if symbol >= 30 {
            return Err("invalid distance code".into());
        }
        let back =
            DISTANCE_BASE[symbol] as usize + bits.take(DISTANCE_EXTRA[symbol] as u32)? as usize;
        if back > out.len() {
            return Err("distance reaches before the start of the output".into());
        }
        if out.len() + len > size {
            return Err("inflates past its recorded size".into());
        }
        for _ in 0..len {
            out.push(out[out.len() - back]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn inflates_stored_fixed_and_dynamic_blocks() {
        let text: Vec<u8> = (0..300)
            .flat_map(|i| format!("line {i} of the egcl container test\n").into_bytes())
            .collect();
        for (deflated, plain) in [
            (hex(STORED_HELLO), b"hello, egcl".to_vec()),
            (hex(FIXED_HELLO), b"hello hello hello egcl".to_vec()),
            (hex(DYNAMIC_LINES), text),
        ] {
            assert_eq!(inflate(&deflated, plain.len()).unwrap(), plain);
            // The recorded size bounds the output both ways.
            assert!(inflate(&deflated, plain.len() - 1).is_err());
            assert!(inflate(&deflated, plain.len() + 1).is_err());
            assert!(inflate(&deflated[..deflated.len() - 1], plain.len()).is_err());
        }
    }

    #[test]
    fn corrupt_deflate_streams_are_errors_not_panics() {
        // Members come from a file anyone can edit: mutate valid streams and
        // try random ones, deterministically.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let seeds = [hex(STORED_HELLO), hex(FIXED_HELLO), hex(DYNAMIC_LINES)];
        for round in 0..3000 {
            let mut stream = if round % 3 == 0 {
                (0..next() % 64).map(|_| next() as u8).collect()
            } else {
                seeds[round % seeds.len()].clone()
            };
            for _ in 0..1 + next() % 4 {
                if !stream.is_empty() {
                    let at = next() as usize % stream.len();
                    stream[at] ^= 1 << (next() % 8);
                }
            }
            let _ = inflate(&stream, (next() % 12000) as usize);
        }
    }

    #[test]
    fn a_written_archive_reads_back_after_a_prefix() {
        let prefix = vec![0x7f; 5000];
        let mut writer = Writer::new(prefix.clone());
        writer
            .add(MANIFEST, b"egcl-container 1\napplication\n", 1, 0o644)
            .unwrap();
        writer.add(CORE, &[1u8; 777], 4096, 0o644).unwrap();
        writer
            .add("lib/libx.so.1", &[2u8; 99], 16384, 0o755)
            .unwrap();
        let bytes = writer.finish().unwrap();
        assert_eq!(&bytes[..prefix.len()], &prefix[..]);

        let archive = read(&bytes[..]).unwrap().unwrap();
        assert_eq!(archive.start, prefix.len() as u64);
        let core = archive.core().unwrap().unwrap();
        assert_eq!(core.data_offset % 4096, 0);
        assert_eq!(read_member(&bytes[..], core).unwrap(), vec![1u8; 777]);
        let libraries: Vec<_> = archive.libraries().collect();
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].data_offset % 16384, 0);
        assert!(archive.manifest(&bytes[..]).unwrap().application);

        // A flipped payload byte fails the CRC.
        let mut bad = bytes.clone();
        bad[libraries[0].data_offset as usize] ^= 1;
        assert!(read_member(&bad[..], libraries[0]).is_err());

        // Not an archive at all, and a truncated one.
        assert!(read(&prefix[..]).unwrap().is_none());
        assert!(read(&bytes[..bytes.len() - 1]).unwrap().is_none());
    }

    #[test]
    fn offsets_relative_to_the_archive_are_corrected() {
        // A zip built on its own and then concatenated after a prefix.
        let mut writer = Writer::new(Vec::new());
        writer.add("lib/liby.so", b"payload", 1, 0o755).unwrap();
        let mut bytes = vec![0u8; 1234];
        bytes.extend(writer.finish().unwrap());
        let archive = read(&bytes[..]).unwrap().unwrap();
        assert_eq!(archive.start, 1234);
        let member = archive.member("lib/liby.so").unwrap();
        assert_eq!(read_member(&bytes[..], member).unwrap(), b"payload");
    }

    #[test]
    fn manifests_round_trip_and_ignore_unknown_records() {
        let manifest = Manifest {
            application: true,
            libraries: vec![(
                "lib/libfoo.so.1".into(),
                vec!["/usr/lib/libfoo.so.1".into(), "foo".into()],
            )],
        };
        let text = manifest.render().unwrap();
        assert_eq!(Manifest::parse(&text).unwrap(), manifest);
        assert_eq!(
            Manifest::parse(&format!("{text}future\trecord\n")).unwrap(),
            manifest
        );
        assert!(Manifest::parse("something else\n").is_err());
        let bad = Manifest {
            application: false,
            libraries: vec![("lib/x".into(), vec!["a\tb".into()])],
        };
        assert!(bad.render().is_err());
    }

    // Raw deflate streams from zlib (wbits -15): level 0, a short string at
    // level 9 (fixed codes), and the 300-line text above at level 9 (dynamic).
    const STORED_HELLO: &str = "010b00f4ff68656c6c6f2c206567636c";
    const FIXED_HELLO: &str = "cb48cdc9c957c8402253d3937300";
    const DYNAMIC_LINES: &str = concat!(
        "8dda4d8adc301445e17956514bf0bd922d7939a1a9fc40d30d49ed9f241bc839d3e28d0ec6e54f4fef3f3f9e",
        "8fe3f1f9edf1faf17c3cbfbfbd3fde3e3f5e5ffffefcebf17afe7e7d79ff37119c284e0c9c983871e2c48513",
        "0b27364edc5c4c44e5aae1ace1aee1b0e1b2e1b4e1b6e1b8e1bae5ba15cf2cd72dd72dd72dd72dd72dd72dd7",
        "2dd71d5c7770dd215e095c7770ddc17507d71d5c7770ddc17527d79d5c7772dd29deb85c7772ddc97527d79d",
        "5c7772dd93eb9e5cf7e4ba27d73dc51f1ad73db9eec9754fae7b72dd8beb5e5cf7e2ba17d7bdb8ee25be17b8",
        "eec5752fae7b71ddc57517d75d5c7771ddc57517d75de2738ceb2eaebbb8eee6ba9beb6eaebbb9eee6ba9beb",
        "6eaebbc5d72ed7dd5cf7e6ba37d7bdb9eecd756fae7b73dd9bebde5cf71698309a109c3884270e018a4388e2",
        "10a43884290e818a43a8e210ac384467c536d1d9c0cdc8cdd0cdd8cde0cde8cdf04df82d0270a9f1b1e82c0c",
        "1781b808c545302ec27111908b905c04e5222c97610e224467c1b908cf45802e427411a48b305d04ea225417",
        "c1ba4c73e2233a0bd945d02ec27611b88bd05d04ef227c1701bc08e1e534476ba2b3405e84f2229817e1bc08",
        "e845482f827a11d68bc05e2e7386293a0bef45802f427c11e48b305f04fa22d417c1be08f76599c362d159d0",
        "2fc27e11f88bd05f04ff22fc1701c00801461030db9cca8bce4281110c8c7060040423241841c1080b466030",
        "4283b9cdfac3ec3fc4024478b0c283151eacf06085072b3c58e1c10a0f5678b0318b26d15978b0c283151eac",
        "f06085072b3c58e1c19a7d9e59e8a98d9ee86c767a66a967b67a66ad67f67a66b1273c58e1c10a0f7698d5a9",
        "e82c3c58e1c10a0f5678b0c283151eacf06085072b3cd86976d4a2b3f06085072b3c58e1c10a0f5678b0c283",
        "151eacf0604f73194074161eacf06085072b3c58e1c10a0f5678b0c283151eec656e5d88cec283151eacf060",
        "85072b3c58e1c10a0f5678b0c2835de67a8be82c3c58e1c10a0f5678b0c283151eacf06085072b3cd86dee11",
        "89cec283151eacf06085072b3c58e1c10a0f5678b0c283bdcd85adff76fe03",
    );
}
