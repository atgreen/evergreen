// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Block primitives for Ironclad's EGCL port.
//!
//! Ironclad replaces a few inner functions with implementation-specific code
//! on SBCL (AES-NI, PCLMULQDQ GHASH, SHA expansion VOPs), CCL and ECL. These
//! are EGCL's counterparts for the functions TLS spends its time in: one AES
//! block, one GHASH multiplication and one SHA-256 compression. Ironclad's
//! portable versions of them measured 193 s to AES-GCM-encrypt 64 KB and 18 s
//! to hash it under EGCL. Everything else in Ironclad stays in Lisp.
//!
//! Every kernel works on Ironclad's own data layout -- octet vectors, and
//! `(unsigned-byte 32)` round-key and register vectors. Octets may use packed
//! byte storage; word vectors use general fixnum slots. Nothing here allocates
//! on the Lisp heap, so a kernel can read and
//! write those vectors in place without rooting them, and every value it
//! stores is a fixnum, which needs no write barrier.
//!
//! The SHA-512 and Keccak kernels are the exception. Their `(unsigned-byte
//! 64)` lanes exceed EGCL's 61-bit fixnums, so a lane is a fixnum or a bignum,
//! and storing one can allocate. They compute on plain `u64` arrays and write
//! the lanes back last, through `set_elt`, with the vector rooted.

use crate::sequences::simple_vector_slots;
use egcl_rt::error::EgclError;
use egcl_rt::value::{EgclVal, NIL};
use std::sync::OnceLock;

/// The Lisp-visible primitives, by bare name, with their argument counts.
pub const PRIMITIVES: &[(&str, usize)] = &[
    ("CRYPTO-AES-ENCRYPT-BLOCK", 6),
    ("CRYPTO-AES-DECRYPT-BLOCK", 6),
    ("CRYPTO-GHASH-MUL", 2),
    ("CRYPTO-SHA256-BLOCK", 3),
    ("CRYPTO-SHA256-WORDS", 2),
    ("CRYPTO-SHA512-BLOCK", 3),
    ("CRYPTO-SHA512-WORDS", 2),
    ("CRYPTO-KECCAK-F1600", 1),
    ("CRYPTO-KECCAK-ABSORB", 4),
];

/// The argument count of primitive `bare`, if it is one.
pub fn primitive_arity(bare: &str) -> Option<usize> {
    PRIMITIVES
        .iter()
        .find(|(name, _)| *name == bare)
        .map(|&(_, arity)| arity)
}

/// Run primitive `bare` on evaluated ARGS; `None` if `bare` is not one.
pub fn call(bare: &str, args: &[EgclVal]) -> Option<Result<EgclVal, EgclError>> {
    let arity = primitive_arity(bare)?;
    if args.len() != arity {
        return Some(Err(EgclError::ProgramError(format!(
            "EGCL-EXT:{bare} takes {arity} arguments, got {}",
            args.len()
        ))));
    }
    Some(match bare {
        "CRYPTO-AES-ENCRYPT-BLOCK" => aes_block(args, false),
        "CRYPTO-AES-DECRYPT-BLOCK" => aes_block(args, true),
        "CRYPTO-GHASH-MUL" => ghash_mul_values(args[0], args[1]),
        "CRYPTO-SHA256-BLOCK" => sha256_block_values(args[0], args[1], args[2]),
        "CRYPTO-SHA256-WORDS" => sha256_words_values(args[0], args[1]),
        "CRYPTO-SHA512-BLOCK" => sha512_block_values(args[0], args[1], args[2]),
        "CRYPTO-SHA512-WORDS" => sha512_words_values(args[0], args[1]),
        "CRYPTO-KECCAK-F1600" => keccak_f1600_values(args[0]),
        "CRYPTO-KECCAK-ABSORB" => keccak_absorb_values(args[0], args[1], args[2], args[3]),
        _ => unreachable!("PRIMITIVES lists {bare}"),
    })
}

// ── Reading and writing Lisp vectors ────────────────────────────────

fn type_error(datum: EgclVal, expected: &str) -> EgclError {
    EgclError::TypeError {
        datum,
        expected: expected.into(),
    }
}

fn index_arg(value: EgclVal) -> Result<usize, EgclError> {
    if value.is_fixnum() && value.as_fixnum() >= 0 {
        Ok(value.as_fixnum() as usize)
    } else {
        Err(type_error(value, "a non-negative fixnum index"))
    }
}

/// The slots `start..start + count` of simple vector V.
fn slots(v: EgclVal, start: usize, count: usize, what: &str) -> Result<*mut EgclVal, EgclError> {
    let Some((base, len)) = simple_vector_slots(v) else {
        return Err(type_error(v, what));
    };
    if start > len || count > len - start {
        return Err(type_error(
            v,
            &format!(
                "{what} with at least {} elements",
                start.saturating_add(count)
            ),
        ));
    }
    Ok(unsafe { base.add(start) })
}

/// Read N elements of V from START, each an integer below 2^BITS.
fn read_words<const N: usize>(
    v: EgclVal,
    start: usize,
    bits: u32,
    what: &str,
) -> Result<[u64; N], EgclError> {
    let base = slots(v, start, N, what)?;
    let mut out = [0u64; N];
    for (index, slot) in out.iter_mut().enumerate() {
        let element = unsafe { *base.add(index) };
        if !element.is_fixnum() || (element.as_fixnum() as u64) >> bits != 0 {
            return Err(type_error(element, what));
        }
        *slot = element.as_fixnum() as u64;
    }
    Ok(out)
}

/// Store WORDS into V from START. Fixnums only, so no write barrier is owed.
fn write_words(v: EgclVal, start: usize, words: &[u64], what: &str) -> Result<(), EgclError> {
    let base = slots(v, start, words.len(), what)?;
    for (index, &word) in words.iter().enumerate() {
        unsafe { *base.add(index) = EgclVal::from_fixnum(word as i64) };
    }
    Ok(())
}

fn read_bytes<const N: usize>(v: EgclVal, start: usize) -> Result<[u8; N], EgclError> {
    let mut out = [0u8; N];
    read_bytes_into(v, start, &mut out)?;
    Ok(out)
}

/// Fill OUT with the octets of V from START, packed or general storage alike.
fn read_bytes_into(v: EgclVal, start: usize, out: &mut [u8]) -> Result<(), EgclError> {
    if let Some(length) = egcl_rt::packed_u8::len(v) {
        if start > length || out.len() > length - start {
            return Err(type_error(
                v,
                "an octet vector containing the requested block",
            ));
        }
        for (index, slot) in out.iter_mut().enumerate() {
            *slot = egcl_rt::packed_u8::get(v, start + index).unwrap();
        }
        return Ok(());
    }
    let base = slots(v, start, out.len(), "an octet vector")?;
    for (index, slot) in out.iter_mut().enumerate() {
        let element = unsafe { *base.add(index) };
        if !element.is_fixnum() || (element.as_fixnum() as u64) >> 8 != 0 {
            return Err(type_error(element, "an octet vector"));
        }
        *slot = element.as_fixnum() as u8;
    }
    Ok(())
}

fn write_bytes(v: EgclVal, start: usize, bytes: &[u8]) -> Result<(), EgclError> {
    if let Some(length) = egcl_rt::packed_u8::len(v) {
        if start > length || bytes.len() > length - start {
            return Err(type_error(
                v,
                "an octet vector containing the requested block",
            ));
        }
        for (index, &byte) in bytes.iter().enumerate() {
            // All indices were checked before the first write. Raw bytes need
            // no reference barrier and these accessors cannot collect.
            egcl_rt::packed_u8::set(v, start + index, byte);
        }
        return Ok(());
    }
    let words: Vec<u64> = bytes.iter().map(|&b| u64::from(b)).collect();
    write_words(v, start, &words, "an octet vector")
}

fn read_u32s<const N: usize>(v: EgclVal, start: usize) -> Result<[u32; N], EgclError> {
    Ok(read_words::<N>(v, start, 32, "an (unsigned-byte 32) vector")?.map(|w| w as u32))
}

fn write_u32s(v: EgclVal, start: usize, words: &[u32]) -> Result<(), EgclError> {
    let words: Vec<u64> = words.iter().map(|&w| u64::from(w)).collect();
    write_words(v, start, &words, "an (unsigned-byte 32) vector")
}

/// Read N `(unsigned-byte 64)` lanes of V from START. A lane at or above
/// 2^60 is a bignum.
fn read_u64s<const N: usize>(v: EgclVal, start: usize, what: &str) -> Result<[u64; N], EgclError> {
    let base = slots(v, start, N, what)?;
    let mut out = [0u64; N];
    for (index, slot) in out.iter_mut().enumerate() {
        let element = unsafe { *base.add(index) };
        *slot = if element.is_fixnum() && element.as_fixnum() >= 0 {
            element.as_fixnum() as u64
        } else {
            match egcl_rt::bignum::bigint_from_val(element) {
                Some(n) if n.sign > 0 && n.mag.len() == 1 => n.mag[0],
                _ => return Err(type_error(element, what)),
            }
        };
    }
    Ok(out)
}

/// Store LANES into V from START. A lane at or above 2^60 needs a fresh
/// bignum, and allocating one can move V, so every store goes through the
/// rooted value instead of a slot pointer.
fn write_u64s(v: EgclVal, start: usize, lanes: &[u64], what: &str) -> Result<(), EgclError> {
    slots(v, start, lanes.len(), what)?;
    egcl_rt::rooted!(v = v);
    for (index, &lane) in lanes.iter().enumerate() {
        let value = egcl_rt::bignum::BigInt::from_mag(1, vec![lane]).to_val();
        crate::sequences::set_elt(*v, start + index, value)?;
    }
    Ok(())
}

// ── AES ─────────────────────────────────────────────────────────────

/// The AES S-box, its inverse, and the round tables of the
/// `rijndael-alg-fst` implementation Ironclad's AES follows: TE0[x] is
/// S[x]·(02 01 01 03) and TD0[x] is Si[x]·(0e 09 0d 0b) as big-endian words,
/// with TE1..TE3 and TD1..TD3 their byte rotations.
struct AesTables {
    sbox: [u8; 256],
    inv_sbox: [u8; 256],
    te: [[u32; 256]; 4],
    td: [[u32; 256]; 4],
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        a = (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 };
        b >>= 1;
    }
    product
}

fn aes_tables() -> &'static AesTables {
    static TABLES: OnceLock<AesTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        // Walk the multiplicative group with generator 3 and its inverse,
        // applying the affine transformation (FIPS-197 5.1.1).
        let mut sbox = [0u8; 256];
        let (mut p, mut q) = (1u8, 1u8);
        loop {
            p ^= (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
            q ^= q << 1;
            q ^= q << 2;
            q ^= q << 4;
            if q & 0x80 != 0 {
                q ^= 0x09;
            }
            sbox[p as usize] = q
                ^ q.rotate_left(1)
                ^ q.rotate_left(2)
                ^ q.rotate_left(3)
                ^ q.rotate_left(4)
                ^ 0x63;
            if p == 1 {
                break;
            }
        }
        sbox[0] = 0x63;
        let mut inv_sbox = [0u8; 256];
        for (x, &s) in sbox.iter().enumerate() {
            inv_sbox[s as usize] = x as u8;
        }
        let mut te = [[0u32; 256]; 4];
        let mut td = [[0u32; 256]; 4];
        for x in 0..256 {
            let s = sbox[x];
            let e = u32::from_be_bytes([gf_mul(s, 2), s, s, gf_mul(s, 3)]);
            let i = inv_sbox[x];
            let d = u32::from_be_bytes([gf_mul(i, 14), gf_mul(i, 9), gf_mul(i, 13), gf_mul(i, 11)]);
            for r in 0..4 {
                te[r][x] = e.rotate_right(8 * r as u32);
                td[r][x] = d.rotate_right(8 * r as u32);
            }
        }
        AesTables {
            sbox,
            inv_sbox,
            te,
            td,
        }
    })
}

fn byte(word: u32, n: u32) -> usize {
    ((word >> (8 * n)) & 0xff) as usize
}

/// Encrypt one block with Ironclad's encryption round keys.
pub fn aes_encrypt(round_keys: &[u32; 60], rounds: usize, input: [u8; 16]) -> [u8; 16] {
    let t = aes_tables();
    let word = |i: usize| u32::from_be_bytes(input[4 * i..4 * i + 4].try_into().unwrap());
    let mut s = [0u32; 4];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = word(i) ^ round_keys[i];
    }
    let mut k = 4;
    for _ in 1..rounds {
        let mut n = [0u32; 4];
        for (i, slot) in n.iter_mut().enumerate() {
            *slot = t.te[0][byte(s[i], 3)]
                ^ t.te[1][byte(s[(i + 1) % 4], 2)]
                ^ t.te[2][byte(s[(i + 2) % 4], 1)]
                ^ t.te[3][byte(s[(i + 3) % 4], 0)]
                ^ round_keys[k + i];
        }
        s = n;
        k += 4;
    }
    let mut out = [0u8; 16];
    for i in 0..4 {
        let w = u32::from_be_bytes([
            t.sbox[byte(s[i], 3)],
            t.sbox[byte(s[(i + 1) % 4], 2)],
            t.sbox[byte(s[(i + 2) % 4], 1)],
            t.sbox[byte(s[(i + 3) % 4], 0)],
        ]) ^ round_keys[k + i];
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

/// Decrypt one block with Ironclad's decryption round keys (the encryption
/// keys reversed, with InvMixColumns applied to all but the outer two).
pub fn aes_decrypt(round_keys: &[u32; 60], rounds: usize, input: [u8; 16]) -> [u8; 16] {
    let t = aes_tables();
    let word = |i: usize| u32::from_be_bytes(input[4 * i..4 * i + 4].try_into().unwrap());
    let mut s = [0u32; 4];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = word(i) ^ round_keys[i];
    }
    let mut k = 4;
    for _ in 1..rounds {
        let mut n = [0u32; 4];
        for (i, slot) in n.iter_mut().enumerate() {
            *slot = t.td[0][byte(s[i], 3)]
                ^ t.td[1][byte(s[(i + 3) % 4], 2)]
                ^ t.td[2][byte(s[(i + 2) % 4], 1)]
                ^ t.td[3][byte(s[(i + 1) % 4], 0)]
                ^ round_keys[k + i];
        }
        s = n;
        k += 4;
    }
    let mut out = [0u8; 16];
    for i in 0..4 {
        let w = u32::from_be_bytes([
            t.inv_sbox[byte(s[i], 3)],
            t.inv_sbox[byte(s[(i + 3) % 4], 2)],
            t.inv_sbox[byte(s[(i + 2) % 4], 1)],
            t.inv_sbox[byte(s[(i + 1) % 4], 0)],
        ]) ^ round_keys[k + i];
        out[4 * i..4 * i + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

/// `(crypto-aes-{en,de}crypt-block round-keys n-rounds in in-start out out-start)`
fn aes_block(args: &[EgclVal], decrypt: bool) -> Result<EgclVal, EgclError> {
    let round_keys = read_u32s::<60>(args[0], 0)?;
    let rounds = index_arg(args[1])?;
    if !matches!(rounds, 10 | 12 | 14) {
        return Err(type_error(args[1], "an AES round count (10, 12 or 14)"));
    }
    let input = read_bytes::<16>(args[2], index_arg(args[3])?)?;
    let output = if decrypt {
        aes_decrypt(&round_keys, rounds, input)
    } else {
        aes_encrypt(&round_keys, rounds, input)
    };
    write_bytes(args[4], index_arg(args[5])?, &output)?;
    Ok(NIL)
}

// ── GHASH ───────────────────────────────────────────────────────────

/// X·H in GCM's GF(2^128) (SP 800-38D, algorithm 1), both operands as
/// big-endian 128-bit integers. Branch-free in the operand bits.
pub fn ghash_multiply(x: u128, h: u128) -> u128 {
    const R: u128 = 0xe1 << 120;
    let mut z = 0u128;
    let mut v = h;
    for i in (0..128).rev() {
        z ^= v & 0u128.wrapping_sub((x >> i) & 1);
        v = (v >> 1) ^ (R & 0u128.wrapping_sub(v & 1));
    }
    z
}

/// `(crypto-ghash-mul accumulator hash-key)`: replace the 16-octet
/// ACCUMULATOR with its product with the 16-octet HASH-KEY.
fn ghash_mul_values(accumulator: EgclVal, key: EgclVal) -> Result<EgclVal, EgclError> {
    let x = u128::from_be_bytes(read_bytes::<16>(accumulator, 0)?);
    let h = u128::from_be_bytes(read_bytes::<16>(key, 0)?);
    write_bytes(accumulator, 0, &ghash_multiply(x, h).to_be_bytes())?;
    Ok(NIL)
}

// ── SHA-256 ─────────────────────────────────────────────────────────

/// `(crypto-sha256-block regs data start)`: compress the 64 octets of DATA
/// from START into the eight (unsigned-byte 32) REGS.
fn sha256_block_values(regs: EgclVal, data: EgclVal, start: EgclVal) -> Result<EgclVal, EgclError> {
    let mut state = read_u32s::<8>(regs, 0)?;
    let block = read_bytes::<64>(data, index_arg(start)?)?;
    egcl_rt::digest::sha256_compress(&mut state, &block);
    write_u32s(regs, 0, &state)?;
    Ok(NIL)
}

/// `(crypto-sha256-words regs block)`: compress the message block held as the
/// first sixteen (unsigned-byte 32) words of BLOCK, as Ironclad's finalizer
/// leaves it.
fn sha256_words_values(regs: EgclVal, block: EgclVal) -> Result<EgclVal, EgclError> {
    let mut state = read_u32s::<8>(regs, 0)?;
    let words = read_u32s::<16>(block, 0)?;
    let mut bytes = [0u8; 64];
    for (chunk, word) in bytes.chunks_exact_mut(4).zip(words) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    egcl_rt::digest::sha256_compress(&mut state, &bytes);
    write_u32s(regs, 0, &state)?;
    Ok(NIL)
}

// ── SHA-512 ─────────────────────────────────────────────────────────

const SHA512_K: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

/// FIPS 180-4 SHA-512 compression of the sixteen message words W into STATE.
/// SHA-384 is the same function from different initial registers.
fn sha512_compress(state: &mut [u64; 8], words: &[u64; 16]) {
    let mut w = [0u64; 80];
    w[..16].copy_from_slice(words);
    for t in 16..80 {
        let s0 = w[t - 15].rotate_right(1) ^ w[t - 15].rotate_right(8) ^ (w[t - 15] >> 7);
        let s1 = w[t - 2].rotate_right(19) ^ w[t - 2].rotate_right(61) ^ (w[t - 2] >> 6);
        w[t] = w[t - 16]
            .wrapping_add(s0)
            .wrapping_add(w[t - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for t in 0..80 {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA512_K[t])
            .wrapping_add(w[t]);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (register, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *register = register.wrapping_add(value);
    }
}

/// `(crypto-sha512-block regs data start)`: compress the 128 octets of DATA
/// from START into the eight (unsigned-byte 64) registers REGS.
fn sha512_block_values(regs: EgclVal, data: EgclVal, start: EgclVal) -> Result<EgclVal, EgclError> {
    let mut state = read_u64s::<8>(regs, 0, "an (unsigned-byte 64) vector")?;
    let bytes = read_bytes::<128>(data, index_arg(start)?)?;
    let mut words = [0u64; 16];
    for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
        *word = u64::from_be_bytes(chunk.try_into().expect("eight octets"));
    }
    sha512_compress(&mut state, &words);
    write_u64s(regs, 0, &state, "an (unsigned-byte 64) vector")?;
    Ok(NIL)
}

/// `(crypto-sha512-words regs block)`: compress the message block held as the
/// first sixteen (unsigned-byte 64) words of BLOCK, as Ironclad's finalizer
/// leaves it.
fn sha512_words_values(regs: EgclVal, block: EgclVal) -> Result<EgclVal, EgclError> {
    let mut state = read_u64s::<8>(regs, 0, "an (unsigned-byte 64) vector")?;
    let words = read_u64s::<16>(block, 0, "an (unsigned-byte 64) vector")?;
    sha512_compress(&mut state, &words);
    write_u64s(regs, 0, &state, "an (unsigned-byte 64) vector")?;
    Ok(NIL)
}

// ── Keccak ──────────────────────────────────────────────────────────

const KECCAK_ROUND_CONSTANTS: [u64; 24] = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808a,
    0x8000000080008000,
    0x000000000000808b,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008a,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000a,
    0x000000008000808b,
    0x800000000000008b,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800a,
    0x800000008000000a,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
];

/// Rotation offset of lane x + 5y, FIPS 202 table 2.
const KECCAK_RHO: [u32; 25] = [
    0, 1, 62, 28, 27, 36, 44, 6, 55, 20, 3, 10, 43, 25, 39, 41, 45, 15, 21, 8, 18, 2, 61, 56, 14,
];

/// The Keccak-f[1600] permutation (FIPS 202) on lanes indexed x + 5y, the
/// layout of Ironclad's keccak-state.
fn keccak_f1600(a: &mut [u64; 25]) {
    for round_constant in KECCAK_ROUND_CONSTANTS {
        // θ
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in 0..5 {
                a[x + 5 * y] ^= d;
            }
        }
        // ρ and π: lane (x, y) moves to (y, 2x + 3y).
        let mut b = [0u64; 25];
        for x in 0..5 {
            for y in 0..5 {
                b[y + 5 * ((2 * x + 3 * y) % 5)] = a[x + 5 * y].rotate_left(KECCAK_RHO[x + 5 * y]);
            }
        }
        // χ
        for y in 0..5 {
            for x in 0..5 {
                a[x + 5 * y] = b[x + 5 * y] ^ (!b[(x + 1) % 5 + 5 * y] & b[(x + 2) % 5 + 5 * y]);
            }
        }
        // ι
        a[0] ^= round_constant;
    }
}

/// `(crypto-keccak-f1600 state)`: permute the 25 (unsigned-byte 64) lanes of
/// STATE in place.
fn keccak_f1600_values(state: EgclVal) -> Result<EgclVal, EgclError> {
    let mut lanes = read_u64s::<25>(state, 0, "a 25-lane (unsigned-byte 64) vector")?;
    keccak_f1600(&mut lanes);
    write_u64s(state, 0, &lanes, "a 25-lane (unsigned-byte 64) vector")?;
    Ok(NIL)
}

/// `(crypto-keccak-absorb state rate-bytes data start)`: XOR the RATE-BYTES
/// octets of DATA from START into STATE's lanes, little-endian, then permute
/// -- one sponge absorption, as Ironclad's keccak-state-merge-input followed
/// by keccak-rounds. RATE-BYTES is a multiple of 8 no larger than 200.
fn keccak_absorb_values(
    state: EgclVal,
    rate_bytes: EgclVal,
    data: EgclVal,
    start: EgclVal,
) -> Result<EgclVal, EgclError> {
    let rate = index_arg(rate_bytes)?;
    if rate % 8 != 0 || rate > 200 {
        return Err(type_error(
            rate_bytes,
            "a Keccak rate in octets: a multiple of 8, at most 200",
        ));
    }
    let start = index_arg(start)?;
    let mut lanes = read_u64s::<25>(state, 0, "a 25-lane (unsigned-byte 64) vector")?;
    let mut block = [0u8; 200];
    read_bytes_into(data, start, &mut block[..rate])?;
    for (lane, chunk) in lanes.iter_mut().zip(block[..rate].chunks_exact(8)) {
        *lane ^= u64::from_le_bytes(chunk.try_into().expect("eight octets"));
    }
    keccak_f1600(&mut lanes);
    write_u64s(state, 0, &lanes, "a 25-lane (unsigned-byte 64) vector")?;
    Ok(NIL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_u8_crypto_blocks_keep_bounds_and_word_vector_compatibility() {
        egcl_rt::rooted!(packed = egcl_rt::packed_u8::make(32, 17).unwrap());
        egcl_rt::rooted!(
            general = crate::sequences::build_filled_simple_vector(32, EgclVal::from_fixnum(17))
        );
        let block: [u8; 16] = std::array::from_fn(|index| (index * 17) as u8);
        for array in [*packed, *general] {
            write_bytes(array, 8, &block).unwrap();
            assert_eq!(read_bytes::<16>(array, 8).unwrap(), block);
            assert!(read_bytes::<16>(array, 17).is_err());
            assert!(write_bytes(array, 17, &block).is_err());
            assert_eq!(read_bytes::<16>(array, 8).unwrap(), block);
            assert!(read_bytes::<1>(array, usize::MAX).is_err());
            assert_eq!(read_bytes::<0>(array, 32).unwrap(), []);
        }
    }

    /// Keccak absorption reads its rate block from packed octet vectors as
    /// well as general ones: Ironclad's SHA3 buffer is an (unsigned-byte 8)
    /// array, so every SHA3 digest failed once those became packed.
    #[test]
    fn keccak_absorb_reads_packed_and_general_octet_vectors() {
        // SHA3-256 of the empty message: 0x06, then 0x80 at the end of the
        // 136-octet rate block, at offset 4 of a 140-octet buffer.
        egcl_rt::rooted!(packed = egcl_rt::packed_u8::make(140, 0).unwrap());
        egcl_rt::rooted!(
            general = crate::sequences::build_filled_simple_vector(140, EgclVal::from_fixnum(0))
        );
        for data in [*packed, *general] {
            write_bytes(data, 4, &[0x06]).unwrap();
            write_bytes(data, 139, &[0x80]).unwrap();
            egcl_rt::rooted!(
                state = crate::sequences::build_filled_simple_vector(25, EgclVal::from_fixnum(0))
            );
            keccak_absorb_values(
                *state,
                EgclVal::from_fixnum(136),
                data,
                EgclVal::from_fixnum(4),
            )
            .unwrap();
            let lanes = read_u64s::<25>(*state, 0, "lanes").unwrap();
            assert_eq!(
                lanes[0].to_le_bytes(),
                [0xa7, 0xff, 0xc6, 0xf8, 0xbf, 0x1e, 0xd7, 0x66]
            );
            assert!(
                keccak_absorb_values(
                    *state,
                    EgclVal::from_fixnum(136),
                    data,
                    EgclVal::from_fixnum(5)
                )
                .is_err()
            );
        }
    }

    /// SHA-512 of the single padded block for "abc" (FIPS 180-4 example).
    #[test]
    fn sha512_compress_matches_fips_180_4_abc() {
        let mut state: [u64; 8] = [
            0x6a09e667f3bcc908,
            0xbb67ae8584caa73b,
            0x3c6ef372fe94f82b,
            0xa54ff53a5f1d36f1,
            0x510e527fade682d1,
            0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b,
            0x5be0cd19137e2179,
        ];
        let mut words = [0u64; 16];
        words[0] = 0x6162638000000000;
        words[15] = 24;
        sha512_compress(&mut state, &words);
        assert_eq!(
            state,
            [
                0xddaf35a193617aba,
                0xcc417349ae204131,
                0x12e6fa4e89a97ea2,
                0x0a9eeee64b55d39a,
                0x2192992a274fc1a8,
                0x36ba3c23a3feebbd,
                0x454d4423643ce80e,
                0x2a9ac94fa54ca49f,
            ]
        );
    }

    /// Keccak-f[1600] on the all-zero state: the first lanes of the published
    /// KeccakF-1600 intermediate values, and the lane SHA3-256("") starts with.
    #[test]
    fn keccak_f1600_matches_published_zero_state_permutation() {
        let mut lanes = [0u64; 25];
        keccak_f1600(&mut lanes);
        assert_eq!(lanes[0], 0xf1258f7940e1dde7);
        assert_eq!(lanes[1], 0x84d5ccf933c0478a);
        assert_eq!(lanes[24], 0xeaf1ff7b5ceca249);
        // SHA3-256 of the empty message: 0x06 then 0x80 at the end of the
        // 136-octet rate block, absorbed into a zero state.
        let mut state = [0u64; 25];
        state[0] ^= 0x06;
        state[16] ^= 0x80 << 56;
        keccak_f1600(&mut state);
        assert_eq!(
            state[0].to_le_bytes(),
            [0xa7, 0xff, 0xc6, 0xf8, 0xbf, 0x1e, 0xd7, 0x66]
        );
    }

    fn hex<const N: usize>(text: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    /// FIPS-197 key expansion, so the tests need no Lisp.
    fn expand(key: &[u8]) -> ([u32; 60], [u32; 60], usize) {
        let t = aes_tables();
        let nk = key.len() / 4;
        let rounds = nk + 6;
        let total = 4 * (rounds + 1);
        let mut w = [0u32; 60];
        for i in 0..nk {
            w[i] = u32::from_be_bytes(key[4 * i..4 * i + 4].try_into().unwrap());
        }
        let sub = |x: u32| {
            u32::from_be_bytes([
                t.sbox[byte(x, 3)],
                t.sbox[byte(x, 2)],
                t.sbox[byte(x, 1)],
                t.sbox[byte(x, 0)],
            ])
        };
        let mut rcon = 1u8;
        for i in nk..total {
            let mut temp = w[i - 1];
            if i % nk == 0 {
                temp = sub(temp.rotate_left(8)) ^ (u32::from(rcon) << 24);
                rcon = gf_mul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                temp = sub(temp);
            }
            w[i] = w[i - nk] ^ temp;
        }
        // Ironclad's decryption schedule: reverse the round order, then apply
        // InvMixColumns to every round key but the first and last.
        let mut d = w;
        for r in 0..=rounds {
            d[4 * r..4 * r + 4].copy_from_slice(&w[4 * (rounds - r)..4 * (rounds - r) + 4]);
        }
        for word in d.iter_mut().take(4 * rounds).skip(4) {
            let x = *word;
            *word = t.td[0][t.sbox[byte(x, 3)] as usize]
                ^ t.td[1][t.sbox[byte(x, 2)] as usize]
                ^ t.td[2][t.sbox[byte(x, 1)] as usize]
                ^ t.td[3][t.sbox[byte(x, 0)] as usize];
        }
        (w, d, rounds)
    }

    #[test]
    fn aes_matches_fips_197_appendix_c() {
        let plain = hex::<16>("00112233445566778899aabbccddeeff");
        let key: Vec<u8> = (0..32).collect();
        for (len, cipher) in [
            (16, "69c4e0d86a7b0430d8cdb78070b4c55a"),
            (24, "dda97ca4864cdfe06eaf70a0ec0d7191"),
            (32, "8ea2b7ca516745bfeafc49904b496089"),
        ] {
            let (enc, dec, rounds) = expand(&key[..len]);
            let encrypted = aes_encrypt(&enc, rounds, plain);
            assert_eq!(encrypted, hex::<16>(cipher), "AES-{}", len * 8);
            assert_eq!(
                aes_decrypt(&dec, rounds, encrypted),
                plain,
                "AES-{}",
                len * 8
            );
        }
    }

    #[test]
    fn ghash_matches_gcm_test_case_2() {
        // McGrew & Viega, GCM test case 2: H = E(0^128, 0^128) and one block
        // of ciphertext, then the length block; GHASH xor E(K, Y0) is the tag.
        let h = u128::from_be_bytes(hex::<16>("66e94bd4ef8a2c3b884cfa59ca342b2e"));
        let c = u128::from_be_bytes(hex::<16>("0388dace60b6a392f328c2b971b2fe78"));
        let y1 = ghash_multiply(c, h);
        let y2 = ghash_multiply(y1 ^ 128, h);
        assert_eq!(
            y2.to_be_bytes(),
            hex::<16>("f38cbb1ad69223dcc3457ae5b6b0f885")
        );
    }
}
