// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! IPC — inter-process communication via kernel channels.
//!
//! Channels are the fundamental IPC primitive in GraphOS. They provide:
//! - Bounded, FIFO message queues between tasks.
//! - Fixed-size message slots (no heap, no dynamic allocation).
//! - Graph integration: each channel is a Channel node in the kernel graph,
//!   with CommunicatesWith edges to connected tasks.
//!
//! ## Design rationale
//!
//! GraphOS services (graphd, modeld, servicemgr, shell3d) communicate through
//! typed messages over channels. The channel abstraction is deliberately simple:
//! - No capability tokens yet (all kernel tasks are trusted at ring 0).
//! - `channel_recv()` is still a non-blocking facade: it returns immediately
//!   with None if the queue is empty. Lower-level wake primitives exist, but
//!   full wait-queue wiring for blocking recv is still unfinished.
//! - No zero-copy: messages are copied into/out of the channel's internal
//!   ring buffer. Zero-copy shared-memory regions will be a future optimisation.
//!
//! ## Paper doctrine alignment
//!
//! The channel system prepares the substrate for:
//! - **Local Operator Engine**: services send scored/timestamped messages that
//!   carry provenance and recency metadata. The channel's graph edges enable
//!   communication topology queries.
//! - **Structural Engine**: channel topology (who talks to whom, how often)
//!   feeds into structural graph summaries and motif detection.
//! - **Causal Decision Engine**: message ordering through channels establishes
//!   happened-before relationships for causal reasoning.
//!
//! ## Message format
//!
//! Messages have a fixed header followed by a variable-length payload:
//!
//! ```text
//! +------------------+------------------------------+
//! | MsgHeader (24 B) | payload (up to max_msg_bytes) |
//! +------------------+------------------------------+
//! ```
//!
//! The header carries the message type tag, reply endpoint, timestamp, and
//! payload length. This enables typed dispatch without parsing the payload.
//!
//! ## Capacity
//! - MAX_CHANNELS = 64
//! - MAX_MSG_SLOTS per channel = 16
//! - MAX_MSG_BYTES per message = 256 (payload only, excluding header)
//!
//! ## Concurrency
//! Single `spin::Mutex` per channel. Acceptable for single-core cooperative
//! scheduling. Under SMP, per-channel locks are already the right granularity.

pub mod capability;
pub mod channel;
pub mod graphd_proto;
pub mod modeld_proto;
pub mod msg;
pub mod scce_proto;
pub mod seqlock;

use crate::arch::serial;
use crate::uuid::ChannelUuid;

#[allow(unused_imports)]
pub use crate::uuid::ChannelUuid as IpcChannelUuid;
#[allow(unused_imports)]
pub use channel::{ChannelId, RecvMeta};
#[allow(unused_imports)]
pub use msg::{MAX_MSG_BYTES, MsgHeader, MsgTag};

/// Create a new IPC channel.
///
/// Returns the channel's `ChannelUuid` (primary key), or None if the table is full.
/// The legacy integer alias can be retrieved via `channel::alias_for_uuid()` for
/// syscall ABI purposes.
pub fn channel_create(max_msg_size: usize) -> Option<ChannelUuid> {
    let cap = if max_msg_size == 0 || max_msg_size > MAX_MSG_BYTES {
        MAX_MSG_BYTES
    } else {
        max_msg_size
    };
    let uuid = channel::create(cap)?;

    // Register the channel in the kernel graph.
    if let Some(node_id) = crate::graph::arena::add_node(
        crate::graph::types::NodeKind::Channel,
        0,
        crate::graph::types::NODE_ID_KERNEL,
    ) {
        serial::write_bytes(b"[ipc] channel -> graph node ");
        serial::write_u64_dec(node_id);
    }

    Some(uuid)
}

/// Reserve a well-known named channel (UUID v5 from service name) for a bootstrap inbox.
pub fn channel_reserve(uuid: ChannelUuid, max_msg_size: usize) -> bool {
    let cap = if max_msg_size == 0 || max_msg_size > MAX_MSG_BYTES {
        MAX_MSG_BYTES
    } else {
        max_msg_size
    };
    if !channel::reserve_named(uuid, cap) {
        return false;
    }

    if let Some(node_id) = crate::graph::arena::add_node(
        crate::graph::types::NodeKind::Channel,
        0,
        crate::graph::types::NODE_ID_KERNEL,
    ) {
        serial::write_bytes(b"[ipc] reserved named channel -> graph node ");
        serial::write_u64_dec(node_id);
    }

    true
}

/// Send a message on a channel (UUID primary key).
pub fn channel_send(uuid: ChannelUuid, payload: &[u8]) -> bool {
    let reply_endpoint = crate::task::table::ipc_endpoint_at(crate::sched::current_index());
    let ok = channel::send_and_wake(uuid, MsgTag::Data, reply_endpoint, payload);
    if ok {
        crate::graph::bootstrap::observe_ipc_send(
            crate::task::table::task_id_at(crate::sched::current_index()),
            uuid,
        );
    }
    ok
}

/// Send a typed message on a channel (UUID primary key).
pub fn channel_send_tagged(uuid: ChannelUuid, tag: MsgTag, payload: &[u8]) -> bool {
    let reply_endpoint = crate::task::table::ipc_endpoint_at(crate::sched::current_index());
    let ok = channel::send_and_wake(uuid, tag, reply_endpoint, payload);
    if ok {
        crate::graph::bootstrap::observe_ipc_send(
            crate::task::table::task_id_at(crate::sched::current_index()),
            uuid,
        );
    }
    ok
}

/// Receive the next message from a channel (non-blocking, UUID primary key).
pub fn channel_recv(uuid: ChannelUuid, buf: &mut [u8]) -> Option<channel::RecvMeta> {
    if channel::is_active(uuid)
        && let Some(alias) = channel::alias_for_uuid(uuid)
    {
        let _ = crate::task::table::set_ipc_endpoint(crate::sched::current_index(), alias);
    }
    channel::recv(uuid, buf)
}

/// Receive the next message from a channel, blocking until one arrives (UUID primary key).
pub fn channel_recv_blocking(uuid: ChannelUuid, buf: &mut [u8]) -> Option<channel::RecvMeta> {
    if !channel::is_active(uuid) {
        return None;
    }
    if let Some(alias) = channel::alias_for_uuid(uuid) {
        let _ = crate::task::table::set_ipc_endpoint(crate::sched::current_index(), alias);
    }
    loop {
        if let Some(meta) = channel::recv(uuid, buf) {
            return Some(meta);
        }
        let cur = crate::sched::current_index();
        if cur == 0 {
            core::hint::spin_loop();
            continue;
        }
        if !crate::task::table::mark_blocked(cur, uuid) {
            if let Some(meta) = channel::recv(uuid, buf) {
                return Some(meta);
            }
            return None;
        }
        unsafe { crate::sched::schedule() };
    }
}
