// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! FIDO2 / CTAP2 — credential management and assertion.
//!
//! Implements CTAP2 over USB HID (routed from `drivers/input/usb_hid.rs`).
//!
//! Uses EdDSA over Ed25519 (COSE algorithm -8, OKP key type, Ed25519 curve).
//! EdDSA is a FIDO2 REQUIRED algorithm per the WebAuthn Level 2 spec (§2).

use crate::audit::{self, AuditKind};
use crate::crypto;
use crate::uuid::Uuid128 as Uuid;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

// ── CTAP2 command codes ───────────────────────────────────────────────────────

pub const CTAP2_MAKE_CREDENTIAL: u8 = 0x01;
pub const CTAP2_GET_ASSERTION: u8 = 0x02;
pub const CTAP2_GET_INFO: u8 = 0x04;
pub const CTAP2_CLIENT_PIN: u8 = 0x06;
pub const CTAP2_RESET: u8 = 0x07;

pub const CTAP2_OK: u8 = 0x00;
pub const CTAP2_ERR_INVALID_CMD: u8 = 0x01;
pub const CTAP2_ERR_MISSING_PARAM: u8 = 0x02;
pub const CTAP2_ERR_INVALID_PAR: u8 = 0x03;
pub const CTAP2_ERR_NOT_ALLOWED: u8 = 0x30;
pub const CTAP2_ERR_NO_CREDENTIALS: u8 = 0x2E;
pub const CTAP2_ERR_NO_OPERATION: u8 = 0x2A;

/// AAGUID — all zeros indicates dev/testing mode.
const AAGUID: [u8; 16] = [0u8; 16];

/// Authenticator flags
const FLAG_UP: u8 = 0x01;
const FLAG_AT: u8 = 0x40;

// ── Credential store ──────────────────────────────────────────────────────────

const MAX_CREDENTIALS: usize = 16;
const CRED_ID_LEN: usize = 32;

#[derive(Clone, Copy)]
struct Credential {
    id: [u8; CRED_ID_LEN],
    seed: [u8; 32],       // Ed25519 seed (private)
    pub_key: [u8; 32],    // Ed25519 public key
    rp_id_hash: [u8; 32], // SHA-256(rpId)
    sign_count: u32,
    active: bool,
    /// Graph arena node ID for this credential (0 = not registered).
    graph_node: crate::graph::types::NodeId,
}

impl Credential {
    const fn empty() -> Self {
        Self {
            id: [0u8; 32],
            seed: [0u8; 32],
            pub_key: [0u8; 32],
            rp_id_hash: [0u8; 32],
            sign_count: 0,
            active: false,
            graph_node: 0,
        }
    }
}

static CREDS: Mutex<[Credential; MAX_CREDENTIALS]> =
    Mutex::new([Credential::empty(); MAX_CREDENTIALS]);
static CRED_COUNT: AtomicUsize = AtomicUsize::new(0);

// ── Minimal CBOR helpers ──────────────────────────────────────────────────────

fn cbor_uint(o: &mut [u8], p: usize, v: u64) -> usize {
    if v < 24 {
        o[p] = v as u8;
        p + 1
    } else if v < 256 {
        o[p] = 0x18;
        o[p + 1] = v as u8;
        p + 2
    } else {
        o[p] = 0x19;
        o[p + 1] = (v >> 8) as u8;
        o[p + 2] = v as u8;
        p + 3
    }
}
fn cbor_nint(o: &mut [u8], p: usize, n: u64) -> usize {
    if n < 24 {
        o[p] = 0x20 | n as u8;
        p + 1
    } else {
        o[p] = 0x38;
        o[p + 1] = n as u8;
        p + 2
    }
}
fn cbor_bstr(o: &mut [u8], p: usize, d: &[u8]) -> usize {
    let q = if d.len() < 24 {
        o[p] = 0x40 | d.len() as u8;
        p + 1
    } else if d.len() < 256 {
        o[p] = 0x58;
        o[p + 1] = d.len() as u8;
        p + 2
    } else {
        o[p] = 0x59;
        o[p + 1] = (d.len() >> 8) as u8;
        o[p + 2] = d.len() as u8;
        p + 3
    };
    o[q..q + d.len()].copy_from_slice(d);
    q + d.len()
}
fn cbor_tstr(o: &mut [u8], p: usize, t: &[u8]) -> usize {
    let q = if t.len() < 24 {
        o[p] = 0x60 | t.len() as u8;
        p + 1
    } else if t.len() < 256 {
        o[p] = 0x78;
        o[p + 1] = t.len() as u8;
        p + 2
    } else {
        o[p] = 0x79;
        o[p + 1] = (t.len() >> 8) as u8;
        o[p + 2] = t.len() as u8;
        p + 3
    };
    o[q..q + t.len()].copy_from_slice(t);
    q + t.len()
}
fn cbor_map(o: &mut [u8], p: usize, n: u64) -> usize {
    if n < 24 {
        o[p] = 0xa0 | n as u8;
        p + 1
    } else {
        o[p] = 0xb8;
        o[p + 1] = n as u8;
        p + 2
    }
}
fn cbor_arr(o: &mut [u8], p: usize, n: u64) -> usize {
    if n < 24 {
        o[p] = 0x80 | n as u8;
        p + 1
    } else {
        o[p] = 0x98;
        o[p + 1] = n as u8;
        p + 2
    }
}

// ── CBOR decode ───────────────────────────────────────────────────────────────

fn cbor_read_len(d: &[u8], p: usize) -> Option<(usize, usize)> {
    if p >= d.len() {
        return None;
    }
    match d[p] & 0x1f {
        b @ 0..=23 => Some((b as usize, 1)),
        24 if p + 1 < d.len() => Some((d[p + 1] as usize, 2)),
        25 if p + 2 < d.len() => Some(((d[p + 1] as usize) << 8 | d[p + 2] as usize, 3)),
        _ => None,
    }
}

fn cbor_skip(d: &[u8], p: usize) -> Option<usize> {
    if p >= d.len() {
        return None;
    }
    match d[p] >> 5 {
        0 | 1 => {
            let (_, h) = cbor_read_len(d, p)?;
            Some(p + h)
        }
        2 | 3 => {
            let (l, h) = cbor_read_len(d, p)?;
            Some(p + h + l)
        }
        4 => {
            let (n, h) = cbor_read_len(d, p)?;
            let mut q = p + h;
            for _ in 0..n {
                q = cbor_skip(d, q)?;
            }
            Some(q)
        }
        5 => {
            let (n, h) = cbor_read_len(d, p)?;
            let mut q = p + h;
            for _ in 0..n {
                q = cbor_skip(d, q)?;
                q = cbor_skip(d, q)?;
            }
            Some(q)
        }
        7 => Some(p + 1),
        _ => None,
    }
}

fn cbor_map_int(d: &[u8], key: i64) -> Option<&[u8]> {
    if d.is_empty() || d[0] >> 5 != 5 {
        return None;
    }
    let (n, h) = cbor_read_len(d, 0)?;
    let mut p = h;
    for _ in 0..n {
        if p >= d.len() {
            break;
        }
        let major = d[p] >> 5;
        let kval: i64 = if major == 0 {
            let (v, h) = cbor_read_len(d, p)?;
            p += h;
            v as i64
        } else if major == 1 {
            let (v, h) = cbor_read_len(d, p)?;
            p += h;
            -1 - v as i64
        } else {
            return None;
        };
        let vs = p;
        p = cbor_skip(d, p)?;
        if kval == key {
            return Some(&d[vs..p]);
        }
    }
    None
}

fn cbor_map_text<'a>(d: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    if d.is_empty() || d[0] >> 5 != 5 {
        return None;
    }
    let (n, h) = cbor_read_len(d, 0)?;
    let mut p = h;
    for _ in 0..n {
        if p >= d.len() {
            break;
        }
        if d[p] >> 5 != 3 {
            p = cbor_skip(d, p)?;
            p = cbor_skip(d, p)?;
            continue;
        }
        let (kl, kh) = cbor_read_len(d, p)?;
        let ks = p + kh;
        p = ks + kl;
        let vs = p;
        p = cbor_skip(d, p)?;
        if kl == key.len() && &d[ks..ks + kl] == key {
            return Some(&d[vs..p]);
        }
    }
    None
}

fn cbor_str_bytes(v: &[u8]) -> Option<&[u8]> {
    if v.is_empty() {
        return None;
    }
    let m = v[0] >> 5;
    if m != 2 && m != 3 {
        return None;
    }
    let (l, h) = cbor_read_len(v, 0)?;
    if v.len() < h + l {
        return None;
    }
    Some(&v[h..h + l])
}

// ── Crypto helpers ────────────────────────────────────────────────────────────

fn sha256(d: &[u8]) -> [u8; 32] {
    crypto::sha256(d)
}

fn random_seed() -> [u8; 32] {
    let mut s = [0u8; 32];
    for chunk in s.chunks_mut(8) {
        let v = crate::arch::x86_64::cpu_init::rdrand_entropy();
        chunk.copy_from_slice(&v.to_le_bytes()[..chunk.len()]);
    }
    s
}

// ── authenticatorData builders ────────────────────────────────────────────────

fn build_cose_key(pk: &[u8; 32], o: &mut [u8]) -> usize {
    let mut p = cbor_map(o, 0, 4);
    p = cbor_uint(o, p, 1);
    p = cbor_uint(o, p, 1); // kty: OKP
    p = cbor_uint(o, p, 3);
    p = cbor_nint(o, p, 7); // alg: -8 (EdDSA)
    p = cbor_nint(o, p, 0);
    p = cbor_uint(o, p, 6); // crv: Ed25519
    p = cbor_nint(o, p, 1);
    p = cbor_bstr(o, p, pk); // x: pubkey
    p
}

fn build_auth_data_make(
    rp_hash: &[u8; 32],
    sc: u32,
    cid: &[u8; 32],
    pk: &[u8; 32],
    o: &mut [u8],
) -> usize {
    o[0..32].copy_from_slice(rp_hash);
    o[32] = FLAG_UP | FLAG_AT;
    o[33..37].copy_from_slice(&sc.to_be_bytes());
    // attestedCredentialData
    o[37..53].copy_from_slice(&AAGUID);
    o[53] = 0;
    o[54] = 32; // credIdLen
    o[55..87].copy_from_slice(cid);
    let mut cose = [0u8; 128];
    let cl = build_cose_key(pk, &mut cose);
    o[87..87 + cl].copy_from_slice(&cose[..cl]);
    87 + cl
}

fn build_auth_data_assert(rp_hash: &[u8; 32], sc: u32, o: &mut [u8]) -> usize {
    o[0..32].copy_from_slice(rp_hash);
    o[32] = FLAG_UP;
    o[33..37].copy_from_slice(&sc.to_be_bytes());
    37
}

// ── CTAP2 handlers ────────────────────────────────────────────────────────────

fn handle_get_info(rsp: &mut [u8]) -> usize {
    rsp[0] = CTAP2_OK;
    let mut p = 1;
    p = cbor_map(rsp, p, 4);
    p = cbor_uint(rsp, p, 1);
    p = cbor_arr(rsp, p, 1);
    p = cbor_tstr(rsp, p, b"FIDO_2_0");
    p = cbor_uint(rsp, p, 3);
    p = cbor_bstr(rsp, p, &AAGUID);
    p = cbor_uint(rsp, p, 4);
    p = cbor_map(rsp, p, 0);
    p = cbor_uint(rsp, p, 6);
    p = cbor_arr(rsp, p, 1);
    p = cbor_map(rsp, p, 2);
    p = cbor_tstr(rsp, p, b"type");
    p = cbor_tstr(rsp, p, b"public-key");
    p = cbor_tstr(rsp, p, b"alg");
    p = cbor_nint(rsp, p, 7);
    p
}

fn handle_make_credential(req: &[u8], rsp: &mut [u8], session: Uuid, now_ms: u64) -> usize {
    audit::emit(AuditKind::Fido2Make, session, Uuid::NIL, 0, now_ms);

    let cdh = match cbor_map_int(req, 1).and_then(cbor_str_bytes) {
        Some(v) if v.len() == 32 => {
            let mut a = [0u8; 32];
            a.copy_from_slice(v);
            a
        }
        _ => {
            rsp[0] = CTAP2_ERR_MISSING_PARAM;
            return 1;
        }
    };
    let rp_val = match cbor_map_int(req, 2) {
        Some(v) => v,
        None => {
            rsp[0] = CTAP2_ERR_MISSING_PARAM;
            return 1;
        }
    };
    let rp_id_bytes = match cbor_map_text(rp_val, b"id").and_then(cbor_str_bytes) {
        Some(v) => v,
        None => {
            rsp[0] = CTAP2_ERR_MISSING_PARAM;
            return 1;
        }
    };

    if CRED_COUNT.load(Ordering::Acquire) >= MAX_CREDENTIALS {
        rsp[0] = CTAP2_ERR_NO_OPERATION;
        return 1;
    }

    let seed = random_seed();
    let (pk, xsk) = crypto::ed25519_sign::ed25519_keygen(&seed);

    let cred_id = {
        let mut buf = [0u8; 48];
        let rn = rp_id_bytes.len().min(16);
        buf[..rn].copy_from_slice(&rp_id_bytes[..rn]);
        buf[16..32].copy_from_slice(&seed[..16]);
        sha256(&buf)
    };
    let rp_hash = sha256(rp_id_bytes);

    {
        let mut creds = CREDS.lock();
        let slot = (0..MAX_CREDENTIALS)
            .find(|&i| !creds[i].active)
            .unwrap_or(0);
        use crate::graph::handles::GraphHandle;
        use crate::graph::types::{EdgeKind, NODE_ID_KERNEL};
        let gn = crate::graph::handles::register_fido_credential(NODE_ID_KERNEL);
        if gn.is_valid() {
            crate::graph::arena::add_edge(NODE_ID_KERNEL, gn.node_id(), EdgeKind::Owns, 0);
        }
        creds[slot] = Credential {
            id: cred_id,
            seed,
            pub_key: pk,
            rp_id_hash: rp_hash,
            sign_count: 0,
            active: true,
            graph_node: gn.node_id(),
        };
    }
    CRED_COUNT.fetch_add(1, Ordering::AcqRel);

    let mut ad = [0u8; 256];
    let al = build_auth_data_make(&rp_hash, 0, &cred_id, &pk, &mut ad);

    let sig = {
        let mut msg = [0u8; 288];
        msg[..al].copy_from_slice(&ad[..al]);
        msg[al..al + 32].copy_from_slice(&cdh);
        crypto::ed25519_sign::ed25519_sign(&xsk, &pk, &msg[..al + 32])
    };

    rsp[0] = CTAP2_OK;
    let mut p = 1;
    p = cbor_map(rsp, p, 3);
    p = cbor_uint(rsp, p, 1);
    p = cbor_tstr(rsp, p, b"packed");
    p = cbor_uint(rsp, p, 2);
    p = cbor_bstr(rsp, p, &ad[..al]);
    p = cbor_uint(rsp, p, 3);
    p = cbor_map(rsp, p, 2);
    p = cbor_tstr(rsp, p, b"alg");
    p = cbor_nint(rsp, p, 7);
    p = cbor_tstr(rsp, p, b"sig");
    p = cbor_bstr(rsp, p, &sig);
    p
}

fn handle_get_assertion(req: &[u8], rsp: &mut [u8], session: Uuid, now_ms: u64) -> usize {
    audit::emit(AuditKind::Fido2Assert, session, Uuid::NIL, 0, now_ms);

    let rp_id_bytes = match cbor_map_int(req, 1).and_then(cbor_str_bytes) {
        Some(v) => v,
        None => {
            rsp[0] = CTAP2_ERR_MISSING_PARAM;
            return 1;
        }
    };
    let cdh = match cbor_map_int(req, 2).and_then(cbor_str_bytes) {
        Some(v) if v.len() == 32 => {
            let mut a = [0u8; 32];
            a.copy_from_slice(v);
            a
        }
        _ => {
            rsp[0] = CTAP2_ERR_MISSING_PARAM;
            return 1;
        }
    };

    let rp_hash = sha256(rp_id_bytes);

    let cred = {
        let mut creds = CREDS.lock();
        let idx = match (0..MAX_CREDENTIALS)
            .find(|&i| creds[i].active && creds[i].rp_id_hash == rp_hash)
        {
            Some(i) => i,
            None => {
                rsp[0] = CTAP2_ERR_NO_CREDENTIALS;
                return 1;
            }
        };
        creds[idx].sign_count = creds[idx].sign_count.wrapping_add(1);
        creds[idx]
    };

    let mut ad = [0u8; 40];
    let al = build_auth_data_assert(&rp_hash, cred.sign_count, &mut ad);

    let (_pk, xsk) = crypto::ed25519_sign::ed25519_keygen(&cred.seed);
    let sig = {
        let mut msg = [0u8; 72];
        msg[..al].copy_from_slice(&ad[..al]);
        msg[al..al + 32].copy_from_slice(&cdh);
        crypto::ed25519_sign::ed25519_sign(&xsk, &cred.pub_key, &msg[..al + 32])
    };

    rsp[0] = CTAP2_OK;
    let mut p = 1;
    p = cbor_map(rsp, p, 3);
    p = cbor_uint(rsp, p, 1);
    p = cbor_bstr(rsp, p, &cred.id);
    p = cbor_uint(rsp, p, 2);
    p = cbor_bstr(rsp, p, &ad[..al]);
    p = cbor_uint(rsp, p, 3);
    p = cbor_bstr(rsp, p, &sig);
    p
}

// ── Public dispatch ───────────────────────────────────────────────────────────

/// Dispatch a raw CTAP2 HID message.  Returns bytes written to `rsp`.
pub fn dispatch(cmd: u8, req: &[u8], rsp: &mut [u8], session: Uuid, now_ms: u64) -> usize {
    if rsp.is_empty() {
        return 0;
    }
    match cmd {
        CTAP2_GET_INFO => handle_get_info(rsp),
        CTAP2_MAKE_CREDENTIAL => handle_make_credential(req, rsp, session, now_ms),
        CTAP2_GET_ASSERTION => handle_get_assertion(req, rsp, session, now_ms),
        CTAP2_RESET => {
            let mut creds = CREDS.lock();
            for c in creds.iter() {
                if c.active && c.graph_node != 0 {
                    crate::graph::arena::detach_node(c.graph_node);
                }
            }
            *creds = [Credential::empty(); MAX_CREDENTIALS];
            CRED_COUNT.store(0, Ordering::Release);
            rsp[0] = CTAP2_OK;
            1
        }
        _ => {
            rsp[0] = CTAP2_ERR_INVALID_CMD;
            1
        }
    }
}
