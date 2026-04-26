// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Kernel security audit subsystem.
//!
//! Provides a lock-free, ring-buffered audit trail for security-critical
//! syscall events.  The ring is never-allocating, always-on, and wraps on
//! overflow — recent events are kept, oldest events are silently dropped
//! (like a flight-recorder / dmesg ring).
//!
//! ## Design goals
//! - Zero-allocation: fixed ring of `AuditRecord` values on a static.
//! - Lock-free writes (single producer / single consumer acceptable; kernel
//!   is nominally single-core during early boot; SpinMutex used on SMP).
//! - Privileged-reader syscall (`SYS_AUDIT_READ`) drains the ring to a
//!   caller-supplied buffer.
//! - Every record carries: event type, caller task index, syscall number,
//!   result (allow / deny), 32-byte opaque context bytes, and a TSC timestamp.

use spin::Mutex;

// ---------------------------------------------------------------------------
// Ring capacity — must be a power of two for cheap modulo via mask.
// ---------------------------------------------------------------------------

const RING_CAPACITY: usize = 512;
const RING_MASK: usize = RING_CAPACITY - 1;

// ---------------------------------------------------------------------------
// Audit event types
// ---------------------------------------------------------------------------

/// Discriminator for security-relevant audit events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AuditEvent {
    /// A syscall was blocked by seccomp policy.
    SeccompDeny = 0x01,
    /// `SYS_SETUID` was called (allowed or denied).
    Setuid = 0x02,
    /// `SYS_LOGIN` was called.
    Login = 0x03,
    /// `SYS_LOGOUT` was called.
    Logout = 0x04,
    /// `SYS_IPC_CAP_GRANT` was called.
    CapGrant = 0x05,
    /// `SYS_IPC_CAP_REVOKE` was called.
    CapRevoke = 0x06,
    /// `SYS_DRIVER_INSTALL` was called.
    DriverInstall = 0x07,
    /// `SYS_MOUNT` was called.
    Mount = 0x08,
    /// Pointer validation rejected a user-supplied address range.
    BadPointer = 0x09,
    /// Unknown / generic security event.
    Generic = 0xFF,
}

// ---------------------------------------------------------------------------
// Record layout
// ---------------------------------------------------------------------------

/// One audit record.  Fixed size so the ring stores them inline.
///
/// Total size: 56 bytes.
#[derive(Clone, Copy)]
pub struct AuditRecord {
    /// TSC value at time of emission. Zero if TSC unavailable.
    pub tsc: u64,
    /// Calling task index (`usize` truncated to u32).
    pub task_index: u32,
    /// Syscall number (0 for non-syscall events).
    pub syscall_nr: u32,
    /// Audit event type.
    pub event: AuditEvent,
    /// 1 = allowed / success, 0 = denied / failure.
    pub allowed: u8,
    /// Padding for alignment.
    _pad: [u8; 2],
    /// Arbitrary context bytes (e.g. path prefix, UID arguments).
    pub context: [u8; 32],
    /// Second u64 reserved for future use (e.g. session UUID low bits).
    pub aux: u64,
}

impl AuditRecord {
    const EMPTY: Self = Self {
        tsc: 0,
        task_index: 0,
        syscall_nr: 0,
        event: AuditEvent::Generic,
        allowed: 0,
        _pad: [0u8; 2],
        context: [0u8; 32],
        aux: 0,
    };
}

// ---------------------------------------------------------------------------
// Ring buffer
// ---------------------------------------------------------------------------

struct AuditRing {
    records: [AuditRecord; RING_CAPACITY],
    /// Write cursor: next slot to write into (wraps via RING_MASK).
    write: usize,
    /// Read cursor: oldest unread record.
    read: usize,
    /// Count of records dropped due to ring-full (reported to reader).
    dropped: u64,
}

impl AuditRing {
    const fn new() -> Self {
        Self {
            records: [AuditRecord::EMPTY; RING_CAPACITY],
            write: 0,
            read: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, rec: AuditRecord) {
        let slot = self.write & RING_MASK;
        self.records[slot] = rec;
        self.write = self.write.wrapping_add(1);

        // If write lapped read, advance read (oldest dropped).
        let used = self.write.wrapping_sub(self.read);
        if used > RING_CAPACITY {
            self.read = self.write.wrapping_sub(RING_CAPACITY);
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Drain up to `max` records into `out`.  Returns (n_copied, total_dropped).
    fn drain(&mut self, out: &mut [AuditRecord]) -> (usize, u64) {
        let available = self.write.wrapping_sub(self.read).min(out.len());
        for (i, record) in out[..available].iter_mut().enumerate() {
            let slot = (self.read.wrapping_add(i)) & RING_MASK;
            *record = self.records[slot];
        }
        self.read = self.read.wrapping_add(available);
        let dropped = self.dropped;
        self.dropped = 0;
        (available, dropped)
    }

    fn len(&self) -> usize {
        self.write.wrapping_sub(self.read).min(RING_CAPACITY)
    }
}

static RING: Mutex<AuditRing> = Mutex::new(AuditRing::new());

// ---------------------------------------------------------------------------
// TSC helper
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_tsc() -> u64 {
    0
}

// ---------------------------------------------------------------------------
// Public emit API
// ---------------------------------------------------------------------------

/// Emit an audit record.
///
/// `context` may be up to 32 bytes of caller-supplied data (e.g. a path
/// fragment, a UID, or an argument value).  Excess bytes are silently
/// truncated.
pub fn emit(event: AuditEvent, task_index: usize, syscall_nr: u64, allowed: bool, context: &[u8]) {
    let mut rec = AuditRecord::EMPTY;
    rec.tsc = read_tsc();
    rec.task_index = task_index as u32;
    rec.syscall_nr = syscall_nr as u32;
    rec.event = event;
    rec.allowed = if allowed { 1 } else { 0 };

    let n = context.len().min(rec.context.len());
    rec.context[..n].copy_from_slice(&context[..n]);

    RING.lock().push(rec);
}

/// Convenience: emit a seccomp-deny event (no context bytes needed).
pub fn emit_seccomp_deny(task_index: usize, syscall_nr: u64) {
    emit(AuditEvent::SeccompDeny, task_index, syscall_nr, false, &[]);
}

// ---------------------------------------------------------------------------
// Reader API — called from SYS_AUDIT_READ syscall handler
// ---------------------------------------------------------------------------

/// Maximum records a single `SYS_AUDIT_READ` call may drain.
pub const MAX_DRAIN: usize = 64;

/// Serialised record as written into userspace buffer.
///
/// Layout: [tsc:u64][task:u32][nr:u32][event:u8][allowed:u8][_pad:u16][context:32][aux:u64]
/// = 56 bytes per record.
pub const RECORD_BYTES: usize = 56;

/// Drain pending audit records into a flat byte buffer provided by userspace.
///
/// `buf` must be a multiple of `RECORD_BYTES` bytes.  Returns the number of
/// records written, packed into the low 32 bits; bits [32..64] carry the
/// dropped-record count (saturating at u32::MAX).
pub fn drain_to_bytes(buf: &mut [u8]) -> u64 {
    let slots = buf.len() / RECORD_BYTES;
    if slots == 0 {
        return 0;
    }
    let max = slots.min(MAX_DRAIN);
    let mut tmp = [AuditRecord::EMPTY; MAX_DRAIN];
    let (n, dropped) = RING.lock().drain(&mut tmp[..max]);

    for (i, rec) in tmp[..n].iter().enumerate() {
        let off = i * RECORD_BYTES;
        let b = &mut buf[off..off + RECORD_BYTES];
        b[0..8].copy_from_slice(&rec.tsc.to_le_bytes());
        b[8..12].copy_from_slice(&rec.task_index.to_le_bytes());
        b[12..16].copy_from_slice(&rec.syscall_nr.to_le_bytes());
        b[16] = rec.event as u8;
        b[17] = rec.allowed;
        b[18] = 0;
        b[19] = 0;
        b[20..52].copy_from_slice(&rec.context);
        b[52..56].copy_from_slice(&(rec.aux as u32).to_le_bytes());
    }

    let dropped_truncated = dropped.min(u32::MAX as u64) as u32;
    (n as u64) | ((dropped_truncated as u64) << 32)
}

/// Returns the number of records currently pending in the ring.
pub fn pending_count() -> usize {
    RING.lock().len()
}

/// Drain pending audit records to the VFS log file `/sys/audit.log`.
///
/// Appends serialised records to the ramfs entry at that path, creating it
/// if it does not yet exist.  Up to `MAX_DRAIN` records are flushed per call.
/// Silently does nothing if no records are pending or the VFS is not ready.
///
/// Intended to be called periodically (e.g., every 5 000 ms from the timer ISR
/// or from the kernel idle loop) to preserve audit history across service restarts.
pub fn drain_to_vfs_log() {
    if RING.lock().len() == 0 {
        return;
    }

    const LOG_PATH: &[u8] = b"/tmp/audit.log";
    const BUF_LEN: usize = MAX_DRAIN * RECORD_BYTES;

    let mut buf = [0u8; BUF_LEN];
    let packed = drain_to_bytes(&mut buf);
    let n_records = (packed & 0xFFFF_FFFF) as usize;
    if n_records == 0 {
        return;
    }
    let data = &buf[..n_records * RECORD_BYTES];

    // Open (or create) the log file, then append.
    let fd = match crate::vfs::open(LOG_PATH) {
        Ok(fd) => fd,
        Err(_) => match crate::vfs::create(LOG_PATH) {
            Ok(fd) => fd,
            Err(_) => return, // VFS not ready yet
        },
    };

    // Seek to end by reading current offset via write (offset auto-advances).
    let _ = crate::vfs::write(fd, data);
    let _ = crate::vfs::close(fd);
}
