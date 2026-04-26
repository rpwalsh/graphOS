// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! sshd — GraphOS SSH server daemon.
//!
//! Architecture (graph-first):
//!  - Registers with servicemgr overseer on startup via `announce_service_ready`
//!  - Reports session open/close events back to servicemgr via channel
//!  - Each accepted TCP connection runs its own SSH2 handshake
//!  - Authentication delegates to `SYS_LOGIN` (kernel UserDb)
//!  - Successful auth allocates a PTY via `SYS_PTY_ALLOC`
//!
//! SSH2 protocol (RFC 4253 / 4252 / 4254):
//!  - Key exchange   : curve25519-sha256 (X25519 + SHA-256)
//!  - Host key       : ssh-ed25519 (Ed25519)
//!  - Encryption     : chacha20-poly1305@openssh.com
//!  - Auth           : password (via SYS_LOGIN)
//!  - Channel        : session + pty-req + shell

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

// ════════════════════════════════════════════════════════════════════
// Compile-time host key (test only — production loads from /data/etc/ssh/)
// In production, replace with a key read via SYS_VFS_OPEN at startup.
// ════════════════════════════════════════════════════════════════════

/// Ed25519 host private key scalar (32 bytes), little-endian.
const HOST_PRIVKEY: [u8; 32] = [
    0x9a, 0x8f, 0x4b, 0x2c, 0x7e, 0x13, 0xd5, 0x60,
    0xaf, 0x32, 0x8e, 0x74, 0xc1, 0x05, 0xb9, 0xd7,
    0x44, 0x6a, 0x2b, 0x91, 0xec, 0x37, 0xf8, 0x0d,
    0x5c, 0xe2, 0x9b, 0x16, 0xa3, 0x78, 0x4f, 0x22,
];

/// Ed25519 host public key (32 bytes).
const HOST_PUBKEY: [u8; 32] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7,
    0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25,
    0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];

const SSH_PORT: u16 = 22;
const SERVER_VERSION: &[u8] = b"SSH-2.0-GraphOS_1.0\r\n";
const MAX_PACKET: usize = 35000;

// ════════════════════════════════════════════════════════════════════
// Inline crypto: SHA-256
// ════════════════════════════════════════════════════════════════════

mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
    ];

    fn rotr(x: u32, n: u32) -> u32 { x.rotate_right(n) }

    pub fn hash(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
            0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19,
        ];
        let mut padded = [0u8; 128];
        let msg_len = data.len();
        let mut pos = 0;
        let mut remaining = data;

        // Process complete 64-byte blocks.
        while remaining.len() >= 64 {
            compress(&mut h, remaining[..64].try_into().unwrap());
            remaining = &remaining[64..];
        }
        // Final block(s).
        padded[..remaining.len()].copy_from_slice(remaining);
        padded[remaining.len()] = 0x80;
        let bit_len = (msg_len as u64) * 8;
        let padding_end = if remaining.len() < 56 { 64 } else { 128 };
        padded[padding_end - 8..padding_end].copy_from_slice(&bit_len.to_be_bytes());
        compress(&mut h, padded[..64].try_into().unwrap());
        if padding_end == 128 {
            compress(&mut h, padded[64..128].try_into().unwrap());
        }
        let _ = pos;

        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[i*4..i*4+4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i*4..i*4+4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = rotr(w[i-15],7) ^ rotr(w[i-15],18) ^ (w[i-15]>>3);
            let s1 = rotr(w[i-2],17) ^ rotr(w[i-2],19) ^ (w[i-2]>>10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let [mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut hh] = *h;
        for i in 0..64 {
            let s1 = rotr(e,6) ^ rotr(e,11) ^ rotr(e,25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = rotr(a,2) ^ rotr(a,13) ^ rotr(a,22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g; g = f; f = e;
            e = d.wrapping_add(t1);
            d = c; c = b; b = a;
            a = t1.wrapping_add(t2);
        }
        h[0]=h[0].wrapping_add(a); h[1]=h[1].wrapping_add(b);
        h[2]=h[2].wrapping_add(c); h[3]=h[3].wrapping_add(d);
        h[4]=h[4].wrapping_add(e); h[5]=h[5].wrapping_add(f);
        h[6]=h[6].wrapping_add(g); h[7]=h[7].wrapping_add(hh);
    }

    /// HMAC-SHA256.
    pub fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
        let mut k = [0u8; 64];
        if key.len() <= 64 {
            k[..key.len()].copy_from_slice(key);
        } else {
            let h = hash(key);
            k[..32].copy_from_slice(&h);
        }
        let mut ipad = k;
        let mut opad = k;
        for b in &mut ipad { *b ^= 0x36; }
        for b in &mut opad { *b ^= 0x5c; }

        // inner = SHA256(ipad || msg)
        let mut inner_buf = [0u8; 64 + 4096];
        let inner_len = 64 + msg.len().min(4032);
        inner_buf[..64].copy_from_slice(&ipad);
        inner_buf[64..64 + msg.len().min(4032)].copy_from_slice(&msg[..msg.len().min(4032)]);
        let inner = hash(&inner_buf[..inner_len]);

        let mut outer_buf = [0u8; 96];
        outer_buf[..64].copy_from_slice(&opad);
        outer_buf[64..96].copy_from_slice(&inner);
        hash(&outer_buf)
    }
}

// ════════════════════════════════════════════════════════════════════
// Inline crypto: ChaCha20
// ════════════════════════════════════════════════════════════════════

mod chacha20 {
    fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        s[a] = s[a].wrapping_add(s[b]); s[d] ^= s[a]; s[d] = s[d].rotate_left(16);
        s[c] = s[c].wrapping_add(s[d]); s[b] ^= s[c]; s[b] = s[b].rotate_left(12);
        s[a] = s[a].wrapping_add(s[b]); s[d] ^= s[a]; s[d] = s[d].rotate_left(8);
        s[c] = s[c].wrapping_add(s[d]); s[b] ^= s[c]; s[b] = s[b].rotate_left(7);
    }

    pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
        let mut state = [0u32; 16];
        state[0] = 0x61707865; state[1] = 0x3320646e;
        state[2] = 0x79622d32; state[3] = 0x6b206574;
        for i in 0..8 { state[4+i] = u32::from_le_bytes(key[i*4..i*4+4].try_into().unwrap()); }
        state[12] = counter;
        for i in 0..3 { state[13+i] = u32::from_le_bytes(nonce[i*4..i*4+4].try_into().unwrap()); }
        let init = state;
        for _ in 0..10 {
            qr(&mut state, 0, 4, 8, 12);
            qr(&mut state, 1, 5, 9, 13);
            qr(&mut state, 2, 6, 10, 14);
            qr(&mut state, 3, 7, 11, 15);
            qr(&mut state, 0, 5, 10, 15);
            qr(&mut state, 1, 6, 11, 12);
            qr(&mut state, 2, 7, 8, 13);
            qr(&mut state, 3, 4, 9, 14);
        }
        let mut out = [0u8; 64];
        for i in 0..16 {
            let w = state[i].wrapping_add(init[i]);
            out[i*4..i*4+4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// XOR `buf` in-place with ChaCha20 keystream starting at counter `ctr`.
    pub fn xor(key: &[u8; 32], ctr: u32, nonce: &[u8; 12], buf: &mut [u8]) {
        let mut c = ctr;
        let mut off = 0;
        while off < buf.len() {
            let ks = block(key, c, nonce);
            let n = (buf.len() - off).min(64);
            for i in 0..n { buf[off+i] ^= ks[i]; }
            off += n;
            c = c.wrapping_add(1);
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Inline crypto: Poly1305
// ════════════════════════════════════════════════════════════════════

mod poly1305 {
    fn clamp(r: &mut [u8; 16]) {
        r[3] &= 15; r[7] &= 15; r[11] &= 15; r[15] &= 15;
        r[4] &= 252; r[8] &= 252; r[12] &= 252;
    }

    pub fn mac(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
        let mut r = [0u8; 16];
        r.copy_from_slice(&key[..16]);
        clamp(&mut r);
        let s = &key[16..32];

        // Use 130-bit arithmetic via 5×26-bit limbs.
        let r0 = (u32::from_le_bytes(r[0..4].try_into().unwrap())) & 0x3ffffff;
        let r1 = (u32::from_le_bytes(r[3..7].try_into().unwrap()) >> 2) & 0x3ffff03;
        let r2 = (u32::from_le_bytes(r[6..10].try_into().unwrap()) >> 4) & 0x3ffc0ff;
        let r3 = (u32::from_le_bytes(r[9..13].try_into().unwrap()) >> 6) & 0x3f03fff;
        let r4 = (u32::from_le_bytes(r[12..16].try_into().unwrap()) >> 8) & 0x00fffff;

        let (mut h0, mut h1, mut h2, mut h3, mut h4) = (0u32, 0u32, 0u32, 0u32, 0u32);

        let mut i = 0;
        while i < msg.len() {
            let mut block = [0u8; 17];
            let n = (msg.len() - i).min(16);
            block[..n].copy_from_slice(&msg[i..i+n]);
            block[n] = 1;
            let m0 = u32::from_le_bytes(block[0..4].try_into().unwrap()) & 0x3ffffff;
            let m1 = (u32::from_le_bytes(block[3..7].try_into().unwrap()) >> 2) & 0x3ffffff;
            let m2 = (u32::from_le_bytes(block[6..10].try_into().unwrap()) >> 4) & 0x3ffffff;
            let m3 = (u32::from_le_bytes(block[9..13].try_into().unwrap()) >> 6) & 0x3ffffff;
            let m4 = (u32::from_le_bytes(block[12..16].try_into().unwrap()) >> 8) | ((block[16] as u32) << 24);

            h0 = h0.wrapping_add(m0);
            h1 = h1.wrapping_add(m1);
            h2 = h2.wrapping_add(m2);
            h3 = h3.wrapping_add(m3);
            h4 = h4.wrapping_add(m4);

            // Multiply (h * r) mod 2^130-5
            let r1_5 = r1.wrapping_mul(5);
            let r2_5 = r2.wrapping_mul(5);
            let r3_5 = r3.wrapping_mul(5);
            let r4_5 = r4.wrapping_mul(5);

            let d0 = (h0 as u64)*r0 as u64 + (h1 as u64)*r4_5 as u64 + (h2 as u64)*r3_5 as u64 + (h3 as u64)*r2_5 as u64 + (h4 as u64)*r1_5 as u64;
            let d1 = (h0 as u64)*r1 as u64 + (h1 as u64)*r0 as u64 + (h2 as u64)*r4_5 as u64 + (h3 as u64)*r3_5 as u64 + (h4 as u64)*r2_5 as u64;
            let d2 = (h0 as u64)*r2 as u64 + (h1 as u64)*r1 as u64 + (h2 as u64)*r0 as u64 + (h3 as u64)*r4_5 as u64 + (h4 as u64)*r3_5 as u64;
            let d3 = (h0 as u64)*r3 as u64 + (h1 as u64)*r2 as u64 + (h2 as u64)*r1 as u64 + (h3 as u64)*r0 as u64 + (h4 as u64)*r4_5 as u64;
            let d4 = (h0 as u64)*r4 as u64 + (h1 as u64)*r3 as u64 + (h2 as u64)*r2 as u64 + (h3 as u64)*r1 as u64 + (h4 as u64)*r0 as u64;

            let mut c = d0 >> 26; h0 = (d0 as u32) & 0x3ffffff;
            let d1 = d1 + c;
            let mut c = d1 >> 26; h1 = (d1 as u32) & 0x3ffffff;
            let d2 = d2 + c;
            let mut c = d2 >> 26; h2 = (d2 as u32) & 0x3ffffff;
            let d3 = d3 + c;
            let mut c = d3 >> 26; h3 = (d3 as u32) & 0x3ffffff;
            let d4 = d4 + c;
            let c = d4 >> 26; h4 = (d4 as u32) & 0x3ffffff;
            h0 = h0.wrapping_add((c as u32).wrapping_mul(5));
            let c2 = h0 >> 26; h0 &= 0x3ffffff; h1 = h1.wrapping_add(c2);

            i += 16;
        }

        // Final reduction mod 2^130-5.
        let c = h1 >> 26; h1 &= 0x3ffffff; h2 = h2.wrapping_add(c);
        let c = h2 >> 26; h2 &= 0x3ffffff; h3 = h3.wrapping_add(c);
        let c = h3 >> 26; h3 &= 0x3ffffff; h4 = h4.wrapping_add(c);
        let c = h4 >> 26; h4 &= 0x3ffffff; h0 = h0.wrapping_add(c.wrapping_mul(5));
        let c = h0 >> 26; h0 &= 0x3ffffff; h1 = h1.wrapping_add(c);

        // Compute h + (-p).
        let mut g0 = h0.wrapping_add(5);
        let c = g0 >> 26; g0 &= 0x3ffffff;
        let mut g1 = h1.wrapping_add(c);
        let c = g1 >> 26; g1 &= 0x3ffffff;
        let mut g2 = h2.wrapping_add(c);
        let c = g2 >> 26; g2 &= 0x3ffffff;
        let mut g3 = h3.wrapping_add(c);
        let c = g3 >> 26; g3 &= 0x3ffffff;
        let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

        // Select h if h < p, else h + (-p).
        let mask = (g4 >> 31).wrapping_sub(1);
        h0 = (h0 & !mask) | (g0 & mask);
        h1 = (h1 & !mask) | (g1 & mask);
        h2 = (h2 & !mask) | (g2 & mask);
        h3 = (h3 & !mask) | (g3 & mask);
        h4 = (h4 & !mask) | (g4 & mask);

        // h = h % (2^128).
        let h = ((h0 as u128) | ((h1 as u128)<<26) | ((h2 as u128)<<52) | ((h3 as u128)<<78) | ((h4 as u128)<<104))
              + u128::from_le_bytes(s.try_into().unwrap());
        let mut out = [0u8; 16];
        out.copy_from_slice(&(h as u128).to_le_bytes()[..16]);
        out
    }
}

// ════════════════════════════════════════════════════════════════════
// Inline crypto: X25519 (Curve25519 scalar multiplication)
// ════════════════════════════════════════════════════════════════════

/// X25519 Diffie-Hellman using TweetNaCl-style 16-limb i64 field arithmetic.
/// GF(2^255-19): elements represented as 16 i64 limbs in radix 2^16.
mod x25519 {
    type Gf = [i64; 16];

    const fn gf0() -> Gf { [0i64; 16] }
    const fn gf1() -> Gf { let mut g = [0i64; 16]; g[0] = 1; g }

    fn pack(r: &mut [u8; 32], x: &Gf) {
        let mut t = *x;
        car(&mut t);
        car(&mut t);
        car(&mut t);
        // Conditional subtract p.
        for _ in 0..2 {
            let mut m = t[15] >> 63; // -1 if negative
            let mut b: i64 = t[0].wrapping_sub(0xffed & m);
            t[0] = b & 0xffff;
            for i in 1..15 {
                b = (b >> 16) + t[i].wrapping_sub(0xffff & m);
                t[i] = b & 0xffff;
            }
            b = (b >> 16) + t[15].wrapping_sub(0x7fff & m);
            t[15] = b & 0xffff;
            m = !m;
        }
        for i in 0..16 {
            r[2*i]   = (t[i] & 0xff) as u8;
            r[2*i+1] = (t[i] >> 8)   as u8;
        }
    }

    fn unpack(r: &mut Gf, b: &[u8; 32]) {
        for i in 0..16 {
            r[i] = b[2*i] as i64 | ((b[2*i+1] as i64) << 8);
        }
        r[15] &= 0x7fff;
    }

    fn car(o: &mut Gf) {
        for i in 0..16 {
            o[i] += 1 << 16;
            let c = o[i] >> 16;
            if i < 15 { o[i+1] += c - 1; } else { o[0] += 38 * (c - 1); }
            o[i] -= c << 16;
        }
    }

    fn add(o: &mut Gf, a: &Gf, b: &Gf) {
        for i in 0..16 { o[i] = a[i] + b[i]; }
    }
    fn sub(o: &mut Gf, a: &Gf, b: &Gf) {
        for i in 0..16 { o[i] = a[i] - b[i]; }
    }
    fn mul(o: &mut Gf, a: &Gf, b: &Gf) {
        let mut t = [0i64; 31];
        for i in 0..16 { for j in 0..16 { t[i+j] += a[i] * b[j]; } }
        for i in 0..15 { t[i] += 38 * t[i+16]; }
        o[..16].copy_from_slice(&t[..16]);
        car(o); car(o);
    }
    fn sqr(o: &mut Gf, a: &Gf) {
        let t = *a;
        mul(o, &t, &t);
    }

    fn sel(p: &mut Gf, q: &mut Gf, b: i64) {
        let mask = -(b as i64);
        for i in 0..16 {
            let t = mask & (p[i] ^ q[i]);
            p[i] ^= t;
            q[i] ^= t;
        }
    }

    fn inv(o: &mut Gf, inp: &Gf) {
        let mut c = *inp;
        let mut t = gf0();
        for a in (0..254i32).rev() {
            let tc = c;
            sqr(&mut t, &tc);
            if a != 2 && a != 4 {
                let tt = t;
                mul(&mut c, &tt, inp);
            } else {
                c = t;
            }
        }
        *o = c;
    }

    /// Montgomery ladder scalar multiplication.
    pub fn scalarmult(q: &mut [u8; 32], n: &[u8; 32], p: &[u8; 32]) {
        let mut z = [0u8; 32];
        z.copy_from_slice(n);
        z[0]  &= 248;
        z[31] &= 127;
        z[31] |= 64;

        let mut x = gf0();
        unpack(&mut x, p);

        let mut a = gf1();
        let mut b = x;
        let mut c = gf0();
        let mut d = gf1();
        let mut e = gf0(); let mut f = gf0();

        for i in (0..255).rev() {
            let r = ((z[i >> 3] >> (i & 7)) & 1) as i64;
            sel(&mut a, &mut b, r);
            sel(&mut c, &mut d, r);
            // e = a + c
            let ta = a; let tc = c;
            add(&mut e, &ta, &tc);
            // a = a - c
            sub(&mut a, &ta, &tc);
            // c = b + d
            let tb = b; let td = d;
            add(&mut c, &tb, &td);
            // b = b - d
            sub(&mut b, &tb, &td);
            let mut t1 = gf0(); let mut t2 = gf0();
            // d = e^2
            let te = e;
            mul(&mut d, &te, &te);
            // t1 = a^2
            let ta2 = a;
            mul(&mut t1, &ta2, &ta2);
            // a = c * a  (both c and a are independent now)
            let tc2 = c; let ta3 = a;
            mul(&mut a, &tc2, &ta3);
            // c = b * e
            let tb2 = b; let te2 = e;
            mul(&mut c, &tb2, &te2);
            // e = a + c
            let ta4 = a; let tc3 = c;
            add(&mut e, &ta4, &tc3);
            // a = a - c
            sub(&mut a, &ta4, &tc3);
            // b = a^2
            let ta5 = a;
            mul(&mut b, &ta5, &ta5);
            // c = d - t1
            let td2 = d; let tt1 = t1;
            sub(&mut c, &td2, &tt1);
            // a24 = 121665
            let mut a24 = gf0(); a24[0] = 121665;
            // t2 = c * 121665
            let tc4 = c;
            mul(&mut t2, &tc4, &a24);
            // a = d + t2
            let td3 = d; let tt2 = t2;
            add(&mut a, &td3, &tt2);
            // c = c * a
            let tc5 = c; let ta6 = a;
            mul(&mut c, &tc5, &ta6);
            // a = d * t1
            let td4 = d; let tt1b = t1;
            mul(&mut a, &td4, &tt1b);
            // d = e^2
            let te3 = e;
            mul(&mut d, &te3, &te3);
            // f is unused but needed for inversion later; use t3 for b*x
            let tb3 = b; let tx = x;
            let mut t3 = gf0();
            mul(&mut t3, &tb3, &tx);
            b = t3;
            sel(&mut a, &mut b, r);
            sel(&mut c, &mut d, r);
        }
        inv(&mut f, &d);
        let ta = a; let tf = f;
        mul(&mut e, &ta, &tf);
        let mut tmp = [0u8; 32];
        pack(&mut tmp, &e);
        q.copy_from_slice(&tmp);
    }

    /// Basepoint for Curve25519: u=9.
    pub const BASEPOINT: [u8; 32] = {
        let mut bp = [0u8; 32];
        bp[0] = 9;
        bp
    };

    pub fn pubkey(privkey: &[u8; 32]) -> [u8; 32] {
        let mut out = [0u8; 32];
        scalarmult(&mut out, privkey, &BASEPOINT);
        out
    }

    pub fn shared_secret(privkey: &[u8; 32], peer_pubkey: &[u8; 32]) -> [u8; 32] {
        let mut out = [0u8; 32];
        scalarmult(&mut out, privkey, peer_pubkey);
        out
    }
}

// ════════════════════════════════════════════════════════════════════
// Inline crypto: SHA-512
// ════════════════════════════════════════════════════════════════════

mod sha512 {
    const K: [u64; 80] = [
        0x428a2f98d728ae22,0x7137449123ef65cd,0xb5c0fbcfec4d3b2f,0xe9b5dba58189dbbc,
        0x3956c25bf348b538,0x59f111f1b605d019,0x923f82a4af194f9b,0xab1c5ed5da6d8118,
        0xd807aa98a3030242,0x12835b0145706fbe,0x243185be4ee4b28c,0x550c7dc3d5ffb4e2,
        0x72be5d74f27b896f,0x80deb1fe3b1696b1,0x9bdc06a725c71235,0xc19bf174cf692694,
        0xe49b69c19ef14ad2,0xefbe4786384f25e3,0x0fc19dc68b8cd5b5,0x240ca1cc77ac9c65,
        0x2de92c6f592b0275,0x4a7484aa6ea6e483,0x5cb0a9dcbd41fbd4,0x76f988da831153b5,
        0x983e5152ee66dfab,0xa831c66d2db43210,0xb00327c898fb213f,0xbf597fc7beef0ee4,
        0xc6e00bf33da88fc2,0xd5a79147930aa725,0x06ca6351e003826f,0x142929670a0e6e70,
        0x27b70a8546d22ffc,0x2e1b21385c26c926,0x4d2c6dfc5ac42aed,0x53380d139d95b3df,
        0x650a73548baf63de,0x766a0abb3c77b2a8,0x81c2c92e47edaee6,0x92722c851482353b,
        0xa2bfe8a14cf10364,0xa81a664bbc423001,0xc24b8b70d0f89791,0xc76c51a30654be30,
        0xd192e819d6ef5218,0xd69906245565a910,0xf40e35855771202a,0x106aa07032bbd1b8,
        0x19a4c116b8d2d0c8,0x1e376c085141ab53,0x2748774cdf8eeb99,0x34b0bcb5e19b48a8,
        0x391c0cb3c5c95a63,0x4ed8aa4ae3418acb,0x5b9cca4f7763e373,0x682e6ff3d6b2b8a3,
        0x748f82ee5defb2fc,0x78a5636f43172f60,0x84c87814a1f0ab72,0x8cc702081a6439ec,
        0x90befffa23631e28,0xa4506cebde82bde9,0xbef9a3f7b2c67915,0xc67178f2e372532b,
        0xca273eceea26619c,0xd186b8c721c0c207,0xeada7dd6cde0eb1e,0xf57d4f7fee6ed178,
        0x06f067aa72176fba,0x0a637dc5a2c898a6,0x113f9804bef90dae,0x1b710b35131c471b,
        0x28db77f523047d84,0x32caab7b40c72493,0x3c9ebe0a15c9bebc,0x431d67c49c100d4c,
        0x4cc5d4becb3e42b6,0x597f299cfc657e2a,0x5fcb6fab3ad6faec,0x6c44198c4a475817,
    ];

    fn compress(st: &mut [u64; 8], blk: &[u8; 128]) {
        let mut w = [0u64; 80];
        for i in 0..16 { w[i] = u64::from_be_bytes(blk[i*8..i*8+8].try_into().unwrap()); }
        for i in 16..80 {
            let s0 = w[i-15].rotate_right(1) ^ w[i-15].rotate_right(8) ^ (w[i-15] >> 7);
            let s1 = w[i-2].rotate_right(19) ^ w[i-2].rotate_right(61) ^ (w[i-2] >> 6);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let [mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut h] = *st;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h=g; g=f; f=e; e=d.wrapping_add(t1); d=c; c=b; b=a; a=t1.wrapping_add(t2);
        }
        st[0]=st[0].wrapping_add(a); st[1]=st[1].wrapping_add(b);
        st[2]=st[2].wrapping_add(c); st[3]=st[3].wrapping_add(d);
        st[4]=st[4].wrapping_add(e); st[5]=st[5].wrapping_add(f);
        st[6]=st[6].wrapping_add(g); st[7]=st[7].wrapping_add(h);
    }

    pub fn hash(data: &[u8]) -> [u8; 64] {
        let mut st: [u64; 8] = [
            0x6a09e667f3bcc908,0xbb67ae8584caa73b,
            0x3c6ef372fe94f82b,0xa54ff53a5f1d36f1,
            0x510e527fade682d1,0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b,0x5be0cd19137e2179,
        ];
        let full = data.len() / 128;
        for i in 0..full { compress(&mut st, data[i*128..i*128+128].try_into().unwrap()); }
        let tail = &data[full*128..];
        let mut buf = [0u8; 256];
        buf[..tail.len()].copy_from_slice(tail);
        buf[tail.len()] = 0x80;
        let pad_end = if tail.len() < 112 { 128 } else { 256 };
        let bits = (data.len() as u128) * 8;
        buf[pad_end-16..pad_end-8].copy_from_slice(&((bits >> 64) as u64).to_be_bytes());
        buf[pad_end-8..pad_end].copy_from_slice(&(bits as u64).to_be_bytes());
        compress(&mut st, buf[..128].try_into().unwrap());
        if pad_end == 256 { compress(&mut st, buf[128..256].try_into().unwrap()); }
        let mut out = [0u8; 64];
        for (i, w) in st.iter().enumerate() { out[i*8..i*8+8].copy_from_slice(&w.to_be_bytes()); }
        out
    }

    /// Hash `prefix || msg` without heap allocation; prefix ≤ 64 bytes, total ≤ 4096 bytes.
    pub fn hash2(prefix: &[u8], msg: &[u8]) -> [u8; 64] {
        let mut buf = [0u8; 4096];
        let end = prefix.len() + msg.len();
        if end > 4096 { return [0u8; 64]; }
        buf[..prefix.len()].copy_from_slice(prefix);
        buf[prefix.len()..end].copy_from_slice(msg);
        hash(&buf[..end])
    }

    /// Hash `a || b || c`; total ≤ 4096 bytes.
    pub fn hash3(a: &[u8], b: &[u8], c: &[u8]) -> [u8; 64] {
        let mut buf = [0u8; 4096];
        let e1 = a.len(); let e2 = e1 + b.len(); let e3 = e2 + c.len();
        if e3 > 4096 { return [0u8; 64]; }
        buf[..e1].copy_from_slice(a); buf[e1..e2].copy_from_slice(b); buf[e2..e3].copy_from_slice(c);
        hash(&buf[..e3])
    }
}

// ════════════════════════════════════════════════════════════════════
// Inline crypto: Ed25519 — RFC 8032 §5.1.6 (sign only)
// Field arithmetic: TweetNaCl-style 16-limb i64, radix 2^16, GF(2^255-19).
// Faithfully translated from TweetNaCl (public domain) to Rust.
// ════════════════════════════════════════════════════════════════════

mod ed25519 {
    type Gf = [i64; 16];
    const GF0: Gf = [0i64; 16];
    const GF1: Gf = { let mut g = [0i64; 16]; g[0] = 1; g };

    // d = -121665/121666 mod p  (TweetNaCl constant)
    const D: Gf = [0xd34a,0x8f25,0x9a9f,0xa498,0x3fad,0x25d2,0x6a3a,0x4b89,
                   0x4d80,0x5fb0,0x12c0,0xb8b6,0xbf0a,0x4b8f,0xf7e9,0x5203];
    // 2*d
    const D2: Gf = [0xa8e4,0x1e4a,0x345f,0x4930,0x7fa4,0x4ab4,0xd474,0x972f,
                    0x9a00,0xbf60,0x2480,0x716c,0x7e14,0x3f1f,0xeff2,0x2406];
    // Base point affine coordinates
    const BX: Gf = [0xd51a,0x8f25,0x2d60,0xc956,0xa7b2,0x9525,0xc760,0x692c,
                    0xdc5c,0xfdd6,0xe231,0xc0a4,0x53fe,0xcd6e,0x36d3,0x2169];
    const BY: Gf = [0x6658,0x6666,0x6666,0x6666,0x6666,0x6666,0x6666,0x6666,
                    0x6666,0x6666,0x6666,0x6666,0x6666,0x6666,0x6666,0x6666];
    // L = group order, byte-per-element little-endian
    const L: [i64; 32] = [
        0xed,0xd3,0xf5,0x5c,0x1a,0x63,0x12,0x58,
        0xd6,0x9c,0xf7,0xa2,0xde,0xf9,0xde,0x14,
        0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0x10,
    ];

    // ── GF(2^255-19) field arithmetic ────────────────────────────────────────

    fn car(o: &mut Gf) {
        for i in 0..16 {
            o[i] += 1 << 16;
            let c = o[i] >> 16;
            if i < 15 { o[i+1] += c - 1; } else { o[0] += 38 * (c - 1); }
            o[i] -= c << 16;
        }
    }
    fn fadd(o: &mut Gf, a: &Gf, b: &Gf) { for i in 0..16 { o[i] = a[i] + b[i]; } }
    fn fsub(o: &mut Gf, a: &Gf, b: &Gf) { for i in 0..16 { o[i] = a[i] - b[i]; } }
    fn fmul(o: &mut Gf, a: &Gf, b: &Gf) {
        let mut t = [0i64; 31];
        for i in 0..16 { for j in 0..16 { t[i+j] += a[i] * b[j]; } }
        for i in 0..15 { t[i] += 38 * t[i+16]; }
        o[..16].copy_from_slice(&t[..16]);
        car(o); car(o);
    }
    fn finv(o: &mut Gf, inp: &Gf) {
        // Fermat: inp^(p-2) where p = 2^255-19 and the exponent computes to
        // the standard addition-chain used in TweetNaCl.
        let mut c = *inp;
        for a in (0i32..=253).rev() {
            let tc = c;
            let mut sq = GF0;
            fmul(&mut sq, &tc, &tc);
            if a != 2 && a != 4 { let tsq = sq; fmul(&mut c, &tsq, inp); } else { c = sq; }
        }
        *o = c;
    }
    fn fsel(p: &mut Gf, q: &mut Gf, b: i64) {
        let mask = -(b as i64);
        for i in 0..16 { let t = mask & (p[i] ^ q[i]); p[i] ^= t; q[i] ^= t; }
    }
    fn pack25519(r: &mut [u8; 32], x: &Gf) {
        let mut t = *x;
        car(&mut t); car(&mut t); car(&mut t);
        for _ in 0..2 {
            // Subtract p = 2^255-19 and select whichever is in [0, p).
            let mut m = GF0;
            m[0] = t[0] - 0xffed;
            for i in 1..15 { m[i] = t[i] - 0xffff - ((m[i-1] >> 16) & 1); m[i-1] &= 0xffff; }
            m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
            let b = (m[15] >> 16) & 1; // 1 ↔ t < p (keep t); 0 ↔ t ≥ p (use m)
            m[14] &= 0xffff;
            fsel(&mut t, &mut m, 1 - b);
        }
        for i in 0..16 { r[2*i] = (t[i] & 0xff) as u8; r[2*i+1] = (t[i] >> 8) as u8; }
    }
    fn par25519(a: &Gf) -> i64 { let mut d = [0u8; 32]; pack25519(&mut d, a); (d[0] & 1) as i64 }

    // ── Edwards curve point operations (extended coordinates [X:Y:Z:T]) ──────

    /// Unified point addition: p += q  (p and q may alias via copy)
    fn add_point(p: &mut [Gf; 4], q: &[Gf; 4]) {
        let mut a = GF0; let mut b = GF0; let mut c = GF0; let mut d = GF0;
        let mut e = GF0; let mut f = GF0; let mut g = GF0; let mut h = GF0;
        let mut t = GF0;
        // a = (Y1-X1)*(Y2-X2)
        fsub(&mut a, &p[1], &p[0]); fsub(&mut t, &q[1], &q[0]);
        let ta = a; let tt = t; fmul(&mut a, &ta, &tt);
        // b = (Y1+X1)*(Y2+X2)
        fadd(&mut b, &p[0], &p[1]); fadd(&mut t, &q[0], &q[1]);
        let tb = b; let tt = t; fmul(&mut b, &tb, &tt);
        // c = T1 * 2d * T2
        fmul(&mut c, &p[3], &q[3]); let tc = c; fmul(&mut c, &tc, &D2);
        // d = 2 * Z1 * Z2
        fmul(&mut d, &p[2], &q[2]); let td = d; fadd(&mut d, &td, &td);
        // e,f,g,h
        fsub(&mut e, &b, &a); fsub(&mut f, &d, &c);
        fadd(&mut g, &d, &c); fadd(&mut h, &b, &a);
        fmul(&mut p[0], &e, &f); fmul(&mut p[1], &g, &h);
        fmul(&mut p[2], &f, &g); fmul(&mut p[3], &e, &h);
    }

    fn cswap(p: &mut [Gf; 4], q: &mut [Gf; 4], b: i64) {
        for i in 0..4 { fsel(&mut p[i], &mut q[i], b); }
    }

    /// Constant-time scalar multiplication: r = s * q
    fn scalarmult(p: &mut [Gf; 4], q: &mut [Gf; 4], s: &[u8; 32]) {
        p[0] = GF0; p[1] = GF1; p[2] = GF1; p[3] = GF0; // identity
        for i in (0..256).rev() {
            let b = ((s[i / 8] >> (i & 7)) & 1) as i64;
            cswap(p, q, b);
            let p_copy = *p;
            add_point(q, &p_copy);
            let p_copy2 = *p;
            add_point(p, &p_copy2); // double
            cswap(p, q, b);
        }
    }

    /// r = s * B  (scalar multiply by the base point)
    fn scalarbase(p: &mut [Gf; 4], s: &[u8; 32]) {
        let mut q: [Gf; 4] = [GF0; 4];
        q[0] = BX; q[1] = BY; q[2] = GF1;
        fmul(&mut q[3], &BX, &BY);
        scalarmult(p, &mut q, s);
    }

    /// Compress an extended point to 32 bytes (RFC 8032 §5.1.2).
    fn pack_point(r: &mut [u8; 32], p: &[Gf; 4]) {
        let mut zi = GF0; let mut tx = GF0; let mut ty = GF0;
        finv(&mut zi, &p[2]);
        fmul(&mut tx, &p[0], &zi);
        fmul(&mut ty, &p[1], &zi);
        pack25519(r, &ty);
        r[31] ^= (par25519(&tx) as u8) << 7;
    }

    // ── Scalar arithmetic mod L ───────────────────────────────────────────────

    /// Reduce x[0..64] mod L, writing result to r[0..32].
    /// Faithful translation of TweetNaCl `modL`.
    fn modl(r: &mut [u8; 32], x: &mut [i64; 64]) {
        let mut i = 63i32;
        while i >= 32 {
            let mut carry = 0i64;
            let base = (i - 32) as usize;
            let lim  = (i - 12) as usize;
            let xi   = x[i as usize];
            for j in base..lim {
                x[j] += carry - 16 * xi * L[j - base];
                carry = (x[j] + 128) >> 8;
                x[j] -= carry << 8;
            }
            x[lim] += carry;
            x[i as usize] = 0;
            i -= 1;
        }
        let mut carry = 0i64;
        for j in 0..32usize {
            x[j] += carry - (x[31] >> 4) * L[j];
            carry = x[j] >> 8;
            x[j] &= 255;
        }
        for j in 0..32usize { x[j] -= carry * L[j]; }
        for i in 0..32usize { x[i+1] += x[i] >> 8; r[i] = (x[i] & 255) as u8; }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// RFC 8032 §5.1.6 — sign `msg` with 32-byte Ed25519 private key seed.
    pub fn sign(privkey: &[u8; 32], pubkey: &[u8; 32], msg: &[u8]) -> [u8; 64] {
        // 1. Expand seed: d = SHA-512(seed); clamp first half → scalar a
        let d = super::sha512::hash(privkey);
        let mut a = [0u8; 32];
        a.copy_from_slice(&d[..32]);
        a[0] &= 248; a[31] &= 63; a[31] |= 64;

        // 2. Nonce: r = SHA-512(d[32..64] || msg) mod L
        let r_hash = super::sha512::hash2(&d[32..64], msg);
        let mut r = [0u8; 32];
        { let mut x = [0i64; 64]; for i in 0..64 { x[i] = r_hash[i] as i64; } modl(&mut r, &mut x); }

        // 3. R = r * BasePoint, compressed to 32 bytes
        let mut p: [Gf; 4] = [GF0; 4];
        scalarbase(&mut p, &r);
        let mut big_r = [0u8; 32];
        pack_point(&mut big_r, &p);

        // 4. Challenge: h = SHA-512(R || pubkey || msg) mod L
        let h_hash = super::sha512::hash3(&big_r, pubkey, msg);
        let mut h = [0u8; 32];
        { let mut x = [0i64; 64]; for i in 0..64 { x[i] = h_hash[i] as i64; } modl(&mut h, &mut x); }

        // 5. s = (r + h * a) mod L
        let mut x = [0i64; 64];
        for i in 0..32 { x[i] = r[i] as i64; }
        for i in 0..32 { for j in 0..32 { x[i+j] += h[i] as i64 * a[j] as i64; } }
        let mut s = [0u8; 32];
        modl(&mut s, &mut x);

        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&big_r);
        sig[32..].copy_from_slice(&s);
        sig
    }
}

// ════════════════════════════════════════════════════════════════════
// SSH packet framing helpers
// ════════════════════════════════════════════════════════════════════

struct Conn {
    sock: runtime::SocketHandle,
    /// Sending sequence number (for ChaCha20-Poly1305 nonce).
    tx_seq: u32,
    /// Receiving sequence number.
    rx_seq: u32,
    /// ChaCha20 key for sending (32 bytes).
    tx_key: [u8; 32],
    /// ChaCha20 key for receiving (32 bytes).
    rx_key: [u8; 32],
    encrypted: bool,
}

impl Conn {
    fn new(sock: runtime::SocketHandle) -> Self {
        Self {
            sock,
            tx_seq: 0,
            rx_seq: 0,
            tx_key: [0; 32],
            rx_key: [0; 32],
            encrypted: false,
        }
    }

    /// Send raw bytes (pre-framed).
    fn send_raw(&mut self, data: &[u8]) -> bool {
        let mut off = 0;
        while off < data.len() {
            match runtime::socket_send(&self.sock, &data[off..]) {
                Some(n) if n > 0 => off += n,
                _ => return false,
            }
        }
        true
    }

    /// Read exactly `n` bytes.
    fn recv_exact(&mut self, out: &mut [u8]) -> bool {
        let mut off = 0;
        let mut retries = 0u32;
        while off < out.len() {
            match runtime::socket_recv(&self.sock, &mut out[off..]) {
                Some(n) if n > 0 => { off += n; retries = 0; }
                Some(0) => {
                    retries += 1;
                    if retries > 200_000 { return false; }
                    runtime::yield_now();
                }
                _ => return false,
            }
        }
        true
    }

    /// Send an SSH packet (plaintext before NEWKEYS, encrypted after).
    fn send_packet(&mut self, payload: &[u8]) -> bool {
        if self.encrypted {
            return self.send_packet_enc(payload);
        }

        // packet_length(4) + padding_length(1) + payload + padding
        let padding = {
            let base = 5 + payload.len();
            let pad = 8 - (base % 8);
            if pad < 4 { pad + 8 } else { pad }
        };
        let packet_len = 1 + payload.len() + padding;
        let mut hdr = [0u8; 5];
        hdr[0..4].copy_from_slice(&(packet_len as u32).to_be_bytes());
        hdr[4] = padding as u8;
        if !self.send_raw(&hdr) { return false; }
        if !self.send_raw(payload) { return false; }
        let pad_bytes = [0u8; 255];
        if !self.send_raw(&pad_bytes[..padding]) { return false; }
        // No MAC in unencrypted phase.
        self.tx_seq = self.tx_seq.wrapping_add(1);
        true
    }

    fn recv_packet_plain(&mut self, out: &mut [u8; MAX_PACKET]) -> Option<usize> {
        let mut len_buf = [0u8; 4];
        if !self.recv_exact(&mut len_buf) { return None; }
        let packet_len = u32::from_be_bytes(len_buf) as usize;
        if packet_len == 0 || packet_len > MAX_PACKET - 4 { return None; }
        let mut body = [0u8; MAX_PACKET];
        if !self.recv_exact(&mut body[..packet_len]) { return None; }
        let padding = body[0] as usize;
        if packet_len < 1 + padding { return None; }
        let payload_len = packet_len - 1 - padding;
        if payload_len > out.len() { return None; }
        out[..payload_len].copy_from_slice(&body[1..1+payload_len]);
        self.rx_seq = self.rx_seq.wrapping_add(1);
        Some(payload_len)
    }

    fn recv_packet_enc(&mut self, out: &mut [u8; MAX_PACKET]) -> Option<usize> {
        let mut c_len = [0u8; 4];
        if !self.recv_exact(&mut c_len) { return None; }

        let mut nonce = [0u8; 12];
        nonce[8..12].copy_from_slice(&self.rx_seq.to_be_bytes());

        // Decrypt the encrypted packet length field.
        let mut len_plain = c_len;
        chacha20::xor(&self.rx_key, 0, &nonce, &mut len_plain);
        let packet_len = u32::from_be_bytes(len_plain) as usize;
        if packet_len == 0 || packet_len > MAX_PACKET - 4 { return None; }

        // Read encrypted body and authentication tag.
        let mut c_body = [0u8; MAX_PACKET];
        if !self.recv_exact(&mut c_body[..packet_len]) { return None; }
        let mut recv_tag = [0u8; 16];
        if !self.recv_exact(&mut recv_tag) { return None; }

        // Verify Poly1305 tag over ciphertext: enc_len || enc_body.
        let poly_key_block = chacha20::block(&self.rx_key, 0, &nonce);
        let mut poly_key = [0u8; 32];
        poly_key.copy_from_slice(&poly_key_block[..32]);
        let mut mac_in = [0u8; MAX_PACKET + 4];
        mac_in[..4].copy_from_slice(&c_len);
        mac_in[4..4+packet_len].copy_from_slice(&c_body[..packet_len]);
        let calc_tag = poly1305::mac(&poly_key, &mac_in[..4 + packet_len]);
        if !ct_eq_16(&calc_tag, &recv_tag) { return None; }

        // Decrypt packet body.
        let mut body = [0u8; MAX_PACKET];
        body[..packet_len].copy_from_slice(&c_body[..packet_len]);
        chacha20::xor(&self.rx_key, 1, &nonce, &mut body[..packet_len]);

        let padding = body[0] as usize;
        if packet_len < 1 + padding { return None; }
        let payload_len = packet_len - 1 - padding;
        if payload_len > out.len() { return None; }
        out[..payload_len].copy_from_slice(&body[1..1+payload_len]);
        self.rx_seq = self.rx_seq.wrapping_add(1);
        Some(payload_len)
    }

    /// Send a ChaCha20-Poly1305 encrypted packet.
    fn send_packet_enc(&mut self, payload: &[u8]) -> bool {
        let padding = {
            let base = 5 + payload.len();
            let pad = 8 - (base % 8);
            if pad < 4 { pad + 8 } else { pad }
        };
        let packet_len = 1 + payload.len() + padding;

        // Build plaintext.
        let mut pt = [0u8; MAX_PACKET];
        let pt_len = 4 + 1 + payload.len() + padding;
        pt[0..4].copy_from_slice(&(packet_len as u32).to_be_bytes());
        pt[4] = padding as u8;
        pt[5..5+payload.len()].copy_from_slice(payload);
        // Padding bytes stay 0.

        // ChaCha20-Poly1305: nonce is seq counter.
        let mut nonce = [0u8; 12];
        nonce[8..12].copy_from_slice(&self.tx_seq.to_be_bytes());

        // Encrypt length field separately with counter=0, body with counter=1.
        let mut ct = [0u8; MAX_PACKET + 16];
        ct[..pt_len].copy_from_slice(&pt[..pt_len]);
        chacha20::xor(&self.tx_key, 0, &nonce, &mut ct[..4]);
        chacha20::xor(&self.tx_key, 1, &nonce, &mut ct[4..pt_len]);

        // Poly1305 key = first 32 bytes of ChaCha20(key, ctr=0).
        let poly_key_block = chacha20::block(&self.tx_key, 0, &nonce);
        let mut poly_key = [0u8; 32];
        poly_key.copy_from_slice(&poly_key_block[..32]);
        let mac = poly1305::mac(&poly_key, &ct[..pt_len]);
        ct[pt_len..pt_len+16].copy_from_slice(&mac);

        if !self.send_raw(&ct[..pt_len+16]) { return false; }
        self.tx_seq = self.tx_seq.wrapping_add(1);
        true
    }

    /// Receive one SSH packet payload into `out`. Returns payload length.
    fn recv_packet(&mut self, out: &mut [u8; MAX_PACKET]) -> Option<usize> {
        if self.encrypted {
            self.recv_packet_enc(out)
        } else {
            self.recv_packet_plain(out)
        }
    }

    /// Non-blocking variant: returns `None` immediately if no data is available.
    fn recv_packet_nonblock(&mut self, out: &mut [u8; MAX_PACKET]) -> Option<usize> {
        // Probe for the 4-byte packet-length field.
        let mut first4 = [0u8; 4];
        match runtime::socket_recv(&self.sock, &mut first4) {
            Some(4) => {}
            Some(0) | None => return None,
            Some(n) => {
                if !self.recv_exact(&mut first4[n..4]) { return None; }
            }
        }

        if self.encrypted {
            let mut nonce = [0u8; 12];
            nonce[8..12].copy_from_slice(&self.rx_seq.to_be_bytes());

            let mut len_plain = first4;
            chacha20::xor(&self.rx_key, 0, &nonce, &mut len_plain);
            let packet_len = u32::from_be_bytes(len_plain) as usize;
            if packet_len == 0 || packet_len > MAX_PACKET - 4 { return None; }

            let mut c_body = [0u8; MAX_PACKET];
            if !self.recv_exact(&mut c_body[..packet_len]) { return None; }
            let mut recv_tag = [0u8; 16];
            if !self.recv_exact(&mut recv_tag) { return None; }

            let poly_key_block = chacha20::block(&self.rx_key, 0, &nonce);
            let mut poly_key = [0u8; 32];
            poly_key.copy_from_slice(&poly_key_block[..32]);
            let mut mac_in = [0u8; MAX_PACKET + 4];
            mac_in[..4].copy_from_slice(&first4);
            mac_in[4..4+packet_len].copy_from_slice(&c_body[..packet_len]);
            let calc_tag = poly1305::mac(&poly_key, &mac_in[..4 + packet_len]);
            if !ct_eq_16(&calc_tag, &recv_tag) { return None; }

            let mut body = [0u8; MAX_PACKET];
            body[..packet_len].copy_from_slice(&c_body[..packet_len]);
            chacha20::xor(&self.rx_key, 1, &nonce, &mut body[..packet_len]);
            let padding = body[0] as usize;
            if packet_len < 1 + padding { return None; }
            let payload_len = packet_len - 1 - padding;
            if payload_len > out.len() { return None; }
            out[..payload_len].copy_from_slice(&body[1..1+payload_len]);
            self.rx_seq = self.rx_seq.wrapping_add(1);
            Some(payload_len)
        } else {
            let packet_len = u32::from_be_bytes(first4) as usize;
            if packet_len == 0 || packet_len > MAX_PACKET - 4 { return None; }
            let mut body = [0u8; MAX_PACKET];
            if !self.recv_exact(&mut body[..packet_len]) { return None; }
            let padding = body[0] as usize;
            if packet_len < 1 + padding { return None; }
            let payload_len = packet_len - 1 - padding;
            if payload_len > out.len() { return None; }
            out[..payload_len].copy_from_slice(&body[1..1+payload_len]);
            self.rx_seq = self.rx_seq.wrapping_add(1);
            Some(payload_len)
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// SSH wire format helpers
// ════════════════════════════════════════════════════════════════════

fn put_u32(buf: &mut [u8], off: &mut usize, v: u32) {
    buf[*off..*off+4].copy_from_slice(&v.to_be_bytes()); *off += 4;
}
fn put_bytes(buf: &mut [u8], off: &mut usize, b: &[u8]) {
    buf[*off..*off+b.len()].copy_from_slice(b); *off += b.len();
}
fn put_string(buf: &mut [u8], off: &mut usize, s: &[u8]) {
    put_u32(buf, off, s.len() as u32); put_bytes(buf, off, s);
}
fn get_u32(buf: &[u8], off: &mut usize) -> Option<u32> {
    if *off + 4 > buf.len() { return None; }
    let v = u32::from_be_bytes(buf[*off..*off+4].try_into().unwrap());
    *off += 4; Some(v)
}
fn get_string<'a>(buf: &'a [u8], off: &mut usize) -> Option<&'a [u8]> {
    let len = get_u32(buf, off)? as usize;
    if *off + len > buf.len() { return None; }
    let s = &buf[*off..*off+len]; *off += len; Some(s)
}

fn ct_eq_16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ════════════════════════════════════════════════════════════════════
// SSH2 protocol constants (message numbers)
// ════════════════════════════════════════════════════════════════════

const MSG_DISCONNECT: u8 = 1;
const MSG_SERVICE_REQUEST: u8 = 5;
const MSG_SERVICE_ACCEPT: u8 = 6;
const MSG_KEXINIT: u8 = 20;
const MSG_NEWKEYS: u8 = 21;
const MSG_KEX_ECDH_INIT: u8 = 30;
const MSG_KEX_ECDH_REPLY: u8 = 31;
const MSG_USERAUTH_REQUEST: u8 = 50;
const MSG_USERAUTH_FAILURE: u8 = 51;
const MSG_USERAUTH_SUCCESS: u8 = 52;
const MSG_CHANNEL_OPEN: u8 = 90;
const MSG_CHANNEL_OPEN_CONFIRM: u8 = 91;
const MSG_CHANNEL_REQUEST: u8 = 98;
const MSG_CHANNEL_SUCCESS: u8 = 99;
const MSG_CHANNEL_DATA: u8 = 94;
const MSG_CHANNEL_EOF: u8 = 96;
const MSG_CHANNEL_CLOSE: u8 = 97;

// ════════════════════════════════════════════════════════════════════
// Graph-first event reporting to servicemgr overseer
// ════════════════════════════════════════════════════════════════════

fn report_session_event(event: &[u8]) {
    runtime::bootstrap_status(event);
}

struct SocketCloseGuard {
    sock: runtime::SocketHandle,
}

impl SocketCloseGuard {
    fn new(sock: runtime::SocketHandle) -> Self {
        Self { sock }
    }
}

impl Drop for SocketCloseGuard {
    fn drop(&mut self) {
        let _ = runtime::socket_close(&self.sock);
    }
}

// ════════════════════════════════════════════════════════════════════
// SSH connection handler
// ════════════════════════════════════════════════════════════════════

fn handle_connection(sock: runtime::SocketHandle) {
    let _socket_guard = SocketCloseGuard::new(sock);
    let mut conn = Conn::new(sock);
    runtime::write_line(b"[sshd] accepted client connection\n");

    // 1. Version exchange.
    if !conn.send_raw(SERVER_VERSION) {
        runtime::write_line(b"[sshd] failed to send server banner\n");
        return;
    }
    runtime::write_line(b"[sshd] server banner sent\n");
    let mut ver_buf = [0u8; 256];
    let mut ver_len = 0usize;
    loop {
        let mut b = [0u8; 1];
        if !conn.recv_exact(&mut b) { return; }
        if ver_len < ver_buf.len() { ver_buf[ver_len] = b[0]; ver_len += 1; }
        if ver_len >= 2 && ver_buf[ver_len-2] == b'\r' && ver_buf[ver_len-1] == b'\n' { break; }
        if ver_len >= 255 { return; }
    }
    // Require SSH-2.0.
    if !ver_buf[..ver_len].starts_with(b"SSH-2.0") { return; }

    // 2. KEXINIT exchange.
    let mut server_kexinit = [0u8; 512];
    let ski_len = build_kexinit(&mut server_kexinit);
    if !conn.send_packet(&server_kexinit[..ski_len]) { return; }

    let mut pkt = [0u8; MAX_PACKET];
    let pkt_len = match conn.recv_packet(&mut pkt) { Some(n) => n, None => return };
    if pkt_len < 1 || pkt[0] != MSG_KEXINIT { return; }
    // Copy the client KEXINIT into its own buffer before the next recv_packet call.
    let mut client_ki_buf = [0u8; 512];
    let cklen = pkt_len.min(512);
    client_ki_buf[..cklen].copy_from_slice(&pkt[..cklen]);
    let client_kexinit = &client_ki_buf[..cklen];

    // 3. ECDH (X25519) key exchange.
    // Receive ECDH init from client first.
    let pkt_len = match conn.recv_packet(&mut pkt) { Some(n) => n, None => return };
    if pkt_len < 5 || pkt[0] != MSG_KEX_ECDH_INIT { return; }
    let mut off = 1;
    let client_eph_pub = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return };
    if client_eph_pub.len() != 32 { return; }
    let mut client_pub32 = [0u8; 32];
    client_pub32.copy_from_slice(client_eph_pub);

    // Generate ephemeral server keypair with kernel-backed CSPRNG bytes.
    let server_eph_priv = match derive_ephemeral_key(&client_pub32) {
        Some(k) => k,
        None => {
            runtime::write_line(b"[sshd] failed to obtain CSPRNG bytes for ephemeral key\n");
            return;
        }
    };
    let server_eph_pub = x25519::pubkey(&server_eph_priv);

    // Compute shared secret K.
    let shared = x25519::shared_secret(&server_eph_priv, &client_pub32);

    // Build H (exchange hash) = SHA-256 of (V_C || V_S || I_C || I_S || K_S || Q_C || Q_S || K).
    let h = compute_exchange_hash(
        &ver_buf[..ver_len],
        SERVER_VERSION,
        client_kexinit,
        &server_kexinit[..ski_len],
        &host_pubkey(),
        &client_pub32,
        &server_eph_pub,
        &shared,
    );

    // Sign H with host key.
    let sig = ed25519::sign(&host_privkey(), &host_pubkey(), &h);

    // Build KEXECDH_REPLY.
    let mut reply = [0u8; 512];
    let mut roff = 0;
    reply[roff] = MSG_KEX_ECDH_REPLY; roff += 1;
    // K_S (host key blob: type + pubkey).
    let mut ks = [0u8; 52];
    let mut koff = 0;
    put_string(&mut ks, &mut koff, b"ssh-ed25519");
    put_string(&mut ks, &mut koff, &host_pubkey());
    put_string(&mut reply, &mut roff, &ks[..koff]);
    // Q_S (server ephemeral pubkey).
    put_string(&mut reply, &mut roff, &server_eph_pub);
    // Signature.
    let mut sig_blob = [0u8; 80];
    let mut soff = 0;
    put_string(&mut sig_blob, &mut soff, b"ssh-ed25519");
    put_string(&mut sig_blob, &mut soff, &sig);
    put_string(&mut reply, &mut roff, &sig_blob[..soff]);

    if !conn.send_packet(&reply[..roff]) { return; }

    // Derive session keys from shared secret.
    let (tx_key, rx_key) = derive_keys(&shared, &h, &h);
    conn.tx_key = tx_key;
    conn.rx_key = rx_key;

    // Send and receive NEWKEYS.
    let newkeys = [MSG_NEWKEYS];
    if !conn.send_packet(&newkeys) { return; }
    let pkt_len = match conn.recv_packet(&mut pkt) { Some(n) => n, None => return };
    if pkt_len < 1 || pkt[0] != MSG_NEWKEYS { return; }
    conn.encrypted = true;

    // 4. Service request (ssh-userauth).
    let pkt_len = match conn.recv_packet(&mut pkt) { Some(n) => n, None => return };
    if pkt_len < 5 || pkt[0] != MSG_SERVICE_REQUEST { return; }
    let mut svc_reply = [0u8; 32];
    let mut soff = 0;
    svc_reply[soff] = MSG_SERVICE_ACCEPT; soff += 1;
    put_string(&mut svc_reply, &mut soff, b"ssh-userauth");
    if !conn.send_packet(&svc_reply[..soff]) { return; }

    // 5. User authentication.
    let authed = do_userauth(&mut conn, &mut pkt);
    if !authed { return; }

    report_session_event(b"session-open:sshd");

    // 6. Channel setup.
    do_channel(&mut conn, &mut pkt);

    report_session_event(b"session-close:sshd");
}

fn do_userauth(conn: &mut Conn, pkt: &mut [u8; MAX_PACKET]) -> bool {
    let mut attempts = 0u32;
    loop {
        let pkt_len = match conn.recv_packet(pkt) { Some(n) => n, None => return false };
        if pkt_len < 2 { return false; }
        if pkt[0] != MSG_USERAUTH_REQUEST { return false; }

        // Extract username, method, password from packet without holding borrows.
        let mut user_buf = [0u8; 64];
        let mut pass_buf = [0u8; 128];
        let (user_len, password_len, is_password) = {
            let mut off = 1;
            let username = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return false };
            let service = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return false };
            if service != b"ssh-userauth" { return false; }
            let method = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return false };
            let is_pw = method == b"password";
            let u_len = username.len().min(63);
            let (p_len, p_data): (usize, &[u8]) = if is_pw {
                if off >= pkt_len { return false; }
                off += 1; // boolean
                let pw = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return false };
                (pw.len().min(127), pw)
            } else { (0, b"") };
            user_buf[..u_len].copy_from_slice(&username[..u_len]);
            if p_len > 0 { pass_buf[..p_len].copy_from_slice(&p_data[..p_len]); }
            (u_len, p_len, is_pw)
        };

        if is_password {
            let username = &user_buf[..user_len];
            let password = &pass_buf[..password_len];
            if kernel_login(username, password) {
                let success = [MSG_USERAUTH_SUCCESS];
                conn.send_packet(&success);
                return true;
            }
        }





        attempts += 1;
        if attempts >= 3 { return false; }

        let mut fail = [0u8; 32];
        let mut foff = 0;
        fail[foff] = MSG_USERAUTH_FAILURE; foff += 1;
        put_string(&mut fail, &mut foff, b"password");
        fail[foff] = 0; foff += 1; // partial_success = false
        let _ = conn.send_packet(&fail[..foff]);
    }
}

fn do_channel(conn: &mut Conn, pkt: &mut [u8; MAX_PACKET]) {
    // Receive CHANNEL_OPEN.
    let pkt_len = match conn.recv_packet(pkt) { Some(n) => n, None => return };
    if pkt_len < 1 || pkt[0] != MSG_CHANNEL_OPEN { return; }
    let sender_channel = {
        let mut off = 1;
        let channel_type = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return };
        if channel_type != b"session" { return; }
        let sender = match get_u32(&pkt[..pkt_len], &mut off) { Some(v) => v, None => return };
        sender





    };

    // Allocate server-side channel and PTY.
    let server_channel = 0u32;
    let tty_idx = runtime::pty_alloc().unwrap_or(0);

    let mut confirm = [0u8; 32];
    let mut coff = 0;
    confirm[coff] = MSG_CHANNEL_OPEN_CONFIRM; coff += 1;
    put_u32(&mut confirm, &mut coff, sender_channel);
    put_u32(&mut confirm, &mut coff, server_channel);
    put_u32(&mut confirm, &mut coff, 32768); // window size
    put_u32(&mut confirm, &mut coff, 32768); // max packet size
    if !conn.send_packet(&confirm[..coff]) { return; }

    // Process channel requests (pty-req, shell).
    loop {
        let pkt_len = match conn.recv_packet(pkt) { Some(n) => n, None => return };
        if pkt_len < 1 { return; }
        match pkt[0] {
            MSG_CHANNEL_REQUEST => {
                let mut off = 1;
                let _rchan = match get_u32(&pkt[..pkt_len], &mut off) { Some(v) => v, None => return };
                let req_type_raw = match get_string(&pkt[..pkt_len], &mut off) { Some(s) => s, None => return };
                let mut req_type_buf = [0u8; 32];
                let req_type_copy = req_type_raw.len().min(31);
                req_type_buf[..req_type_copy].copy_from_slice(&req_type_raw[..req_type_copy]);
                let req_type = &req_type_buf[..req_type_copy];
                if off >= pkt_len { return; }
                let want_reply = pkt[off]; off += 1;
                let _ = off;
                if want_reply != 0 {
                    let mut sreply = [0u8; 9];
                    let mut sroff = 0;
                    sreply[sroff] = MSG_CHANNEL_SUCCESS; sroff += 1;
                    put_u32(&mut sreply, &mut sroff, sender_channel);
                    let _ = conn.send_packet(&sreply[..sroff]);
                }
                if req_type == b"shell" {
                    // Shell loop: forward data between SSH channel and PTY ring buffers.
                    // Send the initial prompt.
                    let welcome = b"\r\nGraphOS 1.0\r\n$ ";
                    let mut data_pkt = [0u8; 64];
                    let mut doff = 0;
                    data_pkt[doff] = MSG_CHANNEL_DATA; doff += 1;
                    put_u32(&mut data_pkt, &mut doff, sender_channel);
                    put_string(&mut data_pkt, &mut doff, welcome);
                    let _ = conn.send_packet(&data_pkt[..doff]);

                    // Bidirectional relay: SSH channel ↔ PTY ring buffers.
                    // This loop is synchronous (single-threaded sshd model):
                    // - Read data sent by the client and write to the PTY input ring.
                    // - Poll the PTY output ring and forward any bytes to the client.
                    // - On EOF/CLOSE from client, exit.
                    let mut rx_pkt = [0u8; MAX_PACKET];
                    let mut idle_spins: u32 = 0;
                    loop {
                        // Non-blocking poll for outgoing PTY data (shell → client).
                        let mut pty_out = [0u8; 256];
                        let n = runtime::pty_read(tty_idx, &mut pty_out);
                        if n > 0 {
                            idle_spins = 0;
                            // Wrap in SSH_MSG_CHANNEL_DATA and send.
                            let mut fwd = [0u8; 270];
                            let mut foff = 0;
                            fwd[foff] = MSG_CHANNEL_DATA; foff += 1;
                            put_u32(&mut fwd, &mut foff, sender_channel);
                            put_string(&mut fwd, &mut foff, &pty_out[..n]);
                            if !conn.send_packet(&fwd[..foff]) { break; }
                        }

                        // Non-blocking poll for incoming SSH channel data (client → PTY).
                        // Use the encrypted recv with a short spin limit.
                        if let Some(pkt_len) = conn.recv_packet_nonblock(&mut rx_pkt) {
                            idle_spins = 0;
                            if pkt_len < 1 { break; }
                            match rx_pkt[0] {
                                MSG_CHANNEL_DATA => {
                                    // Extract payload and write to PTY input ring.
                                    let mut off = 1;
                                    let _chan = get_u32(&rx_pkt[..pkt_len], &mut off);
                                    if let Some(data) = get_string(&rx_pkt[..pkt_len], &mut off) {
                                        let _ = runtime::pty_write(tty_idx, data);
                                    }
                                }
                                MSG_CHANNEL_EOF | MSG_CHANNEL_CLOSE => break,
                                _ => {}
                            }
                        } else {
                            idle_spins += 1;
                            if idle_spins > 500_000 { break; }
                            runtime::yield_now();
                        }
                    }

                    // Send EOF + CLOSE to client.
                    let mut close_pkt = [0u8; 9];
                    let mut coff = 0;
                    close_pkt[coff] = MSG_CHANNEL_EOF; coff += 1;
                    put_u32(&mut close_pkt, &mut coff, sender_channel);
                    let _ = conn.send_packet(&close_pkt[..coff]);
                    let mut cl2 = [0u8; 9];
                    let mut coff2 = 0;
                    cl2[coff2] = MSG_CHANNEL_CLOSE; coff2 += 1;
                    put_u32(&mut cl2, &mut coff2, sender_channel);
                    let _ = conn.send_packet(&cl2[..coff2]);
                    return;
                }
            }
            MSG_CHANNEL_EOF | MSG_CHANNEL_CLOSE => return,
            _ => {}
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Helper functions
// ════════════════════════════════════════════════════════════════════

fn build_kexinit(buf: &mut [u8]) -> usize {
    let mut off = 0;
    buf[off] = MSG_KEXINIT; off += 1;
    // 16-byte cookie (use simple counter bytes).
    for i in 0..16u8 { buf[off+i as usize] = i; } off += 16;
    // kex_algorithms
    put_string(buf, &mut off, b"curve25519-sha256");
    // server_host_key_algorithms
    put_string(buf, &mut off, b"ssh-ed25519");
    // encryption_algorithms_client_to_server
    put_string(buf, &mut off, b"chacha20-poly1305@openssh.com");
    // encryption_algorithms_server_to_client
    put_string(buf, &mut off, b"chacha20-poly1305@openssh.com");
    // mac_algorithms_client_to_server (integrated with chacha20-poly1305)
    put_string(buf, &mut off, b"");
    // mac_algorithms_server_to_client
    put_string(buf, &mut off, b"");
    // compression_algorithms_client_to_server
    put_string(buf, &mut off, b"none");
    // compression_algorithms_server_to_client
    put_string(buf, &mut off, b"none");
    // languages
    put_string(buf, &mut off, b""); put_string(buf, &mut off, b"");
    // first_kex_packet_follows = false
    buf[off] = 0; off += 1;
    // reserved
    buf[off..off+4].copy_from_slice(&[0; 4]); off += 4;
    off
}

/// Build the exchange hash H per RFC 4253 §8.
fn compute_exchange_hash(
    vc: &[u8], vs: &[u8],
    ic: &[u8], is: &[u8],
    host_pubkey: &[u8; 32],
    qc: &[u8; 32], qs: &[u8; 32],
    k: &[u8; 32],
) -> [u8; 32] {
    let mut buf = [0u8; 1024];
    let mut off = 0;
    put_string(&mut buf, &mut off, vc);
    put_string(&mut buf, &mut off, vs);
    put_string(&mut buf, &mut off, ic);
    put_string(&mut buf, &mut off, is);
    // K_S (host key blob).
    let mut ks = [0u8; 52];
    let mut koff = 0;
    put_string(&mut ks, &mut koff, b"ssh-ed25519");
    put_string(&mut ks, &mut koff, host_pubkey);
    put_string(&mut buf, &mut off, &ks[..koff]);
    put_string(&mut buf, &mut off, qc);
    put_string(&mut buf, &mut off, qs);
    // K as mpint.
    put_string(&mut buf, &mut off, k);
    sha256::hash(&buf[..off])
}

/// Derive session encryption keys from shared secret and exchange hash.
fn derive_keys(k: &[u8; 32], h: &[u8; 32], session_id: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    // Keys per RFC 4253 §7.2: K_n = HASH(K || H || X || session_id).
    // We still use fixed-size internal representations, but include session_id
    // to avoid deriving keys from only (K,H,X).
    let mut tx_input = [0u8; 97];
    tx_input[..32].copy_from_slice(k);
    tx_input[32..64].copy_from_slice(h);
    tx_input[64] = b'C';
    tx_input[65..97].copy_from_slice(session_id);
    let tx_key_raw = sha256::hash(&tx_input);

    let mut rx_input = [0u8; 97];
    rx_input[..32].copy_from_slice(k);
    rx_input[32..64].copy_from_slice(h);
    rx_input[64] = b'D';
    rx_input[65..97].copy_from_slice(session_id);
    let rx_key_raw = sha256::hash(&rx_input);

    (tx_key_raw, rx_key_raw)
}

/// Derive ephemeral server private key for forward secrecy.
fn derive_ephemeral_key(client_pubkey: &[u8; 32]) -> Option<[u8; 32]> {
    // Draw fresh entropy from kernel and domain-separate with peer key material.
    let mut seed = [0u8; 32];
    if !runtime::random_fill(&mut seed) {
        return None;
    }
    let mut input = [0u8; 64];
    input[..32].copy_from_slice(&seed);
    input[32..64].copy_from_slice(client_pubkey);
    let mut out = sha256::hash(&input);
    // Clamp for X25519 private scalar requirements.
    out[0] &= 248;
    out[31] &= 127;
    out[31] |= 64;
    Some(out)
}

/// Call SYS_LOGIN with username and password to authenticate against the kernel UserDb.
fn kernel_login(username: &[u8], password: &[u8]) -> bool {
    runtime::login(username, password)
}

// ════════════════════════════════════════════════════════════════════
// Runtime host key — loaded from VFS at startup, falls back to
// the compile-time test key when the production key is absent.
// ════════════════════════════════════════════════════════════════════

/// 64-byte host key store: [0..32] = private scalar, [32..64] = public key.
/// Initialised from the compile-time constants; overwritten by `load_host_key`
/// when /data/etc/ssh/host_ed25519_key is present.
static mut RUNTIME_HOST_KEY: [u8; 64] = {
    let mut k = [0u8; 64];
    // Copy via const initialisation: private then public.
    let mut i = 0;
    while i < 32 { k[i] = HOST_PRIVKEY[i]; i += 1; }
    while i < 64 { k[i] = HOST_PUBKEY[i - 32]; i += 1; }
    k
};

/// Returns a copy of the current host private key scalar.
fn host_privkey() -> [u8; 32] {
    let mut k = [0u8; 32];
    // SAFETY: written once at startup before any connection is accepted.
    k.copy_from_slice(unsafe { &RUNTIME_HOST_KEY[..32] });
    k
}

/// Returns a copy of the current host public key.
fn host_pubkey() -> [u8; 32] {
    let mut k = [0u8; 32];
    k.copy_from_slice(unsafe { &RUNTIME_HOST_KEY[32..64] });
    k
}

/// Attempt to load the host Ed25519 key pair from VFS.
///
/// Expected format: 64 raw bytes — first 32 = private scalar,
/// last 32 = public key.  If the file is absent or malformed the
/// daemon continues with the compile-time test key and logs a warning.
fn load_host_key() -> bool {
    const KEY_PATH: &[u8] = b"/data/etc/ssh/host_ed25519_key";
    let fd = runtime::vfs_open(KEY_PATH);
    if fd == u64::MAX {
        // Key file absent — continue with the compile-time test key that is
        // already baked into RUNTIME_HOST_KEY.  Log a warning only.
        runtime::write_line(b"[sshd] WARNING: host key not found at /data/etc/ssh/host_ed25519_key, using built-in test key\n");
        return true;
    }
    let mut buf = [0u8; 64];
    let n = runtime::vfs_read(fd, &mut buf);
    runtime::vfs_close(fd);
    if n < 64 {
        runtime::write_line(b"[sshd] ERROR: host key file too short\n");
        return false;
    }
    // SAFETY: called once before accept loop, no concurrent access.
    // SAFETY: single-threaded daemon; called once before accept loop; no other
    // reference to RUNTIME_HOST_KEY exists at this point.
    unsafe {
        let dst: *mut [u8; 64] = core::ptr::addr_of_mut!(RUNTIME_HOST_KEY);
        (*dst).copy_from_slice(&buf);
    }
    runtime::write_line(b"[sshd] loaded host key from /data/etc/ssh/host_ed25519_key\n");
    true
}

// ════════════════════════════════════════════════════════════════════
// Entry point — graph-first daemon loop
// ════════════════════════════════════════════════════════════════════

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[sshd] starting\n");

    // Load host key from VFS before accepting connections.
    if !load_host_key() {
        runtime::write_line(b"[sshd] refusing to start without host key\n");
        runtime::exit(1);
    }

    // Graph-first: announce to servicemgr overseer.
    let _ = runtime::bootstrap_named_status(b"service-ready:", b"sshd");
    let _ = runtime::bootstrap_named_status(b"service-bound:", b"sshd");
    runtime::announce_service_ready(b"sshd");

    // Open listening TCP socket on port 22.
    let listen_sock = loop {
        if let Some(s) = runtime::socket_open() { break s; }
        runtime::yield_now();
    };
    if !runtime::socket_bind(&listen_sock, SSH_PORT) {
        runtime::write_line(b"[sshd] bind port 22 failed\n");
        runtime::exit(1);
    }
    if !runtime::socket_listen(&listen_sock) {
        runtime::write_line(b"[sshd] listen failed\n");
        runtime::exit(1);
    }

    runtime::write_line(b"[sshd] listening on port 22\n");

    // Main accept loop.
    let mut idle_accept_polls = 0u32;
    loop {
        match runtime::socket_accept(&listen_sock) {
            Some((client_sock, _ip, _port)) => {
                idle_accept_polls = 0;
                // Handle connection inline (single-threaded; SMP will parallelise).
                handle_connection(client_sock);
            }
            None => {
                idle_accept_polls = idle_accept_polls.wrapping_add(1);
                if idle_accept_polls >= 256 {
                    idle_accept_polls = 0;
                    runtime::yield_now();
                }
            }
        }
    }
}
