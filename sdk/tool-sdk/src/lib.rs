// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Tool SDK — build helpers, manifest schema, signing utilities.
//!
//! Provides the host-side types and helpers used by `gpm` and third-party
//! build toolchains to create, sign, and validate `.gpkg` / `.gapp` bundles.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

// ---------------------------------------------------------------------------
// Package manifest
// ---------------------------------------------------------------------------

/// Package manifest — the canonical description of a `.gpkg` bundle.
#[derive(Debug, Clone)]
pub struct PackageManifest {
    /// Package name (e.g. "graphos-terminal").
    pub name: String,
    /// Semver version string.
    pub version: String,
    /// UUID v5 of the package (name-derived).
    pub uuid: [u8; 16],
    /// ed25519 public key of the signing authority.
    pub pub_key: [u8; 32],
    /// List of dependency package names.
    pub deps: Vec<String>,
    /// List of files included in the archive (path → SHA-256).
    pub files: Vec<FileEntry>,
}

/// A single file entry within a package manifest.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Archive-relative path.
    pub path: String,
    /// SHA-256 digest (hex string).
    pub digest: String,
    /// File size in bytes.
    pub size: u64,
}

// ---------------------------------------------------------------------------
// App manifest (.gapp)
// ---------------------------------------------------------------------------

/// App manifest — the canonical description of a `.gapp` WASM bundle.
#[derive(Debug, Clone)]
pub struct AppManifest {
    /// App name.
    pub name: String,
    /// Semver version string.
    pub version: String,
    /// UUID v5 of the app.
    pub uuid: [u8; 16],
    /// Declared capability set (bitmask).
    pub caps: u32,
    /// ed25519 public key of the signing authority.
    pub pub_key: [u8; 32],
}

// ---------------------------------------------------------------------------
// Signing helpers
// ---------------------------------------------------------------------------

/// Sign `message` with `secret_key` (ed25519) and return the 64-byte signature.
///
/// The signing key is the 64-byte expanded form historically used by the
/// GraphOS SDK. Only the first 32 bytes are required by Ed25519; the trailing
/// 32 bytes are ignored for compatibility with older callers.
pub fn sign(secret_key: &[u8; 64], message: &[u8]) -> [u8; 64] {
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret_key[..32]);
    sign_seed32(&seed, message)
}

/// Verify an ed25519 `signature` over `message` using `public_key`.
///
/// Returns `true` if the signature is valid.
pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    let sig = Signature::from_bytes(signature);
    verifying_key.verify(message, &sig).is_ok()
}

/// Sign `message` using a 32-byte Ed25519 seed.
pub fn sign_seed32(secret_seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let signing_key = SigningKey::from_bytes(secret_seed);
    signing_key.sign(message).to_bytes()
}

/// Derive the 32-byte public key corresponding to an Ed25519 signing seed.
pub fn public_key_from_seed32(secret_seed: &[u8; 32]) -> [u8; 32] {
    let signing_key = SigningKey::from_bytes(secret_seed);
    signing_key.verifying_key().to_bytes()
}

// ---------------------------------------------------------------------------
// Manifest serialisation helpers (minimal, no_std-friendly)
// ---------------------------------------------------------------------------

/// Encode a `PackageManifest` as a minimal binary blob (not JSON — avoids
/// serde dependency in SDK).  Format: length-prefixed fields.
pub fn encode_manifest(m: &PackageManifest) -> Vec<u8> {
    let mut out = Vec::new();
    write_str(&mut out, &m.name);
    write_str(&mut out, &m.version);
    out.extend_from_slice(&m.uuid);
    out.extend_from_slice(&m.pub_key);
    out
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}
