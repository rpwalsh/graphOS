// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const IP_PROTOCOL_ICMP: u8 = 1;
pub const IPV4_HEADER_BYTES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipv4Header {
    pub total_len: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub src: u32,
    pub dst: u32,
    pub ident: u16,
}

pub fn checksum(words: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0usize;
    while i + 1 < words.len() {
        sum = sum.wrapping_add(u16::from_be_bytes([words[i], words[i + 1]]) as u32);
        i += 2;
    }
    if i < words.len() {
        sum = sum.wrapping_add((words[i] as u32) << 8);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

pub fn encode_header(
    out: &mut [u8],
    total_len: u16,
    ttl: u8,
    protocol: u8,
    src: u32,
    dst: u32,
    ident: u16,
) -> Option<usize> {
    if out.len() < IPV4_HEADER_BYTES {
        return None;
    }

    out[0] = 0x45;
    out[1] = 0;
    out[2..4].copy_from_slice(&total_len.to_be_bytes());
    out[4..6].copy_from_slice(&ident.to_be_bytes());
    out[6..8].copy_from_slice(&0u16.to_be_bytes());
    out[8] = ttl;
    out[9] = protocol;
    out[10..12].copy_from_slice(&0u16.to_be_bytes());
    out[12..16].copy_from_slice(&src.to_be_bytes());
    out[16..20].copy_from_slice(&dst.to_be_bytes());
    let csum = checksum(&out[..IPV4_HEADER_BYTES]);
    out[10..12].copy_from_slice(&csum.to_be_bytes());
    Some(IPV4_HEADER_BYTES)
}

pub fn parse_header(packet: &[u8]) -> Option<Ipv4Header> {
    if packet.len() < IPV4_HEADER_BYTES {
        return None;
    }
    if packet[0] >> 4 != 4 || (packet[0] & 0x0F) != 5 {
        return None;
    }
    if checksum(&packet[..IPV4_HEADER_BYTES]) != 0 {
        return None;
    }

    Some(Ipv4Header {
        total_len: u16::from_be_bytes([packet[2], packet[3]]),
        ident: u16::from_be_bytes([packet[4], packet[5]]),
        ttl: packet[8],
        protocol: packet[9],
        src: u32::from_be_bytes([packet[12], packet[13], packet[14], packet[15]]),
        dst: u32::from_be_bytes([packet[16], packet[17], packet[18], packet[19]]),
    })
}
