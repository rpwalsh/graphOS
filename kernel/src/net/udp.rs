// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! In-kernel UDP layer.
//!
//! Provides:
//! - `send(src_port, dst_ip, dst_port, payload)` — build UDP datagram and transmit via IPv4
//! - `handle_datagram(src_ip, src_port, dst_port, payload)` — demux to registered receive
//!   buffers; called from the IPv4 receive path
//! - `bind(port, socket_key)` / `unbind(socket_key)` — associate a socket handle with a port
//! - `recv(socket_key, out)` — drain buffered datagrams

use spin::Mutex;

const UDP_MAX_SOCKS: usize = 16;
const UDP_RECV_BUF: usize = 2048;
const UDP_HEADER_BYTES: usize = 8;
const IP_PROTO_UDP: u8 = 0x11;

// ── checksum ──────────────────────────────────────────────────────────────────

fn udp_checksum(src_ip: u32, dst_ip: u32, udp_seg: &[u8]) -> u16 {
    let seg_len = udp_seg.len() as u16;
    // 12-byte pseudo-header: src(4) dst(4) 0x00 proto(0x11) udp_len(2)
    let mut sum: u32 = 0;
    macro_rules! fold {
        ($w:expr) => {
            sum = sum.wrapping_add($w as u32);
        };
    }
    fold!(src_ip >> 16);
    fold!(src_ip & 0xFFFF);
    fold!(dst_ip >> 16);
    fold!(dst_ip & 0xFFFF);
    fold!(IP_PROTO_UDP as u32);
    fold!(seg_len);
    let mut i = 0;
    while i + 1 < udp_seg.len() {
        fold!(((udp_seg[i] as u32) << 8) | udp_seg[i + 1] as u32);
        i += 2;
    }
    if i < udp_seg.len() {
        fold!((udp_seg[i] as u32) << 8);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let result = !(sum as u16);
    // RFC 768: if checksum computes to 0, send 0xFFFF
    if result == 0 { 0xFFFF } else { result }
}

// ── receive socket table ───────────────────────────────────────────────────────

struct UdpSocket {
    active: bool,
    local_port: u16,
    socket_key: [u8; 16],
    recv_buf: [u8; UDP_RECV_BUF],
    recv_len: usize,
    /// Source IP of the last received datagram.
    last_src_ip: u32,
    /// Source port of the last received datagram.
    last_src_port: u16,
}

impl UdpSocket {
    const fn empty() -> Self {
        Self {
            active: false,
            local_port: 0,
            socket_key: [0; 16],
            recv_buf: [0; UDP_RECV_BUF],
            recv_len: 0,
            last_src_ip: 0,
            last_src_port: 0,
        }
    }
}

struct UdpTable {
    socks: [UdpSocket; UDP_MAX_SOCKS],
}

impl UdpTable {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const E: UdpSocket = UdpSocket::empty();
        Self {
            socks: [E; UDP_MAX_SOCKS],
        }
    }
}

static UDP: Mutex<UdpTable> = Mutex::new(UdpTable::new());

// ── public API ────────────────────────────────────────────────────────────────

/// Bind `socket_key` to `local_port` for UDP receive.
/// Returns `false` if the port is already bound or the table is full.
pub fn bind(socket_key: [u8; 16], local_port: u16) -> bool {
    let mut tbl = UDP.lock();
    // Reject duplicate port binding.
    if tbl
        .socks
        .iter()
        .any(|s| s.active && s.local_port == local_port)
    {
        return false;
    }
    for slot in &mut tbl.socks {
        if !slot.active {
            *slot = UdpSocket {
                active: true,
                local_port,
                socket_key,
                recv_buf: [0; UDP_RECV_BUF],
                recv_len: 0,
                last_src_ip: 0,
                last_src_port: 0,
            };
            return true;
        }
    }
    false
}

/// Remove the UDP binding for `socket_key`.
pub fn unbind(socket_key: [u8; 16]) {
    let mut tbl = UDP.lock();
    for slot in &mut tbl.socks {
        if slot.active && slot.socket_key == socket_key {
            *slot = UdpSocket::empty();
            return;
        }
    }
}

/// Transmit a UDP datagram.
pub fn send(src_port: u16, dst_ip: u32, dst_port: u16, payload: &[u8]) -> bool {
    let our_ip = super::OUR_IPV4;
    let seg_len = UDP_HEADER_BYTES + payload.len();
    if seg_len > 1480 {
        return false; // too large for one Ethernet frame
    }
    let mut seg = [0u8; UDP_HEADER_BYTES + 1472];
    seg[0..2].copy_from_slice(&src_port.to_be_bytes());
    seg[2..4].copy_from_slice(&dst_port.to_be_bytes());
    seg[4..6].copy_from_slice(&(seg_len as u16).to_be_bytes());
    // checksum at [6..8] — computed below
    seg[UDP_HEADER_BYTES..seg_len].copy_from_slice(payload);

    let csum = udp_checksum(our_ip, dst_ip, &seg[..seg_len]);
    seg[6..8].copy_from_slice(&csum.to_be_bytes());

    super::transmit_ipv4_packet(dst_ip, IP_PROTO_UDP, &seg[..seg_len]);
    true
}

/// Handle a received UDP segment (called from the IPv4 receive path).
/// `payload` starts at the UDP header.
pub fn handle_datagram(src_ip: u32, segment: &[u8]) {
    if segment.len() < UDP_HEADER_BYTES {
        return;
    }
    let src_port = u16::from_be_bytes([segment[0], segment[1]]);
    let dst_port = u16::from_be_bytes([segment[2], segment[3]]);
    let data = &segment[UDP_HEADER_BYTES..];

    let mut tbl = UDP.lock();
    for slot in &mut tbl.socks {
        if slot.active && slot.local_port == dst_port {
            let copy = core::cmp::min(data.len(), UDP_RECV_BUF - slot.recv_len);
            if copy > 0 {
                let start = slot.recv_len;
                slot.recv_buf[start..start + copy].copy_from_slice(&data[..copy]);
                slot.recv_len += copy;
            }
            slot.last_src_ip = src_ip;
            slot.last_src_port = src_port;
            return;
        }
    }
    // No socket listening on dst_port — silently drop.
}

/// Drain received data for `socket_key` into `out`.
/// Returns bytes copied, `Some(0)` if no data, `None` if no binding.
pub fn recv(socket_key: [u8; 16], out: &mut [u8]) -> Option<usize> {
    let mut tbl = UDP.lock();
    for slot in &mut tbl.socks {
        if slot.active && slot.socket_key == socket_key {
            if slot.recv_len == 0 {
                return Some(0);
            }
            let copy = core::cmp::min(out.len(), slot.recv_len);
            out[..copy].copy_from_slice(&slot.recv_buf[..copy]);
            slot.recv_buf.copy_within(copy..slot.recv_len, 0);
            slot.recv_len -= copy;
            return Some(copy);
        }
    }
    None
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_bind_recv_unbind() {
        // Reset table.
        {
            let mut tbl = UDP.lock();
            for s in tbl.socks.iter_mut() {
                *s = UdpSocket::empty();
            }
        }

        let key = [0xAAu8; 16];
        assert!(bind(key, 5000));
        // Duplicate port rejected.
        assert!(!bind([0xBBu8; 16], 5000));

        // Inject a datagram (full 8-byte UDP header: src_port, dst_port, len, checksum).
        let datagram = b"\x13\x88\x13\x88\x00\x0D\x00\x00hello"; // src:5000 dst:5000 len=13 cksum=0
        // We call handle_datagram with just the UDP segment starting at header.
        handle_datagram(0x0A00_0201, datagram);

        let mut out = [0u8; 64];
        let n = recv(key, &mut out).unwrap();
        // Payload after 8-byte UDP header = "hello"
        assert_eq!(n, 5);
        assert_eq!(&out[..5], b"hello");

        unbind(key);
        // After unbind, recv returns None.
        assert!(recv(key, &mut out).is_none());
    }
}
