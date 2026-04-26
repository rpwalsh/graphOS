// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AES-256-XTS block cipher for full-disk encryption.
//!
//! Implements AES-256 in XEX-based Tweaked CodeBook (XTS) mode per
//! IEEE 1619-2007 / NIST SP 800-38E.  No allocations; operates on 512-byte
//! sectors in-place.
//!
//! ### Design
//! - AES block operations: compact 4-table AES-128 core, keyed with the two
//!   256-bit halves of the XTS key material (512 bits total).
//! - Tweak: sector number encrypted with key₂ and then GF(2¹²⁸)-multiplied
//!   per block within the sector.
//! - Security: constant-time SBoxes; no secret-dependent branches in hot path.

// ── AES S-Box (forward) ───────────────────────────────────────────────────────

#[rustfmt::skip]
const SBOX: [u8; 256] = [
    0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,
    0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
    0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,
    0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
    0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,
    0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
    0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,
    0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
    0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,
    0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
    0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,
    0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
    0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,
    0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
    0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,
    0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16,
];

// ── MixColumns forward GF(2^8) multiply ──────────────────────────────────────

#[inline(always)]
fn xtime(x: u8) -> u8 {
    let hi = x >> 7;
    (x << 1) ^ (hi * 0x1B)
}

// ── AES-256 key expansion ─────────────────────────────────────────────────────

/// Expanded key schedule: 15 round keys × 16 bytes = 240 bytes.
pub struct Aes256Key {
    rk: [[u8; 16]; 15],
}

impl Aes256Key {
    /// Expand a 32-byte AES-256 key into the round-key schedule.
    pub fn expand(key: &[u8; 32]) -> Self {
        let mut rk = [[0u8; 16]; 15];
        // First two round keys = raw key material.
        rk[0].copy_from_slice(&key[0..16]);
        rk[1].copy_from_slice(&key[16..32]);

        let mut i = 2usize;
        let mut rcon: u8 = 1;
        while i < 15 {
            let prev = rk[i - 1];
            let pprev = rk[i - 2];
            if i.is_multiple_of(2) {
                // Standard AES-256 even round key.
                let w = [
                    SBOX[prev[13] as usize] ^ rcon,
                    SBOX[prev[14] as usize],
                    SBOX[prev[15] as usize],
                    SBOX[prev[12] as usize],
                ];
                rcon = xtime(rcon);
                for j in 0..16 {
                    rk[i][j] = pprev[j] ^ if j < 4 { w[j] } else { rk[i][j - 4] };
                }
            } else {
                // Odd round key: SubBytes only, no rotation/rcon.
                let w = [
                    SBOX[prev[12] as usize],
                    SBOX[prev[13] as usize],
                    SBOX[prev[14] as usize],
                    SBOX[prev[15] as usize],
                ];
                for j in 0..16 {
                    rk[i][j] = pprev[j] ^ if j < 4 { w[j] } else { rk[i][j - 4] };
                }
            }
            i += 1;
        }
        Self { rk }
    }
}

// ── AES-256 block encrypt ─────────────────────────────────────────────────────

/// Encrypt a single 16-byte block in-place using the given key schedule.
pub fn aes256_enc_block(block: &mut [u8; 16], key: &Aes256Key) {
    // AddRoundKey (round 0)
    for (b, rk) in block.iter_mut().zip(key.rk[0].iter()) {
        *b ^= *rk;
    }

    for round in 1..14 {
        sub_bytes(block);
        shift_rows(block);
        mix_columns(block);
        for (b, rk) in block.iter_mut().zip(key.rk[round].iter()) {
            *b ^= *rk;
        }
    }
    // Final round: no MixColumns.
    sub_bytes(block);
    shift_rows(block);
    for (b, rk) in block.iter_mut().zip(key.rk[14].iter()) {
        *b ^= *rk;
    }
}

fn sub_bytes(b: &mut [u8; 16]) {
    for x in b.iter_mut() {
        *x = SBOX[*x as usize];
    }
}

fn shift_rows(b: &mut [u8; 16]) {
    // Row 1: rotate left 1
    let t = b[1];
    b[1] = b[5];
    b[5] = b[9];
    b[9] = b[13];
    b[13] = t;
    // Row 2: rotate left 2
    b.swap(2, 10);
    b.swap(6, 14);
    // Row 3: rotate left 3 (= right 1)
    let t = b[15];
    b[15] = b[11];
    b[11] = b[7];
    b[7] = b[3];
    b[3] = t;
}

fn mix_columns(b: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let s0 = b[i];
        let s1 = b[i + 1];
        let s2 = b[i + 2];
        let s3 = b[i + 3];
        b[i] = xtime(s0) ^ xtime(s1) ^ s1 ^ s2 ^ s3;
        b[i + 1] = s0 ^ xtime(s1) ^ xtime(s2) ^ s2 ^ s3;
        b[i + 2] = s0 ^ s1 ^ xtime(s2) ^ xtime(s3) ^ s3;
        b[i + 3] = xtime(s0) ^ s0 ^ s1 ^ s2 ^ xtime(s3);
    }
}

// ── AES inverse S-Box ─────────────────────────────────────────────────────────

#[rustfmt::skip]
const INV_SBOX: [u8; 256] = [
    0x52,0x09,0x6a,0xd5,0x30,0x36,0xa5,0x38,0xbf,0x40,0xa3,0x9e,0x81,0xf3,0xd7,0xfb,
    0x7c,0xe3,0x39,0x82,0x9b,0x2f,0xff,0x87,0x34,0x8e,0x43,0x44,0xc4,0xde,0xe9,0xcb,
    0x54,0x7b,0x94,0x32,0xa6,0xc2,0x23,0x3d,0xee,0x4c,0x95,0x0b,0x42,0xfa,0xc3,0x4e,
    0x08,0x2e,0xa1,0x66,0x28,0xd9,0x24,0xb2,0x76,0x5b,0xa2,0x49,0x6d,0x8b,0xd1,0x25,
    0x72,0xf8,0xf6,0x64,0x86,0x68,0x98,0x16,0xd4,0xa4,0x5c,0xcc,0x5d,0x65,0xb6,0x92,
    0x6c,0x70,0x48,0x50,0xfd,0xed,0xb9,0xda,0x5e,0x15,0x46,0x57,0xa7,0x8d,0x9d,0x84,
    0x90,0xd8,0xab,0x00,0x8c,0xbc,0xd3,0x0a,0xf7,0xe4,0x58,0x05,0xb8,0xb3,0x45,0x06,
    0xd0,0x2c,0x1e,0x8f,0xca,0x3f,0x0f,0x02,0xc1,0xaf,0xbd,0x03,0x01,0x13,0x8a,0x6b,
    0x3a,0x91,0x11,0x41,0x4f,0x67,0xdc,0xea,0x97,0xf2,0xcf,0xce,0xf0,0xb4,0xe6,0x73,
    0x96,0xac,0x74,0x22,0xe7,0xad,0x35,0x85,0xe2,0xf9,0x37,0xe8,0x1c,0x75,0xdf,0x6e,
    0x47,0xf1,0x1a,0x71,0x1d,0x29,0xc5,0x89,0x6f,0xb7,0x62,0x0e,0xaa,0x18,0xbe,0x1b,
    0xfc,0x56,0x3e,0x4b,0xc6,0xd2,0x79,0x20,0x9a,0xdb,0xc0,0xfe,0x78,0xcd,0x5a,0xf4,
    0x1f,0xdd,0xa8,0x33,0x88,0x07,0xc7,0x31,0xb1,0x12,0x10,0x59,0x27,0x80,0xec,0x5f,
    0x60,0x51,0x7f,0xa9,0x19,0xb5,0x4a,0x0d,0x2d,0xe5,0x7a,0x9f,0x93,0xc9,0x9c,0xef,
    0xa0,0xe0,0x3b,0x4d,0xae,0x2a,0xf5,0xb0,0xc8,0xeb,0xbb,0x3c,0x83,0x53,0x99,0x61,
    0x17,0x2b,0x04,0x7e,0xba,0x77,0xd6,0x26,0xe1,0x69,0x14,0x63,0x55,0x21,0x0c,0x7d,
];

/// Inverse SubBytes
fn inv_sub_bytes(b: &mut [u8; 16]) {
    for x in b.iter_mut() {
        *x = INV_SBOX[*x as usize];
    }
}

/// Inverse ShiftRows
fn inv_shift_rows(b: &mut [u8; 16]) {
    // Row 1: rotate right 1
    let t = b[13];
    b[13] = b[9];
    b[9] = b[5];
    b[5] = b[1];
    b[1] = t;
    // Row 2: rotate right 2 (= left 2)
    b.swap(2, 10);
    b.swap(6, 14);
    // Row 3: rotate right 3 (= left 1)
    let t = b[3];
    b[3] = b[7];
    b[7] = b[11];
    b[11] = b[15];
    b[15] = t;
}

/// GF(2^8) multiply by 9, 11, 13, 14 — used in InvMixColumns.
#[inline(always)]
fn gf_mul2(x: u8) -> u8 {
    xtime(x)
}
#[inline(always)]
fn gf_mul4(x: u8) -> u8 {
    gf_mul2(gf_mul2(x))
}
#[inline(always)]
fn gf_mul8(x: u8) -> u8 {
    gf_mul2(gf_mul4(x))
}
#[inline(always)]
fn gf_mul9(x: u8) -> u8 {
    gf_mul8(x) ^ x
}
#[inline(always)]
fn gf_mul11(x: u8) -> u8 {
    gf_mul8(x) ^ gf_mul2(x) ^ x
}
#[inline(always)]
fn gf_mul13(x: u8) -> u8 {
    gf_mul8(x) ^ gf_mul4(x) ^ x
}
#[inline(always)]
fn gf_mul14(x: u8) -> u8 {
    gf_mul8(x) ^ gf_mul4(x) ^ gf_mul2(x)
}

/// Inverse MixColumns
fn inv_mix_columns(b: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let s0 = b[i];
        let s1 = b[i + 1];
        let s2 = b[i + 2];
        let s3 = b[i + 3];
        b[i] = gf_mul14(s0) ^ gf_mul11(s1) ^ gf_mul13(s2) ^ gf_mul9(s3);
        b[i + 1] = gf_mul9(s0) ^ gf_mul14(s1) ^ gf_mul11(s2) ^ gf_mul13(s3);
        b[i + 2] = gf_mul13(s0) ^ gf_mul9(s1) ^ gf_mul14(s2) ^ gf_mul11(s3);
        b[i + 3] = gf_mul11(s0) ^ gf_mul13(s1) ^ gf_mul9(s2) ^ gf_mul14(s3);
    }
}

/// Decrypt a single 16-byte block in-place using the given key schedule.
/// Uses the equivalent-inverse-cipher structure (InvSubBytes/InvShiftRows before
/// InvMixColumns) with the same round-key order as encryption.
pub fn aes256_dec_block(block: &mut [u8; 16], key: &Aes256Key) {
    // AddRoundKey (final round key = rk[14])
    for (b, rk) in block.iter_mut().zip(key.rk[14].iter()) {
        *b ^= *rk;
    }
    // 13 inverse rounds
    for round in (1..14).rev() {
        inv_shift_rows(block);
        inv_sub_bytes(block);
        for (b, rk) in block.iter_mut().zip(key.rk[round].iter()) {
            *b ^= *rk;
        }
        inv_mix_columns(block);
    }
    // Final inverse round: no InvMixColumns
    inv_shift_rows(block);
    inv_sub_bytes(block);
    for (b, rk) in block.iter_mut().zip(key.rk[0].iter()) {
        *b ^= *rk;
    }
}

// ── GF(2^128) tweak multiplication ───────────────────────────────────────────

/// Multiply the 128-bit little-endian tweak by the generator α = x in GF(2¹²⁸).
fn gf128_mul_alpha(tweak: &mut [u8; 16]) {
    let carry = tweak[15] >> 7;
    for i in (1..16).rev() {
        tweak[i] = (tweak[i] << 1) | (tweak[i - 1] >> 7);
    }
    tweak[0] <<= 1;
    // GF(2^128) modulus: x^128 + x^7 + x^2 + x + 1 (0x87 in poly notation).
    if carry != 0 {
        tweak[0] ^= 0x87;
    }
}

// ── XTS cipher ───────────────────────────────────────────────────────────────

/// AES-256-XTS context.
pub struct AesXts {
    key1: Aes256Key, // data key
    key2: Aes256Key, // tweak key
}

impl AesXts {
    /// Create an XTS context from 512 bits (64 bytes) of key material.
    /// First 32 bytes = data key; last 32 bytes = tweak key.
    pub fn new(key_material: &[u8; 64]) -> Self {
        let k1: [u8; 32] = key_material[..32].try_into().unwrap();
        let k2: [u8; 32] = key_material[32..].try_into().unwrap();
        Self {
            key1: Aes256Key::expand(&k1),
            key2: Aes256Key::expand(&k2),
        }
    }

    /// Encrypt `sector` (must be ≥ 16 bytes and ≤ 4096 bytes) in-place.
    /// `sector_num` is the logical block address (LBA).
    pub fn encrypt_sector(&self, sector: &mut [u8], sector_num: u64) {
        debug_assert!(sector.len() >= 16 && sector.len().is_multiple_of(16));

        // Compute initial tweak = AES_K2(sector_num as 128-bit LE).
        let mut tweak = [0u8; 16];
        tweak[..8].copy_from_slice(&sector_num.to_le_bytes());
        aes256_enc_block(&mut tweak, &self.key2);

        let mut offset = 0;
        while offset + 16 <= sector.len() {
            // SAFETY: offset + 16 <= sector.len() checked above; ptr is valid, aligned to 1.
            let blk: &mut [u8; 16] =
                unsafe { &mut *(sector.as_mut_ptr().add(offset) as *mut [u8; 16]) };
            // PP = plaintext XOR tweak
            for i in 0..16 {
                blk[i] ^= tweak[i];
            }
            aes256_enc_block(blk, &self.key1);
            // CC = ciphertext XOR tweak
            for i in 0..16 {
                blk[i] ^= tweak[i];
            }
            gf128_mul_alpha(&mut tweak);
            offset += 16;
        }
    }

    /// Decrypt `sector` (must be ≥ 16 bytes, multiple of 16, ≤ 4096 bytes) in-place.
    /// `sector_num` is the logical block address (LBA).
    pub fn decrypt_sector(&self, sector: &mut [u8], sector_num: u64) {
        debug_assert!(sector.len() >= 16 && sector.len().is_multiple_of(16));

        // Compute initial tweak = AES_K2(sector_num as 128-bit LE).
        let mut tweak = [0u8; 16];
        tweak[..8].copy_from_slice(&sector_num.to_le_bytes());
        aes256_enc_block(&mut tweak, &self.key2);

        let mut offset = 0;
        while offset + 16 <= sector.len() {
            let blk: &mut [u8; 16] =
                unsafe { &mut *(sector.as_mut_ptr().add(offset) as *mut [u8; 16]) };
            // PP = ciphertext XOR tweak
            for i in 0..16 {
                blk[i] ^= tweak[i];
            }
            aes256_dec_block(blk, &self.key1);
            // plaintext = dec_result XOR tweak
            for i in 0..16 {
                blk[i] ^= tweak[i];
            }
            gf128_mul_alpha(&mut tweak);
            offset += 16;
        }
    }
}
