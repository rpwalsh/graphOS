// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal DHCP client (DISCOVER → OFFER → REQUEST → ACK).
//!
//! Uses a statically-allocated exchange buffer.  Once ACK is received the
//! kernel IP address is updated via `super::set_our_ipv4()`.
//!
//! Transmission uses UDP port 68 (client) → 255.255.255.255:67 (server).
//! This is called from the network init path after link-ready.

use spin::Mutex;

const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;
const BROADCAST_IP: u32 = 0xFFFF_FFFF;

// DHCP message types.
const DHCP_DISCOVER: u8 = 1;
const DHCP_OFFER: u8 = 2;
const DHCP_REQUEST: u8 = 3;
const DHCP_ACK: u8 = 5;

// Fixed-layout DHCP packet fields (legacy BOOTP header: 236 bytes + magic cookie).
const BOOTP_HEADER_LEN: usize = 236;
const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

// DHCP option tags.
const OPT_MSG_TYPE: u8 = 53;
const OPT_PARAMETER_REQUEST_LIST: u8 = 55;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_SERVER_ID: u8 = 54;
const OPT_END: u8 = 255;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum DhcpState {
    Idle = 0,
    Discovering = 1,
    Requesting = 2,
    Bound = 3,
}

struct DhcpClient {
    state: DhcpState,
    xid: u32, // transaction ID
    our_mac: [u8; 6],
    offered_ip: u32,
    server_ip: u32,
    assigned_ip: u32,
}

impl DhcpClient {
    const fn new() -> Self {
        Self {
            state: DhcpState::Idle,
            xid: 0xC0DE_BABE,
            our_mac: [0; 6],
            offered_ip: 0,
            server_ip: 0,
            assigned_ip: 0,
        }
    }
}

static CLIENT: Mutex<DhcpClient> = Mutex::new(DhcpClient::new());

// ── packet builder ────────────────────────────────────────────────────────────

/// Fill a BOOTP/DHCP header into `buf`.  Returns the byte count written.
/// `msg_type` is the DHCP message type option value (DISCOVER=1, REQUEST=3).
/// `ciaddr` is the client IP (0 for DISCOVER, offered IP for REQUEST).
/// `options_extra` appended after the message-type option.
fn build_dhcp_packet(
    buf: &mut [u8; 548],
    msg_type: u8,
    xid: u32,
    mac: [u8; 6],
    ciaddr: u32,
    siaddr: u32,
    extra_options: &[u8],
) -> usize {
    buf.fill(0);
    buf[0] = 1; // op = BOOTREQUEST
    buf[1] = 1; // htype = Ethernet
    buf[2] = 6; // hlen = 6
    buf[3] = 0; // hops
    buf[4..8].copy_from_slice(&xid.to_be_bytes());
    // secs, flags — left 0
    buf[12..16].copy_from_slice(&ciaddr.to_be_bytes()); // ciaddr
    // yiaddr, siaddr, giaddr — leave 0
    buf[28..34].copy_from_slice(&mac); // chaddr (first 6 bytes)
    // sname[64], file[128] — zeroed
    buf[BOOTP_HEADER_LEN..BOOTP_HEADER_LEN + 4].copy_from_slice(&DHCP_MAGIC_COOKIE);
    let _ = siaddr; // used in REQUEST below via extra_options

    // Options.
    let mut pos = BOOTP_HEADER_LEN + 4;
    // Option 53 — DHCP message type.
    buf[pos] = OPT_MSG_TYPE;
    pos += 1;
    buf[pos] = 1;
    pos += 1; // length
    buf[pos] = msg_type;
    pos += 1;
    // Extra caller-supplied options.
    for &b in extra_options {
        buf[pos] = b;
        pos += 1;
    }
    // Option 55 — Parameter request list (subnet mask, router, DNS).
    buf[pos] = OPT_PARAMETER_REQUEST_LIST;
    pos += 1;
    buf[pos] = 3;
    pos += 1;
    buf[pos] = 1;
    pos += 1; // subnet mask
    buf[pos] = 3;
    pos += 1; // router
    buf[pos] = 6;
    pos += 1; // DNS
    // End.
    buf[pos] = OPT_END;
    pos += 1;
    pos
}

// ── public API ────────────────────────────────────────────────────────────────

/// Initiate DHCP DISCOVER.  Call once after link is up.
/// The MAC address must already be set via `net::set_our_mac()`.
pub fn start() {
    let mac = super::NET_CFG.lock().our_mac;
    let mut client = CLIENT.lock();
    if client.state != DhcpState::Idle {
        return;
    }
    client.our_mac = mac;
    client.state = DhcpState::Discovering;
    let xid = client.xid;
    drop(client);

    let mut pkt = [0u8; 548];
    let len = build_dhcp_packet(&mut pkt, DHCP_DISCOVER, xid, mac, 0, 0, &[]);
    super::udp::send(
        DHCP_CLIENT_PORT,
        BROADCAST_IP,
        DHCP_SERVER_PORT,
        &pkt[..len],
    );
}

/// Feed a received UDP datagram from port 67 into the DHCP state machine.
/// Call this from the UDP receive path when `src_port == 67` and
/// `dst_port == 68` is delivered to us.
pub fn handle_dhcp_packet(packet: &[u8]) {
    if packet.len() < BOOTP_HEADER_LEN + 4 {
        return;
    }
    // Validate magic cookie.
    if packet[BOOTP_HEADER_LEN..BOOTP_HEADER_LEN + 4] != DHCP_MAGIC_COOKIE {
        return;
    }
    let xid = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let yiaddr = u32::from_be_bytes([packet[16], packet[17], packet[18], packet[19]]);
    let siaddr = u32::from_be_bytes([packet[20], packet[21], packet[22], packet[23]]);

    // Parse DHCP message type option.
    let mut msg_type: u8 = 0;
    let mut server_id: u32 = 0;
    let opts = &packet[BOOTP_HEADER_LEN + 4..];
    let mut i = 0;
    while i < opts.len() {
        let tag = opts[i];
        i += 1;
        if tag == OPT_END {
            break;
        }
        if tag == 0 {
            continue;
        } // PAD
        if i >= opts.len() {
            break;
        }
        let len = opts[i] as usize;
        i += 1;
        if i + len > opts.len() {
            break;
        }
        match tag {
            OPT_MSG_TYPE if len >= 1 => {
                msg_type = opts[i];
            }
            OPT_SERVER_ID if len >= 4 => {
                server_id = u32::from_be_bytes([opts[i], opts[i + 1], opts[i + 2], opts[i + 3]]);
            }
            _ => {}
        }
        i += len;
    }

    let mut client = CLIENT.lock();
    if client.xid != xid {
        return; // not our transaction
    }

    match (client.state, msg_type) {
        (DhcpState::Discovering, DHCP_OFFER) => {
            client.offered_ip = yiaddr;
            client.server_ip = if server_id != 0 { server_id } else { siaddr };
            client.state = DhcpState::Requesting;
            let mac = client.our_mac;
            let offered = client.offered_ip;
            let srv = client.server_ip;
            drop(client);

            // Build REQUEST with option 50 (requested IP) and option 54 (server ID).
            let mut extra = [0u8; 12];
            let mut p = 0;
            extra[p] = OPT_REQUESTED_IP;
            p += 1;
            extra[p] = 4;
            p += 1;
            extra[p..p + 4].copy_from_slice(&offered.to_be_bytes());
            p += 4;
            extra[p] = OPT_SERVER_ID;
            p += 1;
            extra[p] = 4;
            p += 1;
            extra[p..p + 4].copy_from_slice(&srv.to_be_bytes());

            let mut pkt = [0u8; 548];
            let xid = CLIENT.lock().xid;
            let len = build_dhcp_packet(&mut pkt, DHCP_REQUEST, xid, mac, 0, srv, &extra);
            super::udp::send(
                DHCP_CLIENT_PORT,
                BROADCAST_IP,
                DHCP_SERVER_PORT,
                &pkt[..len],
            );
        }
        (DhcpState::Requesting, DHCP_ACK) => {
            client.assigned_ip = yiaddr;
            client.state = DhcpState::Bound;
            let ip = yiaddr;
            drop(client);
            // Apply the leased IP to the network stack.
            super::set_our_ipv4(ip);
            // IP applied; serial logging omitted (arch not available in lib crate context).
        }
        _ => {} // ignore other message types / wrong state
    }
}

/// Returns `true` if a lease has been obtained.
pub fn is_bound() -> bool {
    CLIENT.lock().state == DhcpState::Bound
}

/// Returns the assigned IP (0 if not yet bound).
pub fn assigned_ip() -> u32 {
    CLIENT.lock().assigned_ip
}
