// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Kernel audit log — immutable append-only ring buffer of security events.
//!
//! All security-relevant syscalls (login, capability grant/revoke, exec,
//! mount, network open) emit an `AuditEvent` here.  The ring is drained by
//! the `audit-daemon` userspace process via a dedicated capability channel.
//!
//! ## Properties
//! - Lock-free single-producer (kernel), multi-reader drain path.
//! - No heap: fixed-size ring of `MAX_EVENTS` entries.
//! - Each event is timestamped in milliseconds since boot and tagged with a
//!   subject UUID and a 64-bit event-kind field.
//! - Events are never overwritten once written (overrun drops new events and
//!   increments `dropped_count`).

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::uuid::Uuid128 as Uuid;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Number of audit events kept in the kernel ring.
const MAX_EVENTS: usize = 1024;

// ── Event kinds ───────────────────────────────────────────────────────────────

/// Security-relevant event kinds.  Stable ABI — only append.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditKind {
    /// A user session was opened (login).
    Login = 0x0001,
    /// A user session was closed (logout).
    Logout = 0x0002,
    /// A capability token was granted.
    CapGrant = 0x0010,
    /// A capability token was revoked.
    CapRevoke = 0x0011,
    /// A capability check failed (access denied).
    CapDenied = 0x0012,
    /// A process was created (exec/spawn).
    ProcessCreate = 0x0020,
    /// A process exited.
    ProcessExit = 0x0021,
    /// A file was opened.
    FileOpen = 0x0030,
    /// A file was written.
    FileWrite = 0x0031,
    /// A file was deleted.
    FileDelete = 0x0032,
    /// A network socket was opened.
    NetOpen = 0x0040,
    /// A TPM PCR was extended.
    TpmPcrExtend = 0x0050,
    /// FIDO2 credential created.
    Fido2Make = 0x0060,
    /// FIDO2 assertion performed.
    Fido2Assert = 0x0061,
    /// Generic security policy violation.
    PolicyViolation = 0xFFFF,
}

// ── Event record ──────────────────────────────────────────────────────────────

/// A single audit event record.  32 bytes.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct AuditEvent {
    /// Milliseconds since boot.
    pub timestamp_ms: u64,
    /// Subject (process/session) UUID.
    pub subject: Uuid,
    /// Object UUID (resource acted upon), or `Uuid::NIL` if N/A.
    pub object: Uuid,
    /// Event kind.
    pub kind: u64,
    /// Additional context (errno, flags, etc.).
    pub detail: u64,
}

impl AuditEvent {
    const fn zero() -> Self {
        Self {
            timestamp_ms: 0,
            subject: Uuid::NIL,
            object: Uuid::NIL,
            kind: 0,
            detail: 0,
        }
    }
}

// ── Ring buffer ───────────────────────────────────────────────────────────────

struct AuditRing {
    events: [AuditEvent; MAX_EVENTS],
    /// Write cursor (never wraps; mod MAX_EVENTS for index).
    write: usize,
    /// Read cursor for drain path.
    read: usize,
    dropped: u64,
}

impl AuditRing {
    const fn new() -> Self {
        Self {
            events: [AuditEvent::zero(); MAX_EVENTS],
            write: 0,
            read: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, ev: AuditEvent) {
        let used = self.write.wrapping_sub(self.read);
        if used >= MAX_EVENTS {
            self.dropped += 1;
            return;
        }
        let idx = self.write % MAX_EVENTS;
        self.events[idx] = ev;
        self.write = self.write.wrapping_add(1);
    }

    /// Drain up to `buf.len()` events into `buf`.  Returns number of events written.
    fn drain(&mut self, buf: &mut [AuditEvent]) -> usize {
        let avail = self.write.wrapping_sub(self.read).min(buf.len());
        for slot in buf.iter_mut().take(avail) {
            *slot = self.events[self.read % MAX_EVENTS];
            self.read = self.read.wrapping_add(1);
        }
        avail
    }
}

static RING: Mutex<AuditRing> = Mutex::new(AuditRing::new());
static DROPPED: AtomicU64 = AtomicU64::new(0);

// ── Public API ────────────────────────────────────────────────────────────────

/// Emit a security audit event.
///
/// This is the primary hot-path function — it should be called on every
/// security boundary crossing.  Returns immediately if the ring is full
/// (event is counted in `dropped_count()`).
#[inline]
pub fn emit(kind: AuditKind, subject: Uuid, object: Uuid, detail: u64, now_ms: u64) {
    let ev = AuditEvent {
        timestamp_ms: now_ms,
        subject,
        object,
        kind: kind as u64,
        detail,
    };
    let mut ring = RING.lock();
    if ring.write.wrapping_sub(ring.read) >= MAX_EVENTS {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    ring.push(ev);
}

/// Drain up to `buf.len()` pending events.  Returns count drained.
pub fn drain(buf: &mut [AuditEvent]) -> usize {
    RING.lock().drain(buf)
}

/// Number of events dropped due to ring overflow.
pub fn dropped_count() -> u64 {
    DROPPED.load(Ordering::Relaxed) + RING.lock().dropped
}
