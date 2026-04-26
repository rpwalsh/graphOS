// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Grid remote memory — borrow free physical pages from a peer node.
//!
//! This allows a task to request additional memory from a less-loaded peer
//! when the local frame allocator is under pressure.  The remote pages are
//! not DMA-mapped locally; instead they are accessed via the IPC forwarding
//! layer as a simple remote key-value slab (suitable for large buffers that
//! don't need CPU-local cache coherence).

use super::protocol::{GridMemAlloc, GridMemAllocReply, GridMsgKind};
use crate::uuid::Uuid128;

/// Request `pages` × 4 KiB from the best available peer.
///
/// Returns a `(peer_node_uuid, remote_phys_base, pages_granted)` triple on
/// success, or `None` if no peer has spare memory or the request timed out.
///
/// **Current implementation**: sends the request and records it as pending;
/// the actual reply is processed by `handle_incoming`.  Callers that need
/// synchronous allocation must spin on the pending table with a timeout.
pub fn request_pages(pages: u32, now_ms: u64) -> Option<(Uuid128, u64, u32)> {
    let peer = super::best_peer_for(super::cap::RAM)?;
    if peer.ram_free_mib == 0 {
        return None;
    }
    let correlation = crate::uuid::Uuid128Gen::v4();
    let msg = GridMemAlloc {
        kind: GridMsgKind::MemAlloc as u8,
        _pad: [0u8; 3],
        correlation_uuid: correlation.to_bytes(),
        pages,
    };
    let mbytes: &[u8; GridMemAlloc::SIZE] = unsafe { core::mem::transmute(&msg) };
    if !send_udp(peer.link_local_ip, mbytes) {
        return None;
    }

    // Poll the reply table (blocking spin up to 50 ms — only acceptable in
    // non-IRQ context; real usage should yield).
    let deadline = now_ms + 50;
    loop {
        let tick = crate::sched::current_tick_ms();
        if let Some(result) = check_reply(correlation) {
            return Some(result);
        }
        if tick >= deadline {
            break;
        }
        core::hint::spin_loop();
    }
    None
}

// ── Reply table ───────────────────────────────────────────────────────────────

const MAX_REPLIES: usize = 8;
struct MemReply {
    valid: bool,
    correlation: Uuid128,
    node_uuid: Uuid128,
    phys_base: u64,
    pages: u32,
}
impl MemReply {
    const EMPTY: Self = Self {
        valid: false,
        correlation: Uuid128::NIL,
        node_uuid: Uuid128::NIL,
        phys_base: 0,
        pages: 0,
    };
}
static REPLIES: spin::Mutex<[MemReply; MAX_REPLIES]> =
    spin::Mutex::new([MemReply::EMPTY; MAX_REPLIES]);

fn check_reply(corr: Uuid128) -> Option<(Uuid128, u64, u32)> {
    let mut r = REPLIES.lock();
    for slot in r.iter_mut() {
        if slot.valid && slot.correlation == corr {
            let result = (slot.node_uuid, slot.phys_base, slot.pages);
            slot.valid = false;
            return Some(result);
        }
    }
    None
}

/// Handle incoming `GridMemAlloc` / `GridMemAllocReply` messages.
pub fn handle_incoming(src_ip: [u8; 16], payload: &[u8], _now_ms: u64) {
    if payload.is_empty() {
        return;
    }
    let kind = GridMsgKind::from_u8(payload[0]);
    match kind {
        GridMsgKind::MemAlloc if payload.len() >= GridMemAlloc::SIZE => {
            let req: &GridMemAlloc = unsafe { &*(payload.as_ptr() as *const GridMemAlloc) };
            // Try to donate pages from local frame allocator.
            let phys = crate::mm::frame_alloc::alloc_contiguous_run(req.pages as usize);
            let (status, granted, base) = match phys {
                Some(addr) => (0u8, req.pages, addr),
                None => (1u8, 0, 0),
            };
            let reply = GridMemAllocReply {
                kind: GridMsgKind::MemAllocReply as u8,
                status,
                _pad: [0u8; 2],
                correlation_uuid: req.correlation_uuid,
                remote_phys_base: base,
                pages_granted: granted,
                _pad2: 0,
            };
            let rbytes: &[u8; GridMemAllocReply::SIZE] = unsafe { core::mem::transmute(&reply) };
            let _ = send_udp(src_ip, rbytes);
        }
        GridMsgKind::MemAllocReply if payload.len() >= GridMemAllocReply::SIZE => {
            let reply: &GridMemAllocReply =
                unsafe { &*(payload.as_ptr() as *const GridMemAllocReply) };
            if reply.status != 0 {
                return;
            }
            let corr = Uuid128::from_bytes(reply.correlation_uuid);
            let node_uuid = super::local_node_uuid(); // sender = peer, we log as local for now
            let _ = node_uuid;
            let mut r = REPLIES.lock();
            for slot in r.iter_mut() {
                if !slot.valid {
                    *slot = MemReply {
                        valid: true,
                        correlation: corr,
                        node_uuid: Uuid128::NIL,
                        phys_base: reply.remote_phys_base,
                        pages: reply.pages_granted,
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
    let mut pkt = [0u8; 512];
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
    use super::protocol::GRID_UDP_PORT;
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
