// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Grid remote IPC — transparent channel forwarding across nodes.
//!
//! When a task sends to a `ChannelUuid` that is owned by a remote node,
//! the kernel serialises the payload into a `GridIpcForward` UDP datagram
//! and delivers it to the peer.  The remote node looks up the channel UUID
//! in its local channel table and dispatches the message as if it arrived
//! from a local sender.
//!
//! Reply messages travel back the same way via `GridIpcReply`.

use super::protocol::{GRID_UDP_PORT, GridIpcForward, GridMsgKind};
use crate::uuid::Uuid128;

/// Maximum forwarded IPC payload (bytes).  Must match `GridIpcForward::payload` size.
pub const GRID_IPC_MAX_PAYLOAD: usize = 200;

/// Called by `ipc/channel.rs` when `channel_send()` cannot find the target
/// channel locally — the IPC layer delegates here for remote dispatch.
///
/// `target_channel_uuid`: the remote channel UUID.
/// `peer_ip`: the 16-byte IPv6 address of the owning node.
/// `payload`: the message bytes to forward.
///
/// Returns `true` if the message was queued successfully.
pub fn forward_to_peer(target_channel_uuid: Uuid128, peer_ip: [u8; 16], payload: &[u8]) -> bool {
    if payload.len() > GRID_IPC_MAX_PAYLOAD {
        return false; // fragmentation not yet implemented
    }
    let correlation_uuid = crate::uuid::Uuid128Gen::v4();
    let mut msg = GridIpcForward {
        kind: GridMsgKind::IpcForward as u8,
        _pad: [0u8; 1],
        payload_len: payload.len() as u16,
        correlation_uuid: correlation_uuid.to_bytes(),
        target_channel_uuid: target_channel_uuid.to_bytes(),
        payload: [0u8; 200],
    };
    msg.payload[..payload.len()].copy_from_slice(payload);

    let mut buf = [0u8; GridIpcForward::SIZE + 8]; // +8 for UDP header
    let n = msg_to_udp(&msg, peer_ip, &mut buf);
    if n == 0 {
        return false;
    }
    // Hand off to virtio-net / net layer.
    crate::drivers::net::virtio_net::transmit(&buf[..n])
}

/// Encode a `GridIpcForward` message wrapped in a minimal IPv6/UDP header.
/// Returns bytes written, or 0 on error.
fn msg_to_udp(msg: &GridIpcForward, dst_ip: [u8; 16], out: &mut [u8]) -> usize {
    use crate::net::ipv6::{IPV6_HEADER_BYTES, encode_header};
    use crate::net::ipv6::{Ipv6Addr, Ipv6Header, PROTO_UDP};

    let payload_len = GridIpcForward::SIZE;
    let udp_len = 8 + payload_len;
    let total = IPV6_HEADER_BYTES + udp_len;
    if out.len() < total + 14 {
        // +14 for Ethernet header; caller must prepend it separately
        return 0;
    }

    let src_ip = crate::net::ipv6::Ipv6Addr(
        crate::net::OUR_IPV6.load(core::sync::atomic::Ordering::Relaxed),
    );
    let dst_ipv6 = Ipv6Addr(dst_ip);

    let hdr = Ipv6Header {
        flow_label: 0,
        payload_len: udp_len as u16,
        next_header: PROTO_UDP,
        hop_limit: 255, // Link-local, must be 255 per RFC 4861.
        src: src_ip,
        dst: dst_ipv6,
    };

    let mut off = 0;
    if encode_header(&mut out[off..], &hdr).is_none() {
        return 0;
    }
    off += IPV6_HEADER_BYTES;

    // UDP header
    out[off..off + 2].copy_from_slice(&GRID_UDP_PORT.to_be_bytes()); // src port
    out[off + 2..off + 4].copy_from_slice(&GRID_UDP_PORT.to_be_bytes()); // dst port
    out[off + 4..off + 6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    out[off + 6..off + 8].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder

    off += 8;

    // IPC forward payload
    let msg_bytes: &[u8; GridIpcForward::SIZE] = unsafe { core::mem::transmute(msg) };
    out[off..off + GridIpcForward::SIZE].copy_from_slice(msg_bytes);

    // Fix UDP checksum
    let udp_payload = &out[IPV6_HEADER_BYTES..IPV6_HEADER_BYTES + udp_len];
    let csum = crate::net::ipv6::upper_layer_checksum(
        &hdr.src,
        &hdr.dst,
        crate::net::ipv6::PROTO_UDP,
        udp_payload,
    );
    let csum_off = IPV6_HEADER_BYTES + 6;
    out[csum_off..csum_off + 2].copy_from_slice(&csum.to_be_bytes());

    total
}

/// Handle an incoming `GridIpcForward` or `GridIpcReply` datagram.
pub fn handle_incoming(_src_ip: [u8; 16], payload: &[u8], _now_ms: u64) {
    if payload.is_empty() {
        return;
    }
    let kind = GridMsgKind::from_u8(payload[0]);
    if kind == GridMsgKind::IpcForward && payload.len() >= GridIpcForward::SIZE {
        let msg: &GridIpcForward = unsafe { &*(payload.as_ptr() as *const GridIpcForward) };
        let plen = msg.payload_len as usize;
        if plen <= GRID_IPC_MAX_PAYLOAD {
            // Deliver to local channel using standard send path.
            use crate::ipc::msg::MsgTag;
            let _ = crate::ipc::channel::send_and_wake(
                crate::uuid::ChannelUuid(crate::uuid::Uuid128::from_bytes(msg.target_channel_uuid)),
                MsgTag::Data,
                0,
                &msg.payload[..plen],
            );
        }
    }
}
