// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Seqlock-based wait-free IPC ring for multicore-safe message passing.
//!
//! ## Design
//! A `SeqlockRing<N>` is a bounded SPSC (single-producer, single-consumer)
//! ring with a seqlock protecting each slot.  The producer increments the
//! slot's sequence number before and after writing; the consumer spins
//! until it reads a consistent (even) sequence number.
//!
//! This gives:
//! - **Wait-free writes** on the producer side (no lock taken).
//! - **Lock-free reads** on the consumer side (spin only on the sequence
//!   number of the current slot, not on a global mutex).
//! - **Zero false sharing** — each slot is padded to a cache line.
//!
//! ## Usage
//! The kernel-global `FAST_RINGS` table holds one `SeqlockRing` per
//! channel (indexed by `ChannelId - 1`).  High-frequency kernel→kernel
//! paths (e.g., virtio Rx → TCP stack) use `fast_send` / `fast_recv`
//! instead of the mutex-protected `channel_send` / `channel_recv`.
//! The existing `channel.rs` mutex path is preserved for low-frequency
//! control messages and userspace IPC.
//!
//! ## Slot layout
//! ```text
//! [seq: AtomicU64][tag: u8][_pad: 7][len: u16][_pad: 6][payload: [u8; MAX_PAYLOAD]]
//! ```
//! Each slot is padded to `SLOT_STRIDE` bytes to avoid false sharing.

use core::sync::atomic::{AtomicU64, Ordering};

// ── constants ────────────────────────────────────────────────────────────────

/// Maximum payload bytes per fast-ring message.
/// Must be ≤ `ipc::msg::MAX_MSG_BYTES`.
pub const FAST_RING_PAYLOAD: usize = 128;

/// Number of slots per ring (power of two).
const RING_SLOTS: usize = 32;

/// Number of fast rings (one per channel).
/// Must be ≥ `channel::MAX_CHANNELS`.
const RING_COUNT: usize = 64;

// ── slot ─────────────────────────────────────────────────────────────────────

/// One message slot in the seqlock ring.
///
/// The layout is intentionally flat (no nested structs) to allow
/// `const` initialisation without `const fn` limitations.
#[repr(C, align(64))]
struct Slot {
    /// Seqlock sequence number.
    /// - Even: slot is idle or fully committed.
    /// - Odd:  slot is being written.
    seq: AtomicU64,
    /// Message tag byte.
    tag: u8,
    _pad0: [u8; 7],
    /// Valid payload length in bytes.
    len: u16,
    _pad1: [u8; 6],
    /// Payload bytes (up to FAST_RING_PAYLOAD).
    payload: [u8; FAST_RING_PAYLOAD],
    /// Padding to fill the 64-byte cache line.
    _fill: [u8; 64 - (8 + 8 + 8 + FAST_RING_PAYLOAD) % 64],
}

// Compile-time check that FAST_RING_PAYLOAD leaves room for the header.
const _: () = assert!(FAST_RING_PAYLOAD + 16 <= 512, "slot too large");

impl Slot {
    const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            tag: 0,
            _pad0: [0u8; 7],
            len: 0,
            _pad1: [0u8; 6],
            payload: [0u8; FAST_RING_PAYLOAD],
            _fill: [0u8; 64 - (8 + 8 + 8 + FAST_RING_PAYLOAD) % 64],
        }
    }
}

// ── seqlock ring ─────────────────────────────────────────────────────────────

/// A wait-free SPSC ring backed by seqlocks.
pub struct SeqlockRing {
    slots: [Slot; RING_SLOTS],
    /// Producer write cursor (index mod RING_SLOTS).
    write_at: AtomicU64,
    /// Consumer read cursor (index mod RING_SLOTS).
    read_at: AtomicU64,
}

impl SeqlockRing {
    pub const fn new() -> Self {
        Self {
            slots: [const { Slot::new() }; RING_SLOTS],
            write_at: AtomicU64::new(0),
            read_at: AtomicU64::new(0),
        }
    }

    /// Attempt to enqueue a message.  Returns false if the ring is full.
    ///
    /// Wait-free: completes in O(1) without spinning.
    ///
    /// # Concurrency
    /// Must be called by **only one producer** at a time.
    pub fn send(&self, tag: u8, payload: &[u8]) -> bool {
        let write = self.write_at.load(Ordering::Relaxed);
        let read = self.read_at.load(Ordering::Acquire);
        // Full check: ring has RING_SLOTS capacity.
        if write.wrapping_sub(read) >= RING_SLOTS as u64 {
            return false;
        }
        let slot_idx = (write as usize) & (RING_SLOTS - 1);
        let slot = &self.slots[slot_idx];

        // Begin write: set seq to odd.
        let cur_seq = slot.seq.load(Ordering::Acquire);
        slot.seq.store(cur_seq.wrapping_add(1), Ordering::Release);
        core::sync::atomic::fence(Ordering::SeqCst);

        // Write payload.
        let len = payload.len().min(FAST_RING_PAYLOAD);
        // SAFETY: slot is exclusively owned by the producer at this write_at position.
        let slot_ptr = slot as *const Slot as *mut Slot;
        unsafe {
            (*slot_ptr).tag = tag;
            (*slot_ptr).len = len as u16;
            core::ptr::copy_nonoverlapping(payload.as_ptr(), (*slot_ptr).payload.as_mut_ptr(), len);
        }

        // Commit: set seq to next even value.
        core::sync::atomic::fence(Ordering::SeqCst);
        slot.seq.store(cur_seq.wrapping_add(2), Ordering::Release);

        self.write_at
            .store(write.wrapping_add(1), Ordering::Release);
        true
    }

    /// Attempt to dequeue a message into `out_payload`.
    ///
    /// Returns `Some((tag, len))` on success, `None` if the ring is empty.
    /// Spins on the seqlock of the current read slot until the writer commits.
    ///
    /// # Concurrency
    /// Must be called by **only one consumer** at a time.
    pub fn recv(&self, out_payload: &mut [u8; FAST_RING_PAYLOAD]) -> Option<(u8, usize)> {
        let read = self.read_at.load(Ordering::Relaxed);
        let write = self.write_at.load(Ordering::Acquire);
        if read == write {
            return None; // empty
        }
        let slot_idx = (read as usize) & (RING_SLOTS - 1);
        let slot = &self.slots[slot_idx];

        // Spin until the seqlock shows an even (committed) sequence.
        // Cap iterations to avoid infinite spin on buggy producers.
        let mut spins = 0u32;
        loop {
            let seq = slot.seq.load(Ordering::Acquire);
            if seq & 1 == 0 {
                break; // committed
            }
            spins += 1;
            if spins > 1_000_000 {
                // Producer stalled — bail out without advancing read cursor.
                return None;
            }
            core::hint::spin_loop();
        }

        // Read payload under seqlock retry.
        loop {
            let seq1 = slot.seq.load(Ordering::Acquire);
            if seq1 & 1 != 0 {
                // Mid-write race — spin.
                core::hint::spin_loop();
                continue;
            }
            // SAFETY: consumer exclusively reads this slot while read_at == write_at is false.
            let (tag, len) = unsafe {
                let t = (*(slot as *const Slot)).tag;
                let l = (*(slot as *const Slot)).len as usize;
                let src = (*(slot as *const Slot)).payload.as_ptr();
                let copy_len = l.min(FAST_RING_PAYLOAD);
                core::ptr::copy_nonoverlapping(src, out_payload.as_mut_ptr(), copy_len);
                (t, copy_len)
            };
            let seq2 = slot.seq.load(Ordering::Acquire);
            if seq1 == seq2 {
                // Consistent read.
                self.read_at.store(read.wrapping_add(1), Ordering::Release);
                return Some((tag, len));
            }
            // Torn read — retry.
            core::hint::spin_loop();
        }
    }

    /// Returns `true` if the ring has at least one message available.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.read_at.load(Ordering::Relaxed) == self.write_at.load(Ordering::Acquire)
    }
}

// ── global fast-ring table ────────────────────────────────────────────────────

/// One fast ring per channel (indexed by ChannelId - 1).
static FAST_RINGS: [SeqlockRing; RING_COUNT] = [const { SeqlockRing::new() }; RING_COUNT];

/// Send a message on the fast seqlock path for the given channel.
///
/// `channel_id` is 1-based (matches `ipc::channel::ChannelId`).
/// Returns false if the channel ID is out of range or the ring is full.
pub fn fast_send(channel_id: u32, tag: u8, payload: &[u8]) -> bool {
    let idx = channel_id.wrapping_sub(1) as usize;
    if idx >= RING_COUNT {
        return false;
    }
    FAST_RINGS[idx].send(tag, payload)
}

/// Receive a message on the fast seqlock path for the given channel.
///
/// Returns `Some((tag, len))` with `out` filled if a message was available,
/// `None` if the ring was empty.
pub fn fast_recv(channel_id: u32, out: &mut [u8; FAST_RING_PAYLOAD]) -> Option<(u8, usize)> {
    let idx = channel_id.wrapping_sub(1) as usize;
    if idx >= RING_COUNT {
        return None;
    }
    FAST_RINGS[idx].recv(out)
}
