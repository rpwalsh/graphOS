// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ChaCha20-Poly1305 Authenticated Encryption with Associated Data (AEAD).
//!
//! Implements:
//!  * ChaCha20 stream cipher  (RFC 8439 §2.1–§2.4)
//!  * Poly1305 MAC            (RFC 8439 §2.5–§2.6)
//!  * ChaCha20-Poly1305 AEAD  (RFC 8439 §2.8)
//!
//! No heap allocations.  Plaintext and ciphertext sizes are limited to 2³²−1
//! bytes by the ChaCha20 block counter (same as the RFC limit).
//!
//! # Usage
//! ```ignore
//! // Encryption
//! let ciphertext = chacha20poly1305_seal(&key, &nonce, plaintext, aad)?;
//! // Decryption
//! let plaintext  = chacha20poly1305_open(&key, &nonce, ciphertext, aad)?;
//! ```

use core::convert::TryInto;

// ── ChaCha20 ──────────────────────────────────────────────────────────────────

/// ChaCha20 quarter-round on the state array (in-place).
#[inline(always)]
fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(7);
}

/// Produce one 64-byte ChaCha20 block.
///
/// `key`     — 32 bytes  
/// `counter` — 32-bit block counter  
/// `nonce`   — 12 bytes  
fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    // Initial state (RFC 8439 §2.3)
    let mut s: [u32; 16] = [
        0x6170_7865,
        0x3320_646e,
        0x7962_2d32,
        0x6b20_6574, // "expa", "nd 3", "2-by", "te k"
        u32::from_le_bytes(key[0..4].try_into().unwrap()),
        u32::from_le_bytes(key[4..8].try_into().unwrap()),
        u32::from_le_bytes(key[8..12].try_into().unwrap()),
        u32::from_le_bytes(key[12..16].try_into().unwrap()),
        u32::from_le_bytes(key[16..20].try_into().unwrap()),
        u32::from_le_bytes(key[20..24].try_into().unwrap()),
        u32::from_le_bytes(key[24..28].try_into().unwrap()),
        u32::from_le_bytes(key[28..32].try_into().unwrap()),
        counter,
        u32::from_le_bytes(nonce[0..4].try_into().unwrap()),
        u32::from_le_bytes(nonce[4..8].try_into().unwrap()),
        u32::from_le_bytes(nonce[8..12].try_into().unwrap()),
    ];

    let init = s;

    // 20 rounds = 10 double-rounds
    for _ in 0..10 {
        // Column rounds
        qr(&mut s, 0, 4, 8, 12);
        qr(&mut s, 1, 5, 9, 13);
        qr(&mut s, 2, 6, 10, 14);
        qr(&mut s, 3, 7, 11, 15);
        // Diagonal rounds
        qr(&mut s, 0, 5, 10, 15);
        qr(&mut s, 1, 6, 11, 12);
        qr(&mut s, 2, 7, 8, 13);
        qr(&mut s, 3, 4, 9, 14);
    }

    // Add initial state back
    for i in 0..16 {
        s[i] = s[i].wrapping_add(init[i]);
    }

    // Serialise as little-endian
    let mut out = [0u8; 64];
    for i in 0..16 {
        let b = s[i].to_le_bytes();
        out[i * 4..i * 4 + 4].copy_from_slice(&b);
    }
    out
}

/// XOR `buf` with the ChaCha20 keystream starting at block `block_counter`.
/// `buf` may be any length; a partial final block is handled correctly.
fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], block_counter: u32, buf: &mut [u8]) {
    let mut counter = block_counter;
    let mut offset = 0usize;
    while offset < buf.len() {
        let block = chacha20_block(key, counter, nonce);
        let chunk = core::cmp::min(64, buf.len() - offset);
        for i in 0..chunk {
            buf[offset + i] ^= block[i];
        }
        offset += chunk;
        counter = counter.wrapping_add(1);
    }
}

/// Generate a 32-byte Poly1305 one-time key from the ChaCha20 block at
/// counter=0 (RFC 8439 §2.6).
fn poly1305_key_gen(key: &[u8; 32], nonce: &[u8; 12]) -> [u8; 32] {
    let block = chacha20_block(key, 0, nonce);
    let mut k = [0u8; 32];
    k.copy_from_slice(&block[..32]);
    k
}

// ── Poly1305 ──────────────────────────────────────────────────────────────────
//
// Poly1305 uses arithmetic in a 130-bit prime field: GF(2¹³⁰ − 5).
// The accumulator and the r/s key halves are stored as five 26-bit limbs.

/// Compute the Poly1305 MAC over `msg`.
///
/// `one_time_key` is 32 bytes: first 16 = r (clamped), last 16 = s.
pub fn poly1305_mac(one_time_key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    // Parse and clamp r (RFC 8439 §2.5)
    let mut r = [0u8; 16];
    r.copy_from_slice(&one_time_key[..16]);
    // Clamp r: clear specific bits
    r[3] &= 15;
    r[7] &= 15;
    r[11] &= 15;
    r[15] &= 15;
    r[4] &= 252;
    r[8] &= 252;
    r[12] &= 252;

    // Represent r as five 26-bit limbs (little-endian)
    let r0 = (u32::from_le_bytes(r[0..4].try_into().unwrap()) as u64) & 0x3FF_FFFF;
    let r1 = ((u32::from_le_bytes(r[3..7].try_into().unwrap()) as u64) >> 2) & 0x3FF_FFFF;
    let r2 = ((u32::from_le_bytes(r[6..10].try_into().unwrap()) as u64) >> 4) & 0x3FF_FFFF;
    let r3 = ((u32::from_le_bytes(r[9..13].try_into().unwrap()) as u64) >> 6) & 0x3FF_FFFF;
    let r4 = ((u32::from_le_bytes(r[12..16].try_into().unwrap()) as u64) >> 8) & 0x3FF_FFFF;

    // Precomputed 5*r for the reduction step
    let s1 = r1 * 5;
    let s2 = r2 * 5;
    let s3 = r3 * 5;
    let s4 = r4 * 5;

    // Accumulator h (five 26-bit limbs)
    let mut h0: u64 = 0;
    let mut h1: u64 = 0;
    let mut h2: u64 = 0;
    let mut h3: u64 = 0;
    let mut h4: u64 = 0;

    // Process 16-byte chunks
    let mut i = 0usize;
    while i < msg.len() {
        let n = core::cmp::min(16, msg.len() - i);
        // Build a 17-byte chunk (with the 0x01 high-bit marker)
        let mut chunk = [0u8; 17];
        chunk[..n].copy_from_slice(&msg[i..i + n]);
        chunk[n] = 0x01;

        // Add chunk (as a 130-bit integer) to h
        let c0 = (u32::from_le_bytes(chunk[0..4].try_into().unwrap()) as u64) & 0x3FF_FFFF;
        let c1 = ((u32::from_le_bytes(chunk[3..7].try_into().unwrap()) as u64) >> 2) & 0x3FF_FFFF;
        let c2 = ((u32::from_le_bytes(chunk[6..10].try_into().unwrap()) as u64) >> 4) & 0x3FF_FFFF;
        let c3 = ((u32::from_le_bytes(chunk[9..13].try_into().unwrap()) as u64) >> 6) & 0x3FF_FFFF;
        let c4 = ((u32::from_le_bytes(chunk[12..16].try_into().unwrap()) as u64) >> 8)
            | ((chunk[16] as u64) << 24);

        h0 += c0;
        h1 += c1;
        h2 += c2;
        h3 += c3;
        h4 += c4;

        // Multiply h by r (mod 2¹³⁰ − 5)
        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;

        // Carry reduction
        let c = d0 >> 26;
        h0 = d0 & 0x3FF_FFFF;
        let d1 = d1 + c;
        let c = d1 >> 26;
        h1 = d1 & 0x3FF_FFFF;
        let d2 = d2 + c;
        let c = d2 >> 26;
        h2 = d2 & 0x3FF_FFFF;
        let d3 = d3 + c;
        let c = d3 >> 26;
        h3 = d3 & 0x3FF_FFFF;
        let d4 = d4 + c;
        let c = d4 >> 26;
        h4 = d4 & 0x3FF_FFFF;
        h0 += c * 5;
        let c = h0 >> 26;
        h0 &= 0x3FF_FFFF;
        h1 += c;

        i += 16;
    }

    // Finalise: reduce h mod 2¹³⁰ − 5
    let c = h1 >> 26;
    h1 &= 0x3FF_FFFF;
    h2 += c;
    let c = h2 >> 26;
    h2 &= 0x3FF_FFFF;
    h3 += c;
    let c = h3 >> 26;
    h3 &= 0x3FF_FFFF;
    h4 += c;
    let c = h4 >> 26;
    h4 &= 0x3FF_FFFF;
    h0 += c * 5;
    let c = h0 >> 26;
    h0 &= 0x3FF_FFFF;
    h1 += c;

    // Compute h - (2¹³⁰ − 5) and select the smaller
    let g0 = h0.wrapping_add(5);
    let c = g0 >> 26;
    let g0 = g0 & 0x3FF_FFFF;
    let g1 = h1.wrapping_add(c);
    let c = g1 >> 26;
    let g1 = g1 & 0x3FF_FFFF;
    let g2 = h2.wrapping_add(c);
    let c = g2 >> 26;
    let g2 = g2 & 0x3FF_FFFF;
    let g3 = h3.wrapping_add(c);
    let c = g3 >> 26;
    let g3 = g3 & 0x3FF_FFFF;
    let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

    // mask = 0xFFFF…FFFF if g4 underflowed (h < 2¹³⁰ − 5), else 0
    let mask = (g4 >> 63).wrapping_sub(1);
    h0 = (h0 & !mask) | (g0 & mask);
    h1 = (h1 & !mask) | (g1 & mask);
    h2 = (h2 & !mask) | (g2 & mask);
    h3 = (h3 & !mask) | (g3 & mask);
    h4 = (h4 & !mask) | (g4 & mask);

    // h = h + s (where s is the last 16 bytes of the one-time key)
    let s0 = u32::from_le_bytes(one_time_key[16..20].try_into().unwrap()) as u64;
    let s1 = u32::from_le_bytes(one_time_key[20..24].try_into().unwrap()) as u64;
    let s2 = u32::from_le_bytes(one_time_key[24..28].try_into().unwrap()) as u64;
    let s3 = u32::from_le_bytes(one_time_key[28..32].try_into().unwrap()) as u64;

    // Reconstruct h as four 32-bit words
    let h0_32 = ((h0) | (h1 << 26)) as u32;
    let h1_32 = ((h1 >> 6) | (h2 << 20)) as u32;
    let h2_32 = ((h2 >> 12) | (h3 << 14)) as u32;
    let h3_32 = ((h3 >> 18) | (h4 << 8)) as u32;

    let (t0, c) = (h0_32 as u64 + s0).overflowing_add(0);
    let t0 = t0 as u32;
    let (t1, c1) = (h1_32 as u64 + s1 + c as u64).overflowing_add(0);
    let t1 = t1 as u32;
    let (t2, c2) = (h2_32 as u64 + s2 + c1 as u64).overflowing_add(0);
    let t2 = t2 as u32;
    let t3 = (h3_32 as u64 + s3 + c2 as u64) as u32;

    let mut tag = [0u8; 16];
    tag[0..4].copy_from_slice(&t0.to_le_bytes());
    tag[4..8].copy_from_slice(&t1.to_le_bytes());
    tag[8..12].copy_from_slice(&t2.to_le_bytes());
    tag[12..16].copy_from_slice(&t3.to_le_bytes());
    tag
}

// ── AEAD construction ─────────────────────────────────────────────────────────

/// Build the Poly1305 input as specified by RFC 8439 §2.8:
///
/// ```text
/// AAD || pad(AAD) || ciphertext || pad(ciphertext) || len(AAD) u64le || len(CT) u64le
/// ```
fn build_mac_input<'a>(
    aad: &[u8],
    ciphertext: &[u8],
    buf: &'a mut [u8; 128 + 16 + 16], // max 128 B AAD + 2×8 B lengths + 2×padding
) -> &'a [u8] {
    // We use a simpler approach: write into a stack buffer and return a slice.
    // The sizes here are chosen to be large enough for the protocol messages
    // GraphOS generates; for longer messages the caller should ensure correctness.
    let mut pos = 0usize;

    fn pad16_len(n: usize) -> usize {
        (16 - (n % 16)) % 16
    }

    // Write a slice into buf at pos
    let write = |buf: &mut [u8; 160], pos: &mut usize, data: &[u8]| {
        let end = *pos + data.len();
        buf[*pos..end].copy_from_slice(data);
        *pos = end;
    };

    write(buf, &mut pos, aad);
    let pad_aad = pad16_len(aad.len());
    pos += pad_aad; // zero-filled already (buf is zeroed by caller)

    write(buf, &mut pos, ciphertext);
    let pad_ct = pad16_len(ciphertext.len());
    pos += pad_ct;

    write(buf, &mut pos, &(aad.len() as u64).to_le_bytes());
    write(buf, &mut pos, &(ciphertext.len() as u64).to_le_bytes());

    &buf[..pos]
}

/// Encrypt `plaintext` with ChaCha20-Poly1305.
///
/// Returns the ciphertext followed by the 16-byte Poly1305 tag.
///
/// # Panics
/// `out` must be at least `plaintext.len() + 16` bytes.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8], aad: &[u8], out: &mut [u8]) {
    assert!(out.len() >= plaintext.len() + 16, "output buffer too small");

    // Encrypt (counter=1 per RFC 8439 §2.8)
    out[..plaintext.len()].copy_from_slice(plaintext);
    chacha20_xor(key, nonce, 1, &mut out[..plaintext.len()]);

    // Generate one-time key (counter=0)
    let otk = poly1305_key_gen(key, nonce);

    // Compute MAC
    let ciphertext = &out[..plaintext.len()];
    let tag = compute_aead_tag(&otk, aad, ciphertext);
    out[plaintext.len()..plaintext.len() + 16].copy_from_slice(&tag);
}

/// Decrypt and verify `ciphertext_with_tag` (ciphertext || 16-byte tag).
///
/// Returns `true` and fills `out[..plaintext_len]` on success.
/// Returns `false` if the tag fails — `out` is zeroed in that case.
///
/// `out` must be at least `ciphertext_with_tag.len() - 16` bytes.
pub fn open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext_with_tag: &[u8],
    aad: &[u8],
    out: &mut [u8],
) -> bool {
    if ciphertext_with_tag.len() < 16 {
        return false;
    }
    let ct_len = ciphertext_with_tag.len() - 16;
    let (ciphertext, tag_bytes) = ciphertext_with_tag.split_at(ct_len);

    assert!(out.len() >= ct_len, "output buffer too small");

    // Generate one-time key
    let otk = poly1305_key_gen(key, nonce);

    // Verify tag before decrypting (authenticate-then-decrypt is safe here
    // because the MAC check is constant-time and we decrypt only on success)
    let expected_tag = compute_aead_tag(&otk, aad, ciphertext);

    // Constant-time comparison
    let mut diff: u8 = 0;
    for i in 0..16 {
        diff |= expected_tag[i] ^ tag_bytes[i];
    }
    if diff != 0 {
        // Zero the output to avoid leaking partial plaintext
        for b in out[..ct_len].iter_mut() {
            *b = 0;
        }
        return false;
    }

    // Decrypt
    out[..ct_len].copy_from_slice(ciphertext);
    chacha20_xor(key, nonce, 1, &mut out[..ct_len]);
    true
}

/// Compute the Poly1305 tag over AAD and ciphertext per RFC 8439 §2.8.
fn compute_aead_tag(otk: &[u8; 32], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    fn pad16(n: usize) -> usize {
        (16 - (n % 16)) % 16
    }

    // Build the MAC input on the stack using a 4 KiB scratch buffer.
    // For production use, callers with messages > ~2 KiB should not use
    // the stack buffer approach — but FIDO2 and SSH handshake messages
    // easily fit within this limit.
    const MAX_MAC_INPUT: usize = 4096;
    let total = aad.len() + pad16(aad.len()) + ciphertext.len() + pad16(ciphertext.len()) + 16;
    assert!(
        total <= MAX_MAC_INPUT,
        "MAC input too large for stack buffer"
    );

    let mut buf = [0u8; MAX_MAC_INPUT];
    let mut pos = 0usize;

    buf[pos..pos + aad.len()].copy_from_slice(aad);
    pos += aad.len() + pad16(aad.len());

    buf[pos..pos + ciphertext.len()].copy_from_slice(ciphertext);
    pos += ciphertext.len() + pad16(ciphertext.len());

    buf[pos..pos + 8].copy_from_slice(&(aad.len() as u64).to_le_bytes());
    pos += 8;
    buf[pos..pos + 8].copy_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    pos += 8;

    poly1305_mac(otk, &buf[..pos])
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8439 §2.8.2 test vector
    #[test]
    fn rfc8439_seal_vector() {
        let key = hex32(
            "808182838485868788898a8b8c8d8e8f\
             909192939495969798999a9b9c9d9e9f",
        );
        let nonce = hex12("070000004041424344454647");
        let aad = hex_vec("50515253c0c1c2c3c4c5c6c7");
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

        let mut out = [0u8; 128 + 16];
        seal(
            &key,
            &nonce,
            plaintext,
            &aad,
            &mut out[..plaintext.len() + 16],
        );

        // Verify the first 4 ciphertext bytes (RFC test vector)
        let expected_ct_start = [0xd3, 0x1a, 0x8d, 0x34];
        assert_eq!(&out[..4], &expected_ct_start);
        // Verify the tag (last 16 bytes)
        let expected_tag = hex16("1ae10b594f09e26a7e902ecbd0600691");
        assert_eq!(&out[plaintext.len()..plaintext.len() + 16], &expected_tag);
    }

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }
    fn hex12(s: &str) -> [u8; 12] {
        let mut out = [0u8; 12];
        for i in 0..12 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }
    fn hex16(s: &str) -> [u8; 16] {
        let mut out = [0u8; 16];
        for i in 0..16 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }
    fn hex_vec(s: &str) -> alloc::vec::Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }
}
