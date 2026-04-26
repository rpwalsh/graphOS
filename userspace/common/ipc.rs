// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Shared IPC result decoding helpers for GraphOS userspace services.

/// `sys_channel_recv` result when the channel is empty.
pub const RECV_EMPTY: u64 = 0;

/// `sys_channel_recv` result when the kernel reports an error.
pub const RECV_ERROR: u64 = u64::MAX;

/// Decoded receive metadata returned by the kernel ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvEnvelope {
    /// Number of payload bytes copied into the caller's buffer.
    pub payload_len: usize,
    /// Message tag for dispatch.
    pub tag: u8,
    /// IPC endpoint to use when replying to the sender.
    pub reply_endpoint: u32,
}

/// Decode the packed `sys_channel_recv` result.
///
/// Layout:
/// - bits [0..16): payload length
/// - bits [16..24): MsgTag
/// - bits [24..56): reply endpoint
pub fn decode_recv_result(result: u64) -> Option<RecvEnvelope> {
    if result == RECV_EMPTY || result == RECV_ERROR {
        return None;
    }

    Some(RecvEnvelope {
        payload_len: (result & 0xFFFF) as usize,
        tag: ((result >> 16) & 0xFF) as u8,
        reply_endpoint: ((result >> 24) & 0xFFFF_FFFF) as u32,
    })
}
