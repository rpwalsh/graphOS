// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Grid remote VFS — mount a peer node's filesystem as a local VFS subtree.
//!
//! A remote mount is identified by a UUID and appears in the local VFS under
//! `/grid/<node-uuid>/`.  Reads and writes are forwarded as `GridVfsRead` /
//! `GridVfsWrite` UDP datagrams.  Maximum single-op transfer is 512 bytes
//! (fragmentation support planned for a later session).

use super::protocol::{GRID_UDP_PORT, GridMsgKind, GridVfsRead, GridVfsReadReply};
use crate::uuid::Uuid128;

const MAX_REMOTE_MOUNTS: usize = 8;
const READ_TIMEOUT_MS: u64 = 500;

struct RemoteMount {
    active: bool,
    /// UUID of the remote node.
    node_uuid: Uuid128,
    /// IPv6 link-local address of the remote node.
    peer_ip: [u8; 16],
}
impl RemoteMount {
    const EMPTY: Self = Self {
        active: false,
        node_uuid: Uuid128::NIL,
        peer_ip: [0u8; 16],
    };
}

static MOUNTS: spin::Mutex<[RemoteMount; MAX_REMOTE_MOUNTS]> =
    spin::Mutex::new([RemoteMount::EMPTY; MAX_REMOTE_MOUNTS]);

/// Register a remote node's filesystem as a grid mount.
pub fn add_mount(node_uuid: Uuid128, peer_ip: [u8; 16]) -> bool {
    let mut m = MOUNTS.lock();
    for slot in m.iter_mut() {
        if !slot.active {
            *slot = RemoteMount {
                active: true,
                node_uuid,
                peer_ip,
            };
            return true;
        }
    }
    false
}

/// Remove a remote mount by node UUID.
pub fn remove_mount(node_uuid: Uuid128) {
    let mut m = MOUNTS.lock();
    for slot in m.iter_mut() {
        if slot.active && slot.node_uuid == node_uuid {
            slot.active = false;
        }
    }
}

/// Read up to 512 bytes from `path` on the peer at `node_uuid`.
///
/// Blocks (spin) for up to `READ_TIMEOUT_MS`.  Returns bytes read, or 0 on
/// error / timeout.
pub fn remote_read(node_uuid: Uuid128, path: &[u8], offset: u64, out: &mut [u8]) -> usize {
    let peer_ip = {
        let m = MOUNTS.lock();
        let mut found = None;
        for slot in m.iter() {
            if slot.active && slot.node_uuid == node_uuid {
                found = Some(slot.peer_ip);
                break;
            }
        }
        match found {
            Some(ip) => ip,
            None => return 0,
        }
    };

    let correlation = crate::uuid::Uuid128Gen::v4();
    let mut req = GridVfsRead {
        kind: GridMsgKind::VfsRead as u8,
        _pad: [0u8; 3],
        correlation_uuid: correlation.to_bytes(),
        path: [0u8; 128],
        offset,
        length: out.len().min(512) as u16,
        _pad2: [0u8; 6],
    };
    let copy_len = path.len().min(127);
    req.path[..copy_len].copy_from_slice(&path[..copy_len]);

    let rbytes: &[u8; GridVfsRead::SIZE] = unsafe { core::mem::transmute(&req) };
    if !send_udp(peer_ip, rbytes) {
        return 0;
    }

    // Spin-poll for reply.
    let now = crate::sched::current_tick_ms();
    let deadline = now + READ_TIMEOUT_MS;
    loop {
        if let Some(data) = check_read_reply(correlation) {
            let n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
            return n;
        }
        if crate::sched::current_tick_ms() >= deadline {
            break;
        }
        core::hint::spin_loop();
    }
    0
}

// ── Reply cache ───────────────────────────────────────────────────────────────

const MAX_READ_REPLIES: usize = 8;
struct ReadReply {
    valid: bool,
    correlation: Uuid128,
    data: [u8; 512],
    len: u16,
}
impl ReadReply {
    const EMPTY: Self = Self {
        valid: false,
        correlation: Uuid128::NIL,
        data: [0u8; 512],
        len: 0,
    };
}
static READ_REPLIES: spin::Mutex<[ReadReply; MAX_READ_REPLIES]> =
    spin::Mutex::new([ReadReply::EMPTY; MAX_READ_REPLIES]);

fn check_read_reply(corr: Uuid128) -> Option<[u8; 512]> {
    let mut r = READ_REPLIES.lock();
    for slot in r.iter_mut() {
        if slot.valid && slot.correlation == corr {
            let data = slot.data;
            slot.valid = false;
            return Some(data);
        }
    }
    None
}

/// Handle incoming VFS-related grid messages.
pub fn handle_incoming(src_ip: [u8; 16], payload: &[u8], _now_ms: u64) {
    if payload.is_empty() {
        return;
    }
    let kind = GridMsgKind::from_u8(payload[0]);
    match kind {
        GridMsgKind::VfsRead if payload.len() >= GridVfsRead::SIZE => {
            let req: &GridVfsRead = unsafe { &*(payload.as_ptr() as *const GridVfsRead) };
            let path_end = req.path.iter().position(|&b| b == 0).unwrap_or(128);
            let path_bytes = &req.path[..path_end];
            let mut data = [0u8; 512];
            let bytes_read =
                crate::vfs::read_at(path_bytes, req.offset, &mut data[..req.length as usize])
                    .unwrap_or(0);
            let reply = GridVfsReadReply {
                kind: GridMsgKind::VfsReadReply as u8,
                status: if bytes_read == 0 { 1 } else { 0 },
                _pad: [0u8; 2],
                correlation_uuid: req.correlation_uuid,
                bytes_read: bytes_read as u16,
                _pad2: [0u8; 6],
                data,
            };
            let rbytes: &[u8; GridVfsReadReply::SIZE] = unsafe { core::mem::transmute(&reply) };
            let _ = send_udp(src_ip, rbytes);
        }
        GridMsgKind::VfsReadReply if payload.len() >= GridVfsReadReply::SIZE => {
            let reply: &GridVfsReadReply =
                unsafe { &*(payload.as_ptr() as *const GridVfsReadReply) };
            if reply.status != 0 {
                return;
            }
            let corr = Uuid128::from_bytes(reply.correlation_uuid);
            let mut r = READ_REPLIES.lock();
            for slot in r.iter_mut() {
                if !slot.valid {
                    *slot = ReadReply {
                        valid: true,
                        correlation: corr,
                        data: reply.data,
                        len: reply.bytes_read,
                    };
                    break;
                }
            }
        }
        _ => {}
    }
}

fn send_udp(dst_ip: [u8; 16], data: &[u8]) -> bool {
    use crate::net::ipv6::{IPV6_HEADER_BYTES, Ipv6Addr, Ipv6Header, PROTO_UDP, encode_header};
    use core::sync::atomic::Ordering;
    let src_ip = Ipv6Addr(crate::net::OUR_IPV6.load(Ordering::Relaxed));
    let dst = Ipv6Addr(dst_ip);
    let udp_len = 8 + data.len();
    let total = IPV6_HEADER_BYTES + udp_len;
    let mut pkt = [0u8; 1024];
    if total > pkt.len() {
        return false;
    }
    let hdr = Ipv6Header {
        flow_label: 0,
        payload_len: udp_len as u16,
        next_header: PROTO_UDP,
        hop_limit: 255,
        src: src_ip,
        dst,
    };
    let _ = encode_header(&mut pkt, &hdr);
    let off = IPV6_HEADER_BYTES;
    pkt[off..off + 2].copy_from_slice(&GRID_UDP_PORT.to_be_bytes());
    pkt[off + 2..off + 4].copy_from_slice(&GRID_UDP_PORT.to_be_bytes());
    pkt[off + 4..off + 6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    pkt[off + 6..off + 8].copy_from_slice(&0u16.to_be_bytes());
    pkt[off + 8..off + 8 + data.len()].copy_from_slice(data);
    let csum =
        crate::net::ipv6::upper_layer_checksum(&src_ip, &dst, PROTO_UDP, &pkt[off..off + udp_len]);
    pkt[off + 6..off + 8].copy_from_slice(&csum.to_be_bytes());
    crate::drivers::net::virtio_net::transmit(&pkt[..total])
}
