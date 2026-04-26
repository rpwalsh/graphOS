// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use graphos_tool_sdk::{public_key_from_seed32, sign_seed32, verify};

const TEST_SEED: [u8; 32] = [
    0x42, 0x8f, 0x1d, 0x7a, 0xe3, 0x11, 0x09, 0x5c, 0x2b, 0x9d, 0x70, 0x4f, 0x8a, 0x36, 0xcd, 0x12,
    0x7f, 0x44, 0x93, 0x22, 0x6a, 0xbc, 0xde, 0x01, 0x5a, 0x73, 0x18, 0xe4, 0x9c, 0x20, 0x6d, 0xb8,
];

const WRONG_SEED: [u8; 32] = [
    0x19, 0x23, 0xaa, 0x72, 0x51, 0x09, 0x64, 0x8e, 0xbc, 0x31, 0x02, 0x17, 0x4f, 0x55, 0xe0, 0x1a,
    0xd2, 0x44, 0x81, 0x33, 0x7a, 0x6b, 0x99, 0x0f, 0x2e, 0x71, 0x08, 0x6d, 0xcf, 0x30, 0x15, 0x9a,
];

fn fixture() -> ([u8; 32], Vec<u8>, [u8; 64]) {
    let payload = b"graphos-release-artifact-fixture-v1".to_vec();
    let pub_key = public_key_from_seed32(&TEST_SEED);
    let sig = sign_seed32(&TEST_SEED, &payload);
    (pub_key, payload, sig)
}

#[test]
fn host_sign_verify_success_case() {
    let (pub_key, payload, sig) = fixture();
    assert!(verify(&pub_key, &payload, &sig));
}

#[test]
fn host_sign_verify_tampered_payload_fails() {
    let (pub_key, mut payload, sig) = fixture();
    payload[0] ^= 0x5a;
    assert!(!verify(&pub_key, &payload, &sig));
}

#[test]
fn host_sign_verify_tampered_signature_fails() {
    let (pub_key, payload, mut sig) = fixture();
    sig[17] ^= 0xa5;
    assert!(!verify(&pub_key, &payload, &sig));
}

#[test]
fn host_sign_verify_wrong_public_key_fails() {
    let (_, payload, sig) = fixture();
    let wrong_pub = public_key_from_seed32(&WRONG_SEED);
    assert!(!verify(&wrong_pub, &payload, &sig));
}
