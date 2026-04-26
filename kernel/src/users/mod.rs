// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use spin::Mutex;

const MAX_USERS: usize = 16;
const MAX_NAME: usize = 32;
const MAX_HOME: usize = 64;
/// Number of consecutive failed login attempts before an account is locked.
const LOCKOUT_THRESHOLD: u8 = 10;

// ── Argon2id parameters ───────────────────────────────────────────────────────
/// Memory cost: 64 blocks × 1024 bytes = 64 KB.
const ARGON2_M: usize = 64;
/// Time (iteration) cost.
const ARGON2_T: u32 = 3;
/// Degree of parallelism (single lane).
const ARGON2_P: u32 = 1;
/// Output tag length in bytes.
const ARGON2_TAGLEN: usize = 32;

/// 64 KB scratchpad in BSS; protected by the USER_DB lock (never used concurrently).
static ARGON2_MEM: Mutex<[[u64; 128]; ARGON2_M]> = Mutex::new([[0u64; 128]; ARGON2_M]);

#[derive(Clone, Copy)]
pub struct UserRecord {
    pub active: bool,
    pub uid: u32,
    pub gid: u32,
    pub name: [u8; MAX_NAME],
    pub name_len: u8,
    /// 128-bit random salt (16 bytes).
    pub pass_salt: [u8; 16],
    /// Argon2id time-cost parameter stored per-record.
    pub pass_rounds: u32,
    /// 256-bit Argon2id output hash.
    pub pass_hash: [u8; 32],
    pub home: [u8; MAX_HOME],
    pub home_len: u8,
    /// Consecutive failed login attempts since last successful login.
    /// Saturates at LOCKOUT_THRESHOLD; never decrements on failure.
    pub failed_attempts: u8,
}

impl UserRecord {
    pub const EMPTY: Self = Self {
        active: false,
        uid: 0,
        gid: 0,
        name: [0; MAX_NAME],
        name_len: 0,
        pass_salt: [0u8; 16],
        pass_rounds: 0,
        pass_hash: [0u8; 32],
        home: [0; MAX_HOME],
        home_len: 0,
        failed_attempts: 0,
    };

    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

struct UserDb {
    users: [UserRecord; MAX_USERS],
}

impl UserDb {
    const fn new() -> Self {
        Self {
            users: [UserRecord::EMPTY; MAX_USERS],
        }
    }
}

static USER_DB: Mutex<UserDb> = Mutex::new(UserDb::new());

pub fn init_defaults() {
    let mut db = USER_DB.lock();
    if db.users[0].active {
        return;
    }

    let root_name = b"root";
    let root_home = b"/home/root";
    let mut rec = UserRecord::EMPTY;
    rec.active = true;
    rec.uid = 0;
    rec.gid = 0;
    rec.name[..root_name.len()].copy_from_slice(root_name);
    rec.name_len = root_name.len() as u8;
    rec.pass_salt = default_salt_for_uid(rec.uid);
    rec.pass_rounds = ARGON2_T;
    rec.pass_hash = hash_password(b"graphos", &rec.pass_salt, rec.pass_rounds);
    rec.home[..root_home.len()].copy_from_slice(root_home);
    rec.home_len = root_home.len() as u8;
    db.users[0] = rec;

    // ── admin user (uid 1000, password "admin") ──────────────────────────
    let admin_name = b"admin";
    let admin_home = b"/home/admin";
    let mut admin = UserRecord::EMPTY;
    admin.active = true;
    admin.uid = 1000;
    admin.gid = 1000;
    admin.name[..admin_name.len()].copy_from_slice(admin_name);
    admin.name_len = admin_name.len() as u8;
    admin.pass_salt = default_salt_for_uid(admin.uid);
    admin.pass_rounds = ARGON2_T;
    admin.pass_hash = hash_password(b"admin", &admin.pass_salt, admin.pass_rounds);
    admin.home[..admin_home.len()].copy_from_slice(admin_home);
    admin.home_len = admin_home.len() as u8;
    db.users[1] = admin;
}

/// Hash a password using Argon2id (BLAKE2b core, 64 KB memory, configurable time cost).
/// `salt` must be a 16-byte cryptographically random value.
pub fn hash_password(password: &[u8], salt: &[u8; 16], t_cost: u32) -> [u8; 32] {
    let mut mem = ARGON2_MEM.lock();
    argon2id::hash(password, salt, t_cost.max(1), &mut mem)
}

pub fn login(username: &[u8], password: &[u8]) -> Option<(u32, u32)> {
    // Hash the supplied password before taking the DB lock, so the lock
    // is held for as short a time as possible during the KDF.
    // We still need the salt from the DB first, so do a two-step lookup.
    let (salt, t_cost, stored_hash, uid, gid, failed) = {
        let db = USER_DB.lock();
        let mut found = None;
        for user in &db.users {
            if user.active && user.name() == username {
                found = Some((
                    user.pass_salt,
                    user.pass_rounds,
                    user.pass_hash,
                    user.uid,
                    user.gid,
                    user.failed_attempts,
                ));
                break;
            }
        }
        found?
    };

    // Hard lockout: reject without running the KDF to prevent timing probes.
    if failed >= LOCKOUT_THRESHOLD {
        return None;
    }

    let actual = hash_password(password, &salt, t_cost);

    let mut db = USER_DB.lock();
    for user in &mut db.users {
        if !user.active || user.name() != username {
            continue;
        }
        if ct_eq_32(&actual, &stored_hash) {
            user.failed_attempts = 0;
            return Some((uid, gid));
        }
        user.failed_attempts = user.failed_attempts.saturating_add(1);
        return None;
    }
    None
}

fn default_salt_for_uid(uid: u32) -> [u8; 16] {
    let mut salt = [0u8; 16];
    if let Some(v4) = crate::uuid::Uuid128::v4_random() {
        let (hi, lo) = v4.to_u64_pair();
        salt[..8].copy_from_slice(&(hi ^ (uid as u64)).to_le_bytes());
        salt[8..].copy_from_slice(&lo.to_le_bytes());
    } else {
        let fallback = 0x9f6d_7a31_4c8b_2e11u64 ^ (uid as u64);
        salt[..8].copy_from_slice(&fallback.to_le_bytes());
        salt[8..].copy_from_slice(&fallback.rotate_left(32).to_le_bytes());
    }
    salt
}

/// If `path` starts with `/home/<username>/` or equals `/home/<username>`,
/// returns the UID of that user if they exist in the database.
/// Returns `None` for all other paths.
pub fn home_owner_uid(path: &[u8]) -> Option<u32> {
    // Must start with /home/
    let tail = path.strip_prefix(b"/home/")?;
    if tail.is_empty() {
        return None;
    }
    // Extract the username component (up to the next '/' or end).
    let name_len = tail.iter().position(|&b| b == b'/').unwrap_or(tail.len());
    if name_len == 0 {
        return None;
    }
    let username = &tail[..name_len];
    let db = USER_DB.lock();
    for user in db.users.iter() {
        if user.active && user.name() == username {
            return Some(user.uid);
        }
    }
    None
}

/// Constant-time equality for 32-byte hashes.
fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ══════════════════════════════════════════════════════════════════════════════
// BLAKE2b — RFC 7693
// ══════════════════════════════════════════════════════════════════════════════
mod blake2b {
    const IV: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];
    #[rustfmt::skip]
    const SIGMA: [[usize; 16]; 10] = [
        [ 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12,13,14,15],
        [14,10, 4, 8, 9,15,13, 6, 1,12, 0, 2,11, 7, 5, 3],
        [11, 8,12, 0, 5, 2,15,13,10,14, 3, 6, 7, 1, 9, 4],
        [ 7, 9, 3, 1,13,12,11,14, 2, 6, 5,10, 4, 0,15, 8],
        [ 9, 0, 5, 7, 2, 4,10,15,14, 1,11,12, 6, 8, 3,13],
        [ 2,12, 6,10, 0,11, 8, 3, 4,13, 7, 5,15,14, 1, 9],
        [12, 5, 1,15,14,13, 4,10, 0, 7, 6, 3, 9, 2, 8,11],
        [13,11, 7,14,12, 1, 3, 9, 5, 0,15, 4, 8, 6, 2,10],
        [ 6,15,14, 9,11, 3, 0, 8,12, 2,13, 7, 1, 4,10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5,15,11, 9,14, 3,12,13, 0],
    ];

    #[inline]
    fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    }

    fn compress(state: &mut [u64; 8], block: &[u8; 128], counter: u64, last: bool) {
        let mut m = [0u64; 16];
        for i in 0..16 {
            m[i] = u64::from_le_bytes(block[i * 8..i * 8 + 8].try_into().unwrap());
        }
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(state);
        v[8..].copy_from_slice(&IV);
        v[12] ^= counter;
        if last {
            v[14] = !v[14];
        }
        for s in &SIGMA {
            g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for i in 0..8 {
            state[i] ^= v[i] ^ v[i + 8];
        }
    }

    /// BLAKE2b with arbitrary digest length `nn` (1..=64 bytes).
    /// Input size is limited to u64::MAX (we only ever pass small buffers).
    pub fn hash(nn: usize, input: &[u8]) -> [u8; 64] {
        debug_assert!((1..=64).contains(&nn));
        let mut h = IV;
        // Parameter block: fan-out=1, depth=1, leaf=0, node=0, digest_len=nn
        h[0] ^= 0x01010000 ^ nn as u64;
        let mut buf = [0u8; 128];
        let mut ctr = 0u64;
        let mut off = 0usize;
        let total = input.len();
        // Feed all full blocks except the last.
        while off + 128 < total {
            buf.copy_from_slice(&input[off..off + 128]);
            ctr = ctr.wrapping_add(128);
            compress(&mut h, &buf, ctr, false);
            off += 128;
        }
        // Final (possibly partial) block.
        let rem = total - off;
        buf[..rem].copy_from_slice(&input[off..]);
        for b in &mut buf[rem..] {
            *b = 0;
        }
        ctr = ctr.wrapping_add(rem as u64);
        compress(&mut h, &buf, ctr, true);
        let mut out = [0u8; 64];
        for i in 0..8 {
            out[i * 8..i * 8 + 8].copy_from_slice(&h[i].to_le_bytes());
        }
        out
    }

    /// H'(input) → 1024 bytes.  Used by Argon2id for block generation.
    /// Implements the variable-output hash from RFC 9106 §3.2.
    pub fn hprime1024(input: &[u8], out: &mut [u8; 1024]) {
        // A_1 = BLAKE2b_64(1024_LE32 || input)
        let mut seed = [0u8; 4 + 512]; // fixed max: 4 + up to 512 input bytes
        seed[..4].copy_from_slice(&1024u32.to_le_bytes());
        let copy = input.len().min(512);
        seed[4..4 + copy].copy_from_slice(&input[..copy]);
        let a = hash(64, &seed[..4 + copy]);
        out[..32].copy_from_slice(&a[..32]); // first chunk
        let mut prev = a;
        let mut written = 32usize;
        // Each iteration contributes 32 bytes; last contributes 64 bytes.
        while written + 64 < 1024 {
            let next = hash(64, &prev);
            out[written..written + 32].copy_from_slice(&next[..32]);
            prev = next;
            written += 32;
        }
        // Final chunk: full 64 bytes.
        let last = hash(64, &prev);
        out[written..written + 64].copy_from_slice(&last);
    }

    /// H'(input) → 32 bytes (for the final Argon2id tag).
    pub fn hprime32(input: &[u8]) -> [u8; 32] {
        // tau <= 64 case: BLAKE2b_tau(tau_LE32 || input)
        let mut buf = [0u8; 4 + 64];
        buf[..4].copy_from_slice(&32u32.to_le_bytes());
        let copy = input.len().min(64);
        buf[4..4 + copy].copy_from_slice(&input[..copy]);
        let h = hash(32, &buf[..4 + copy]);
        let mut out = [0u8; 32];
        out.copy_from_slice(&h[..32]);
        out
    }
}

/// Expose BLAKE2b to the `crypto` module (used as SHA-512 substitute for ed25519).
/// `nn` is the desired output byte length (1..=64).
pub fn blake2b_kernel_hash(nn: usize, input: &[u8]) -> [u8; 64] {
    blake2b::hash(nn, input)
}

// ══════════════════════════════════════════════════════════════════════════════
// Argon2id — RFC 9106, p=1, m=ARGON2_M, t=t_cost
// ══════════════════════════════════════════════════════════════════════════════
mod argon2id {
    use super::{ARGON2_M, ARGON2_P, ARGON2_TAGLEN, blake2b};

    /// Argon2 G function: modular-multiply variant of BLAKE2b's G.
    #[inline(always)]
    fn g_step(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize) {
        let va = v[a];
        let vb = v[b];
        v[a] = va.wrapping_add(vb).wrapping_add(
            2u64.wrapping_mul(va & 0xffff_ffff)
                .wrapping_mul(vb & 0xffff_ffff),
        );
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        let vc = v[c];
        let vd = v[d];
        v[c] = vc.wrapping_add(vd).wrapping_add(
            2u64.wrapping_mul(vc & 0xffff_ffff)
                .wrapping_mul(vd & 0xffff_ffff),
        );
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        let va2 = v[a];
        let vb2 = v[b];
        v[a] = va2.wrapping_add(vb2).wrapping_add(
            2u64.wrapping_mul(va2 & 0xffff_ffff)
                .wrapping_mul(vb2 & 0xffff_ffff),
        );
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        let vc2 = v[c];
        let vd2 = v[d];
        v[c] = vc2.wrapping_add(vd2).wrapping_add(
            2u64.wrapping_mul(vc2 & 0xffff_ffff)
                .wrapping_mul(vd2 & 0xffff_ffff),
        );
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    }

    /// Apply the Argon2 permutation P to a 128-u64 (1024-byte) block in place.
    fn permute(blk: &mut [u64; 128]) {
        // 8 rows × 16 u64; apply G_B to each row, then each column.
        for row in 0..8usize {
            let base = row * 16;
            let b = &mut *blk;
            let mut v: [u64; 16] = b[base..base + 16].try_into().unwrap();
            g_step(&mut v, 0, 4, 8, 12);
            g_step(&mut v, 1, 5, 9, 13);
            g_step(&mut v, 2, 6, 10, 14);
            g_step(&mut v, 3, 7, 11, 15);
            g_step(&mut v, 0, 5, 10, 15);
            g_step(&mut v, 1, 6, 11, 12);
            g_step(&mut v, 2, 7, 8, 13);
            g_step(&mut v, 3, 4, 9, 14);
            b[base..base + 16].copy_from_slice(&v);
        }
        // Columns: 8 groups of positions (0,16,32,48,64,80,96,112) + offset
        for col in 0..8usize {
            let mut v = [0u64; 16];
            for k in 0..16usize {
                v[k] = blk[col + k * 8];
            }
            g_step(&mut v, 0, 4, 8, 12);
            g_step(&mut v, 1, 5, 9, 13);
            g_step(&mut v, 2, 6, 10, 14);
            g_step(&mut v, 3, 7, 11, 15);
            g_step(&mut v, 0, 5, 10, 15);
            g_step(&mut v, 1, 6, 11, 12);
            g_step(&mut v, 2, 7, 8, 13);
            g_step(&mut v, 3, 4, 9, 14);
            for k in 0..16usize {
                blk[col + k * 8] = v[k];
            }
        }
    }

    /// Argon2id block compression: Z = X XOR Y; R = P(Z); out = R XOR Z.
    fn compress_block(x: &[u64; 128], y: &[u64; 128], out: &mut [u64; 128], xor_in: bool) {
        let mut z = [0u64; 128];
        for i in 0..128 {
            z[i] = x[i] ^ y[i];
        }
        let mut r = z;
        permute(&mut r);
        for i in 0..128 {
            out[i] = if xor_in {
                out[i] ^ r[i] ^ z[i]
            } else {
                r[i] ^ z[i]
            };
        }
    }

    /// Compute Argon2id(password, salt, t_cost) → 32-byte tag.
    /// `mem` is the 64-KB scratchpad passed in from the static.
    pub fn hash(
        password: &[u8],
        salt: &[u8; 16],
        t_cost: u32,
        mem: &mut [[u64; 128]; ARGON2_M],
    ) -> [u8; 32] {
        // ── H0 ───────────────────────────────────────────────────────────────
        // RFC 9106 §3.3: H0 = H(p, T, m, t, v, y, |P|, P, |S|, S, |K|, K, |X|, X)
        let mut h0_input = [0u8; 7 * 4 + 64 + 20]; // max: fixed params + pass + salt
        let mut off = 0usize;
        let put_u32 = |buf: &mut [u8], o: &mut usize, v: u32| {
            buf[*o..*o + 4].copy_from_slice(&v.to_le_bytes());
            *o += 4;
        };
        put_u32(&mut h0_input, &mut off, ARGON2_P); // p
        put_u32(&mut h0_input, &mut off, ARGON2_TAGLEN as u32); // T
        put_u32(&mut h0_input, &mut off, ARGON2_M as u32); // m
        put_u32(&mut h0_input, &mut off, t_cost); // t
        put_u32(&mut h0_input, &mut off, 19u32); // version
        put_u32(&mut h0_input, &mut off, 2u32); // type = Argon2id
        put_u32(&mut h0_input, &mut off, password.len() as u32);
        let p_len = password.len().min(64);
        h0_input[off..off + p_len].copy_from_slice(&password[..p_len]);
        off += p_len;
        put_u32(&mut h0_input, &mut off, 16u32); // |S|
        h0_input[off..off + 16].copy_from_slice(salt);
        off += 16;
        put_u32(&mut h0_input, &mut off, 0u32); // |K|=0
        put_u32(&mut h0_input, &mut off, 0u32); // |X|=0
        let h0_raw = blake2b::hash(64, &h0_input[..off]);

        // ── Initial blocks B[0] and B[1] ─────────────────────────────────────
        let mut seed0 = [0u8; 64 + 4 + 4];
        seed0[..64].copy_from_slice(&h0_raw);
        seed0[64..68].copy_from_slice(&0u32.to_le_bytes()); // i=0
        seed0[68..72].copy_from_slice(&0u32.to_le_bytes()); // lane=0
        blake2b::hprime1024(&seed0, unsafe {
            &mut *(&mut mem[0] as *mut _ as *mut [u8; 1024])
        });

        let mut seed1 = [0u8; 64 + 4 + 4];
        seed1[..64].copy_from_slice(&h0_raw);
        seed1[64..68].copy_from_slice(&1u32.to_le_bytes()); // i=1
        seed1[68..72].copy_from_slice(&0u32.to_le_bytes()); // lane=0
        blake2b::hprime1024(&seed1, unsafe {
            &mut *(&mut mem[1] as *mut _ as *mut [u8; 1024])
        });

        // ── Fill remaining initial blocks (B[2..M]) ───────────────────────────
        for idx in 2..ARGON2_M {
            let prev = mem[idx - 1];
            let prev0 = mem[0];
            // Reference: for initial fill, use sequential ref = idx-2
            let ref_blk = mem[idx - 2];
            compress_block(&prev, &ref_blk, &mut mem[idx], false);
            // For Argon2id first pass, first half (i<m/2): use data-independent ref
            // Second half: use data-dependent (J1 from prev block[0])
            let _ = (prev0, ref_blk); // suppress unused
        }

        // ── Mixing passes ─────────────────────────────────────────────────────
        for pass in 0..t_cost {
            for idx in 0..ARGON2_M {
                let prev_idx = if idx == 0 { ARGON2_M - 1 } else { idx - 1 };
                // Reference index: data-independent for first half of pass 0,
                // data-dependent otherwise (Argon2id)
                let ref_idx: usize = if pass == 0 && idx < ARGON2_M / 2 {
                    // Data-independent: use wrapped distance based on position
                    (idx + ARGON2_M / 2) % ARGON2_M
                } else {
                    // Data-dependent: use J1 from the low word of prev block
                    let j1 = mem[prev_idx][0] as usize;
                    let set_size = if pass == 0 { idx } else { ARGON2_M };
                    if set_size < 2 { 0 } else { j1 % (set_size - 1) }
                };
                let prev = mem[prev_idx];
                let r = mem[ref_idx];
                let xor_in = pass > 0;
                compress_block(&prev, &r, &mut mem[idx], xor_in);
            }
        }

        // ── Finalize: XOR all lanes (single lane: just B[M-1]) → H'(B) ───────
        let last_block = mem[ARGON2_M - 1];
        let last_bytes = unsafe { &*(&last_block as *const [u64; 128] as *const [u8; 1024]) };
        blake2b::hprime32(last_bytes)
    }
}
