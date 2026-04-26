// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! TPM 2.0 remote attestation.
//!
//! Implements the TPM2_Quote command to produce a PCR attestation quote
//! signed by the TPM's Attestation Identity Key (AIK).  The quote is
//! serialised as a UUID-keyed VFS node at `/sys/tpm/quote`.
//!
//! ## References
//! - TCG TPM 2.0 Library, Part 3 §18 (PCR), §24 (Hierarchy)
//! - TCG EK Credential Profile v2.3

use crate::tpm;

/// PCR selection for attestation (PCRs 0–7 covering firmware + boot path).
const ATTEST_PCR_MASK: u32 = 0x0000_00FF;

/// Maximum quote blob size returned by `TPM2_Quote`.
const MAX_QUOTE_SIZE: usize = 512;

/// The serialised attestation quote stored in the VFS.
pub struct AttestationQuote {
    /// Raw TPMS_ATTEST structure (marshalled).
    pub attest_bytes: [u8; MAX_QUOTE_SIZE],
    pub attest_len: usize,
    /// TPMT_SIGNATURE structure (marshalled).
    pub sig_bytes: [u8; 256],
    pub sig_len: usize,
    /// PCR values that were quoted.
    pub pcr_values: [[u8; 32]; 8],
    /// Which PCR values are valid (bitmask).
    pub pcr_mask: u32,
    /// Whether the quote was successfully obtained from the TPM.
    pub valid: bool,
}

impl AttestationQuote {
    const fn empty() -> Self {
        Self {
            attest_bytes: [0u8; MAX_QUOTE_SIZE],
            attest_len: 0,
            sig_bytes: [0u8; 256],
            sig_len: 0,
            pcr_values: [[0u8; 32]; 8],
            pcr_mask: 0,
            valid: false,
        }
    }
}

static mut CACHED_QUOTE: AttestationQuote = AttestationQuote::empty();

// ---------------------------------------------------------------------------
// TPM2_Quote wrapper
// ---------------------------------------------------------------------------

/// Perform a TPM2_Quote of the selected PCRs.
///
/// `qualifying_data` is up to 32 bytes of nonce/challenge data included in
/// the quote to prevent replay attacks.  Returns `true` if the quote was
/// obtained successfully.
pub fn generate_quote(qualifying_data: &[u8]) -> bool {
    // Read current PCR values first.
    let mut pcr_values = [[0u8; 32]; 8];
    for i in 0..8u32 {
        if ATTEST_PCR_MASK & (1 << i) != 0 {
            let val = tpm::pcr_read(i as u8);
            pcr_values[i as usize] = val;
        }
    }

    // Call TPM2_Quote.
    let mut attest_buf = [0u8; MAX_QUOTE_SIZE];
    let mut sig_buf = [0u8; 256];
    let (attest_len, sig_len) = tpm::tpm2_quote(
        qualifying_data,
        ATTEST_PCR_MASK,
        &mut attest_buf,
        &mut sig_buf,
    );

    let valid = attest_len > 0 && sig_len > 0;

    // Cache the result.
    // SAFETY: called at most once during boot, before any concurrent access.
    unsafe {
        CACHED_QUOTE.attest_bytes[..attest_len].copy_from_slice(&attest_buf[..attest_len]);
        CACHED_QUOTE.attest_len = attest_len;
        CACHED_QUOTE.sig_bytes[..sig_len].copy_from_slice(&sig_buf[..sig_len]);
        CACHED_QUOTE.sig_len = sig_len;
        CACHED_QUOTE.pcr_values = pcr_values;
        CACHED_QUOTE.pcr_mask = ATTEST_PCR_MASK;
        CACHED_QUOTE.valid = valid;
    }

    if valid {
        crate::arch::serial::write_line(b"[tpm/attest] quote generated");
    } else {
        crate::arch::serial::write_line(b"[tpm/attest] quote failed");
    }
    valid
}

/// Return a reference to the cached attestation quote.
///
/// # Safety
/// The caller must ensure no concurrent mutation (only safe to call after
/// `generate_quote` has returned and before any subsequent `generate_quote`
/// call — both guaranteed during single-threaded boot).
pub fn cached_quote() -> &'static AttestationQuote {
    unsafe { &*core::ptr::addr_of!(CACHED_QUOTE) }
}

/// Verify that the attestation quote was signed by the TPM's AIK.
///
/// `aik_pub` is the 32-byte AIK public key (Ed25519 for the software-attestation
/// fallback path; ECC-P256 / RSA2048 for real hardware TPMs, which is a v1.1 concern).
pub fn verify_quote(aik_pub: &[u8; 32]) -> bool {
    verify_quote_from(unsafe { &*core::ptr::addr_of!(CACHED_QUOTE) }, aik_pub)
}

/// Verify quote bytes directly — testable without depending on the global cache.
fn verify_quote_from(q: &AttestationQuote, aik_pub: &[u8; 32]) -> bool {
    if !q.valid || q.sig_len < 64 {
        return false;
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&q.sig_bytes[..64]);
    crate::crypto::ed25519::verify(aik_pub, &q.attest_bytes[..q.attest_len], &sig)
}

/// Full attestation quote verification: signature + nonce anti-replay + PCR mask.
///
/// In addition to signature verification this function:
/// - Confirms that `nonce` bytes appear within `attest_bytes` at the expected
///   offset (TCG TPMS_ATTEST qualifyingData field at byte offset 10 for the
///   GraphOS software-attestation format; offset 8 for hardware path after the
///   2-byte qualifiedSigner size field when signer name is empty).
/// - Confirms that the PCR selection bitmask stored in the quote equals
///   `expected_pcr_mask` (guards against a signer substituting a different
///   PCR set than the verifier intended).
///
/// Returns `true` only when all three checks pass.
pub fn verify_quote_full(aik_pub: &[u8; 32], nonce: &[u8], expected_pcr_mask: u32) -> bool {
    let q = unsafe { &*core::ptr::addr_of!(CACHED_QUOTE) };
    verify_quote_full_from(q, aik_pub, nonce, expected_pcr_mask)
}

fn verify_quote_full_from(
    q: &AttestationQuote,
    aik_pub: &[u8; 32],
    nonce: &[u8],
    expected_pcr_mask: u32,
) -> bool {
    // 1. Signature check.
    if !verify_quote_from(q, aik_pub) {
        return false;
    }

    let data = &q.attest_bytes[..q.attest_len];

    // 2. Nonce anti-replay: qualifying_data starts at byte 10 in the GraphOS
    //    software-attestation format (magic[4]+type[2]+signerSize[2]+qdSize[2]).
    //    Verify the nonce bytes are present there.
    let qd_size_offset = 8usize;
    if data.len() < qd_size_offset + 2 {
        return false;
    }
    let qd_len = u16::from_be_bytes([data[qd_size_offset], data[qd_size_offset + 1]]) as usize;
    let qd_start = qd_size_offset + 2;
    if data.len() < qd_start + qd_len {
        return false;
    }
    let stored_nonce = &data[qd_start..qd_start + qd_len];
    let nonce_match_len = nonce.len().min(qd_len);
    if nonce_match_len == 0 || stored_nonce[..nonce_match_len] != nonce[..nonce_match_len] {
        return false;
    }

    // 3. PCR mask check: PCR selection bitmask is at a fixed offset after the
    //    variable-length nonce: clock(17)+firmware(8)+count(4)+hashAlg(2)+szSel(1) = 32
    //    bytes after the end of the nonce field.
    let pcr_mask_offset = qd_start + qd_len + 32;
    if data.len() < pcr_mask_offset + 3 {
        return false;
    }
    let stored_mask: u32 = (data[pcr_mask_offset] as u32)
        | ((data[pcr_mask_offset + 1] as u32) << 8)
        | ((data[pcr_mask_offset + 2] as u32) << 16);
    stored_mask == expected_pcr_mask
}

// ---------------------------------------------------------------------------
// Hostile attestation tests
// ---------------------------------------------------------------------------
//
// These tests exercise the software-attestation code path directly using
// constructed AttestationQuote values with known Ed25519 key material.
// They are gated behind cfg(test) and run as part of the integration test
// suite (sdk/tool-sdk/tests/tpm_attestation_hostile.rs).

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 8037 §2 test vector 1 (empty message):
    // seed (private scalar):
    //   9d61b19deffd5a60ba844af492ec2a7f6614e4b6f29f9ef46aeb01b112054b11
    // public key:
    //   d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
    const TEST_AIK_SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2a,
        0x7f, 0x66, 0x14, 0xe4, 0xb6, 0xf2, 0x9f, 0x9e, 0xf4, 0x6a, 0xeb, 0x01, 0xb1, 0x12, 0x05,
        0x4b, 0x11,
    ];
    const TEST_AIK_PUB: [u8; 32] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];

    /// Build a minimal AttestationQuote signed with TEST_AIK_SEED.
    fn make_quote(nonce: &[u8], pcr_mask: u32, pcr_vals: &[[u8; 32]; 8]) -> AttestationQuote {
        let qd_len = nonce.len().min(32);
        let mut ab = [0u8; MAX_QUOTE_SIZE];
        let mut pos = 0usize;
        // magic
        ab[pos..pos + 4].copy_from_slice(&0xFF544347u32.to_be_bytes());
        pos += 4;
        // type
        ab[pos..pos + 2].copy_from_slice(&0x8018u16.to_be_bytes());
        pos += 2;
        // qualifiedSigner size = 0
        ab[pos..pos + 2].copy_from_slice(&0u16.to_be_bytes());
        pos += 2;
        // qualifyingData
        ab[pos..pos + 2].copy_from_slice(&(qd_len as u16).to_be_bytes());
        pos += 2;
        ab[pos..pos + qd_len].copy_from_slice(&nonce[..qd_len]);
        pos += qd_len;
        // clockInfo(17) + firmwareVersion(8)
        pos += 25;
        // TPML_PCR_SELECTION: count=1
        ab[pos..pos + 4].copy_from_slice(&1u32.to_be_bytes());
        pos += 4;
        // hashAlg=SHA256, sizeofSelect=3
        ab[pos..pos + 2].copy_from_slice(&0x000Bu16.to_be_bytes());
        pos += 2;
        ab[pos] = 3;
        pos += 1;
        ab[pos] = (pcr_mask & 0xFF) as u8;
        ab[pos + 1] = ((pcr_mask >> 8) & 0xFF) as u8;
        ab[pos + 2] = ((pcr_mask >> 16) & 0xFF) as u8;
        pos += 3;
        // PCR digest (SHA-256 of selected PCR values in index order)
        let digest = compute_pcr_digest(&pcr_mask, pcr_vals);
        ab[pos..pos + 2].copy_from_slice(&32u16.to_be_bytes());
        pos += 2;
        ab[pos..pos + 32].copy_from_slice(&digest);
        pos += 32;
        let alen = pos;

        // Sign with TEST_AIK_SEED
        let (pk, xsk) = crate::crypto::ed25519_sign::ed25519_keygen(&TEST_AIK_SEED);
        let _ = pk;
        let sig = crate::crypto::ed25519_sign::ed25519_sign(&xsk, &TEST_AIK_PUB, &ab[..alen]);
        let mut sb = [0u8; 256];
        sb[..64].copy_from_slice(&sig);

        AttestationQuote {
            attest_bytes: ab,
            attest_len: alen,
            sig_bytes: sb,
            sig_len: 64,
            pcr_values: *pcr_vals,
            pcr_mask,
            valid: true,
        }
    }

    fn compute_pcr_digest(mask: &u32, vals: &[[u8; 32]; 8]) -> [u8; 32] {
        let mut buf = [0u8; 256];
        let mut n = 0usize;
        for i in 0..8u32 {
            if mask & (1 << i) != 0 {
                buf[n..n + 32].copy_from_slice(&vals[i as usize]);
                n += 32;
            }
        }
        crate::crypto::sha256::hash(&buf[..n])
    }

    #[test]
    fn valid_quote_passes() {
        let nonce = b"test-nonce-0001";
        let pcr_mask = ATTEST_PCR_MASK;
        let pcr_vals = [[0u8; 32]; 8];
        let q = make_quote(nonce, pcr_mask, &pcr_vals);
        assert!(
            verify_quote_from(&q, &TEST_AIK_PUB),
            "valid quote must verify"
        );
        assert!(
            verify_quote_full_from(&q, &TEST_AIK_PUB, nonce, pcr_mask),
            "full verify of valid quote must pass"
        );
    }

    #[test]
    fn tampered_attest_bytes_fails() {
        let nonce = b"test-nonce-0002";
        let pcr_mask = ATTEST_PCR_MASK;
        let pcr_vals = [[0u8; 32]; 8];
        let mut q = make_quote(nonce, pcr_mask, &pcr_vals);
        // Flip one bit in the attested data (after the signature was computed).
        q.attest_bytes[4] ^= 0x01;
        assert!(
            !verify_quote_from(&q, &TEST_AIK_PUB),
            "tampered attest_bytes must fail sig check"
        );
    }

    #[test]
    fn wrong_signer_fails() {
        let nonce = b"test-nonce-0003";
        let pcr_mask = ATTEST_PCR_MASK;
        let pcr_vals = [[0u8; 32]; 8];
        let q = make_quote(nonce, pcr_mask, &pcr_vals);
        // Use a different public key (all-zeros is not a valid Ed25519 key).
        let wrong_pub = [0u8; 32];
        assert!(!verify_quote_from(&q, &wrong_pub), "wrong signer must fail");
    }

    #[test]
    fn mismatched_pcr_mask_fails() {
        let nonce = b"test-nonce-0004";
        let pcr_mask = ATTEST_PCR_MASK;
        let pcr_vals = [[0u8; 32]; 8];
        let q = make_quote(nonce, pcr_mask, &pcr_vals);
        // Expect a different PCR mask than what was quoted.
        let wrong_mask = pcr_mask ^ 0x01;
        assert!(
            !verify_quote_full_from(&q, &TEST_AIK_PUB, nonce, wrong_mask),
            "mismatched PCR mask must fail full verification"
        );
    }
}
