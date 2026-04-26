// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! In-kernel TCP state machine.
//!
//! Features:
//!  - 16 simultaneous connections (static table, no heap)
//!  - Active open (SYN → SYN-ACK → ACK)
//!  - PSH+ACK data send
//!  - FIN/ACK teardown
//!  - 4 KiB per-connection receive ring buffer
//!  - IRQ-safe via `without_interrupts` on all lock sites

use spin::Mutex;

/// Run `f` with interrupts disabled (kernel context only).
/// In test builds, just call `f` directly — no privileged instructions allowed.
#[inline(always)]
fn without_irq<F: FnOnce() -> R, R>(f: F) -> R {
    #[cfg(all(not(test), target_arch = "x86_64"))]
    return x86_64::instructions::interrupts::without_interrupts(f);
    #[cfg(any(test, not(target_arch = "x86_64")))]
    f()
}

// ── constants ────────────────────────────────────────────────────────────────

const TCP_MAX_CONNS: usize = 16;
/// Per-connection linear receive buffer (bytes).
const TCP_RECV_BUF: usize = 4096;
/// Window advertised to the peer.
const WINDOW_SIZE: u16 = 4096;
const TCP_HEADER_BYTES: usize = 20;
const IP_PROTO_TCP: u8 = 0x06;

// ── TCP flags ────────────────────────────────────────────────────────────────

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
#[allow(dead_code)]
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

// ── State ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum TcpState {
    Closed = 0,
    SynSent = 1,
    Established = 2,
    FinWait1 = 3,
    FinWait2 = 4,
    CloseWait = 5,
    LastAck = 6,
    TimeWait = 7,
    /// Passive open: SYN received, SYN-ACK sent, waiting for ACK.
    SynReceived = 8,
    /// Passive open: three-way handshake complete, waiting to be accepted.
    AcceptPending = 9,
}

// ── Connection record ────────────────────────────────────────────────────────

struct TcpConn {
    active: bool,
    state: TcpState,
    local_port: u16,
    remote_ip: u32,
    remote_port: u16,
    /// Next sequence number to send.
    snd_nxt: u32,
    /// Oldest unacknowledged sequence number.
    snd_una: u32,
    /// Next expected receive sequence number.
    rcv_nxt: u32,
    /// Socket handle UUID bytes (used as lookup key from socket layer).
    socket_key: [u8; 16],
    /// Linear receive buffer.
    recv_buf: [u8; TCP_RECV_BUF],
    /// Bytes valid from the start of recv_buf.
    recv_len: usize,
    // ── Retransmit timer ─────────────────────────────────────────────
    /// Retransmit timeout in milliseconds (doubles on each miss, capped at 60 s).
    rto_ms: u32,
    /// Tick-ms timestamp when the oldest unacknowledged segment was sent.
    rto_deadline_ms: u64,
    /// Number of consecutive retransmissions (RST + close after 5).
    retransmit_count: u8,
    /// Snapshot of last unacked data (up to 512 bytes) for retransmission.
    retransmit_buf: [u8; 512],
    retransmit_len: usize,
    // ── Keepalive ──────────────────────────────────────────────────────
    /// Milliseconds since last data/ack received on this connection.
    keepalive_idle_ms: u64,
    /// Number of keepalive probes sent without a response.
    keepalive_probes: u8,
    // ── TIME_WAIT ──────────────────────────────────────────────────────
    /// Tick-ms deadline for TIME_WAIT expiry (2×MSL = 120 s).
    /// Zero means not in TIME_WAIT.
    time_wait_deadline_ms: u64,
}

impl TcpConn {
    const fn empty() -> Self {
        Self {
            active: false,
            state: TcpState::Closed,
            local_port: 0,
            remote_ip: 0,
            remote_port: 0,
            snd_nxt: 0,
            snd_una: 0,
            rcv_nxt: 0,
            socket_key: [0; 16],
            recv_buf: [0; TCP_RECV_BUF],
            recv_len: 0,
            rto_ms: 1000,
            rto_deadline_ms: 0,
            retransmit_count: 0,
            retransmit_buf: [0; 512],
            retransmit_len: 0,
            keepalive_idle_ms: 0,
            keepalive_probes: 0,
            time_wait_deadline_ms: 0,
        }
    }
}

// ── Listen table ─────────────────────────────────────────────────────────────

/// Maximum number of simultaneously listening ports.
const MAX_LISTENERS: usize = 8;
/// Maximum pending (AcceptPending) connections per listener.
const ACCEPT_BACKLOG: usize = 4;

#[derive(Clone, Copy)]
struct Listener {
    active: bool,
    local_port: u16,
    /// Indices into TcpTable::conns of connections ready to accept.
    pending: [u8; ACCEPT_BACKLOG],
    pending_len: usize,
}

impl Listener {
    const fn empty() -> Self {
        Self {
            active: false,
            local_port: 0,
            pending: [0; ACCEPT_BACKLOG],
            pending_len: 0,
        }
    }
}

struct TcpTable {
    conns: [TcpConn; TCP_MAX_CONNS],
    isn_counter: u32,
    listeners: [Listener; MAX_LISTENERS],
    /// 16-byte secret mixed into SYN cookie MACs.  Initialised with RDRAND
    /// on first use; falls back to a compile-time constant if RDRAND is
    /// unavailable (acceptable for VMs without hardware RNG).
    syn_secret: [u8; 16],
    syn_secret_ready: bool,
}

impl TcpTable {
    const fn new() -> Self {
        // SAFETY: const-initialising array of TcpConn::empty()
        #[allow(clippy::declare_interior_mutable_const)]
        const E: TcpConn = TcpConn::empty();
        #[allow(clippy::declare_interior_mutable_const)]
        const L: Listener = Listener::empty();
        Self {
            conns: [E; TCP_MAX_CONNS],
            isn_counter: 0x1234_5678,
            listeners: [L; MAX_LISTENERS],
            syn_secret: [0u8; 16],
            syn_secret_ready: false,
        }
    }
}

static TCP: Mutex<TcpTable> = Mutex::new(TcpTable::new());

// ── helpers ──────────────────────────────────────────────────────────────────

/// Compute TCP checksum using IPv4 pseudo-header.
fn tcp_checksum(src_ip: u32, dst_ip: u32, tcp_seg: &[u8]) -> u16 {
    let seg_len = tcp_seg.len() as u16;
    // 12-byte pseudo-header: src(4) dst(4) 0x00 proto(0x06) tcp_len(2)
    let mut pseudo = [0u8; 12];
    pseudo[0..4].copy_from_slice(&src_ip.to_be_bytes());
    pseudo[4..8].copy_from_slice(&dst_ip.to_be_bytes());
    pseudo[8] = 0;
    pseudo[9] = IP_PROTO_TCP;
    pseudo[10..12].copy_from_slice(&seg_len.to_be_bytes());

    // Sum pseudo-header and segment separately to avoid copying into a temp buffer.
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < pseudo.len() {
        let word = ((pseudo[i] as u32) << 8) | pseudo[i + 1] as u32;
        sum = sum.wrapping_add(word);
        i += 2;
    }
    let mut j = 0;
    while j + 1 < tcp_seg.len() {
        let word = ((tcp_seg[j] as u32) << 8) | tcp_seg[j + 1] as u32;
        sum = sum.wrapping_add(word);
        j += 2;
    }
    if j < tcp_seg.len() {
        sum = sum.wrapping_add((tcp_seg[j] as u32) << 8);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build and transmit a TCP segment.  `data` may be empty.
#[allow(clippy::too_many_arguments)]
fn send_segment_inner(
    src_ip: u32,
    src_port: u16,
    dst_ip: u32,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    data: &[u8],
) {
    // 1514 bytes max Ethernet payload; TCP header 20 bytes.
    const MAX_SEG: usize = 1494;
    let data_len = core::cmp::min(data.len(), MAX_SEG);
    let seg_len = TCP_HEADER_BYTES + data_len;
    let mut seg = [0u8; TCP_HEADER_BYTES + MAX_SEG];

    // Build TCP header.
    seg[0..2].copy_from_slice(&src_port.to_be_bytes());
    seg[2..4].copy_from_slice(&dst_port.to_be_bytes());
    seg[4..8].copy_from_slice(&seq.to_be_bytes());
    seg[8..12].copy_from_slice(&ack.to_be_bytes());
    seg[12] = 5 << 4; // data offset = 5 (20 bytes), reserved = 0
    seg[13] = flags;
    seg[14..16].copy_from_slice(&WINDOW_SIZE.to_be_bytes());
    // seg[16..18] = checksum (filled below)
    // seg[18..20] = urgent pointer = 0
    if data_len > 0 {
        seg[TCP_HEADER_BYTES..TCP_HEADER_BYTES + data_len].copy_from_slice(&data[..data_len]);
    }

    // Compute checksum over pseudo-header + segment.
    let csum = tcp_checksum(src_ip, dst_ip, &seg[..seg_len]);
    seg[16..18].copy_from_slice(&csum.to_be_bytes());

    // Transmit through the IPv4 stack.
    super::transmit_ipv4_packet(dst_ip, IP_PROTO_TCP, &seg[..seg_len]);
}

/// Advance the ISN counter (cheap deterministic ISN — fallback only).
fn alloc_isn(table: &mut TcpTable) -> u32 {
    // Mix the seed with the counter for uniqueness.
    let seed = TCP_SEED.load(core::sync::atomic::Ordering::Relaxed);
    let val = seed
        .wrapping_add(table.isn_counter as u64)
        .wrapping_mul(0x9e3779b97f4a7c15);
    table.isn_counter = table.isn_counter.wrapping_add(0x0001_0000);
    (val ^ (val >> 32)) as u32
}

/// Ensure `table.syn_secret` is populated with random bytes.
fn ensure_syn_secret(table: &mut TcpTable) {
    if table.syn_secret_ready {
        return;
    }
    let seed = TCP_SEED.load(core::sync::atomic::Ordering::Relaxed);
    // Derive two independent 64-bit values from the seed.
    let a = seed
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(0x6c62272e07bb0142);
    let b = a
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(0xbf58476d1ce4e5b9);
    table.syn_secret[..8].copy_from_slice(&a.to_ne_bytes());
    table.syn_secret[8..].copy_from_slice(&b.to_ne_bytes());
    table.syn_secret_ready = true;
}

/// Compute a SYN cookie ISN using a self-contained mixing function.
/// `src_ip`, `src_port`, `dst_port` uniquely identify the half-open flow.
fn syn_cookie_isn(secret: &[u8; 16], src_ip: u32, src_port: u16, dst_port: u16) -> u32 {
    // FNV-1a-inspired mixing — fast, no external dependencies.
    // Safety: secret is exactly 16 bytes; [..8] is always a valid 8-byte slice.
    let lo: [u8; 8] = secret[..8].try_into().unwrap_or([0u8; 8]);
    let mut h: u64 = u64::from_le_bytes(lo);
    h ^= src_ip as u64;
    h = h.wrapping_mul(0x9e3779b97f4a7c15);
    h ^= (src_port as u64) | ((dst_port as u64) << 16);
    h = h.wrapping_mul(0x6c62272e07bb0142);
    h ^= h >> 33;
    h as u32
}

// ── TCP seed ──────────────────────────────────────────────────────────────────

use core::sync::atomic::{AtomicU64, Ordering};

/// Random seed for ISN generation and SYN-cookie MAC.
/// Initialised to a compile-time non-zero constant; replaced with hardware
/// entropy by `tcp::seed_entropy()` during boot.
static TCP_SEED: AtomicU64 = AtomicU64::new(0xd69c_6b3a_f1e0_2547);

/// Seed the TCP ISN generator and SYN-cookie MAC key with hardware entropy.
/// Should be called once during early boot, after RDRAND is available.
/// Calling this more than once is safe (each call re-seeds with fresh entropy).
pub fn seed_entropy(entropy: u64) {
    TCP_SEED.store(entropy, Ordering::Relaxed);
    // Invalidate any previously derived secret so it is re-derived from the
    // new seed on the next SYN.
    without_irq(|| {
        let mut tbl = TCP.lock();
        tbl.syn_secret_ready = false;
    });
}

// ── public API ───────────────────────────────────────────────────────────────

/// Initiate an active TCP open.  Returns `true` when a connection slot was
/// allocated and a SYN was transmitted.  The socket layer should poll
/// `is_established` before attempting data transfer.
pub fn connect(socket_key: [u8; 16], local_port: u16, remote_ip: u32, remote_port: u16) -> bool {
    let our_ip = super::our_ipv4();
    without_irq(|| {
        let mut tbl = TCP.lock();
        // Check for free slot.
        let slot = tbl.conns.iter().position(|c| !c.active);
        let Some(idx) = slot else {
            return false;
        };
        let isn = alloc_isn(&mut tbl);
        tbl.conns[idx] = TcpConn {
            active: true,
            state: TcpState::SynSent,
            local_port,
            remote_ip,
            remote_port,
            snd_nxt: isn.wrapping_add(1), // SYN consumes one seq
            snd_una: isn,
            rcv_nxt: 0,
            socket_key,
            recv_buf: [0; TCP_RECV_BUF],
            recv_len: 0,
            rto_ms: 1000,
            rto_deadline_ms: 0,
            retransmit_count: 0,
            retransmit_buf: [0; 512],
            retransmit_len: 0,
            keepalive_idle_ms: 0,
            keepalive_probes: 0,
            time_wait_deadline_ms: 0,
        };
        send_segment_inner(our_ip, local_port, remote_ip, remote_port, isn, 0, SYN, &[]);
        true
    })
}

/// Send data on an established TCP connection.
/// Returns number of bytes accepted, or `None` on error.
pub fn send(socket_key: [u8; 16], data: &[u8]) -> Option<usize> {
    let our_ip = super::our_ipv4();
    without_irq(|| {
        let mut tbl = TCP.lock();
        let conn = tbl
            .conns
            .iter_mut()
            .find(|c| c.active && c.socket_key == socket_key)?;
        if conn.state != TcpState::Established && conn.state != TcpState::CloseWait {
            return None;
        }
        const MAX_CHUNK: usize = 1460; // MSS for no-option TCP over Ethernet
        let chunk = core::cmp::min(data.len(), MAX_CHUNK);
        let seq = conn.snd_nxt;
        send_segment_inner(
            our_ip,
            conn.local_port,
            conn.remote_ip,
            conn.remote_port,
            seq,
            conn.rcv_nxt,
            PSH | ACK,
            &data[..chunk],
        );
        conn.snd_nxt = conn.snd_nxt.wrapping_add(chunk as u32);
        Some(chunk)
    })
}

/// Copy buffered received data into `out`.  Returns bytes copied.
/// Returns `Some(0)` if connected but no data yet; `None` if no matching conn.
pub fn recv(socket_key: [u8; 16], out: &mut [u8]) -> Option<usize> {
    without_irq(|| {
        let mut tbl = TCP.lock();
        let conn = tbl
            .conns
            .iter_mut()
            .find(|c| c.active && c.socket_key == socket_key)?;
        if conn.recv_len == 0 {
            // If the peer has closed (CloseWait or later), signal EOF so callers
            // don't spin waiting for data that will never arrive.
            match conn.state {
                TcpState::CloseWait | TcpState::LastAck | TcpState::Closed | TcpState::TimeWait => {
                    return None;
                }
                _ => {}
            }
            return Some(0);
        }
        let copy = core::cmp::min(out.len(), conn.recv_len);
        out[..copy].copy_from_slice(&conn.recv_buf[..copy]);
        // Shift remaining data down.
        conn.recv_buf.copy_within(copy..conn.recv_len, 0);
        conn.recv_len -= copy;
        Some(copy)
    })
}

/// Initiate an active TCP close (FIN).
pub fn close(socket_key: [u8; 16]) {
    let our_ip = super::our_ipv4();
    without_irq(|| {
        let mut tbl = TCP.lock();
        if let Some(conn) = tbl
            .conns
            .iter_mut()
            .find(|c| c.active && c.socket_key == socket_key)
        {
            match conn.state {
                TcpState::Established => {
                    let seq = conn.snd_nxt;
                    let ack = conn.rcv_nxt;
                    send_segment_inner(
                        our_ip,
                        conn.local_port,
                        conn.remote_ip,
                        conn.remote_port,
                        seq,
                        ack,
                        FIN | ACK,
                        &[],
                    );
                    conn.snd_nxt = conn.snd_nxt.wrapping_add(1);
                    conn.state = TcpState::FinWait1;
                }
                TcpState::CloseWait => {
                    let seq = conn.snd_nxt;
                    let ack = conn.rcv_nxt;
                    send_segment_inner(
                        our_ip,
                        conn.local_port,
                        conn.remote_ip,
                        conn.remote_port,
                        seq,
                        ack,
                        FIN | ACK,
                        &[],
                    );
                    conn.snd_nxt = conn.snd_nxt.wrapping_add(1);
                    conn.state = TcpState::LastAck;
                }
                _ => {
                    conn.active = false;
                    conn.state = TcpState::Closed;
                }
            }
        }
    });
}

/// Returns `true` if the connection associated with `socket_key` is in
/// ESTABLISHED state.
pub fn is_established(socket_key: [u8; 16]) -> bool {
    without_irq(|| {
        let tbl = TCP.lock();
        tbl.conns
            .iter()
            .any(|c| c.active && c.socket_key == socket_key && c.state == TcpState::Established)
    })
}

/// Register `local_port` as a passive listener.  Returns false if already
/// listening or no listener slot is free.
pub fn listen(local_port: u16) -> bool {
    without_irq(|| {
        let mut tbl = TCP.lock();
        // Already listening?
        if tbl
            .listeners
            .iter()
            .any(|l| l.active && l.local_port == local_port)
        {
            return true; // idempotent
        }
        if let Some(slot) = tbl.listeners.iter_mut().find(|l| !l.active) {
            *slot = Listener {
                active: true,
                local_port,
                pending: [0; ACCEPT_BACKLOG],
                pending_len: 0,
            };
            return true;
        }
        false
    })
}

/// Accept one pending connection on `local_port`.  On success writes the
/// socket key for the new connection into `key_out` (16 bytes) and returns
/// `Some((remote_ip, remote_port))`.  Returns `None` when no connection is
/// waiting.
pub fn accept(local_port: u16, key_out: &mut [u8; 16]) -> Option<(u32, u16)> {
    without_irq(|| {
        let mut tbl = TCP.lock();
        // Find listener.
        let li = tbl
            .listeners
            .iter_mut()
            .find(|l| l.active && l.local_port == local_port)?;
        if li.pending_len == 0 {
            return None;
        }
        let conn_idx = li.pending[0] as usize;
        li.pending_len -= 1;
        li.pending.copy_within(1..li.pending_len + 1, 0);

        let conn = &mut tbl.conns[conn_idx];
        // Passive open is fully handshake-complete before entering the listener
        // backlog. Promote it now so the socket layer can send immediately
        // after accept() returns (for example, the SSH server banner).
        if conn.state == TcpState::AcceptPending {
            conn.state = TcpState::Established;
        }
        *key_out = conn.socket_key;
        Some((conn.remote_ip, conn.remote_port))
    })
}

/// Process an inbound TCP segment received from `src_ip` destined for `dst_ip`.
/// Called from the IPv4 receive path (IRQ context).
pub fn handle_segment(src_ip: u32, dst_ip: u32, segment: &[u8]) {
    if segment.len() < TCP_HEADER_BYTES {
        return;
    }

    let src_port = u16::from_be_bytes([segment[0], segment[1]]);
    let dst_port = u16::from_be_bytes([segment[2], segment[3]]);
    let seq_num = u32::from_be_bytes([segment[4], segment[5], segment[6], segment[7]]);
    let ack_num = u32::from_be_bytes([segment[8], segment[9], segment[10], segment[11]]);
    let data_offset = (segment[12] >> 4) as usize * 4;
    let flags = segment[13];
    // Verify data_offset is valid.
    if data_offset < TCP_HEADER_BYTES || data_offset > segment.len() {
        return;
    }
    let payload = &segment[data_offset..];

    let our_ip = super::our_ipv4();

    let mut tbl = TCP.lock();

    // Find matching connection: (local_port=dst_port, remote_ip=src_ip, remote_port=src_port)
    let maybe_idx = tbl.conns.iter().position(|c| {
        c.active && c.local_port == dst_port && c.remote_ip == src_ip && c.remote_port == src_port
    });

    let maybe_idx = maybe_idx.or_else(|| {
        // Check if dst_port is being listened on and this is a SYN.
        if flags & SYN == 0 || flags & ACK != 0 {
            return None;
        }
        let has_listener = tbl
            .listeners
            .iter()
            .any(|l| l.active && l.local_port == dst_port);
        if !has_listener {
            return None;
        }

        // Check whether the listener's backlog is already full.
        let backlog_full = tbl
            .listeners
            .iter()
            .any(|l| l.active && l.local_port == dst_port && l.pending_len >= ACCEPT_BACKLOG);
        let all_slots_used = tbl.conns.iter().all(|c| c.active);

        if backlog_full || all_slots_used {
            // SYN cookie path: send a SYN-ACK whose ISN encodes a MAC of the
            // flow 4-tuple.  We allocate no state — the connection will be
            // materialised when (if) the ACK arrives and the cookie validates.
            ensure_syn_secret(&mut tbl);
            let secret = tbl.syn_secret;
            let cookie = syn_cookie_isn(&secret, src_ip, src_port, dst_port);
            send_segment_inner(
                our_ip,
                dst_port,
                src_ip,
                src_port,
                cookie,
                seq_num.wrapping_add(1),
                SYN | ACK,
                &[],
            );
            return None; // no slot allocated yet
        }

        // Allocate a new connection slot for the passive open.
        let slot = tbl.conns.iter().position(|c| !c.active)?;
        ensure_syn_secret(&mut tbl);
        let isn = alloc_isn(&mut tbl);
        tbl.conns[slot] = TcpConn {
            active: true,
            state: TcpState::SynReceived,
            local_port: dst_port,
            remote_ip: src_ip,
            remote_port: src_port,
            snd_nxt: isn.wrapping_add(1),
            snd_una: isn,
            rcv_nxt: seq_num.wrapping_add(1),
            socket_key: {
                // Generate a unique key from src/dst IP/port combination.
                let mut k = [0u8; 16];
                k[0..4].copy_from_slice(&src_ip.to_le_bytes());
                k[4..6].copy_from_slice(&src_port.to_le_bytes());
                k[6..8].copy_from_slice(&dst_port.to_le_bytes());
                k[8..12].copy_from_slice(&isn.to_le_bytes());
                k[12..16].copy_from_slice(&seq_num.to_le_bytes());
                k
            },
            recv_buf: [0; TCP_RECV_BUF],
            recv_len: 0,
            rto_ms: 1000,
            rto_deadline_ms: 0,
            retransmit_count: 0,
            retransmit_buf: [0; 512],
            retransmit_len: 0,
            keepalive_idle_ms: 0,
            keepalive_probes: 0,
            time_wait_deadline_ms: 0,
        };
        // Send SYN-ACK.
        send_segment_inner(
            our_ip,
            dst_port,
            src_ip,
            src_port,
            isn,
            tbl.conns[slot].rcv_nxt,
            SYN | ACK,
            &[],
        );
        Some(slot)
    });

    // SYN-cookie ACK validation path:
    // If no slot matched and this is a pure ACK (no SYN), check whether ack_num
    // matches the cookie we would have sent for this flow.
    let maybe_idx = if maybe_idx.is_none() && flags & ACK != 0 && flags & SYN == 0 {
        let has_listener = tbl
            .listeners
            .iter()
            .any(|l| l.active && l.local_port == dst_port);
        if has_listener && tbl.syn_secret_ready {
            let secret = tbl.syn_secret;
            let expected_cookie = syn_cookie_isn(&secret, src_ip, src_port, dst_port);
            // ack_num should be cookie + 1 (client ACKing our SYN-ACK).
            if ack_num == expected_cookie.wrapping_add(1) {
                // Cookie valid — materialise connection now.
                if let Some(slot) = tbl.conns.iter().position(|c| !c.active) {
                    let isn = expected_cookie;
                    tbl.conns[slot] = TcpConn {
                        active: true,
                        state: TcpState::AcceptPending,
                        local_port: dst_port,
                        remote_ip: src_ip,
                        remote_port: src_port,
                        snd_nxt: isn.wrapping_add(1),
                        snd_una: ack_num,
                        rcv_nxt: seq_num, // no SYN here, seq_num is data start
                        socket_key: {
                            let mut k = [0u8; 16];
                            k[0..4].copy_from_slice(&src_ip.to_le_bytes());
                            k[4..6].copy_from_slice(&src_port.to_le_bytes());
                            k[6..8].copy_from_slice(&dst_port.to_le_bytes());
                            k[8..12].copy_from_slice(&isn.to_le_bytes());
                            k[12..16].copy_from_slice(&seq_num.to_le_bytes());
                            k
                        },
                        recv_buf: [0; TCP_RECV_BUF],
                        recv_len: 0,
                        rto_ms: 1000,
                        rto_deadline_ms: 0,
                        retransmit_count: 0,
                        retransmit_buf: [0; 512],
                        retransmit_len: 0,
                        keepalive_idle_ms: 0,
                        keepalive_probes: 0,
                        time_wait_deadline_ms: 0,
                    };
                    // Enqueue into listener's pending backlog.
                    if let Some(li) = tbl
                        .listeners
                        .iter_mut()
                        .find(|l| l.active && l.local_port == dst_port)
                        && li.pending_len < ACCEPT_BACKLOG
                    {
                        li.pending[li.pending_len] = slot as u8;
                        li.pending_len += 1;
                    }
                    Some(slot)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        maybe_idx
    };

    let Some(idx) = maybe_idx else {
        // No matching connection — send RST if not already RST.
        if flags & RST == 0 {
            send_rst(our_ip, dst_ip, dst_port, src_ip, src_port, ack_num, seq_num);
        }
        return;
    };

    let state = tbl.conns[idx].state;

    match state {
        TcpState::SynSent => {
            // Expect SYN+ACK.
            if flags & (SYN | ACK) == (SYN | ACK) {
                let snd_una = tbl.conns[idx].snd_una;
                if ack_num != snd_una.wrapping_add(1) {
                    return; // unexpected ack
                }
                tbl.conns[idx].rcv_nxt = seq_num.wrapping_add(1);
                tbl.conns[idx].snd_una = ack_num;
                tbl.conns[idx].state = TcpState::Established;
                let lp = tbl.conns[idx].local_port;
                let rp = tbl.conns[idx].remote_port;
                let snd_nxt = tbl.conns[idx].snd_nxt;
                let rcv_nxt = tbl.conns[idx].rcv_nxt;
                // Send ACK (3rd leg of handshake).
                send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
            }
        }

        TcpState::Established | TcpState::CloseWait => {
            // ACK for our sent data.
            if flags & ACK != 0 {
                let prev_una = tbl.conns[idx].snd_una;
                if ack_num.wrapping_sub(prev_una) <= tbl.conns[idx].snd_nxt.wrapping_sub(prev_una) {
                    tbl.conns[idx].snd_una = ack_num;
                }
            }

            // Deliver payload.
            if !payload.is_empty() {
                let copy = core::cmp::min(payload.len(), TCP_RECV_BUF - tbl.conns[idx].recv_len);
                if copy > 0 {
                    let start = tbl.conns[idx].recv_len;
                    tbl.conns[idx].recv_buf[start..start + copy].copy_from_slice(&payload[..copy]);
                    tbl.conns[idx].recv_len += copy;
                    tbl.conns[idx].rcv_nxt = tbl.conns[idx].rcv_nxt.wrapping_add(copy as u32);
                }
                // Send ACK for received data.
                let lp = tbl.conns[idx].local_port;
                let rp = tbl.conns[idx].remote_port;
                let snd_nxt = tbl.conns[idx].snd_nxt;
                let rcv_nxt = tbl.conns[idx].rcv_nxt;
                send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
            }

            // Peer closing.
            if flags & FIN != 0 && state == TcpState::Established {
                tbl.conns[idx].rcv_nxt = tbl.conns[idx].rcv_nxt.wrapping_add(1);
                tbl.conns[idx].state = TcpState::CloseWait;
                let lp = tbl.conns[idx].local_port;
                let rp = tbl.conns[idx].remote_port;
                let snd_nxt = tbl.conns[idx].snd_nxt;
                let rcv_nxt = tbl.conns[idx].rcv_nxt;
                send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
            }
        }

        TcpState::FinWait1 => {
            if flags & ACK != 0 {
                tbl.conns[idx].snd_una = ack_num;
                if flags & FIN != 0 {
                    // Simultaneous close.
                    tbl.conns[idx].rcv_nxt = tbl.conns[idx].rcv_nxt.wrapping_add(1);
                    let lp = tbl.conns[idx].local_port;
                    let rp = tbl.conns[idx].remote_port;
                    let snd_nxt = tbl.conns[idx].snd_nxt;
                    let rcv_nxt = tbl.conns[idx].rcv_nxt;
                    send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
                    tbl.conns[idx].state = TcpState::TimeWait;
                } else {
                    tbl.conns[idx].state = TcpState::FinWait2;
                }
            }
        }

        TcpState::FinWait2 => {
            if flags & FIN != 0 {
                tbl.conns[idx].rcv_nxt = tbl.conns[idx].rcv_nxt.wrapping_add(1);
                let lp = tbl.conns[idx].local_port;
                let rp = tbl.conns[idx].remote_port;
                let snd_nxt = tbl.conns[idx].snd_nxt;
                let rcv_nxt = tbl.conns[idx].rcv_nxt;
                send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
                tbl.conns[idx].state = TcpState::TimeWait;
            }
        }

        TcpState::LastAck => {
            if flags & ACK != 0 {
                // Fully closed.
                tbl.conns[idx].active = false;
                tbl.conns[idx].state = TcpState::Closed;
            }
        }

        TcpState::TimeWait => {
            // Re-send ACK if peer retransmits FIN.
            if flags & FIN != 0 {
                let lp = tbl.conns[idx].local_port;
                let rp = tbl.conns[idx].remote_port;
                let snd_nxt = tbl.conns[idx].snd_nxt;
                let rcv_nxt = tbl.conns[idx].rcv_nxt;
                send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
                // Restart 2MSL on receipt of retransmitted FIN (RFC 793).
                tbl.conns[idx].time_wait_deadline_ms = 0;
            }
            // Expiry is handled by tick_timers(); do not close inline.
        }

        TcpState::SynReceived => {
            // Expect ACK to complete three-way handshake.
            if flags & ACK != 0 {
                tbl.conns[idx].snd_una = ack_num;
                tbl.conns[idx].state = TcpState::AcceptPending;
                // Put this conn index in the listener's pending queue.
                let lp = tbl.conns[idx].local_port;
                if let Some(listener) = tbl
                    .listeners
                    .iter_mut()
                    .find(|l| l.active && l.local_port == lp)
                    && listener.pending_len < ACCEPT_BACKLOG
                {
                    listener.pending[listener.pending_len] = idx as u8;
                    listener.pending_len += 1;
                }
            }
        }

        TcpState::AcceptPending => {
            if flags & ACK != 0 {
                let prev_una = tbl.conns[idx].snd_una;
                if ack_num.wrapping_sub(prev_una) <= tbl.conns[idx].snd_nxt.wrapping_sub(prev_una) {
                    tbl.conns[idx].snd_una = ack_num;
                }
            }

            // Connection queued for accept; deliver any early data.
            if !payload.is_empty() {
                let copy = core::cmp::min(payload.len(), TCP_RECV_BUF - tbl.conns[idx].recv_len);
                if copy > 0 {
                    let start = tbl.conns[idx].recv_len;
                    tbl.conns[idx].recv_buf[start..start + copy].copy_from_slice(&payload[..copy]);
                    tbl.conns[idx].recv_len += copy;
                    tbl.conns[idx].rcv_nxt = tbl.conns[idx].rcv_nxt.wrapping_add(copy as u32);
                }
            }

            if flags & FIN != 0 {
                tbl.conns[idx].rcv_nxt = tbl.conns[idx].rcv_nxt.wrapping_add(1);
                tbl.conns[idx].state = TcpState::CloseWait;
            }

            // Delivered data — reset keepalive idle counter.
            tbl.conns[idx].keepalive_idle_ms = 0;
            tbl.conns[idx].keepalive_probes = 0;
            if !payload.is_empty() || flags & FIN != 0 {
                let lp = tbl.conns[idx].local_port;
                let rp = tbl.conns[idx].remote_port;
                let snd_nxt = tbl.conns[idx].snd_nxt;
                let rcv_nxt = tbl.conns[idx].rcv_nxt;
                send_segment_inner(our_ip, lp, src_ip, rp, snd_nxt, rcv_nxt, ACK, &[]);
            }
        }

        TcpState::Closed => {}
    }
}

/// Called from the kernel timer tick (~1 ms interval) to fire retransmissions.
/// On 5 consecutive unacknowledged retransmits the connection is RST-closed.
pub fn tick_retransmits(now_ms: u64) {
    without_irq(|| {
        let mut tbl = TCP.lock();
        let our_ip = crate::net::our_ipv4();
        for conn in tbl.conns.iter_mut() {
            if !conn.active {
                continue;
            }
            if conn.retransmit_len == 0 {
                continue;
            }
            if now_ms < conn.rto_deadline_ms {
                continue;
            }

            if conn.retransmit_count >= 5 {
                // Give up — send RST and close.
                send_segment_inner(
                    our_ip,
                    conn.local_port,
                    conn.remote_ip,
                    conn.remote_port,
                    conn.snd_una,
                    0,
                    RST,
                    &[],
                );
                conn.active = false;
                conn.state = TcpState::Closed;
                continue;
            }

            // Retransmit stored segment.
            let len = conn.retransmit_len;
            let mut tmp = [0u8; 512];
            tmp[..len].copy_from_slice(&conn.retransmit_buf[..len]);
            send_segment_inner(
                our_ip,
                conn.local_port,
                conn.remote_ip,
                conn.remote_port,
                conn.snd_una,
                conn.rcv_nxt,
                PSH | ACK,
                &tmp[..len],
            );
            conn.retransmit_count += 1;
            // Exponential back-off, cap at 60 s.
            conn.rto_ms = (conn.rto_ms.saturating_mul(2)).min(60_000);
            conn.rto_deadline_ms = now_ms + conn.rto_ms as u64;
        }
    });
}

/// Keepalive idle threshold before probing (7200 s = 2 hours).
const KEEPALIVE_IDLE_MS: u64 = 7_200_000;
/// Interval between keepalive probes (75 s).
const KEEPALIVE_INTERVAL_MS: u64 = 75_000;
/// Drop connection after this many unanswered probes.
const KEEPALIVE_PROBE_MAX: u8 = 9;
/// TIME_WAIT duration: 2 × MSL (2 × 60 s).
const TIME_WAIT_MS: u64 = 120_000;

/// Process keepalive probes and TIME_WAIT expiry for all active connections.
///
/// Called by the network timer (e.g., every 1 000 ms from the PIT ISR).
/// `elapsed_ms` is the number of milliseconds since this was last called.
pub fn tick_timers(now_ms: u64, elapsed_ms: u64) {
    without_irq(|| {
        let mut tbl = TCP.lock();
        let our_ip = crate::net::our_ipv4();
        for conn in tbl.conns.iter_mut() {
            if !conn.active {
                continue;
            }

            // ── TIME_WAIT expiry ────────────────────────────────────────
            if conn.state == TcpState::TimeWait {
                if conn.time_wait_deadline_ms == 0 {
                    // Not yet set — set now.
                    conn.time_wait_deadline_ms = now_ms + TIME_WAIT_MS;
                } else if now_ms >= conn.time_wait_deadline_ms {
                    conn.active = false;
                    conn.state = TcpState::Closed;
                }
                continue;
            }

            // Only probe ESTABLISHED connections.
            if conn.state != TcpState::Established {
                continue;
            }

            // ── Keepalive ───────────────────────────────────────────────
            conn.keepalive_idle_ms += elapsed_ms;
            if conn.keepalive_idle_ms < KEEPALIVE_IDLE_MS {
                continue;
            }

            // Idle threshold crossed — send/continue probing.
            if conn.keepalive_probes >= KEEPALIVE_PROBE_MAX {
                // No response after max probes — abort connection.
                send_segment_inner(
                    our_ip,
                    conn.local_port,
                    conn.remote_ip,
                    conn.remote_port,
                    conn.snd_una,
                    0,
                    RST,
                    &[],
                );
                conn.active = false;
                conn.state = TcpState::Closed;
                continue;
            }

            // Send keepalive probe: zero-length segment with snd_una - 1
            // (triggers an ACK from the peer if it is still alive).
            let probe_seq = conn.snd_una.wrapping_sub(1);
            send_segment_inner(
                our_ip,
                conn.local_port,
                conn.remote_ip,
                conn.remote_port,
                probe_seq,
                conn.rcv_nxt,
                ACK,
                &[],
            );
            conn.keepalive_probes += 1;
            // Reset idle timer to the probe interval.
            conn.keepalive_idle_ms = KEEPALIVE_IDLE_MS.saturating_sub(KEEPALIVE_INTERVAL_MS);
        }
    });
}

/// Send a bare RST segment (called when no matching connection exists).
fn send_rst(
    src_ip: u32,
    _dst_ip: u32,
    src_port: u16,
    dst_ip: u32,
    dst_port: u16,
    seq: u32,
    _ack: u32,
) {
    send_segment_inner(src_ip, src_port, dst_ip, dst_port, seq, 0, RST, &[]);
}

// ── unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a full SYN → SYN-ACK → ACK → data → FIN/ACK sequence via
    /// direct calls to `handle_segment` without touching real hardware.
    #[test]
    fn tcp_three_way_handshake_and_data() {
        // -- Reset state -------------------------------------------------
        {
            let mut tbl = TCP.lock();
            for c in tbl.conns.iter_mut() {
                c.active = false;
                c.state = TcpState::Closed;
            }
        }

        // We don't have a real TX hook in tests; patch a no-op.
        // (super::transmit_ipv4_packet is called but safely does nothing
        //  if there is no tx_hook registered.)

        let key = [0x01u8; 16];
        let local_port: u16 = 12345;
        let remote_ip: u32 = 0xC0A8_0001; // 192.168.0.1 (fake)
        let remote_port: u16 = 80;

        // Initiate connect — allocates slot, sends SYN.
        let ok = connect(key, local_port, remote_ip, remote_port);
        assert!(ok, "connect should succeed");

        // Verify SYN_SENT state.
        {
            let tbl = TCP.lock();
            let conn = tbl
                .conns
                .iter()
                .find(|c| c.active && c.socket_key == key)
                .unwrap();
            assert_eq!(conn.state, TcpState::SynSent);
            let isn = conn.snd_una;
            drop(tbl);

            // Simulate incoming SYN-ACK from peer.
            let peer_isn: u32 = 0xDEAD_BEEF;
            let mut syn_ack = [0u8; TCP_HEADER_BYTES];
            syn_ack[0..2].copy_from_slice(&remote_port.to_be_bytes()); // src port
            syn_ack[2..4].copy_from_slice(&local_port.to_be_bytes()); // dst port
            syn_ack[4..8].copy_from_slice(&peer_isn.to_be_bytes()); // seq
            syn_ack[8..12].copy_from_slice(&(isn.wrapping_add(1)).to_be_bytes()); // ack
            syn_ack[12] = 5 << 4; // data offset
            syn_ack[13] = SYN | ACK;
            syn_ack[14..16].copy_from_slice(&WINDOW_SIZE.to_be_bytes());

            handle_segment(remote_ip, crate::net::OUR_IPV4, &syn_ack);
        }

        // Verify ESTABLISHED.
        assert!(is_established(key), "should be established after SYN-ACK");

        // Verify rcv_nxt = peer_isn + 1.
        {
            let tbl = TCP.lock();
            let conn = tbl
                .conns
                .iter()
                .find(|c| c.active && c.socket_key == key)
                .unwrap();
            assert_eq!(conn.rcv_nxt, 0xDEAD_BEF0u32); // peer_isn + 1
        }

        // Simulate incoming data from peer.
        let peer_isn: u32 = 0xDEAD_BEEF;
        let hello = b"hello";
        let mut data_seg = [0u8; TCP_HEADER_BYTES + 5];
        data_seg[0..2].copy_from_slice(&remote_port.to_be_bytes());
        data_seg[2..4].copy_from_slice(&local_port.to_be_bytes());
        let data_seq = peer_isn.wrapping_add(1);
        data_seg[4..8].copy_from_slice(&data_seq.to_be_bytes());
        data_seg[8..12].copy_from_slice(&[0u8; 4]); // ack
        data_seg[12] = 5 << 4;
        data_seg[13] = PSH | ACK;
        data_seg[14..16].copy_from_slice(&WINDOW_SIZE.to_be_bytes());
        data_seg[TCP_HEADER_BYTES..].copy_from_slice(hello);
        handle_segment(remote_ip, crate::net::OUR_IPV4, &data_seg);

        // Drain via recv.
        let mut out = [0u8; 32];
        let n = recv(key, &mut out).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&out[..5], b"hello");

        // Close.
        close(key);
        {
            let tbl = TCP.lock();
            let conn = tbl
                .conns
                .iter()
                .find(|c| c.active && c.socket_key == key)
                .unwrap();
            assert_eq!(conn.state, TcpState::FinWait1);
        }
    }
}
