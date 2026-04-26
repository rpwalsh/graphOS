// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
pub const ICMP_ECHO_REPLY: u8 = 0;
pub const ICMP_ECHO_REQUEST: u8 = 8;
pub const ICMP_HEADER_BYTES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EchoHeader {
    pub typ: u8,
    pub code: u8,
    pub ident: u16,
    pub sequence: u16,
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

pub fn build_echo(
    typ: u8,
    ident: u16,
    sequence: u16,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    let total = ICMP_HEADER_BYTES + payload.len();
    if out.len() < total {
        return None;
    }

    out[0] = typ;
    out[1] = 0;
    out[2..4].copy_from_slice(&0u16.to_be_bytes());
    out[4..6].copy_from_slice(&ident.to_be_bytes());
    out[6..8].copy_from_slice(&sequence.to_be_bytes());
    out[8..total].copy_from_slice(payload);
    let csum = checksum(&out[..total]);
    out[2..4].copy_from_slice(&csum.to_be_bytes());
    Some(total)
}

pub fn parse_echo(packet: &[u8]) -> Option<(EchoHeader, &[u8])> {
    if packet.len() < ICMP_HEADER_BYTES {
        return None;
    }
    if checksum(packet) != 0 {
        return None;
    }
    let header = EchoHeader {
        typ: packet[0],
        code: packet[1],
        ident: u16::from_be_bytes([packet[4], packet[5]]),
        sequence: u16::from_be_bytes([packet[6], packet[7]]),
    };
    Some((header, &packet[ICMP_HEADER_BYTES..]))
}

pub fn reply_from_request(request: &[u8], out: &mut [u8]) -> Option<usize> {
    let (header, payload) = parse_echo(request)?;
    if header.typ != ICMP_ECHO_REQUEST || header.code != 0 {
        return None;
    }
    build_echo(ICMP_ECHO_REPLY, header.ident, header.sequence, payload, out)
}
