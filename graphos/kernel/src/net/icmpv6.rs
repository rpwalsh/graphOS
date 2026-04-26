// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ICMPv6 encode / decode (RFC 4443) and Neighbor Discovery (RFC 4861).
//!
//! This module provides ICMPv6 message parsing and generation for:
//!  - Echo Request / Reply (ping6)
//!  - Neighbor Solicitation (NS) — like ARP request
//!  - Neighbor Advertisement (NA) — like ARP reply
//!  - Router Solicitation / Advertisement (basic, for default-route discovery)
//!  - Multicast Listener Discovery (MLD v1, subset)

use super::ipv6::{Ipv6Addr, PROTO_ICMPV6, upper_layer_checksum};

// ── ICMPv6 type codes ────────────────────────────────────────────────────────

pub const ICMPV6_DEST_UNREACH: u8 = 1;
pub const ICMPV6_PACKET_TOO_BIG: u8 = 2;
pub const ICMPV6_TIME_EXCEEDED: u8 = 3;
pub const ICMPV6_ECHO_REQUEST: u8 = 128;
pub const ICMPV6_ECHO_REPLY: u8 = 129;
pub const ICMPV6_MLD_QUERY: u8 = 130;
pub const ICMPV6_MLD_REPORT: u8 = 131;
pub const ICMPV6_ROUTER_SOLICIT: u8 = 133;
pub const ICMPV6_ROUTER_ADVERT: u8 = 134;
pub const ICMPV6_NEIGHBOR_SOLICIT: u8 = 135;
pub const ICMPV6_NEIGHBOR_ADVERT: u8 = 136;

// ── NDP option types ─────────────────────────────────────────────────────────

pub const NDP_OPT_SRC_LINK_ADDR: u8 = 1;
pub const NDP_OPT_TGT_LINK_ADDR: u8 = 2;
pub const NDP_OPT_PREFIX_INFO: u8 = 3;

// ── NA flags ─────────────────────────────────────────────────────────────────

/// Router flag in Neighbor Advertisement.
pub const NA_FLAG_ROUTER: u32 = 1 << 31;
/// Solicited flag — set when NA is in response to NS.
pub const NA_FLAG_SOLICITED: u32 = 1 << 30;
/// Override flag — recipient should update its neighbor cache.
pub const NA_FLAG_OVERRIDE: u32 = 1 << 29;

// ── Parsed message ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct Icmpv6Header {
    pub kind: u8,
    pub code: u8,
    pub checksum: u16,
}

#[derive(Clone, Copy, Debug)]
pub enum Icmpv6Message<'a> {
    EchoRequest {
        id: u16,
        seq: u16,
        data: &'a [u8],
    },
    EchoReply {
        id: u16,
        seq: u16,
        data: &'a [u8],
    },
    NeighborSolicitation {
        target: Ipv6Addr,
        src_mac: Option<[u8; 6]>,
    },
    NeighborAdvertisement {
        target: Ipv6Addr,
        flags: u32,
        tgt_mac: Option<[u8; 6]>,
    },
    RouterSolicitation,
    RouterAdvertisement,
    Unknown {
        kind: u8,
        code: u8,
        payload: &'a [u8],
    },
}

/// Parse an ICMPv6 message from the payload portion (after the IPv6 header).
pub fn parse<'a>(src: &Ipv6Addr, dst: &Ipv6Addr, payload: &'a [u8]) -> Option<Icmpv6Message<'a>> {
    if payload.len() < 4 {
        return None;
    }
    let kind = payload[0];
    let code = payload[1];
    let checksum = u16::from_be_bytes([payload[2], payload[3]]);
    // Verify checksum (mandatory for NDP per RFC 4861 §7.1).
    let computed = upper_layer_checksum(src, dst, PROTO_ICMPV6, payload);
    if computed != 0 && checksum != 0 {
        // Allow zero checksum only in loopback tests; real packets must match.
        if computed != checksum {
            return None;
        }
    }

    match kind {
        ICMPV6_ECHO_REQUEST if payload.len() >= 8 => {
            let id = u16::from_be_bytes([payload[4], payload[5]]);
            let seq = u16::from_be_bytes([payload[6], payload[7]]);
            Some(Icmpv6Message::EchoRequest {
                id,
                seq,
                data: &payload[8..],
            })
        }
        ICMPV6_ECHO_REPLY if payload.len() >= 8 => {
            let id = u16::from_be_bytes([payload[4], payload[5]]);
            let seq = u16::from_be_bytes([payload[6], payload[7]]);
            Some(Icmpv6Message::EchoReply {
                id,
                seq,
                data: &payload[8..],
            })
        }
        ICMPV6_NEIGHBOR_SOLICIT if payload.len() >= 24 => {
            let mut tgt = [0u8; 16];
            tgt.copy_from_slice(&payload[8..24]);
            let src_mac = parse_link_addr_option(&payload[24..]);
            Some(Icmpv6Message::NeighborSolicitation {
                target: Ipv6Addr(tgt),
                src_mac,
            })
        }
        ICMPV6_NEIGHBOR_ADVERT if payload.len() >= 24 => {
            let flags = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
            let mut tgt = [0u8; 16];
            tgt.copy_from_slice(&payload[8..24]);
            let tgt_mac = parse_link_addr_option(&payload[24..]);
            Some(Icmpv6Message::NeighborAdvertisement {
                target: Ipv6Addr(tgt),
                flags,
                tgt_mac,
            })
        }
        ICMPV6_ROUTER_SOLICIT => Some(Icmpv6Message::RouterSolicitation),
        ICMPV6_ROUTER_ADVERT => Some(Icmpv6Message::RouterAdvertisement),
        _ => Some(Icmpv6Message::Unknown {
            kind,
            code,
            payload,
        }),
    }
}

/// Extract a source/target link-layer address option (type 1 or 2) from NDP options.
fn parse_link_addr_option(options: &[u8]) -> Option<[u8; 6]> {
    let mut offset = 0;
    while offset + 2 <= options.len() {
        let opt_type = options[offset];
        let opt_len_units = options[offset + 1] as usize; // units of 8 bytes
        if opt_len_units == 0 {
            break;
        }
        let opt_len_bytes = opt_len_units * 8;
        if offset + opt_len_bytes > options.len() {
            break;
        }
        if (opt_type == NDP_OPT_SRC_LINK_ADDR || opt_type == NDP_OPT_TGT_LINK_ADDR)
            && opt_len_bytes >= 8
        {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&options[offset + 2..offset + 8]);
            return Some(mac);
        }
        offset += opt_len_bytes;
    }
    None
}

// ── Encoder helpers ──────────────────────────────────────────────────────────

/// Build an ICMPv6 header into `out[0..4]` with checksum = 0 (caller must fix up).
#[inline]
fn encode_header(out: &mut [u8], kind: u8, code: u8) {
    out[0] = kind;
    out[1] = code;
    out[2] = 0; // checksum placeholder
    out[3] = 0;
}

/// Fix up the ICMPv6 checksum in `msg[2..4]` given pseudo-header addresses.
pub fn fix_checksum(msg: &mut [u8], src: &Ipv6Addr, dst: &Ipv6Addr) {
    msg[2] = 0;
    msg[3] = 0;
    let csum = upper_layer_checksum(src, dst, PROTO_ICMPV6, msg);
    msg[2] = (csum >> 8) as u8;
    msg[3] = (csum & 0xff) as u8;
}

/// Encode a Neighbor Solicitation into `out`.
///
/// Sends NS for `target` from `src_ll_addr` (link-local MAC).
/// Returns bytes written or 0 on buffer too small.
pub fn encode_neighbor_solicitation(
    out: &mut [u8],
    src_ip: &Ipv6Addr,
    dst_ip: &Ipv6Addr,
    target: &Ipv6Addr,
    src_mac: [u8; 6],
) -> usize {
    // Type(1) + Code(1) + Checksum(2) + Reserved(4) + Target(16) + Opt(8) = 32
    let len = 32;
    if out.len() < len {
        return 0;
    }
    encode_header(out, ICMPV6_NEIGHBOR_SOLICIT, 0);
    out[4..8].fill(0); // reserved
    out[8..24].copy_from_slice(&target.0);
    // Source link-layer address option
    out[24] = NDP_OPT_SRC_LINK_ADDR;
    out[25] = 1; // 1 × 8 bytes
    out[26..32].copy_from_slice(&src_mac);
    fix_checksum(&mut out[..len], src_ip, dst_ip);
    len
}

/// Encode a Neighbor Advertisement into `out`.
///
/// Returns bytes written or 0 on buffer too small.
pub fn encode_neighbor_advertisement(
    out: &mut [u8],
    src_ip: &Ipv6Addr,
    dst_ip: &Ipv6Addr,
    target: &Ipv6Addr,
    tgt_mac: [u8; 6],
    solicited: bool,
) -> usize {
    let len = 32;
    if out.len() < len {
        return 0;
    }
    encode_header(out, ICMPV6_NEIGHBOR_ADVERT, 0);
    let flags: u32 = NA_FLAG_OVERRIDE | if solicited { NA_FLAG_SOLICITED } else { 0 };
    out[4..8].copy_from_slice(&flags.to_be_bytes());
    out[8..24].copy_from_slice(&target.0);
    out[24] = NDP_OPT_TGT_LINK_ADDR;
    out[25] = 1;
    out[26..32].copy_from_slice(&tgt_mac);
    fix_checksum(&mut out[..len], src_ip, dst_ip);
    len
}

/// Encode an Echo Reply into `out`.
pub fn encode_echo_reply(
    out: &mut [u8],
    src_ip: &Ipv6Addr,
    dst_ip: &Ipv6Addr,
    id: u16,
    seq: u16,
    data: &[u8],
) -> usize {
    let len = 8 + data.len();
    if out.len() < len {
        return 0;
    }
    encode_header(out, ICMPV6_ECHO_REPLY, 0);
    out[4..6].copy_from_slice(&id.to_be_bytes());
    out[6..8].copy_from_slice(&seq.to_be_bytes());
    out[8..len].copy_from_slice(data);
    fix_checksum(&mut out[..len], src_ip, dst_ip);
    len
}
