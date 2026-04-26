// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! IPv6 packet encode / decode (RFC 8200).
//!
//! GraphOS uses IPv6 as the primary transport for all grid operations.
//! Link-local addresses (fe80::/10) are auto-configured from the MAC address
//! using the modified EUI-64 procedure, giving every NIC a collision-free
//! address without DHCPv6 or manual configuration.

pub const ETHERTYPE_IPV6: u16 = 0x86DD;
pub const IPV6_HEADER_BYTES: usize = 40;
pub const IPV6_HOP_LIMIT_DEFAULT: u8 = 64;

// Well-known IPv6 protocol numbers
pub const PROTO_HOPBYHOP: u8 = 0;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;
pub const PROTO_ICMPV6: u8 = 58;
pub const PROTO_NONE: u8 = 59;

/// A 128-bit IPv6 address stored as big-endian bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ipv6Addr(pub [u8; 16]);

impl Ipv6Addr {
    pub const UNSPECIFIED: Self = Self([0u8; 16]);
    pub const LOOPBACK: Self = Self([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

    /// All-nodes multicast `ff02::1` — used for grid discovery.
    pub const ALL_NODES: Self = Self([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01]);

    /// All-routers multicast `ff02::2`.
    pub const ALL_ROUTERS: Self = Self([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]);

    /// GraphOS grid multicast `ff02::6749` — "gO" in hex.
    /// All nodes participating in a grid cluster join this group.
    pub const GRID_MULTICAST: Self =
        Self([0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x67, 0x49]);

    /// Solicited-node multicast prefix `ff02::1:ff00:0/104`.
    pub fn solicited_node(addr: &Ipv6Addr) -> Self {
        let mut out = [0u8; 16];
        out[0] = 0xff;
        out[1] = 0x02;
        out[11] = 0x01;
        out[12] = 0xff;
        out[13] = addr.0[13];
        out[14] = addr.0[14];
        out[15] = addr.0[15];
        Self(out)
    }

    /// Build a link-local address from a MAC address using EUI-64.
    ///
    /// `fe80::` + modified EUI-64 from 48-bit MAC.
    pub fn link_local_from_mac(mac: [u8; 6]) -> Self {
        let mut out = [0u8; 16];
        out[0] = 0xfe;
        out[1] = 0x80;
        // EUI-64: insert 0xFFFE in the middle and flip the U/L bit.
        out[8] = mac[0] ^ 0x02;
        out[9] = mac[1];
        out[10] = mac[2];
        out[11] = 0xff;
        out[12] = 0xfe;
        out[13] = mac[3];
        out[14] = mac[4];
        out[15] = mac[5];
        Self(out)
    }

    /// True if this is a link-local address (fe80::/10).
    pub fn is_link_local(&self) -> bool {
        self.0[0] == 0xfe && (self.0[1] & 0xc0) == 0x80
    }

    /// True if this is a multicast address (ff00::/8).
    pub fn is_multicast(&self) -> bool {
        self.0[0] == 0xff
    }

    /// Multicast MAC for this IPv6 multicast address (33:33:xx:xx:xx:xx).
    pub fn multicast_mac(&self) -> [u8; 6] {
        [0x33, 0x33, self.0[12], self.0[13], self.0[14], self.0[15]]
    }
}

/// Fixed 40-byte IPv6 header.
#[derive(Clone, Copy, Debug)]
pub struct Ipv6Header {
    /// Traffic class + flow label (packed).
    pub flow_label: u32,
    /// Payload length (bytes after the 40-byte header).
    pub payload_len: u16,
    /// Next header protocol number.
    pub next_header: u8,
    /// Hop limit (TTL equivalent).
    pub hop_limit: u8,
    pub src: Ipv6Addr,
    pub dst: Ipv6Addr,
}

/// Parse the 40-byte IPv6 fixed header from `packet`.
pub fn parse_header(packet: &[u8]) -> Option<Ipv6Header> {
    if packet.len() < IPV6_HEADER_BYTES {
        return None;
    }
    let version = packet[0] >> 4;
    if version != 6 {
        return None;
    }
    let flow_label = u32::from_be_bytes([packet[0] & 0x0f, packet[1], packet[2], packet[3]]);
    let payload_len = u16::from_be_bytes([packet[4], packet[5]]);
    let next_header = packet[6];
    let hop_limit = packet[7];
    let mut src = [0u8; 16];
    let mut dst = [0u8; 16];
    src.copy_from_slice(&packet[8..24]);
    dst.copy_from_slice(&packet[24..40]);
    Some(Ipv6Header {
        flow_label,
        payload_len,
        next_header,
        hop_limit,
        src: Ipv6Addr(src),
        dst: Ipv6Addr(dst),
    })
}

/// Encode an IPv6 header into `out`. Returns bytes written (always 40) or `None`
/// if `out` is too short.
pub fn encode_header(out: &mut [u8], hdr: &Ipv6Header) -> Option<usize> {
    if out.len() < IPV6_HEADER_BYTES {
        return None;
    }
    out[0] = 0x60 | ((hdr.flow_label >> 24) as u8 & 0x0f);
    out[1] = (hdr.flow_label >> 16) as u8;
    out[2] = (hdr.flow_label >> 8) as u8;
    out[3] = hdr.flow_label as u8;
    out[4..6].copy_from_slice(&hdr.payload_len.to_be_bytes());
    out[6] = hdr.next_header;
    out[7] = hdr.hop_limit;
    out[8..24].copy_from_slice(&hdr.src.0);
    out[24..40].copy_from_slice(&hdr.dst.0);
    Some(IPV6_HEADER_BYTES)
}

/// Compute the IPv6 upper-layer checksum (pseudo-header + payload).
/// Used by ICMPv6, TCP, and UDP over IPv6.
pub fn upper_layer_checksum(
    src: &Ipv6Addr,
    dst: &Ipv6Addr,
    next_header: u8,
    payload: &[u8],
) -> u16 {
    let mut sum = 0u32;

    // Pseudo-header: src (16) + dst (16) + upper-layer length (4) + zeros (3) + next_header (1)
    for chunk in src.0.chunks(2) {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    for chunk in dst.0.chunks(2) {
        sum = sum.wrapping_add(u16::from_be_bytes([chunk[0], chunk[1]]) as u32);
    }
    let len = payload.len() as u32;
    sum = sum.wrapping_add(len >> 16);
    sum = sum.wrapping_add(len & 0xffff);
    sum = sum.wrapping_add(next_header as u32);

    // Payload
    let mut i = 0;
    while i + 1 < payload.len() {
        sum = sum.wrapping_add(u16::from_be_bytes([payload[i], payload[i + 1]]) as u32);
        i += 2;
    }
    if i < payload.len() {
        sum = sum.wrapping_add((payload[i] as u32) << 8);
    }

    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}
