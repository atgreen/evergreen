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
//! `(unsigned-byte 32)` round-key and register vectors -- which EGCL stores as
//! simple vectors of fixnums. Nothing here allocates, so a kernel can read and
//! write those vectors in place without rooting them, and every value it
//! stores is a fixnum, which needs no write barrier.

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
            &format!("{what} with at least {} elements", start + count),
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
    let words = read_words::<N>(v, start, 8, "an octet vector")?;
    Ok(words.map(|w| w as u8))
}

fn write_bytes(v: EgclVal, start: usize, bytes: &[u8]) -> Result<(), EgclError> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
