// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! IPC channel — bounded FIFO message queue.
//!
//! Each channel is a fixed-size ring buffer of message slots. Slots are
//! `MsgHeader + payload` pairs stored inline (no pointers, no heap).
//!
//! ## Ring buffer design
//!
//! ```text
//! slots: [ slot 0 | slot 1 | ... | slot N-1 ]
//!           ^head              ^tail
//! ```
//!
//! - `head`: index of the next slot to read (oldest message).
//! - `tail`: index of the next slot to write (next free slot).
//! - `count`: number of messages currently in the queue.
//! - When `count == MAX_MSG_SLOTS`, the queue is full and sends fail.
//! - When `count == 0`, the queue is empty and receives return None.
//!
//! ## Storage
//!
//! All channels are statically allocated in BSS. Each channel has
//! `MAX_MSG_SLOTS` slots, each slot is `SLOT_SIZE` bytes (header + max payload).
//! This is generous for early boot but will be replaced with page-backed
//! per-channel allocation once the frame allocator supports it.
//!
//! ## Concurrency
//!
//! One `spin::Mutex` protects the entire channel table. Under SMP this
//! should become per-channel locking. For single-core cooperative scheduling,
//! contention is impossible.

use crate::arch::interrupts;
use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::ptr;
use spin::Mutex;

use crate::arch::serial;
use crate::graph::arena;
use crate::uuid::{ChannelUuid, Uuid128};

use super::msg::{MAX_MSG_BYTES, MsgHeader, MsgTag};

/// Receiver-side metadata returned alongside the payload.
#[derive(Debug, Clone, Copy)]
pub struct RecvMeta {
    /// Message type tag.
    pub tag: MsgTag,
    /// Reply endpoint for the sending task.
    pub reply_endpoint: u32,
    /// Payload length (bytes actually copied into caller's buffer).
    pub payload_len: usize,
}

/// Channel identifier (index into the channel table).
///
/// `0` is reserved as "no channel / no reply endpoint". Real channels start at `1`.
pub type ChannelId = u32;

/// Maximum number of channels.
pub const MAX_CHANNELS: usize = 64;

/// Maximum messages queued per channel.
const MAX_MSG_SLOTS: usize = 16;

/// Size of one message slot in bytes (header + max payload).
const SLOT_SIZE: usize = core::mem::size_of::<MsgHeader>() + MAX_MSG_BYTES;

/// Total ring buffer size per channel in bytes.
const RING_SIZE: usize = SLOT_SIZE * MAX_MSG_SLOTS;

/// A single IPC channel.
struct Channel {
    /// Whether this channel slot is allocated.
    active: bool,
    /// Canonical UUID handle for this channel.
    uuid: ChannelUuid,
    /// Maximum payload size configured at creation (≤ MAX_MSG_BYTES).
    max_payload: usize,
    /// Ring buffer storage. Lazily heap-allocated on create().
    ring: *mut u8,
    /// Index of the next slot to read.
    head: usize,
    /// Index of the next slot to write.
    tail: usize,
    /// Number of messages currently in the queue.
    count: usize,
    /// Total messages ever sent through this channel (for diagnostics).
    total_sent: u64,
    /// Total messages ever received from this channel.
    total_recv: u64,
}

// SAFETY: Channel contains a raw pointer to heap-owned ring storage, but all
// access is serialized behind TABLE: Mutex<ChannelTable>.
unsafe impl Send for Channel {}

impl Channel {
    const fn new() -> Self {
        Self {
            active: false,
            uuid: ChannelUuid(Uuid128::NIL),
            max_payload: 0,
            ring: ptr::null_mut(),
            head: 0,
            tail: 0,
            count: 0,
            total_sent: 0,
            total_recv: 0,
        }
    }

    /// Get a pointer to the start of slot `index` in the ring buffer.
    fn slot_ptr(&self, index: usize) -> *const u8 {
        let offset = index * SLOT_SIZE;
        // SAFETY: offset is always < RING_SIZE because index < MAX_MSG_SLOTS.
        unsafe { self.ring.add(offset) as *const u8 }
    }

    /// Get a mutable pointer to the start of slot `index`.
    fn slot_ptr_mut(&mut self, index: usize) -> *mut u8 {
        let offset = index * SLOT_SIZE;
        // SAFETY: offset is always < RING_SIZE because index < MAX_MSG_SLOTS.
        unsafe { self.ring.add(offset) }
    }

    /// Enqueue a message. Returns false if the channel is full or payload
    /// exceeds max_payload.
    fn enqueue(&mut self, tag: MsgTag, reply_endpoint: u32, payload: &[u8]) -> bool {
        if self.ring.is_null() {
            return false;
        }
        if self.count >= MAX_MSG_SLOTS {
            return false;
        }
        if payload.len() > self.max_payload {
            return false;
        }

        let header = MsgHeader {
            tag,
            _pad: [0; 3],
            payload_len: payload.len() as u32,
            reply_endpoint,
            timestamp: arena::generation(),
        };

        let slot = self.slot_ptr_mut(self.tail);

        // Write header.
        let header_bytes = unsafe {
            // SAFETY: MsgHeader is repr(C), Copy, and we're writing into
            // our own ring buffer which is large enough.
            core::slice::from_raw_parts(
                &header as *const MsgHeader as *const u8,
                core::mem::size_of::<MsgHeader>(),
            )
        };
        unsafe {
            // SAFETY: slot points into self.ring, and SLOT_SIZE ≥ header + payload.
            core::ptr::copy_nonoverlapping(header_bytes.as_ptr(), slot, header_bytes.len());
        }

        // Write payload after the header.
        if !payload.is_empty() {
            unsafe {
                // SAFETY: slot + header_size is still within the slot's SLOT_SIZE.
                let payload_dst = slot.add(core::mem::size_of::<MsgHeader>());
                core::ptr::copy_nonoverlapping(payload.as_ptr(), payload_dst, payload.len());
            }
        }

        self.tail = (self.tail + 1) % MAX_MSG_SLOTS;
        self.count += 1;
        self.total_sent += 1;
        true
    }

    /// Dequeue the next message. Copies the payload into `buf` and returns
    /// metadata (tag, reply endpoint, payload length). Returns None if the queue is empty.
    fn dequeue(&mut self, buf: &mut [u8]) -> Option<RecvMeta> {
        if self.ring.is_null() {
            return None;
        }
        if self.count == 0 {
            return None;
        }

        let slot = self.slot_ptr(self.head);

        // Read the header.
        let header = unsafe {
            // SAFETY: slot points to a valid, previously-written slot in our ring.
            let ptr = slot as *const MsgHeader;
            ptr.read()
        };

        let payload_len = header.payload_len as usize;
        let copy_len = payload_len.min(buf.len());

        if copy_len > 0 {
            unsafe {
                // SAFETY: slot + header_size points to the payload region.
                let payload_src = slot.add(core::mem::size_of::<MsgHeader>());
                core::ptr::copy_nonoverlapping(payload_src, buf.as_mut_ptr(), copy_len);
            }
        }

        self.head = (self.head + 1) % MAX_MSG_SLOTS;
        self.count -= 1;
        self.total_recv += 1;
        Some(RecvMeta {
            tag: header.tag,
            reply_endpoint: header.reply_endpoint,
            payload_len: copy_len,
        })
    }
}

/// The global channel table.
struct ChannelTable {
    channels: [Channel; MAX_CHANNELS],
    next_id: ChannelId,
    /// Bitmap: bit i set = slot i is free. O(1) alloc via trailing_zeros.
    free_bitmap: u64,
}

impl ChannelTable {
    const fn new() -> Self {
        const EMPTY: Channel = Channel::new();
        Self {
            channels: [EMPTY; MAX_CHANNELS],
            next_id: 0,
            free_bitmap: u64::MAX, // all 64 bits set = all free
        }
    }
}

static TABLE: Mutex<ChannelTable> = Mutex::new(ChannelTable::new());

fn table_index(id: ChannelId) -> Option<usize> {
    if id == 0 {
        return None;
    }
    let idx = (id - 1) as usize;
    if idx < MAX_CHANNELS { Some(idx) } else { None }
}

/// Internal: find the slot index for a UUID (O(n) scan, n ≤ MAX_CHANNELS=64).
/// Caller must hold the table lock (operates on a reference to the locked table).
fn index_for_uuid_locked(table: &ChannelTable, uuid: ChannelUuid) -> Option<usize> {
    if uuid.into_inner() == Uuid128::NIL {
        return None;
    }
    let mut i = 0usize;
    while i < MAX_CHANNELS {
        let ch = &table.channels[i];
        if ch.active && ch.uuid == uuid {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn activate_slot(
    table: &mut ChannelTable,
    index: usize,
    channel_uuid: ChannelUuid,
    max_payload: usize,
) -> bool {
    table.free_bitmap &= !(1u64 << index);

    let ch = &mut table.channels[index];
    ch.active = true;
    ch.uuid = channel_uuid;
    ch.max_payload = max_payload.min(MAX_MSG_BYTES);
    ch.head = 0;
    ch.tail = 0;
    ch.count = 0;
    ch.total_sent = 0;
    ch.total_recv = 0;

    let layout = match Layout::from_size_align(RING_SIZE, core::mem::align_of::<MsgHeader>()) {
        Ok(l) => l,
        Err(_) => {
            ch.active = false;
            ch.uuid = ChannelUuid(Uuid128::NIL);
            table.free_bitmap |= 1u64 << index;
            return false;
        }
    };
    let ring = unsafe { alloc_zeroed(layout) };
    if ring.is_null() {
        ch.active = false;
        ch.uuid = ChannelUuid(Uuid128::NIL);
        table.free_bitmap |= 1u64 << index;
        return false;
    }
    ch.ring = ring;
    true
}

fn runtime_channel_uuid(id: ChannelId) -> ChannelUuid {
    if let Some(uuid) = Uuid128::v4_random() {
        ChannelUuid(uuid)
    } else {
        ChannelUuid::from_channel_id(id)
    }
}

/// Create a new channel with the given maximum payload size.
/// Returns the channel's `ChannelUuid` (primary key), or None if the table is full.
/// Use `alias_for_uuid()` to get the legacy integer alias for syscall ABI purposes.
pub fn create(max_payload: usize) -> Option<ChannelUuid> {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();

        // O(1) free-slot lookup via bitmap trailing_zeros.
        if table.free_bitmap == 0 {
            serial::write_line(b"[ipc] ERROR: channel table full");
            return None;
        }
        let i = table.free_bitmap.trailing_zeros() as usize;
        let id = (i as ChannelId) + 1;
        let channel_uuid = runtime_channel_uuid(id);
        if !activate_slot(&mut table, i, channel_uuid, max_payload) {
            return None;
        }
        if id >= table.next_id {
            table.next_id = id + 1;
        }

        serial::write_bytes(b"[ipc] created channel id=");
        serial::write_u64_dec_inline(id as u64);
        serial::write_bytes(b" max_payload=");
        serial::write_u64_dec(max_payload as u64);

        Some(channel_uuid)
    })
}

/// Reserve a channel slot for a well-known UUID (e.g. UUID v5 from a service name).
/// Picks any free slot and assigns `uuid` as its primary identity.
/// Use this instead of `reserve(id, ...)` for all new named-service inboxes.
pub fn reserve_named(uuid: ChannelUuid, max_payload: usize) -> bool {
    if uuid.into_inner() == Uuid128::NIL {
        return false;
    }
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        // Reject if the UUID is already active.
        if index_for_uuid_locked(&table, uuid).is_some() {
            return true; // already reserved — idempotent
        }
        if table.free_bitmap == 0 {
            serial::write_line(b"[ipc] ERROR: channel table full (reserve_named)");
            return false;
        }
        let i = table.free_bitmap.trailing_zeros() as usize;
        let id = (i as ChannelId) + 1;
        if !activate_slot(&mut table, i, uuid, max_payload) {
            return false;
        }
        if id >= table.next_id {
            table.next_id = id + 1;
        }
        serial::write_bytes(b"[ipc] reserve_named slot=");
        serial::write_u64_dec_inline(id as u64);
        serial::write_bytes(b" max_payload=");
        serial::write_u64_dec(max_payload as u64);
        true
    })
}

/// Send a message on a channel identified by its `ChannelUuid` (primary key).
/// `reply_endpoint` is the caller's inbox channel alias for responses.
pub fn send(uuid: ChannelUuid, tag: MsgTag, reply_endpoint: u32, payload: &[u8]) -> bool {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let Some(idx) = index_for_uuid_locked(&table, uuid) else {
            return false;
        };
        let ch = &mut table.channels[idx];
        if !ch.active {
            return false;
        }
        let ok = ch.enqueue(tag, reply_endpoint, payload);
        if ok {
            drop(table); // release lock before twin ingestion
            crate::graph::twin::ingest_ipc_send(crate::arch::timer::ticks());
        }
        ok
    })
}

/// Send a message and wake any task blocked on this channel.
///
/// This is the preferred send path when blocking IPC is enabled.
/// After enqueuing the message, it checks the task table for any task
/// blocked on this channel and moves it to Ready.
pub fn send_and_wake(uuid: ChannelUuid, tag: MsgTag, reply_endpoint: u32, payload: &[u8]) -> bool {
    let ok = send(uuid, tag, reply_endpoint, payload);
    if ok {
        crate::task::table::wake_blocked_on(uuid);
    }
    ok
}

/// Receive the next message from a channel identified by its `ChannelUuid`.
/// Returns metadata (tag, reply endpoint, payload length) and copies payload into `buf`.
pub fn recv(uuid: ChannelUuid, buf: &mut [u8]) -> Option<RecvMeta> {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let idx = index_for_uuid_locked(&table, uuid)?;
        let ch = &mut table.channels[idx];
        if !ch.active {
            return None;
        }
        ch.dequeue(buf)
    })
}

/// Return whether a `ChannelUuid` currently refers to an active channel.
pub fn is_active(uuid: ChannelUuid) -> bool {
    if uuid.into_inner() == Uuid128::NIL {
        return false;
    }
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        index_for_uuid_locked(&table, uuid).is_some()
    })
}

/// Return whether the legacy integer alias currently refers to an active channel.
/// Shim used only at the syscall ABI boundary.
pub fn is_active_alias(id: ChannelId) -> bool {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        match table_index(id) {
            Some(idx) => table.channels[idx].active,
            None => false,
        }
    })
}

pub fn uuid(id: ChannelId) -> Option<ChannelUuid> {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        let idx = table_index(id)?;
        let channel = &table.channels[idx];
        channel.active.then_some(channel.uuid)
    })
}

pub fn uuid_for_alias(id: ChannelId) -> ChannelUuid {
    if id == 0 {
        return ChannelUuid(Uuid128::NIL);
    }
    uuid(id).unwrap_or_else(|| ChannelUuid::from_channel_id(id))
}

pub fn alias_for_uuid(channel_uuid: ChannelUuid) -> Option<ChannelId> {
    if channel_uuid.into_inner() == Uuid128::NIL {
        return None;
    }

    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        let mut index = 0usize;
        while index < table.channels.len() {
            let channel = &table.channels[index];
            if channel.active && channel.uuid == channel_uuid {
                return Some((index as ChannelId) + 1);
            }
            index += 1;
        }
        None
    })
}

/// Return the number of messages currently queued in a channel.
pub fn pending_count(uuid: ChannelUuid) -> usize {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        let Some(idx) = index_for_uuid_locked(&table, uuid) else {
            return 0;
        };
        table.channels[idx].count
    })
}

/// Return the total number of active channels.
pub fn active_count() -> usize {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        table.channels.iter().filter(|ch| ch.active).count()
    })
}

/// Dump all active channels to serial for diagnostics.
pub fn dump_all() {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        serial::write_line(b"[ipc] === Channel Table ===");
        let mut active = 0u32;
        for (i, ch) in table.channels.iter().enumerate() {
            if ch.active {
                serial::write_bytes(b"  ch=");
                serial::write_u64_dec_inline((i as u64) + 1);
                serial::write_bytes(b" queued=");
                serial::write_u64_dec_inline(ch.count as u64);
                serial::write_bytes(b" sent=");
                serial::write_u64_dec_inline(ch.total_sent);
                serial::write_bytes(b" recv=");
                serial::write_u64_dec_inline(ch.total_recv);
                serial::write_bytes(b" max=");
                serial::write_u64_dec(ch.max_payload as u64);
                active += 1;
            }
        }
        serial::write_bytes(b"[ipc] active channels: ");
        serial::write_u64_dec(active as u64);
        serial::write_line(b"[ipc] === End Channel Table ===");
    })
}

/// Destroy a channel. All queued messages are lost.
pub fn destroy(uuid: ChannelUuid) -> bool {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let Some(idx) = index_for_uuid_locked(&table, uuid) else {
            return false;
        };
        table.channels[idx].active = false;
        table.channels[idx].uuid = ChannelUuid(Uuid128::NIL);
        table.free_bitmap |= 1u64 << idx;
        table.channels[idx].count = 0;
        table.channels[idx].head = 0;
        table.channels[idx].tail = 0;
        if !table.channels[idx].ring.is_null() {
            if let Ok(layout) =
                Layout::from_size_align(RING_SIZE, core::mem::align_of::<MsgHeader>())
            {
                unsafe {
                    dealloc(table.channels[idx].ring, layout);
                }
            }
            table.channels[idx].ring = ptr::null_mut();
        }

        serial::write_bytes(b"[ipc] destroyed channel alias=");
        serial::write_u64_dec((idx + 1) as u64);
        true
    })
}
