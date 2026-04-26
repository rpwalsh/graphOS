// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! WPA2-PSK PMK derivation and EAPOL 4-way handshake.
//!
//! PMK derivation uses PBKDF2-HMAC-SHA256(passphrase, ssid, 4096, 32).
//! The EAPOL 4-way handshake derives PTK from PMK + nonces and derives
//! GTK for multicast/broadcast decryption.
//!
//! In this initial revision the HAL functions (send/receive EAPOL frames)
//! are stubs; the crypto (PMK derivation) is fully implemented.

// ---------------------------------------------------------------------------
// SHA-256 primitive (re-uses the kernel's existing crypto module)
// ---------------------------------------------------------------------------

/// Compute SHA-256 of `data`.
fn sha256(data: &[u8]) -> [u8; 32] {
    // Forward to the kernel SHA-256 in crypto::ed25519 (which internally uses
    // the same SHA-512 core).  We expose a standalone SHA-256 entry point.
    crate::crypto::sha256(data)
}

/// HMAC-SHA256(key, data).
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = sha256(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] = k[i] ^ 0x36;
        opad[i] = k[i] ^ 0x5c;
    }
    // inner = SHA256(ipad || data)
    let inner = {
        let mut buf = [0u8; BLOCK + 256];
        buf[..BLOCK].copy_from_slice(&ipad);
        let data_len = data.len().min(256);
        buf[BLOCK..BLOCK + data_len].copy_from_slice(&data[..data_len]);
        sha256(&buf[..BLOCK + data_len])
    };
    // outer = SHA256(opad || inner)
    let mut obuf = [0u8; BLOCK + 32];
    obuf[..BLOCK].copy_from_slice(&opad);
    obuf[BLOCK..].copy_from_slice(&inner);
    sha256(&obuf)
}

// ---------------------------------------------------------------------------
// PBKDF2-HMAC-SHA256
// ---------------------------------------------------------------------------

/// PBKDF2-HMAC-SHA256(password, salt, iterations, 32).
///
/// Used to derive the WPA2 PMK from a passphrase and SSID.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8; 32]) {
    // Single block (32 bytes output): U1 = HMAC(P, S || 0x00000001)
    let mut salt_block = [0u8; 36 + 4]; // max SSID 32 bytes + 4-byte int
    let salt_len = salt.len().min(32);
    salt_block[..salt_len].copy_from_slice(&salt[..salt_len]);
    salt_block[salt_len] = 0;
    salt_block[salt_len + 1] = 0;
    salt_block[salt_len + 2] = 0;
    salt_block[salt_len + 3] = 1;
    let mut u = hmac_sha256(password, &salt_block[..salt_len + 4]);
    let mut accum = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for i in 0..32 {
            accum[i] ^= u[i];
        }
    }
    *out = accum;
}

/// Derive WPA2 PMK from `passphrase` (8–63 bytes) and SSID.
///
/// Returns the 256-bit (32-byte) PMK.
pub fn derive_pmk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut pmk = [0u8; 32];
    pbkdf2_sha256(passphrase, ssid, 4096, &mut pmk);
    pmk
}

// ---------------------------------------------------------------------------
// PTK derivation  (PRF-512 per IEEE 802.11-2016 12.7.1.2)
// ---------------------------------------------------------------------------

/// PTK label used in IEEE 802.11.
const PTK_LABEL: &[u8] = b"Pairwise key expansion";

/// Derive PTK from PMK, two MAC addresses, and two nonces.
///
/// Returns the first 48 bytes of the PRF-512 output (TK1+TK2 for CCMP).
pub fn derive_ptk(
    pmk: &[u8; 32],
    mac_a: &[u8; 6],
    mac_b: &[u8; 6],
    nonce_a: &[u8; 32],
    nonce_b: &[u8; 32],
) -> [u8; 48] {
    // Concatenate inputs in the order specified by 802.11.
    let mut data = [0u8; 6 + 6 + 32 + 32];
    // min/max ordering of the MACs.
    let (mmin, mmax) = if mac_a <= mac_b {
        (mac_a, mac_b)
    } else {
        (mac_b, mac_a)
    };
    data[0..6].copy_from_slice(mmin);
    data[6..12].copy_from_slice(mmax);
    // min/max ordering of the nonces.
    let (nmin, nmax) = if nonce_a <= nonce_b {
        (nonce_a, nonce_b)
    } else {
        (nonce_b, nonce_a)
    };
    data[12..44].copy_from_slice(nmin);
    data[44..76].copy_from_slice(nmax);

    // PRF-512: two 256-bit HMAC-SHA256 rounds with counter suffix 0 and 1.
    let mut ptk = [0u8; 48];
    let r0 = prf_block(pmk, PTK_LABEL, &data, 0);
    let r1 = prf_block(pmk, PTK_LABEL, &data, 1);
    ptk[..32].copy_from_slice(&r0);
    ptk[32..48].copy_from_slice(&r1[..16]);
    ptk
}

fn prf_block(key: &[u8], label: &[u8], data: &[u8], counter: u8) -> [u8; 32] {
    // HMAC-SHA256(key, label || 0x00 || data || counter)
    let mut buf = [0u8; 64 + 76 + 2];
    let label_len = label.len().min(64);
    let data_len = data.len().min(76);
    buf[..label_len].copy_from_slice(&label[..label_len]);
    buf[label_len] = 0x00;
    buf[label_len + 1..label_len + 1 + data_len].copy_from_slice(&data[..data_len]);
    buf[label_len + 1 + data_len] = counter;
    hmac_sha256(key, &buf[..label_len + 2 + data_len])
}
