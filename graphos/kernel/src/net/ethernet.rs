// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Ethernet II frame encode / decode.
//!
//! Handles the 14-byte Ethernet header (dst MAC, src MAC, EtherType) used
//! for all frames delivered to and from the virtio-net data path.

pub const ETHERNET_HEADER_SIZE: usize = 14;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// Parsed Ethernet II header with a reference to the payload slice.
#[derive(Clone, Copy)]
pub struct EthernetFrame<'a> {
    pub dst_mac: [u8; 6],
    pub src_mac: [u8; 6],
    pub ethertype: u16,
    pub payload: &'a [u8],
}

/// Parse an Ethernet II frame from a raw byte slice.
///
/// Returns `None` if the slice is shorter than 14 bytes.
pub fn parse(data: &[u8]) -> Option<EthernetFrame<'_>> {
    if data.len() < ETHERNET_HEADER_SIZE {
        return None;
    }
    let mut dst_mac = [0u8; 6];
    let mut src_mac = [0u8; 6];
    dst_mac.copy_from_slice(&data[0..6]);
    src_mac.copy_from_slice(&data[6..12]);
    let ethertype = u16::from_be_bytes([data[12], data[13]]);
    Some(EthernetFrame {
        dst_mac,
        src_mac,
        ethertype,
        payload: &data[ETHERNET_HEADER_SIZE..],
    })
}

/// Encode an Ethernet II frame into `out`.
///
/// Returns the total number of bytes written (header + payload length).
/// Returns 0 if `out` is too short to hold the frame.
pub fn encode(
    dst_mac: [u8; 6],
    src_mac: [u8; 6],
    ethertype: u16,
    payload: &[u8],
    out: &mut [u8],
) -> usize {
    let total = ETHERNET_HEADER_SIZE + payload.len();
    if out.len() < total {
        return 0;
    }
    out[0..6].copy_from_slice(&dst_mac);
    out[6..12].copy_from_slice(&src_mac);
    out[12] = (ethertype >> 8) as u8;
    out[13] = (ethertype & 0xFF) as u8;
    out[ETHERNET_HEADER_SIZE..total].copy_from_slice(payload);
    total
}

/// Broadcast MAC address (FF:FF:FF:FF:FF:FF).
pub const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

/// Returns `true` when the MAC is the all-ones broadcast address.
pub fn is_broadcast(mac: [u8; 6]) -> bool {
    mac == BROADCAST_MAC
}
