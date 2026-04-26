// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal in-kernel crypto primitives.
//!
//! Provides ed25519 signature verification using TweetNaCl-style
//! 16-limb GF(2^255-19) field arithmetic (no_std, no heap).

pub mod aes_xts;
pub mod chacha20poly;
pub mod curve25519;
pub mod ed25519_sign;

pub mod ed25519 {
    type Fe = [i64; 16];

    const fn fe0() -> Fe {
        [0i64; 16]
    }
    const fn fe1() -> Fe {
        let mut f = [0i64; 16];
        f[0] = 1;
        f
    }

    fn unpack(r: &mut Fe, b: &[u8; 32]) {
        for i in 0..16 {
            r[i] = b[2 * i] as i64 | ((b[2 * i + 1] as i64) << 8);
        }
        r[15] &= 0x7fff;
    }

    fn pack(r: &mut [u8; 32], x: &Fe) {
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
    fn fis_zero(a: &Fe) -> bool {
        let mut b = [0u8; 32];
        pack(&mut b, a);
        b == [0u8; 32]
    }

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
    const SQRTM1: Fe = {
        let mut f = fe0();
        f[0] = -32595792;
        f[1] = -7943725;
        f[2] = 9377950;
        f[3] = 3500415;
        f[4] = 12389472;
        f[5] = -272473;
        f[6] = -25146209;
        f[7] = -2005654;
        f[8] = 326686;
        f[9] = 11406482;
        f[10] = 5352972;
        f[11] = -8147646;
        f[12] = 15512398;
        f[13] = -8025745;
        f[14] = -3516369;
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
        let mut tmp = fe0();
        let mut a0 = fe0();
        let mut b0 = fe0();
        let mut c = fe0();
        let mut d = fe0();
        let mut e = fe0();
        let mut f = fe0();
        let mut g = fe0();
        let mut h = fe0();
        // A = (Y1-X1)*(Y2-X2)
        let ta = a.y;
        let tay = a.x;
        fsub(&mut a0, &ta, &tay);
        let bymx = {
            let mut u = fe0();
            let tby = b.y;
            let tbx = b.x;
            fsub(&mut u, &tby, &tbx);
            u
        };
        fmul(&mut tmp, &a0, &bymx);
        a0 = tmp;
        // B = (Y1+X1)*(Y2+X2)
        let ta2 = a.y;
        let tax2 = a.x;
        fadd(&mut b0, &ta2, &tax2);
        let bypx = {
            let mut u = fe0();
            let tby2 = b.y;
            let tbx2 = b.x;
            fadd(&mut u, &tby2, &tbx2);
            u
        };
        fmul(&mut tmp, &b0, &bypx);
        b0 = tmp;
        // C = T1*2d*T2
        let tat = a.t;
        let tbt = b.t;
        fmul(&mut c, &tat, &tbt);
        let tc = c;
        fmul(&mut tmp, &tc, &D2);
        c = tmp;
        // D = Z1*2*Z2
        let taz = a.z;
        let tbz = b.z;
        fmul(&mut d, &taz, &tbz);
        let td = d;
        fadd(&mut tmp, &td, &td);
        d = tmp;
        let ta0 = a0;
        let tb0 = b0;
        fsub(&mut e, &tb0, &ta0);
        let tc2 = c;
        let td2 = d;
        fsub(&mut f, &td2, &tc2);
        let tc3 = c;
        let td3 = d;
        fadd(&mut g, &td3, &tc3);
        let ta02 = a0;
        let tb02 = b0;
        fadd(&mut h, &tb02, &ta02);
        fmul(&mut out.x, &e, &f);
        fmul(&mut out.y, &h, &g);
        fmul(&mut out.z, &g, &f);
        fmul(&mut out.t, &e, &h);
    }

    fn scalarmult(r: &mut P, s: &[u8; 32], base: &P) {
        *r = NEUTRAL;
        for i in (0..256).rev() {
            let cur = *r;
            point_add(r, &cur, &cur);
            if (s[i >> 3] >> (i & 7)) & 1 != 0 {
                let cur2 = *r;
                point_add(r, &cur2, base);
            }
        }
    }

    fn compress(p: &P) -> [u8; 32] {
        let mut zinv = fe0();
        finv(&mut zinv, &p.z);
        let mut x = fe0();
        let mut y = fe0();
        let tpx = p.x;
        let tz = zinv;
        fmul(&mut x, &tpx, &tz);
        let tpy = p.y;
        let tz2 = zinv;
        fmul(&mut y, &tpy, &tz2);
        let mut out = [0u8; 32];
        pack(&mut out, &y);
        let mut xb = [0u8; 32];
        pack(&mut xb, &x);
        out[31] ^= (xb[0] & 1) << 7;
        out
    }

    fn decompress(p: &mut P, b: &[u8; 32]) -> bool {
        let sign = b[31] >> 7;
        let mut y = fe0();
        unpack(&mut y, b);
        p.z = fe1();
        p.y = y;
        let mut y2 = fe0();
        fsqr(&mut y2, &y);
        let mut u = fe0();
        fsub(&mut u, &y2, &fe1());
        let mut v = fe0();
        {
            let ty2 = y2;
            fmul(&mut v, &D, &ty2);
        }
        let tv = v;
        fadd(&mut v, &tv, &fe1());
        // Compute x^((p+3)/8) candidate via u*v^3*(u*v^7)^((p-5)/8)
        let mut v3 = fe0();
        let mut v7 = fe0();
        let mut x = fe0();
        let mut tmp = fe0();
        {
            let tv2 = v;
            fsqr(&mut v3, &tv2);
        }
        {
            let tv3 = v3;
            let tv2b = v;
            fmul(&mut tmp, &tv3, &tv2b);
            v3 = tmp;
        }
        {
            let tv3b = v3;
            fsqr(&mut v7, &tv3b);
        }
        {
            let tv7 = v7;
            let tv2c = v;
            fmul(&mut tmp, &tv7, &tv2c);
            v7 = tmp;
        }
        {
            let tv7b = v7;
            let tu = u;
            fmul(&mut x, &tv7b, &tu);
        }
        // x = x^((p-5)/8)
        let mut pw = x;
        for bit in (3..251i32).rev() {
            let tpw = pw;
            fsqr(&mut tmp, &tpw);
            if bit == 3 {
                pw = tmp;
            } else {
                let tt = tmp;
                let tx = x;
                fmul(&mut pw, &tt, &tx);
            }
        }
        {
            let tpw = pw;
            let tv3c = v3;
            fmul(&mut tmp, &tpw, &tv3c);
            pw = tmp;
        }
        {
            let tpw = pw;
            let tu2 = u;
            fmul(&mut x, &tpw, &tu2);
        }
        // Check v*x^2 == u
        let mut vx2 = fe0();
        let mut chk = fe0();
        {
            let tx = x;
            fsqr(&mut tmp, &tx);
        }
        {
            let tt = tmp;
            let tv = v;
            fmul(&mut vx2, &tt, &tv);
        }
        {
            let tvx2 = vx2;
            let tu3 = u;
            fsub(&mut chk, &tvx2, &tu3);
        }
        if !fis_zero(&chk) {
            let tx2 = x;
            fmul(&mut tmp, &tx2, &SQRTM1);
            x = tmp;
        }
        let mut xb = [0u8; 32];
        pack(&mut xb, &x);
        if (xb[0] & 1) != sign {
            let tx3 = x;
            fsub(&mut x, &fe0(), &tx3);
        }
        p.x = x;
        {
            let tpx = p.x;
            let tpy2 = p.y;
            fmul(&mut p.t, &tpx, &tpy2);
        }
        pack(&mut xb, &p.x);
        !(xb == [0u8; 32] && sign == 1)
    }

    fn h512(input: &[u8]) -> [u8; 64] {
        super::blake2b_hash(64, input)
    }

    fn reduce64(h: &[u8; 64]) -> [u8; 32] {
        let mut s = [0u8; 32];
        s.copy_from_slice(&h[..32]);
        s[31] &= 0x7f;
        s
    }

    /// Verify an Ed25519 signature.
    /// Returns `true` iff `sig` is a valid signature of `msg` under `public_key`.
    pub fn verify(public_key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
        if sig[63] & 0x80 != 0 {
            return false;
        }
        let r_bytes: &[u8; 32] = sig[..32].try_into().unwrap();
        let mut r_pt = NEUTRAL;
        if !decompress(&mut r_pt, r_bytes) {
            return false;
        }
        let mut a_pt = NEUTRAL;
        if !decompress(&mut a_pt, public_key) {
            return false;
        }
        let mut k_buf = [0u8; 32 + 32 + 512];
        let clip = msg.len().min(512);
        k_buf[..32].copy_from_slice(r_bytes);
        k_buf[32..64].copy_from_slice(public_key);
        k_buf[64..64 + clip].copy_from_slice(&msg[..clip]);
        let k_hash = h512(&k_buf[..64 + clip]);
        let k_scalar = reduce64(&k_hash);
        let s_bytes: [u8; 32] = sig[32..64].try_into().unwrap();
        let base = P {
            x: GX,
            y: GY,
            z: fe1(),
            t: {
                let mut t = fe0();
                let tgx = GX;
                let tgy = GY;
                fmul(&mut t, &tgx, &tgy);
                t
            },
        };
        let mut sb = NEUTRAL;
        scalarmult(&mut sb, &s_bytes, &base);
        let mut ka = NEUTRAL;
        scalarmult(&mut ka, &k_scalar, &a_pt);
        let mut rka = NEUTRAL;
        point_add(&mut rka, &r_pt, &ka);
        compress(&sb) == compress(&rka)
    }
}

pub(crate) fn blake2b_hash(nn: usize, input: &[u8]) -> [u8; 64] {
    crate::users::blake2b_kernel_hash(nn, input)
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

/// Compute SHA-256 of `data`.  Pure software, no heap, no_std.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pre-processing: length encoding.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let padded_len = {
        let rem = (data.len() + 1 + 8) % 64;
        if rem == 0 {
            data.len() + 1 + 8
        } else {
            data.len() + 1 + 8 + (64 - rem)
        }
    };

    let mut pad = [0u8; 256];
    let copy_len = data.len().min(pad.len() - 9);
    pad[..copy_len].copy_from_slice(&data[..copy_len]);
    pad[copy_len] = 0x80;
    // Write big-endian bit length at the end of the padded block.
    let bl = bit_len.to_be_bytes();
    let end = padded_len.min(pad.len());
    if end >= 8 {
        pad[end - 8..end].copy_from_slice(&bl);
    }

    let blocks = padded_len / 64;
    for b in 0..blocks.min(4) {
        let chunk = &pad[b * 64..(b + 1) * 64];
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b2, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let tmp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b2) ^ (a & c) ^ (b2 & c);
            let tmp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(tmp1);
            d = c;
            c = b2;
            b2 = a;
            a = tmp1.wrapping_add(tmp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b2);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for i in 0..8 {
        let b = h[i].to_be_bytes();
        out[4 * i..4 * i + 4].copy_from_slice(&b);
    }
    out
}
