// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
/// Integration-level hostile tests for TPM software-attestation verify_quote_full logic.
///
/// These tests run on the host (std Rust) using ed25519-dalek to construct
/// software-format attestation quotes and verify that the attestation layer
/// accepts valid quotes and rejects all hostile mutations.
///
/// The software-attestation quote format (TPMS_ATTEST subset):
///   [0..4]   magic     = 0xff544347 (GraphOS)
///   [4..6]   type      = 0x8018 (TPM2_ST_ATTEST_QUOTE)
///   [6..8]   signerSize
///   [8..10]  qdSize    (qualifying data / nonce length)
///   [10..]   qd bytes
///   [10+qd_len..] clock[17] + fw[8] + count[4] + hashAlg[2] + szSel[1]
///                 + pcr_mask[3] + digestSize[2] + digest[32]
///
/// The sig is stored as a raw 64-byte Ed25519 signature over attest_bytes.
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

// ── TPMS_ATTEST builder ───────────────────────────────────────────────────────

struct QuoteBuilder {
    nonce: Vec<u8>,
    pcr_mask: [u8; 3],
    digest: [u8; 32],
}

impl QuoteBuilder {
    fn new(nonce: &[u8], pcr_mask: [u8; 3], digest: [u8; 32]) -> Self {
        Self {
            nonce: nonce.to_vec(),
            pcr_mask,
            digest,
        }
    }

    /// Serialise the TPMS_ATTEST structure and sign it with `key`.
    /// Returns (attest_bytes, sig_bytes, pubkey_bytes).
    fn build(&self, key: &SigningKey) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut attest = Vec::new();
        // magic
        attest.extend_from_slice(&0xff544347u32.to_be_bytes());
        // type
        attest.extend_from_slice(&0x8018u16.to_be_bytes());
        // signerSize (0 — software path)
        attest.extend_from_slice(&0u16.to_be_bytes());
        // qdSize
        attest.extend_from_slice(&(self.nonce.len() as u16).to_be_bytes());
        // qd (nonce)
        attest.extend_from_slice(&self.nonce);
        // clock (17 bytes placeholder)
        attest.extend_from_slice(&[0u8; 17]);
        // fw version (8 bytes)
        attest.extend_from_slice(&[0u8; 8]);
        // PCR count (4 bytes, big-endian = 1)
        attest.extend_from_slice(&1u32.to_be_bytes());
        // hashAlg (SHA-256 = 0x000b)
        attest.extend_from_slice(&0x000bu16.to_be_bytes());
        // szSel
        attest.push(3);
        // PCR selection mask (3 bytes)
        attest.extend_from_slice(&self.pcr_mask);
        // digestSize (32)
        attest.extend_from_slice(&32u16.to_be_bytes());
        // PCR composite digest
        attest.extend_from_slice(&self.digest);

        let sig: ed25519_dalek::Signature = key.sign(&attest);
        let sig_bytes = sig.to_bytes().to_vec();
        let pub_bytes = key.verifying_key().to_bytes().to_vec();
        (attest, sig_bytes, pub_bytes)
    }
}

// ── Software-attestation quote format used by the kernel verify path ─────────
// We replicate the CachedQuote layout here so the tests are self-contained.

#[derive(Clone)]
struct SoftwareQuote {
    attest_bytes: [u8; 256],
    attest_len: usize,
    sig_bytes: [u8; 64],
    valid: bool,
}

impl SoftwareQuote {
    fn from_build(attest: &[u8], sig: &[u8]) -> Self {
        assert!(attest.len() <= 256, "attest too large for test fixture");
        assert_eq!(sig.len(), 64);
        let mut q = Self {
            attest_bytes: [0u8; 256],
            attest_len: attest.len(),
            sig_bytes: [0u8; 64],
            valid: true,
        };
        q.attest_bytes[..attest.len()].copy_from_slice(attest);
        q.sig_bytes.copy_from_slice(sig);
        q
    }
}

// ── Verify logic (mirrors kernel verify_quote_full_from) ─────────────────────

fn verify_quote_full(
    q: &SoftwareQuote,
    pub_key: &[u8; 32],
    nonce: &[u8],
    expected_pcr_mask: [u8; 3],
) -> bool {
    if !q.valid || q.attest_len < 12 {
        return false;
    }
    let attest = &q.attest_bytes[..q.attest_len];

    // 1. Ed25519 signature check
    let vk = match VerifyingKey::from_bytes(pub_key) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let sig_arr: [u8; 64] = q.sig_bytes;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
    if vk.verify_strict(attest, &sig).is_err() {
        return false;
    }

    // 2. Nonce anti-replay: qdSize at [8..10], qd at [10..10+qdSize]
    let qd_len = u16::from_be_bytes([attest[8], attest[9]]) as usize;
    if attest.len() < 10 + qd_len {
        return false;
    }
    let stored_nonce = &attest[10..10 + qd_len];
    let match_len = nonce.len().min(stored_nonce.len());
    if stored_nonce[..match_len] != nonce[..match_len] {
        return false;
    }

    // 3. PCR mask check: offset = 10 + qd_len + 17 + 8 + 4 + 2 + 1 = 10 + qd_len + 32
    let mask_off = 10 + qd_len + 32;
    if attest.len() < mask_off + 3 {
        return false;
    }
    let got_mask = [attest[mask_off], attest[mask_off + 1], attest[mask_off + 2]];
    got_mask == expected_pcr_mask
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn test_key_seed() -> [u8; 32] {
    // RFC 8037 §2 test vector seed (well-known, no secret value)
    [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0x44, 0xda, 0x4b, 0x06, 0x2b, 0x11, 0x5c, 0x7f, 0x59, 0x5f, 0xac, 0x32, 0x4f, 0x84, 0x05,
        0x2d, 0xef,
    ]
}

fn build_key() -> SigningKey {
    SigningKey::from_bytes(&test_key_seed())
}

fn make_quote(nonce: &[u8], pcr_mask: [u8; 3]) -> (SoftwareQuote, [u8; 32]) {
    let key = build_key();
    let digest = [0xabu8; 32];
    let qb = QuoteBuilder::new(nonce, pcr_mask, digest);
    let (attest, sig, pub_bytes) = qb.build(&key);
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pub_bytes);
    (SoftwareQuote::from_build(&attest, &sig), pk)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn valid_quote_passes() {
    let nonce = b"test-nonce-12345";
    let pcr_mask = [0x01, 0x00, 0x00];
    let (q, pk) = make_quote(nonce, pcr_mask);
    assert!(
        verify_quote_full(&q, &pk, nonce, pcr_mask),
        "valid quote must pass"
    );
}

#[test]
fn tampered_attest_bytes_fails() {
    let nonce = b"test-nonce-12345";
    let pcr_mask = [0x01, 0x00, 0x00];
    let (mut q, pk) = make_quote(nonce, pcr_mask);
    // Flip a byte inside the attest body (after the magic header)
    q.attest_bytes[10] ^= 0xff;
    assert!(
        !verify_quote_full(&q, &pk, nonce, pcr_mask),
        "tampered attest must fail"
    );
}

#[test]
fn wrong_signer_fails() {
    let nonce = b"test-nonce-12345";
    let pcr_mask = [0x01, 0x00, 0x00];
    let (q, _pk) = make_quote(nonce, pcr_mask);
    // Use a different key as the expected signer
    let other_seed = [0x42u8; 32];
    let other_key = SigningKey::from_bytes(&other_seed);
    let other_pk = other_key.verifying_key().to_bytes();
    assert!(
        !verify_quote_full(&q, &other_pk, nonce, pcr_mask),
        "wrong signer must fail"
    );
}

#[test]
fn mismatched_pcr_mask_fails() {
    let nonce = b"test-nonce-12345";
    let signed_mask = [0x01, 0x00, 0x00];
    let expected_mask = [0xff, 0x00, 0x00]; // different from what was signed
    let (q, pk) = make_quote(nonce, signed_mask);
    assert!(
        !verify_quote_full(&q, &pk, nonce, expected_mask),
        "wrong PCR mask must fail"
    );
}

#[test]
fn wrong_nonce_fails() {
    let nonce = b"test-nonce-12345";
    let wrong_nonce = b"WRONG-nonce-9999";
    let pcr_mask = [0x01, 0x00, 0x00];
    let (q, pk) = make_quote(nonce, pcr_mask);
    assert!(
        !verify_quote_full(&q, &pk, wrong_nonce, pcr_mask),
        "wrong nonce must fail"
    );
}

#[test]
fn invalid_valid_flag_fails() {
    let nonce = b"test-nonce-12345";
    let pcr_mask = [0x01, 0x00, 0x00];
    let (mut q, pk) = make_quote(nonce, pcr_mask);
    q.valid = false;
    assert!(
        !verify_quote_full(&q, &pk, nonce, pcr_mask),
        "invalid flag must fail"
    );
}
