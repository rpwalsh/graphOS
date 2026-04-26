// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
pub mod arp;
pub mod dhcp;
pub mod ethernet;
pub mod http;
pub mod icmp;
pub mod icmpv6;
pub mod ipv4;
pub mod ipv6;
pub mod ndp;
pub mod tcp;
pub mod tls;
pub mod udp;

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use spin::Mutex;

use crate::uuid::Uuid128;

const MAX_SOCKETS: usize = 64;
const MAX_SOCKET_PAYLOAD: usize = 1536;
pub const LOOPBACK_IPV4: u32 = 0x7f00_0001;
/// Default guest IP assigned by QEMU SLIRP (10.0.2.15).  Updated on DHCP ACK.
pub const OUR_IPV4_DEFAULT: u32 = 0x0A00_020F;
static OUR_IPV4_ATOM: AtomicU32 = AtomicU32::new(OUR_IPV4_DEFAULT);
pub static TX_HOOK_CALL_COUNT: AtomicU32 = AtomicU32::new(0);
pub static TX_NO_HOOK_COUNT: AtomicU32 = AtomicU32::new(0);
pub static SET_TX_HOOK_COUNT: AtomicU32 = AtomicU32::new(0);
/// Returns the current host IPv4 address (may be updated by DHCP).
#[inline]
pub fn our_ipv4() -> u32 {
    OUR_IPV4_ATOM.load(Ordering::Relaxed)
}
/// Compile-time alias kept for constant contexts — resolves to default.
pub const OUR_IPV4: u32 = OUR_IPV4_DEFAULT;

// ── IPv6 address (link-local, derived from MAC via EUI-64) ───────────────────

/// Our IPv6 link-local address stored as two u64 halves (big-endian).
/// Bytes [0..8] in `OUR_IPV6_HI`, bytes [8..16] in `OUR_IPV6_LO`.
static OUR_IPV6_HI: AtomicU64 = AtomicU64::new(0xfe80_0000_0000_0000);
static OUR_IPV6_LO: AtomicU64 = AtomicU64::new(0);

/// Atomic `[u8; 16]` shim — loads IPv6 address as two u64s.
pub struct AtomicIpv6;
impl AtomicIpv6 {
    pub fn load(&self, _ord: Ordering) -> [u8; 16] {
        let hi = OUR_IPV6_HI.load(Ordering::Relaxed).to_be_bytes();
        let lo = OUR_IPV6_LO.load(Ordering::Relaxed).to_be_bytes();
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&hi);
        out[8..].copy_from_slice(&lo);
        out
    }
}

/// Global accessor for our IPv6 link-local address.
pub static OUR_IPV6: AtomicIpv6 = AtomicIpv6;

/// Update our IPv6 link-local address (called by virtio_net after reading MAC).
pub fn set_our_ipv6(addr: [u8; 16]) {
    let hi = u64::from_be_bytes([
        addr[0], addr[1], addr[2], addr[3], addr[4], addr[5], addr[6], addr[7],
    ]);
    let lo = u64::from_be_bytes([
        addr[8], addr[9], addr[10], addr[11], addr[12], addr[13], addr[14], addr[15],
    ]);
    OUR_IPV6_HI.store(hi, Ordering::Relaxed);
    OUR_IPV6_LO.store(lo, Ordering::Relaxed);
}

const ARP_CACHE_SIZE: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SocketState {
    Open = 0,
    Bound = 1,
    Connected = 2,
    Listening = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetStats {
    pub link_ready: bool,
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub loopback_packets: u64,
    pub dropped_packets: u64,
}

impl NetStats {
    const EMPTY: Self = Self {
        link_ready: false,
        tx_packets: 0,
        rx_packets: 0,
        loopback_packets: 0,
        dropped_packets: 0,
    };
}

#[derive(Clone, Copy)]
struct SocketRecord {
    active: bool,
    handle: Uuid128,
    owner_task_index: usize,
    state: SocketState,
    local_port: u16,
    remote_ipv4: u32,
    remote_port: u16,
    recv_len: u16,
    recv_buf: [u8; MAX_SOCKET_PAYLOAD],
    /// When true, send/recv delegate to the TCP state machine in `tcp.rs`.
    has_tcp_conn: bool,
    /// When true, send/recv delegate to the UDP socket in `udp.rs`.
    has_udp_sock: bool,
    /// Graph arena node ID for this socket (0 = not yet registered).
    #[allow(dead_code)]
    graph_node: u64,
}

impl SocketRecord {
    const EMPTY: Self = Self {
        active: false,
        handle: Uuid128::NIL,
        owner_task_index: usize::MAX,
        state: SocketState::Open,
        local_port: 0,
        remote_ipv4: 0,
        remote_port: 0,
        recv_len: 0,
        recv_buf: [0; MAX_SOCKET_PAYLOAD],
        has_tcp_conn: false,
        has_udp_sock: false,
        graph_node: 0,
    };
}

struct SocketTable {
    sockets: [SocketRecord; MAX_SOCKETS],
}

impl SocketTable {
    const fn new() -> Self {
        Self {
            sockets: [SocketRecord::EMPTY; MAX_SOCKETS],
        }
    }
}

static SOCKETS: Mutex<SocketTable> = Mutex::new(SocketTable::new());
static STATS: Mutex<NetStats> = Mutex::new(NetStats::EMPTY);
static SOCKET_COUNTER: AtomicU64 = AtomicU64::new(1);
static EPHEMERAL_PORT_COUNTER: AtomicU64 = AtomicU64::new(49_152);

pub(super) struct NetConfig {
    our_mac: [u8; 6],
    tx_hook: Option<fn(&[u8]) -> bool>,
}
impl NetConfig {
    const fn new() -> Self {
        Self {
            our_mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            tx_hook: None,
        }
    }
}
pub(super) static NET_CFG: Mutex<NetConfig> = Mutex::new(NetConfig::new());

struct ArpCache {
    ip: [u32; ARP_CACHE_SIZE],
    mac: [[u8; 6]; ARP_CACHE_SIZE],
    count: usize,
}
impl ArpCache {
    const fn new() -> Self {
        Self {
            ip: [0; ARP_CACHE_SIZE],
            mac: [[0; 6]; ARP_CACHE_SIZE],
            count: 0,
        }
    }
    #[allow(dead_code)]
    fn lookup(&self, ip: u32) -> Option<[u8; 6]> {
        for i in 0..self.count {
            if self.ip[i] == ip {
                return Some(self.mac[i]);
            }
        }
        None
    }
    fn insert(&mut self, ip: u32, mac: [u8; 6]) {
        for i in 0..self.count {
            if self.ip[i] == ip {
                self.mac[i] = mac;
                return;
            }
        }
        let slot = self.count % ARP_CACHE_SIZE;
        self.ip[slot] = ip;
        self.mac[slot] = mac;
        if self.count < ARP_CACHE_SIZE {
            self.count += 1;
        }
    }
}
static ARP_CACHE: Mutex<ArpCache> = Mutex::new(ArpCache::new());

fn next_socket_uuid() -> Uuid128 {
    if let Some(v4) = Uuid128::v4_random() {
        return v4;
    }

    let counter = SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
    Uuid128::v5_with_namespace(
        Uuid128::from_u64_pair(0x534f_434b_4554_2d31, 0xa821_57c4_91fb_0010),
        &counter.to_be_bytes(),
    )
}

pub fn socket_open(owner_task_index: usize) -> Option<Uuid128> {
    let mut table = SOCKETS.lock();
    let handle = next_socket_uuid();
    for slot in &mut table.sockets {
        if !slot.active {
            let graph_node: u64 = 0;
            *slot = SocketRecord {
                active: true,
                handle,
                owner_task_index,
                state: SocketState::Open,
                local_port: 0,
                remote_ipv4: 0,
                remote_port: 0,
                recv_len: 0,
                recv_buf: [0; MAX_SOCKET_PAYLOAD],
                has_tcp_conn: false,
                has_udp_sock: false,
                graph_node,
            };
            return Some(handle);
        }
    }
    None
}

pub fn socket_bind(owner_task_index: usize, handle: Uuid128, local_port: u16) -> bool {
    if local_port == 0 {
        return false;
    }
    let mut table = SOCKETS.lock();
    for slot in &table.sockets {
        if slot.active && slot.local_port == local_port && slot.handle != handle {
            return false;
        }
    }
    for slot in &mut table.sockets {
        if slot.active && slot.handle == handle && slot.owner_task_index == owner_task_index {
            slot.local_port = local_port;
            if slot.state == SocketState::Open {
                slot.state = SocketState::Bound;
            }
            // Register a UDP receive binding for unconnected sockets.
            if !slot.has_tcp_conn {
                let key = slot.handle.to_bytes();
                if udp::bind(key, local_port) {
                    slot.has_udp_sock = true;
                }
            }
            return true;
        }
    }
    false
}

pub fn socket_connect(
    owner_task_index: usize,
    handle: Uuid128,
    remote_ipv4: u32,
    remote_port: u16,
) -> bool {
    let mut table = SOCKETS.lock();
    for slot in &mut table.sockets {
        if slot.active && slot.handle == handle && slot.owner_task_index == owner_task_index {
            if slot.local_port == 0 {
                let next = EPHEMERAL_PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
                slot.local_port = 49_152 + (next as u16 % (65_535 - 49_152));
            }
            slot.remote_ipv4 = remote_ipv4;
            slot.remote_port = remote_port;
            slot.state = SocketState::Connected;
            // For non-loopback destinations, initiate a TCP active open.
            if remote_ipv4 != LOOPBACK_IPV4 {
                let key = slot.handle.to_bytes();
                let lp = slot.local_port;
                if !tcp::connect(key, lp, remote_ipv4, remote_port) {
                    slot.state = SocketState::Bound;
                    slot.remote_ipv4 = 0;
                    slot.remote_port = 0;
                    return false;
                }
                slot.has_tcp_conn = true;
            }
            return true;
        }
    }
    false
}

pub fn socket_send(owner_task_index: usize, handle: Uuid128, payload: &[u8]) -> Option<usize> {
    let mut table = SOCKETS.lock();
    let mut sender_idx = None;
    for (idx, slot) in table.sockets.iter().enumerate() {
        if slot.active && slot.handle == handle && slot.owner_task_index == owner_task_index {
            sender_idx = Some(idx);
            break;
        }
    }
    let sender_idx = sender_idx?;
    let sender = table.sockets[sender_idx];
    if sender.state != SocketState::Connected || sender.remote_port == 0 {
        return None;
    }

    if sender.remote_ipv4 == LOOPBACK_IPV4 {
        for recipient in &mut table.sockets {
            if !recipient.active || recipient.local_port != sender.remote_port {
                continue;
            }
            if recipient.recv_len != 0 {
                let mut stats = STATS.lock();
                stats.dropped_packets = stats.dropped_packets.saturating_add(1);
                return None;
            }
            let copy_len = core::cmp::min(payload.len(), MAX_SOCKET_PAYLOAD);
            recipient.recv_buf[..copy_len].copy_from_slice(&payload[..copy_len]);
            recipient.recv_len = copy_len as u16;
            let mut stats = STATS.lock();
            stats.loopback_packets = stats.loopback_packets.saturating_add(1);
            stats.tx_packets = stats.tx_packets.saturating_add(1);
            stats.rx_packets = stats.rx_packets.saturating_add(1);
            return Some(copy_len);
        }
        let mut stats = STATS.lock();
        stats.dropped_packets = stats.dropped_packets.saturating_add(1);
        return None;
    }

    let link_ready = STATS.lock().link_ready;
    if link_ready && sender.has_tcp_conn {
        let key = sender.handle.to_bytes();
        return tcp::send(key, payload);
    }
    if link_ready && sender.has_udp_sock {
        let key = sender.handle.to_bytes();
        let _ = key; // local_port is on the socket record
        let src_port = sender.local_port;
        let dst_ip = sender.remote_ipv4;
        let dst_port = sender.remote_port;
        if udp::send(src_port, dst_ip, dst_port, payload) {
            let mut stats = STATS.lock();
            stats.tx_packets = stats.tx_packets.saturating_add(1);
            return Some(payload.len());
        }
        return None;
    }
    let mut stats = STATS.lock();
    stats.dropped_packets = stats.dropped_packets.saturating_add(1);
    None
}

pub fn socket_recv(owner_task_index: usize, handle: Uuid128, out: &mut [u8]) -> Option<usize> {
    // For TCP sockets, drain the TCP connection's recv buffer.
    let has_tcp = {
        let table = SOCKETS.lock();
        table
            .sockets
            .iter()
            .find(|s| s.active && s.handle == handle && s.owner_task_index == owner_task_index)
            .map(|s| s.has_tcp_conn)
            .unwrap_or(false)
    };
    if has_tcp {
        return tcp::recv(handle.to_bytes(), out);
    }
    // Check for UDP socket — only if no loopback data is pending.
    let (has_udp, loopback_pending) = {
        let table = SOCKETS.lock();
        table
            .sockets
            .iter()
            .find(|s| s.active && s.handle == handle && s.owner_task_index == owner_task_index)
            .map(|s| (s.has_udp_sock, s.recv_len > 0))
            .unwrap_or((false, false))
    };
    if has_udp && !loopback_pending {
        return udp::recv(handle.to_bytes(), out);
    }
    let mut table = SOCKETS.lock();
    for slot in &mut table.sockets {
        if slot.active && slot.handle == handle && slot.owner_task_index == owner_task_index {
            if slot.recv_len != 0 {
                let recv_len = core::cmp::min(out.len(), slot.recv_len as usize);
                out[..recv_len].copy_from_slice(&slot.recv_buf[..recv_len]);
                slot.recv_len = 0;
                return Some(recv_len);
            }
            return Some(0);
        }
    }
    None
}

/// Put a bound socket into passive-listen mode for inbound TCP connections.
/// Returns false if the socket is not bound or has wrong owner.
pub fn socket_listen(owner_task_index: usize, handle: Uuid128) -> bool {
    let mut table = SOCKETS.lock();
    for slot in &mut table.sockets {
        if slot.active && slot.handle == handle && slot.owner_task_index == owner_task_index {
            if slot.local_port == 0 {
                return false;
            }
            slot.state = SocketState::Listening;
            return tcp::listen(slot.local_port);
        }
    }
    false
}

/// Accept one pending inbound TCP connection on a listening socket.
/// On success, creates a new socket record for the accepted connection and
/// writes its UUID handle bytes into `key_out`.  Returns the accepted peer's
/// IPv4+port, or `None` when there is nothing to accept.
pub fn socket_accept(
    owner_task_index: usize,
    handle: Uuid128,
    key_out: &mut [u8; 16],
) -> Option<(u32, u16)> {
    let mut table = SOCKETS.lock();
    let local_port = table
        .sockets
        .iter()
        .find(|s| {
            s.active
                && s.handle == handle
                && s.owner_task_index == owner_task_index
                && s.state == SocketState::Listening
        })
        .map(|s| s.local_port)?;

    let free_slot = table.sockets.iter().position(|s| !s.active)?;

    let mut accepted_key = [0u8; 16];
    let (remote_ip, remote_port) = tcp::accept(local_port, &mut accepted_key)?;

    // Register the accepted connection in the socket table so send/recv work.
    let accepted_handle = crate::uuid::Uuid128::from_bytes(accepted_key);
    table.sockets[free_slot] = SocketRecord {
        active: true,
        handle: accepted_handle,
        owner_task_index,
        state: SocketState::Connected,
        local_port,
        remote_ipv4: remote_ip,
        remote_port,
        recv_len: 0,
        recv_buf: [0; MAX_SOCKET_PAYLOAD],
        has_tcp_conn: true,
        has_udp_sock: false,
        graph_node: 0,
    };
    key_out.copy_from_slice(&accepted_key);
    Some((remote_ip, remote_port))
}

pub fn socket_is_listening(owner_task_index: usize, handle: Uuid128) -> bool {
    let table = SOCKETS.lock();
    table.sockets.iter().any(|s| {
        s.active
            && s.handle == handle
            && s.owner_task_index == owner_task_index
            && s.state == SocketState::Listening
    })
}

pub fn socket_close(owner_task_index: usize, handle: Uuid128) -> bool {
    let has_tcp = {
        let table = SOCKETS.lock();
        table
            .sockets
            .iter()
            .find(|s| s.active && s.handle == handle && s.owner_task_index == owner_task_index)
            .map(|s| s.has_tcp_conn)
            .unwrap_or(false)
    };
    if has_tcp {
        tcp::close(handle.to_bytes());
    }
    // UDP unbind.
    {
        let table = SOCKETS.lock();
        if table
            .sockets
            .iter()
            .any(|s| s.active && s.handle == handle && s.has_udp_sock)
        {
            udp::unbind(handle.to_bytes());
        }
    }
    let mut table = SOCKETS.lock();
    for slot in &mut table.sockets {
        if slot.active && slot.handle == handle && slot.owner_task_index == owner_task_index {
            *slot = SocketRecord::EMPTY;
            return true;
        }
    }
    false
}

pub fn set_link_ready(ready: bool) {
    STATS.lock().link_ready = ready;
    if ready {
        dhcp::start();
    }
}

pub fn set_our_mac(mac: [u8; 6]) {
    NET_CFG.lock().our_mac = mac;
}

/// Update the kernel's IPv4 address (called by the DHCP client on lease).
pub fn set_our_ipv4(ip: u32) {
    OUR_IPV4_ATOM.store(ip, Ordering::Relaxed);
}

pub fn set_tx_hook(f: fn(&[u8]) -> bool) {
    NET_CFG.lock().tx_hook = Some(f);
    SET_TX_HOOK_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Called by the virtio-net driver for each received Ethernet frame.
pub fn receive_raw_frame(frame: &[u8]) {
    use ethernet::{ETHERNET_HEADER_SIZE, ETHERTYPE_ARP, ETHERTYPE_IPV4};

    let Some(eth) = ethernet::parse(frame) else {
        return;
    };

    match eth.ethertype {
        ETHERTYPE_ARP => handle_arp(eth.src_mac, eth.payload),
        ETHERTYPE_IPV4 => handle_ipv4(eth.src_mac, eth.payload),
        _ => {}
    }

    let mut stats = STATS.lock();
    stats.rx_packets = stats.rx_packets.saturating_add(1);
    let _ = ETHERNET_HEADER_SIZE; // suppress unused import warning
}

fn handle_arp(_sender_mac: [u8; 6], payload: &[u8]) {
    // ARP packet: htype(2) ptype(2) hlen(1) plen(1) oper(2) sha(6) spa(4) tha(6) tpa(4) = 28 bytes
    if payload.len() < 28 {
        return;
    }
    let oper = u16::from_be_bytes([payload[6], payload[7]]);
    let mut sha = [0u8; 6];
    sha.copy_from_slice(&payload[8..14]);
    let spa = u32::from_be_bytes([payload[14], payload[15], payload[16], payload[17]]);
    let tpa = u32::from_be_bytes([payload[24], payload[25], payload[26], payload[27]]);

    // Cache the sender's MAC.
    ARP_CACHE.lock().insert(spa, sha);

    if oper != 1 {
        return;
    } // Only handle ARP requests
    let our_ip = our_ipv4();
    if tpa != our_ip {
        return;
    } // Not for us

    let cfg = NET_CFG.lock();
    let our_mac = cfg.our_mac;
    let tx_hook = cfg.tx_hook;
    drop(cfg);

    // Build ARP reply.
    let mut arp_reply = [0u8; 28];
    arp_reply[0] = 0x00;
    arp_reply[1] = 0x01; // htype = Ethernet
    arp_reply[2] = 0x08;
    arp_reply[3] = 0x00; // ptype = IPv4
    arp_reply[4] = 6; // hlen
    arp_reply[5] = 4; // plen
    arp_reply[6] = 0x00;
    arp_reply[7] = 0x02; // oper = reply
    arp_reply[8..14].copy_from_slice(&our_mac); // sha = our MAC
    arp_reply[14..18].copy_from_slice(&our_ip.to_be_bytes()); // spa = our IP
    arp_reply[18..24].copy_from_slice(&sha); // tha = requester MAC
    arp_reply[24..28].copy_from_slice(&spa.to_be_bytes()); // tpa = requester IP

    let mut frame_buf = [0u8; 14 + 28];
    ethernet::encode(
        sha,
        our_mac,
        ethernet::ETHERTYPE_ARP,
        &arp_reply,
        &mut frame_buf,
    );
    if let Some(tx) = tx_hook {
        tx(&frame_buf);
    }
}

fn handle_ipv4(src_mac: [u8; 6], payload: &[u8]) {
    let Some(hdr) = ipv4::parse_header(payload) else {
        return;
    };
    if hdr.dst != our_ipv4() && hdr.dst != 0xFFFF_FFFFu32 {
        return;
    }
    if payload.len() < ipv4::IPV4_HEADER_BYTES {
        return;
    }
    let ip_body = &payload[ipv4::IPV4_HEADER_BYTES..];
    match hdr.protocol {
        ipv4::IP_PROTOCOL_ICMP => handle_icmp(src_mac, hdr.src, ip_body),
        0x06 /* TCP */ => tcp::handle_segment(hdr.src, hdr.dst, ip_body),
        0x11 /* UDP */ => {
            udp::handle_datagram(hdr.src, ip_body);
            // Route DHCP replies (src port 67, dst port 68) to DHCP client.
            if ip_body.len() >= 8 {
                let src_port = u16::from_be_bytes([ip_body[0], ip_body[1]]);
                let dst_port = u16::from_be_bytes([ip_body[2], ip_body[3]]);
                if src_port == 67 && dst_port == 68 && ip_body.len() > 8 {
                    dhcp::handle_dhcp_packet(&ip_body[8..]);
                }
            }
        }
        _ => {}
    }
}

fn handle_icmp(src_mac: [u8; 6], src_ip: u32, payload: &[u8]) {
    use icmp::{ICMP_ECHO_REQUEST, ICMP_HEADER_BYTES};
    if payload.len() < ICMP_HEADER_BYTES {
        return;
    }
    if payload[0] != ICMP_ECHO_REQUEST {
        return;
    }

    let cfg = NET_CFG.lock();
    let our_mac = cfg.our_mac;
    let tx_hook = cfg.tx_hook;
    drop(cfg);
    let Some(tx) = tx_hook else {
        return;
    };

    // Build ICMP echo reply.
    let mut icmp_buf = [0u8; 1480];
    let icmp_len = match icmp::reply_from_request(payload, &mut icmp_buf) {
        Some(n) => n,
        None => return,
    };

    // Wrap in IPv4.
    let mut ip_buf = [0u8; ipv4::IPV4_HEADER_BYTES + 1480];
    let ip_len = ipv4::IPV4_HEADER_BYTES + icmp_len;
    ipv4::encode_header(
        &mut ip_buf[..ipv4::IPV4_HEADER_BYTES],
        ip_len as u16,
        64,
        ipv4::IP_PROTOCOL_ICMP,
        our_ipv4(),
        src_ip,
        0,
    );
    ip_buf[ipv4::IPV4_HEADER_BYTES..ip_len].copy_from_slice(&icmp_buf[..icmp_len]);

    // Wrap in Ethernet.
    let frame_len = ethernet::ETHERNET_HEADER_SIZE + ip_len;
    let mut frame_buf = [0u8; ethernet::ETHERNET_HEADER_SIZE + ipv4::IPV4_HEADER_BYTES + 1480];
    ethernet::encode(
        src_mac,
        our_mac,
        ethernet::ETHERTYPE_IPV4,
        &ip_buf[..ip_len],
        &mut frame_buf,
    );

    let _ = frame_len;
    tx(&frame_buf[..ethernet::ETHERNET_HEADER_SIZE + ip_len]);
}

/// Build an IPv4 packet and transmit it via the registered TX hook.
/// Resolves the destination MAC via the ARP cache; falls back to broadcast.
/// Called by `tcp` and other submodules via `super::transmit_ipv4_packet`.
pub(super) fn transmit_ipv4_packet(dst_ip: u32, protocol: u8, transport_payload: &[u8]) {
    let ip_len = ipv4::IPV4_HEADER_BYTES + transport_payload.len();
    if ip_len > 1500 {
        return; // too large for one Ethernet frame
    }

    let cfg = NET_CFG.lock();
    let our_mac = cfg.our_mac;
    let tx_hook = cfg.tx_hook;
    let our_ip = our_ipv4();
    drop(cfg);

    let Some(tx) = tx_hook else {
        TX_NO_HOOK_COUNT.fetch_add(1, Ordering::Relaxed);
        return;
    };
    TX_HOOK_CALL_COUNT.fetch_add(1, Ordering::Relaxed);

    // ARP lookup; default to broadcast MAC.
    let dst_mac = ARP_CACHE.lock().lookup(dst_ip).unwrap_or([0xFF; 6]);

    let mut ip_buf = [0u8; 20 + 1480];
    ipv4::encode_header(
        &mut ip_buf[..ipv4::IPV4_HEADER_BYTES],
        ip_len as u16,
        64,
        protocol,
        our_ip,
        dst_ip,
        0,
    );
    ip_buf[ipv4::IPV4_HEADER_BYTES..ip_len].copy_from_slice(transport_payload);

    let frame_len = ethernet::ETHERNET_HEADER_SIZE + ip_len;
    let mut frame_buf = [0u8; ethernet::ETHERNET_HEADER_SIZE + 20 + 1480];
    ethernet::encode(
        dst_mac,
        our_mac,
        ethernet::ETHERTYPE_IPV4,
        &ip_buf[..ip_len],
        &mut frame_buf,
    );
    tx(&frame_buf[..frame_len]);
}

pub fn stats() -> NetStats {
    *STATS.lock()
}

pub fn loopback_ping_reply(request: &[u8], out: &mut [u8]) -> Option<usize> {
    icmp::reply_from_request(request, out)
}

#[cfg(test)]
pub fn reset_for_tests() {
    let mut table = SOCKETS.lock();
    for slot in &mut table.sockets {
        *slot = SocketRecord::EMPTY;
    }
    *STATS.lock() = NetStats::EMPTY;
}
