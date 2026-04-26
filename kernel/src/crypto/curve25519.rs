// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! X25519 Diffie-Hellman key agreement (RFC 7748).
//!
//! Implements the Montgomery-ladder scalar multiplication on Curve25519
//!   y² = x³ + 486662·x² + x   over GF(2²⁵⁵ − 19)
//!
//! All arithmetic uses a 16-limb radix-2¹⁶ representation identical to the
//! TweetNaCl convention already used in the Ed25519 module, keeping the
//! constant-time property and avoiding data-dependent branches in field ops.
//!
//! # API
//! ```ignore
//! let shared = x25519(my_private_key, their_public_key);
//! let pubkey  = x25519_public_key(&my_private_key);
//! ```

// ── Field type ────────────────────────────────────────────────────────────────

/// GF(2²⁵⁵−19) element: 16 signed 64-bit limbs in radix 2¹⁶.
type Fe = [i64; 16];

#[inline(always)]
const fn fe_0() -> Fe {
    [0i64; 16]
}

#[inline(always)]
fn fe_1() -> Fe {
    let mut f = [0i64; 16];
    f[0] = 1;
    f
}

/// Carry-reduce one pass.  Applied twice after any multiply.
#[inline(always)]
fn carry(f: &mut Fe) {
    for i in 0..15 {
        f[i + 1] += f[i] >> 16;
        f[i] &= 0xFFFF;
    }
    f[0] += 38 * (f[15] >> 16);
    f[15] &= 0xFFFF;
}

fn fe_add(a: &Fe, b: &Fe) -> Fe {
    let mut o = fe_0();
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

fn fe_sub(a: &Fe, b: &Fe) -> Fe {
    let mut o = fe_0();
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

fn fe_mul(a: &Fe, b: &Fe) -> Fe {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o = fe_0();
    o.copy_from_slice(&t[..16]);
    carry(&mut o);
    carry(&mut o);
    o
}

fn fe_sqr(a: &Fe) -> Fe {
    fe_mul(a, a)
}

/// Constant-time conditional swap.  Swap (a, b) if `bit == 1`.
#[inline(always)]
fn cswap(bit: i64, a: &mut Fe, b: &mut Fe) {
    let mask = -bit; // 0x0000…0000 or 0xFFFF…FFFF
    for i in 0..16 {
        let t = mask & (a[i] ^ b[i]);
        a[i] ^= t;
        b[i] ^= t;
    }
}

/// Modular inverse via Fermat: a^(p−2) = a^(2²⁵⁵−21).
/// Uses the same addition chain as TweetNaCl `inv25519`.
fn fe_inv(a: &Fe) -> Fe {
    let mut c = *a;
    // a^(2^255 - 21): addition chain over the exponent
    // Step through bits of the exponent 2^255 - 21.
    for i in (0i32..254).rev() {
        c = fe_sqr(&c);
        // bit i of (2^255 - 21): all bits 1 except bit 1 and bit 0
        if i != 1 {
            c = fe_mul(&c, a);
        }
    }
    c
}

// ── Unpack / Pack ─────────────────────────────────────────────────────────────

fn unpack(n: &[u8; 32]) -> Fe {
    let mut o = fe_0();
    for i in 0..16 {
        o[i] = (n[2 * i] as i64) | ((n[2 * i + 1] as i64) << 8);
    }
    o
}

fn pack(n: &Fe) -> [u8; 32] {
    let mut m = *n;
    // Normalise the representation into [0, p−1]
    carry(&mut m);
    carry(&mut m);
    carry(&mut m);
    for _ in 0..2 {
        // Subtract p if m >= p
        let mut t = m;
        t[0] -= 0xFFED;
        for i in 0..15 {
            t[i + 1] -= 1 + (t[i] >> 63);
            t[i] &= 0xFFFF;
        }
        t[15] -= 1;
        if t[15] >> 63 == 0 {
            m = t;
        }
    }
    let mut out = [0u8; 32];
    for i in 0..16 {
        out[2 * i] = m[i] as u8;
        out[2 * i + 1] = (m[i] >> 8) as u8;
    }
    out
}

// ── Scalar clamping ───────────────────────────────────────────────────────────

/// Apply RFC 7748 §5 scalar clamping in-place.
///   - Clear the three low bits of byte 0 (cofactor clearing)
///   - Set bit 254 of byte 31 (ensure scalar in the correct subgroup range)
///   - Clear bit 255 of byte 31 (field element bound)
#[inline]
fn clamp_scalar(k: &mut [u8; 32]) {
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
}

// ── Montgomery ladder ─────────────────────────────────────────────────────────

/// Constant-time Montgomery ladder scalar multiplication.
///
/// Computes the x-coordinate of `k · (u, _)` on Curve25519.
/// a24 = 121666  (= (486662 − 2) / 4)
fn ladder(k: &[u8; 32], u: &Fe) -> Fe {
    const A24: i64 = 121666;

    let x1 = *u;
    let mut x2 = fe_1();
    let mut z2 = fe_0();
    let mut x3 = *u;
    let mut z3 = fe_1();
    let mut swap: i64 = 0;

    // Iterate bits of the clamped scalar from bit 254 down to 0.
    for t in (0i32..255).rev() {
        let k_t = ((k[(t >> 3) as usize] >> (t & 7)) & 1) as i64;
        swap ^= k_t;
        cswap(swap, &mut x2, &mut x3);
        cswap(swap, &mut z2, &mut z3);
        swap = k_t;

        // Montgomery differential addition and doubling
        let a = fe_add(&x2, &z2);
        let aa = fe_sqr(&a);
        let b = fe_sub(&x2, &z2);
        let bb = fe_sqr(&b);
        let e = fe_sub(&aa, &bb);
        let c = fe_add(&x3, &z3);
        let d = fe_sub(&x3, &z3);
        let da = fe_mul(&d, &a);
        let cb = fe_mul(&c, &b);
        let da_cb = fe_add(&da, &cb);
        x3 = fe_sqr(&da_cb);
        let da_sub_cb = fe_sub(&da, &cb);
        let da_sub_cb_sq = fe_sqr(&da_sub_cb);
        z3 = fe_mul(&x1, &da_sub_cb_sq);
        x2 = fe_mul(&aa, &bb);
        // z2 = E * (AA + a24 * E)
        let mut a24e = fe_0();
        for i in 0..16 {
            a24e[i] = A24 * e[i];
        }
        carry(&mut a24e);
        carry(&mut a24e);
        let aa_plus_a24e = fe_add(&aa, &a24e);
        z2 = fe_mul(&e, &aa_plus_a24e);
    }

    cswap(swap, &mut x2, &mut x3);
    cswap(swap, &mut z2, &mut z3);

    fe_mul(&x2, &fe_inv(&z2))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Curve25519 base point u-coordinate.
const BASE_POINT: [u8; 32] = {
    let mut bp = [0u8; 32];
    bp[0] = 9;
    bp
};

/// Perform X25519 Diffie-Hellman.
///
/// `scalar` is a 32-byte private key (will be clamped per RFC 7748 §5).
/// `point`  is a 32-byte peer public key (u-coordinate).
///
/// Returns the 32-byte shared secret u-coordinate.
///
/// # All-zero output check
/// If the output is all-zero the function returns `None`; this indicates a
/// low-order point attack (see RFC 7748 §6).
pub fn x25519(scalar: &[u8; 32], point: &[u8; 32]) -> Option<[u8; 32]> {
    let mut k = *scalar;
    clamp_scalar(&mut k);
    let u = unpack(point);
    let result = ladder(&k, &u);
    let out = pack(&result);
    // Reject all-zero output (low-order point)
    if out.iter().all(|&b| b == 0) {
        None
    } else {
        Some(out)
    }
}

/// Derive the X25519 public key from a private scalar.
///
/// Equivalent to `x25519(scalar, BASE_POINT)`.
pub fn x25519_public_key(scalar: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    clamp_scalar(&mut k);
    let u = unpack(&BASE_POINT);
    let result = ladder(&k, &u);
    pack(&result)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7748 §6.1 test vector 1
    #[test]
    fn rfc7748_vector1() {
        let alice_priv = hex("77076d0a7318a57d3c16c17251b26645 \
             df38b7b0e8f28abfb2c5ddc4de5ba6a1");
        let alice_pub_expected = hex("8520f0098930a754748b7ddcb43ef75a \
             0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let pub_actual = x25519_public_key(&alice_priv);
        assert_eq!(pub_actual, alice_pub_expected);
    }

    /// RFC 7748 §6.1 test vector — shared secret
    #[test]
    fn rfc7748_shared_secret() {
        let alice_priv = hex("77076d0a7318a57d3c16c17251b26645 \
             df38b7b0e8f28abfb2c5ddc4de5ba6a1");
        let bob_pub = hex("de9edb7d7b7dc1b4d35b61c2ece43537 \
             3f8343c85b78674dadfc7e146f882b4f");
        let expected_shared = hex("4a5d9d5ba4ce2de1728e3bf480350f25 \
             e07e21c947d19e3376f09b3c1e161742");
        let shared = x25519(&alice_priv, &bob_pub).unwrap();
        assert_eq!(shared, expected_shared);
    }

    fn hex(s: &str) -> [u8; 32] {
        let s: alloc::string::String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }
}
