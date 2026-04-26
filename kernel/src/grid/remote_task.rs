// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Grid remote task migration — spawn a task on the best-fit peer node.
//!
//! When `SYS_GRID_SPAWN` is called, the scheduler:
//!  1. Picks the least-loaded peer with `cap::CPU` set.
//!  2. Sends a `GridTaskSpawn` UDP datagram to that peer.
//!  3. Records a pending correlation UUID so the reply can be matched.
//!  4. On `GridTaskSpawnReply`, updates the task table with the remote UUID.
//!
//! The local task that requested the remote spawn blocks on a channel until
//! the reply arrives (or a 2-second timeout fires and the task is run locally).

use super::protocol::{GRID_UDP_PORT, GridMsgKind, GridTaskSpawn, GridTaskSpawnReply};
use crate::uuid::Uuid128;

const MAX_PENDING: usize = 16;

/// Pending remote spawn correlation table.
struct PendingSpawn {
    active: bool,
    correlation_uuid: Uuid128,
    /// Local task index waiting for the reply.
    requester_task_idx: usize,
    /// Deadline tick (expire and fall back to local run if exceeded).
    deadline_ms: u64,
}

impl PendingSpawn {
    const EMPTY: Self = Self {
        active: false,
        correlation_uuid: Uuid128::NIL,
        requester_task_idx: usize::MAX,
        deadline_ms: 0,
    };
}

static PENDING: spin::Mutex<[PendingSpawn; MAX_PENDING]> =
    spin::Mutex::new([PendingSpawn::EMPTY; MAX_PENDING]);

/// Attempt to spawn `task_binary_uuid` on a remote peer node.
///
/// Returns `Some(correlation_uuid)` if the request was sent, or `None` if
/// no suitable peer was found (caller should run the task locally).
pub fn try_remote_spawn(
    task_binary_uuid: Uuid128,
    arg: u64,
    required_caps: u8,
    requester_task_idx: usize,
    now_ms: u64,
) -> Option<Uuid128> {
    let peer = super::best_peer_for(super::cap::CPU)?;
    let correlation = crate::uuid::Uuid128Gen::v4();

    let msg = GridTaskSpawn {
        kind: GridMsgKind::TaskSpawn as u8,
        _pad: [0u8; 3],
        correlation_uuid: correlation.to_bytes(),
        task_binary_uuid: task_binary_uuid.to_bytes(),
        arg,
        required_caps,
        _pad2: [0u8; 7],
    };

    let mut buf = [0u8; GridTaskSpawn::SIZE];
    let mbytes: &[u8; GridTaskSpawn::SIZE] = unsafe { core::mem::transmute(&msg) };
    buf.copy_from_slice(mbytes);

    // Send via UDP to peer's link-local address on GRID_UDP_PORT.
    if !send_udp(peer.link_local_ip, &buf) {
        return None;
    }

    // Record pending.
    let mut p = PENDING.lock();
    for slot in p.iter_mut() {
        if !slot.active {
            *slot = PendingSpawn {
                active: true,
                correlation_uuid: correlation,
                requester_task_idx,
                deadline_ms: now_ms + 2000,
            };
            return Some(correlation);
        }
    }
    None
}

/// Handle an incoming grid task protocol message.
pub fn handle_incoming(src_ip: [u8; 16], payload: &[u8], now_ms: u64) {
    if payload.is_empty() {
        return;
    }
    let kind = GridMsgKind::from_u8(payload[0]);
    match kind {
        GridMsgKind::TaskSpawn if payload.len() >= GridTaskSpawn::SIZE => {
            let msg: &GridTaskSpawn = unsafe { &*(payload.as_ptr() as *const GridTaskSpawn) };
            // Accept if we have capacity.
            let r = super::resource::can_accept_task(msg.required_caps, 4);
            let mut reply = GridTaskSpawnReply {
                kind: GridMsgKind::TaskSpawnReply as u8,
                status: if r { 0 } else { 2 },
                _pad: [0u8; 2],
                correlation_uuid: msg.correlation_uuid,
                remote_task_uuid: [0u8; 16],
            };
            if r {
                let remote_uuid = crate::uuid::Uuid128Gen::v4();
                reply.remote_task_uuid = remote_uuid.to_bytes();
                // Launch the task binary referenced by task_binary_uuid via VFS.
                // The binary is stored at /bin/<hex-uuid> as a flat executable.
                let uuid = crate::uuid::Uuid128::from_bytes(msg.task_binary_uuid);
                let uuid_bytes = uuid.to_bytes();
                let mut path = [0u8; 46]; // b"/bin/" (5) + 32 hex chars + NUL
                path[..5].copy_from_slice(b"/bin/");
                // Encode UUID as 32 lowercase hex digits.
                const HEX: &[u8; 16] = b"0123456789abcdef";
                for (i, &b) in uuid_bytes.iter().enumerate() {
                    path[5 + i * 2] = HEX[(b >> 4) as usize];
                    path[5 + i * 2 + 1] = HEX[(b & 0xF) as usize];
                }
                let path_len = 5 + 32;
                if let Ok(fd) = crate::vfs::open(&path[..path_len]) {
                    // Read up to 64 KiB of the binary to get the entry point.
                    const MAX_BINARY: usize = 65536;
                    static mut TASK_BIN_BUF: [u8; MAX_BINARY] = [0u8; MAX_BINARY];
                    let bin = unsafe { &mut *core::ptr::addr_of_mut!(TASK_BIN_BUF) };
                    let n = crate::vfs::read(fd, bin).unwrap_or(0);
                    let _ = crate::vfs::close(fd);
                    if n >= 8 {
                        // The entry point is the first 8 bytes as a little-endian u64.
                        // (GraphOS flat binary format: u64 entry_phys at offset 0.)
                        let entry = u64::from_le_bytes(bin[..8].try_into().unwrap_or([0u8; 8]));
                        if entry != 0 {
                            let mut name = [0u8; 16];
                            let nlen = path_len.min(16);
                            name[..nlen].copy_from_slice(&path[..nlen]);
                            let _ = crate::task::table::create_kernel_task(&name[..nlen], entry);
                        }
                    }
                }
            }
            let rbytes: &[u8; GridTaskSpawnReply::SIZE] = unsafe { core::mem::transmute(&reply) };
            let _ = send_udp(src_ip, rbytes);
        }
        GridMsgKind::TaskSpawnReply if payload.len() >= GridTaskSpawnReply::SIZE => {
            let reply: &GridTaskSpawnReply =
                unsafe { &*(payload.as_ptr() as *const GridTaskSpawnReply) };
            let corr = Uuid128::from_bytes(reply.correlation_uuid);
            let mut p = PENDING.lock();
            for slot in p.iter_mut() {
                if slot.active && slot.correlation_uuid == corr {
                    slot.active = false;
                    // Unblock the waiting task.
                    crate::sched::wake_task(slot.requester_task_idx);
                    break;
                }
            }
        }
        _ => {}
    }

    // Expire timed-out pending spawns.
    let mut p = PENDING.lock();
    for slot in p.iter_mut() {
        if slot.active && now_ms > slot.deadline_ms {
            slot.active = false;
            crate::sched::wake_task(slot.requester_task_idx);
        }
    }
}

/// Send `data` as a UDP payload to `dst_ip:GRID_UDP_PORT`.
fn send_udp(dst_ip: [u8; 16], data: &[u8]) -> bool {
    use crate::net::ipv6::{IPV6_HEADER_BYTES, Ipv6Addr, Ipv6Header, PROTO_UDP, encode_header};
    use core::sync::atomic::Ordering;

    let src_ip = Ipv6Addr(crate::net::OUR_IPV6.load(Ordering::Relaxed));
    let dst = Ipv6Addr(dst_ip);

    let udp_len = 8 + data.len();
    let total = IPV6_HEADER_BYTES + udp_len;
    // +14 Ethernet header allocated in virtio_net::transmit via a scratch buffer.
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
