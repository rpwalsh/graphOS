// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! TLS capability flag — coarse-grained TLS readiness gate.
//!
//! graphOS does not yet implement a full in-kernel TLS 1.3 stack, but this
//! module provides the single source of truth for whether TLS is available
//! so that security-sensitive callers (OTA fetch, PKI, LDAP) can fail closed
//! when TLS is not ready rather than silently falling back to plaintext.
//!
//! ## Promotion path
//! 1. At boot, `is_available()` returns `false` (flag cleared).
//! 2. When a ring-3 `tlsd` service completes its handshake and calls
//!    `SYS_TLS_SET_AVAILABLE`, the flag is set to `true`.
//! 3. Callers that require TLS gate on `is_available()` and return an error
//!    if it is `false`, rather than performing a cleartext fallback.
//!
//! ## Why kernel-resident?
//! The flag must live in the kernel so that a compromised or absent ring-3
//! service cannot fool the kernel into accepting plaintext OTA bundles by
//! simply not calling the availability syscall.  The kernel sets the flag
//! only after verifying the handshake via a signed attestation token.

use core::sync::atomic::{AtomicBool, Ordering};

/// `true` once a verified TLS session layer is available for kernel use.
static TLS_AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Returns `true` if TLS is available for kernel-initiated connections.
///
/// Until this returns `true`, operations that require confidential transport
/// (OTA bundle fetch, PKI enrollment, LDAP lookups) must fail closed.
#[inline]
pub fn is_available() -> bool {
    TLS_AVAILABLE.load(Ordering::Acquire)
}

/// Mark TLS as available.
///
/// Called from `sys_tls_set_available` after the kernel verifies the ring-3
/// TLS service has completed a validated handshake.
///
/// # Safety requirements (enforced by syscall layer)
/// - Caller must be a `protected_strict` task (uid=0, MODE_PROTECTED_STRICT).
/// - The caller must present a valid attestation token (checked in syscall).
pub fn set_available() {
    TLS_AVAILABLE.store(true, Ordering::Release);
}

/// Mark TLS as unavailable (e.g. after the TLS service crashes).
pub fn set_unavailable() {
    TLS_AVAILABLE.store(false, Ordering::Release);
}
