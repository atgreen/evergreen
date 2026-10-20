// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The EGCL-EXT block primitives Ironclad's EGCL port calls (AES, GHASH,
//! SHA-256, SHA-512, Keccak), through the full CLI: known answers from the standards, the same
//! answers from a compiled caller, and argument checking.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A Lisp form that reads hex text into a fresh octet vector.
fn octets(hex: &str) -> String {
    let bytes: Vec<String> = (0..hex.len() / 2)
        .map(|i| {
            u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
                .unwrap()
                .to_string()
        })
        .collect();
    format!(
        "(make-array {} :element-type '(unsigned-byte 8) :initial-contents '({}))",
        bytes.len(),
        bytes.join(" ")
    )
}

const HEX: &str = "(lambda (v) (string-downcase (format nil \"~{~2,'0x~}\" (coerce v 'list))))";

/// FIPS-197 appendix A.1: the expanded key of 2b7e151628aed2a6abf7158809cf4f3c,
/// padded to the 60 words of Ironclad's round-key vector.
const ROUND_KEYS: &str = "(let ((v (make-array 60 :element-type '(unsigned-byte 32) :initial-element 0))) \
  (replace v '(#x2b7e1516 #x28aed2a6 #xabf71588 #x09cf4f3c #xa0fafe17 #x88542cb1 #x23a33939 #x2a6c7605 \
               #xf2c295f2 #x7a96b943 #x5935807a #x7359f67f #x3d80477d #x4716fe3e #x1e237e44 #x6d7a883b \
               #xef44a541 #xa8525b7f #xb671253b #xdb0bad00 #xd4d1c6f8 #x7c839d87 #xcaf2b8bc #x11f915bc \
               #x6d88a37a #x110b3efd #xdbf98641 #xca0093fd #x4e54f70e #x5f5fc9f3 #x84a64fb2 #x4ea6dc4f \
               #xead27321 #xb58dbad2 #x312bf560 #x7f8d292f #xac7766f3 #x19fadc21 #x28d12941 #x575c006e \
               #xd014f9a8 #xc9ee2589 #xe13f0cc8 #xb6630ca6)))";

#[test]
fn aes_block_matches_fips_197_appendix_b() {
    let program = format!(
        "(let ((out (make-array 20 :element-type '(unsigned-byte 8) :initial-element 0))) \
           (egcl-ext:crypto-aes-encrypt-block {ROUND_KEYS} 10 {} 0 out 4) \
           (funcall {HEX} (subseq out 4)))",
        octets("3243f6a8885a308d313198a2e0370734")
    );
    assert_eq!(eval(&program), "\"3925841d02dc09fbdc118597196a0b32\"");
}

#[test]
fn ghash_mul_matches_gcm_test_case_2() {
    // GHASH of one ciphertext block and the length block, with H = E(0, 0).
    let program = format!(
        "(let ((acc {}) (h {}) (len {})) \
           (egcl-ext:crypto-ghash-mul acc h) \
           (dotimes (i 16) (setf (aref acc i) (logxor (aref acc i) (aref len i)))) \
           (egcl-ext:crypto-ghash-mul acc h) \
           (funcall {HEX} acc))",
        octets("0388dace60b6a392f328c2b971b2fe78"),
        octets("66e94bd4ef8a2c3b884cfa59ca342b2e"),
        octets("00000000000000000000000000000080"),
    );
    assert_eq!(eval(&program), "\"f38cbb1ad69223dcc3457ae5b6b0f885\"");
}

/// SHA-256 of "abc" as one padded block, through the octet and the word entry
/// points, the second from a function called often enough to tier up.
#[test]
fn sha256_block_and_words_agree_with_the_standard() {
    let block = "(let ((b (make-array 64 :element-type '(unsigned-byte 8) :initial-element 0))) \
                   (setf (aref b 0) 97 (aref b 1) 98 (aref b 2) 99 (aref b 3) #x80 (aref b 63) 24) b)";
    let program = format!(
        "(progn \
           (defun fresh-regs () \
             (make-array 8 :element-type '(unsigned-byte 32) :initial-contents \
               '(#x6a09e667 #xbb67ae85 #x3c6ef372 #xa54ff53a #x510e527f #x9b05688c #x1f83d9ab #x5be0cd19))) \
           (defun words-digest (block) \
             (let ((regs (fresh-regs)) (words (make-array 64 :element-type '(unsigned-byte 32) :initial-element 0))) \
               (dotimes (i 16) \
                 (setf (aref words i) (logior (ash (aref block (* 4 i)) 24) (ash (aref block (+ 1 (* 4 i))) 16) \
                                              (ash (aref block (+ 2 (* 4 i))) 8) (aref block (+ 3 (* 4 i)))))) \
               (egcl-ext:crypto-sha256-words regs words) \
               regs)) \
           (let ((block {block}) (regs (fresh-regs)) (agree t)) \
             (egcl-ext:crypto-sha256-block regs block 0) \
             (dotimes (i 50) (unless (equalp (words-digest block) regs) (setf agree nil))) \
             (list agree (string-downcase (format nil \"~{{~8,'0x~}}\" (coerce regs 'list))))))"
    );
    assert_eq!(
        eval(&program),
        "(T \"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\")"
    );
}

#[test]
fn primitives_are_fbound_and_check_their_arguments() {
    let program = "(list (fboundp 'egcl-ext:crypto-ghash-mul) \
         (handler-case (egcl-ext:crypto-ghash-mul (make-array 16 :initial-element 0)) \
           (program-error () :arity)) \
         (handler-case (egcl-ext:crypto-ghash-mul (make-array 15 :initial-element 0) \
                                                  (make-array 16 :initial-element 0)) \
           (type-error () :short)) \
         (handler-case (egcl-ext:crypto-ghash-mul (make-array 16 :initial-element 256) \
                                                  (make-array 16 :initial-element 0)) \
           (type-error () :not-an-octet)))";
    assert_eq!(eval(program), "(T :ARITY :SHORT :NOT-AN-OCTET)");
}

/// SHA-512 of "abc" (FIPS 180-4) through both entry points. The registers are
/// (unsigned-byte 64), so most are bignums in EGCL and the primitives must
/// read and store them as such.
#[test]
fn sha512_block_and_words_agree_with_the_standard() {
    let block = "(let ((b (make-array 128 :element-type '(unsigned-byte 8) :initial-element 0))) \
                   (setf (aref b 0) 97 (aref b 1) 98 (aref b 2) 99 (aref b 3) #x80 (aref b 127) 24) b)";
    let program = format!(
        "(progn \
           (defun fresh-regs () \
             (make-array 8 :element-type '(unsigned-byte 64) :initial-contents \
               '(#x6a09e667f3bcc908 #xbb67ae8584caa73b #x3c6ef372fe94f82b #xa54ff53a5f1d36f1 \
                 #x510e527fade682d1 #x9b05688c2b3e6c1f #x1f83d9abfb41bd6b #x5be0cd19137e2179))) \
           (defun words-digest (block) \
             (let ((regs (fresh-regs)) (words (make-array 80 :element-type '(unsigned-byte 64) :initial-element 0))) \
               (dotimes (i 16) \
                 (let ((w 0)) (dotimes (k 8) (setf w (logior (ash w 8) (aref block (+ k (* 8 i)))))) \
                   (setf (aref words i) w))) \
               (egcl-ext:crypto-sha512-words regs words) \
               regs)) \
           (let ((block {block}) (regs (fresh-regs)) (agree t)) \
             (egcl-ext:crypto-sha512-block regs block 0) \
             (dotimes (i 50) (unless (equalp (words-digest block) regs) (setf agree nil))) \
             (list agree (string-downcase (format nil \"~{{~16,'0x~}}\" (coerce regs 'list))))))"
    );
    assert_eq!(
        eval(&program),
        "(T \"ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f\")"
    );
}

/// SHA3-256 of the empty message (FIPS 202): one padded 136-octet block
/// absorbed into a zero state, read back little-endian from the first four
/// lanes. Absorbing must equal XORing the block in and then permuting.
#[test]
fn keccak_absorb_and_permutation_agree_with_the_standard() {
    let program = "(let ((block (make-array 136 :element-type '(unsigned-byte 8) :initial-element 0)) \
               (absorbed (make-array 25 :element-type '(unsigned-byte 64) :initial-element 0)) \
               (permuted (make-array 25 :element-type '(unsigned-byte 64) :initial-element 0))) \
           (setf (aref block 0) 6 (aref block 135) #x80) \
           (egcl-ext:crypto-keccak-absorb absorbed 136 block 0) \
           (setf (aref permuted 0) 6 (aref permuted 16) (ash #x80 56)) \
           (egcl-ext:crypto-keccak-f1600 permuted) \
           (list (equalp absorbed permuted) \
                 (string-downcase \
                  (format nil \"~{~2,'0x~}\" \
                          (loop for i below 32 \
                                collect (ldb (byte 8 (* 8 (mod i 8))) (aref absorbed (floor i 8))))))))";
    assert_eq!(
        eval(program),
        "(T \"a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a\")"
    );
}

#[test]
fn sixty_four_bit_primitives_check_their_arguments() {
    let program = "(list (fboundp 'egcl-ext:crypto-keccak-f1600) \
         (handler-case (egcl-ext:crypto-keccak-f1600) (program-error () :arity)) \
         (handler-case (egcl-ext:crypto-keccak-f1600 (make-array 24 :initial-element 0)) \
           (type-error () :short)) \
         (handler-case (egcl-ext:crypto-keccak-f1600 (make-array 25 :initial-element -1)) \
           (type-error () :negative-lane)) \
         (handler-case (egcl-ext:crypto-keccak-f1600 (make-array 25 :initial-element (expt 2 64))) \
           (type-error () :wide-lane)) \
         (handler-case (egcl-ext:crypto-keccak-absorb (make-array 25 :initial-element 0) 12 \
                                                      (make-array 200 :initial-element 0) 0) \
           (type-error () :bad-rate)))";
    assert_eq!(
        eval(program),
        "(T :ARITY :SHORT :NEGATIVE-LANE :WIDE-LANE :BAD-RATE)"
    );
}
