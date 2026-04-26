// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Ed25519 signature generation (RFC 8032).
//!
//! This module provides Ed25519 key generation and signing, complementing the
//! verification-only code in `crypto::ed25519`.
//!
//! All field arithmetic is a copy of the TweetNaCl 16-limb GF(2Â²âµâµâˆ’19)
//! representation used in the verifier so that the two modules remain
//! independent and each can be audited in isolation.
//!
//! # Key representation
//! * `seed`           â€” 32 random bytes (private)
//! * `public_key`     â€” 32 bytes (compressed Edwards y-coordinate with sign bit)
//! * `expanded_secret`â€” 64 bytes = SHA-512(seed); first 32 bytes clamped as the
//!   scalar, last 32 bytes as the nonce prefix
//!
//! # API
//! ```ignore
//! let (pk, xsk) = ed25519_keygen(&seed);
//! let sig = ed25519_sign(&xsk, &pk, msg);
//! assert!(crate::crypto::ed25519::verify(&pk, msg, &sig));
//! ```

// â”€â”€ Field arithmetic â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
type Fe = [i64; 16];

const fn fe0() -> Fe {
    [0i64; 16]
}
const fn fe1() -> Fe {
    let mut f = [0i64; 16];
    f[0] = 1;
    f
}

fn unpack_fe(r: &mut Fe, b: &[u8; 32]) {
    for i in 0..16 {
        r[i] = b[2 * i] as i64 | ((b[2 * i + 1] as i64) << 8);
    }
    r[15] &= 0x7fff;
}

fn pack_fe(r: &mut [u8; 32], x: &Fe) {
    let mut t = *x;
    car(&mut t);
    car(&mut t);
    car(&mut t);
    for _ in 0..2 {
        let m = -(((t[15] + 1) >> 16) & 1);
        let mut b: i64 = t[0].wrapping_sub(0xffed & m);
        t[0] = b & 0xffff;
        for ti in t.iter_mut().take(15).skip(1) {
            b = (b >> 16) + ti.wrapping_sub(0xffff & m);
            *ti = b & 0xffff;
        }
        b = (b >> 16) + t[15].wrapping_sub(0x7fff & m);
        t[15] = b & 0xffff;
    }
    for i in 0..16 {
        r[2 * i] = (t[i] & 0xff) as u8;
        r[2 * i + 1] = (t[i] >> 8) as u8;
    }
}

fn car(o: &mut Fe) {
    for i in 0..16 {
        o[i] += 1 << 16;
        let c = o[i] >> 16;
        if i < 15 {
            o[i + 1] += c - 1;
        } else {
            o[0] += 38 * (c - 1);
        }
        o[i] -= c << 16;
    }
}

fn fadd(o: &mut Fe, a: &Fe, b: &Fe) {
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
}
fn fsub(o: &mut Fe, a: &Fe, b: &Fe) {
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
}
fn fmul(o: &mut Fe, a: &Fe, b: &Fe) {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    o[..16].copy_from_slice(&t[..16]);
    car(o);
    car(o);
}
fn fsqr(o: &mut Fe, a: &Fe) {
    let t = *a;
    fmul(o, &t, &t);
}
fn finv(o: &mut Fe, a: &Fe) {
    let mut c = *a;
    let mut t = fe0();
    for n in (0..254i32).rev() {
        let tc = c;
        fsqr(&mut t, &tc);
        if n != 2 && n != 4 {
            let tt = t;
            fmul(&mut c, &tt, a);
        } else {
            c = t;
        }
    }
    *o = c;
}

// Edwards curve constants (same as crypto/mod.rs)
const D: Fe = {
    let mut f = fe0();
    f[0] = -10913610;
    f[1] = 13857413;
    f[2] = -15372611;
    f[3] = 722;
    f[4] = 6925020;
    f[5] = -8218;
    f[6] = -12672607;
    f[7] = -830;
    f[8] = 9717120;
    f[9] = -4922;
    f[10] = -10301846;
    f[11] = 6423;
    f[12] = 8677535;
    f[13] = -7289;
    f[14] = 5765164;
    f[15] = 0;
    f
};
const D2: Fe = {
    let mut f = fe0();
    f[0] = -21827060;
    f[1] = 27714826;
    f[2] = -30745222;
    f[3] = 1444;
    f[4] = 13850040;
    f[5] = -16436;
    f[6] = -25345214;
    f[7] = -1660;
    f[8] = 19434240;
    f[9] = -9844;
    f[10] = -20603692;
    f[11] = 12846;
    f[12] = 17355070;
    f[13] = -14578;
    f[14] = 11530328;
    f[15] = 0;
    f
};
const GX: Fe = {
    let mut f = fe0();
    f[0] = -14297830;
    f[1] = -7645148;
    f[2] = 16144683;
    f[3] = -16471763;
    f[4] = -8899398;
    f[5] = 3037714;
    f[6] = 13512395;
    f[7] = 3184326;
    f[8] = -9560195;
    f[9] = -5064539;
    f[10] = 14680462;
    f[11] = -9501656;
    f[12] = -13295986;
    f[13] = 8596489;
    f[14] = -6543080;
    f[15] = 0;
    f
};
const GY: Fe = {
    let mut f = fe0();
    f[0] = -15701636;
    f[1] = 8036851;
    f[2] = 16531918;
    f[3] = 13492809;
    f[4] = 18271233;
    f[5] = -15540053;
    f[6] = -4498788;
    f[7] = -9396990;
    f[8] = -14310303;
    f[9] = 9702765;
    f[10] = 5609697;
    f[11] = 12323301;
    f[12] = -6432341;
    f[13] = -15350910;
    f[14] = -3048281;
    f[15] = 0;
    f
};

// â”€â”€ Point arithmetic â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

#[derive(Clone, Copy)]
struct P {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}
const NEUTRAL: P = P {
    x: fe0(),
    y: fe1(),
    z: fe1(),
    t: fe0(),
};

fn point_add(out: &mut P, a: &P, b: &P) {
    let mut a0 = fe0();
    let mut b0 = fe0();
    let mut c = fe0();
    let mut d = fe0();
    let mut e = fe0();
    let mut f = fe0();
    let mut g = fe0();
    let mut h = fe0();
    let mut tmp = fe0();

    fsub(&mut a0, &a.y, &a.x);
    let bymx = {
        let mut u = fe0();
        fsub(&mut u, &b.y, &b.x);
        u
    };
    fmul(&mut tmp, &a0, &bymx);
    a0 = tmp;

    fadd(&mut b0, &a.y, &a.x);
    let bypx = {
        let mut u = fe0();
        fadd(&mut u, &b.y, &b.x);
        u
    };
    fmul(&mut tmp, &b0, &bypx);
    b0 = tmp;

    fmul(&mut c, &a.t, &b.t);
    let tc = c;
    fmul(&mut tmp, &tc, &D2);
    c = tmp;

    fmul(&mut d, &a.z, &b.z);
    let td = d;
    fadd(&mut tmp, &td, &td);
    d = tmp;

    fsub(&mut e, &b0, &a0);
    fsub(&mut f, &d, &c);
    fadd(&mut g, &d, &c);
    fadd(&mut h, &b0, &a0);

    fmul(&mut out.x, &e, &f);
    fmul(&mut out.y, &h, &g);
    fmul(&mut out.z, &g, &f);
    fmul(&mut out.t, &e, &h);
}

fn scalarmult_base(r: &mut P, s: &[u8; 32]) {
    let base = P {
        x: GX,
        y: GY,
        z: fe1(),
        t: {
            let mut t = fe0();
            fmul(&mut t, &GX, &GY);
            t
        },
    };
    *r = NEUTRAL;
    for i in (0..256).rev() {
        let cur = *r;
        point_add(r, &cur, &cur);
        if (s[i >> 3] >> (i & 7)) & 1 != 0 {
            let cur2 = *r;
            point_add(r, &cur2, &base);
        }
    }
}

fn compress_point(p: &P) -> [u8; 32] {
    let mut zinv = fe0();
    finv(&mut zinv, &p.z);
    let mut x = fe0();
    let mut y = fe0();
    fmul(&mut x, &p.x, &zinv);
    fmul(&mut y, &p.y, &zinv);
    let mut out = [0u8; 32];
    pack_fe(&mut out, &y);
    let mut xb = [0u8; 32];
    pack_fe(&mut xb, &x);
    out[31] ^= (xb[0] & 1) << 7;
    out
}

// â”€â”€ Group order L and scalar arithmetic â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
//
// L = 2^252 + 27742317777372353535851937790883648493
//   little-endian bytes:
const L: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

/// Reduce a 64-byte scalar `s` modulo L.
/// Returns the canonical 32-byte little-endian representative.
///
/// Algorithm from SUPERCOP/TweetNaCl `modL`.
fn mod_l(s: &[u8; 64]) -> [u8; 32] {
    // Work in i64 to avoid overflow â€” each limb is at most 21 bits wide.
    let mut t = [0i64; 64];
    for i in 0..64 {
        t[i] = s[i] as i64;
    }

    // Reduce t[63..32] using precomputed multiples of L.
    // mu = floor(2^512 / L)  precomputed per RFC 8032 / SUPERCOP.
    for i in (32..64usize).rev() {
        let carry = t[i];
        if carry == 0 {
            continue;
        }
        t[i] = 0;
        // Subtract carry * L starting at position i-32.
        // L fits in 32 bytes so offsets are i-32..i.
        for j in 0..32usize {
            t[i - 32 + j] -= carry * L[j] as i64;
        }
        // Propagate borrows
        for j in (i - 32)..(i - 1) {
            t[j + 1] += t[j] >> 8;
            t[j] &= 0xff;
        }
    }

    // Reduce the lower 32 bytes once more
    for i in 0..32usize {
        if i < 31 {
            t[i + 1] += t[i] >> 8;
            t[i] &= 0xff;
        }
    }

    // Final conditional subtraction of L
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = t[i] as u8;
    }
    // Conditional subtract: if out >= L, subtract L
    let borrow = {
        let mut b = 0i64;
        let mut tmp = [0u8; 32];
        for i in 0..32 {
            let v = out[i] as i64 - L[i] as i64 + b;
            tmp[i] = (v & 0xff) as u8;
            b = v >> 8;
        }
        // b < 0 means out < L (no subtraction needed)
        if b >= 0 {
            out = tmp;
        }
        b
    };
    let _ = borrow;
    out
}

/// Compute `(a + b*c) mod L`, where all inputs are 32-byte little-endian scalars.
///
/// Used to compute S = r + H(R||A||M) Â· a  in Ed25519.
fn scalar_muladd(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    // Multiply b*c using schoolbook 32Ã—32 â†’ 64 bytes.
    let mut bc = [0i64; 64];
    for i in 0..32 {
        for j in 0..32 {
            bc[i + j] += (b[i] as i64) * (c[j] as i64);
        }
    }
    // Carry-normalise bc into bytes
    for i in 0..63 {
        bc[i + 1] += bc[i] >> 8;
        bc[i] &= 0xff;
    }

    // Add a to bc (a is 32 bytes = low 32 bytes of bc)
    for i in 0..32 {
        bc[i] += a[i] as i64;
    }
    for i in 0..63 {
        bc[i + 1] += bc[i] >> 8;
        bc[i] &= 0xff;
    }

    let mut buf = [0u8; 64];
    for i in 0..64 {
        buf[i] = (bc[i] & 0xff) as u8;
    }
    mod_l(&buf)
}

// â”€â”€ SHA-512 via BLAKE2b-512 â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn h512(input: &[u8]) -> [u8; 64] {
    crate::crypto::blake2b_hash(64, input)
}

// â”€â”€ Key generation and signing â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Clamp the low 32 bytes of the expanded secret key per RFC 8032 Â§5.1.5.
fn clamp_scalar(s: &mut [u8; 32]) {
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
}

/// Derive the public key and expanded secret key from a 32-byte seed.
///
/// Returns `(public_key, expanded_secret_key)`.
/// `expanded_secret_key` is 64 bytes: first 32 clamped scalar, last 32 nonce prefix.
pub fn ed25519_keygen(seed: &[u8; 32]) -> ([u8; 32], [u8; 64]) {
    let h = h512(seed);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&h[..32]);
    clamp_scalar(&mut scalar);

    let mut pt = NEUTRAL;
    scalarmult_base(&mut pt, &scalar);
    let pubkey = compress_point(&pt);

    let mut xsk = [0u8; 64];
    xsk[..32].copy_from_slice(&scalar);
    xsk[32..].copy_from_slice(&h[32..]);

    (pubkey, xsk)
}

/// Sign `msg` using the expanded secret key `xsk` and the corresponding public key `pk`.
///
/// Returns the 64-byte Ed25519 signature `R || S`.
pub fn ed25519_sign(xsk: &[u8; 64], pk: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let scalar = &xsk[..32];
    let nonce_prefix = &xsk[32..];

    // Compute the nonce scalar r = H(nonce_prefix || msg) mod L
    let r_hash = {
        let mut buf = [0u8; 32 + 4096]; // nonce_prefix (32) + msg (up to 4064 B)
        let msglen = msg.len().min(4064);
        buf[..32].copy_from_slice(nonce_prefix);
        buf[32..32 + msglen].copy_from_slice(&msg[..msglen]);
        h512(&buf[..32 + msglen])
    };
    let r_scalar = mod_l(&r_hash);

    // R = r * B
    let mut r_pt = NEUTRAL;
    scalarmult_base(&mut r_pt, &r_scalar);
    let r_bytes = compress_point(&r_pt);

    // k = H(R || A || msg) mod L
    let k_scalar = {
        let mut kbuf = [0u8; 32 + 32 + 4096];
        let msglen = msg.len().min(4096);
        kbuf[..32].copy_from_slice(&r_bytes);
        kbuf[32..64].copy_from_slice(pk);
        kbuf[64..64 + msglen].copy_from_slice(&msg[..msglen]);
        let khash = h512(&kbuf[..64 + msglen]);
        mod_l(&khash)
    };

    // S = (r + k * scalar) mod L
    let mut scalar32 = [0u8; 32];
    scalar32.copy_from_slice(scalar);
    let s_bytes = scalar_muladd(&r_scalar, &k_scalar, &scalar32);

    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&r_bytes);
    sig[32..].copy_from_slice(&s_bytes);
    sig
}

/// Convenience: sign from seed directly (expands the seed on the fly).
pub fn ed25519_sign_seed(seed: &[u8; 32], msg: &[u8]) -> ([u8; 32], [u8; 64]) {
    let (pk, xsk) = ed25519_keygen(seed);
    let sig = ed25519_sign(&xsk, &pk, msg);
    (pk, sig)
}
