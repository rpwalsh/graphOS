// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Grid Computing — distributed resource sharing across a subnet.
//!
//! Architecture
//! ============
//! Every GraphOS node on the same subnet (sharing a link-local IPv6 prefix)
//! automatically discovers peers by sending a `GridHello` beacon to the
//! link-scoped multicast address `ff02::6749`.  No configuration required.
//!
//! Once discovered, nodes share:
//!   - **CPU**: remote task spawn (`SYS_GRID_SPAWN`) migrates tasks to a peer
//!   - **RAM**: remote slab allocation (`SYS_GRID_ALLOC`) borrows pages
//!   - **GPU**: remote surface submission (`SYS_GRID_SURFACE`) routes to peer GPU
//!   - **Storage**: remote VFS mount (`SYS_GRID_MOUNT`) exposes peer filesystem
//!   - **IPC**: transparent channel forwarding so local channels work remotely
//!
//! Each grid peer is identified by a `Uuid128` node UUID.  The union of all
//! peer-node and resource graph edges forms the **cluster graph** — a
//! distributed extension of the single-node kernel graph.

pub mod discovery;
pub mod protocol;
pub mod remote_ipc;
pub mod remote_mem;
pub mod remote_task;
pub mod remote_vfs;
pub mod resource;

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Mutex;

use crate::uuid::{Uuid128, Uuid128Gen};

// ── Global cluster state ─────────────────────────────────────────────────────

/// Maximum peer nodes in the cluster (static allocation).
const MAX_PEERS: usize = 64;

/// True once `init()` has been called and we have a local node UUID.
static GRID_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Per-peer record.
#[derive(Clone, Copy)]
pub struct PeerNode {
    pub node_uuid: Uuid128,
    pub link_local_ip: [u8; 16],
    pub mac: [u8; 6],
    /// Grid protocol version reported in GridHello.
    pub version: u8,
    /// Capabilities bitmap: bit 0 = CPU offload, 1 = RAM share, 2 = GPU share,
    ///                       3 = storage share, 4 = IPC forwarding.
    pub capabilities: u8,
    /// Total CPU logical cores advertised.
    pub cpu_cores: u8,
    /// Available RAM in MiB (0..=255, saturating at 255 MiB).
    pub ram_free_mib: u8,
    /// Millisecond tick of last beacon (for timeout detection).
    pub last_seen_ms: u64,
    pub valid: bool,
}

impl PeerNode {
    const EMPTY: Self = Self {
        node_uuid: Uuid128::NIL,
        link_local_ip: [0u8; 16],
        mac: [0u8; 6],
        version: 0,
        capabilities: 0,
        cpu_cores: 0,
        ram_free_mib: 0,
        last_seen_ms: 0,
        valid: false,
    };
}

/// Capability bit masks for `PeerNode::capabilities`.
pub mod cap {
    pub const CPU: u8 = 1 << 0;
    pub const RAM: u8 = 1 << 1;
    pub const GPU: u8 = 1 << 2;
    pub const STORAGE: u8 = 1 << 3;
    pub const IPC: u8 = 1 << 4;
    /// Full-feature: all capabilities.
    pub const ALL: u8 = 0x1f;
}

struct GridState {
    local_node_uuid: Uuid128,
    local_capabilities: u8,
    peers: [PeerNode; MAX_PEERS],
    peer_count: usize,
}

impl GridState {
    const fn new() -> Self {
        Self {
            local_node_uuid: Uuid128::NIL,
            local_capabilities: cap::ALL,
            peers: [PeerNode::EMPTY; MAX_PEERS],
            peer_count: 0,
        }
    }
}

static GRID: Mutex<GridState> = Mutex::new(GridState::new());

// ── Init ─────────────────────────────────────────────────────────────────────

/// Initialise the grid subsystem.
///
/// Called once during kernel init, after the network stack is up.
/// Assigns a stable node UUID (UUID v5 from the machine MAC address) and
/// marks the grid as active so discovery beacons start on the next net tick.
pub fn init(mac: [u8; 6]) {
    let node_uuid = Uuid128Gen::v5_name(crate::uuid::UUID_NS_GRAPH, mac.as_slice());
    let mut g = GRID.lock();
    g.local_node_uuid = node_uuid;
    drop(g);
    GRID_ACTIVE.store(true, Ordering::Release);
    crate::arch::serial::write_line(b"[grid] init: node UUID assigned");
}

/// Returns the local node UUID, or `Uuid128::NIL` if not yet initialised.
pub fn local_node_uuid() -> Uuid128 {
    GRID.lock().local_node_uuid
}

/// Returns true if the grid subsystem is active (after `init()`).
#[inline]
pub fn is_active() -> bool {
    GRID_ACTIVE.load(Ordering::Acquire)
}

// ── Peer registry ─────────────────────────────────────────────────────────────

/// Called by the discovery layer when a `GridHello` beacon is received.
pub struct PeerHelloUpdate {
    pub node_uuid: Uuid128,
    pub link_local_ip: [u8; 16],
    pub mac: [u8; 6],
    pub version: u8,
    pub capabilities: u8,
    pub cpu_cores: u8,
    pub ram_free_mib: u8,
}

pub fn on_peer_hello(update: PeerHelloUpdate, now_ms: u64) {
    let mut g = GRID.lock();
    // Update existing peer record.
    for peer in g.peers.iter_mut().filter(|p| p.valid) {
        if peer.node_uuid == update.node_uuid {
            // Only overwrite network info if the caller is a full Hello (version > 0).
            if update.version > 0 {
                peer.link_local_ip = update.link_local_ip;
                peer.mac = update.mac;
                peer.version = update.version;
                peer.capabilities = update.capabilities;
                peer.cpu_cores = update.cpu_cores;
            }
            peer.ram_free_mib = update.ram_free_mib;
            peer.last_seen_ms = now_ms;
            return;
        }
    }
    // Don't create stub records for update-only calls.
    if update.version == 0 {
        return;
    }
    // New peer — find a free slot.
    for peer in &mut g.peers {
        if !peer.valid {
            *peer = PeerNode {
                node_uuid: update.node_uuid,
                link_local_ip: update.link_local_ip,
                mac: update.mac,
                version: update.version,
                capabilities: update.capabilities,
                cpu_cores: update.cpu_cores,
                ram_free_mib: update.ram_free_mib,
                last_seen_ms: now_ms,
                valid: true,
            };
            g.peer_count = g.peer_count.saturating_add(1);
            crate::arch::serial::write_line(b"[grid] new peer registered");
            return;
        }
    }
}

/// Expire peers not seen within `timeout_ms` milliseconds.
pub fn expire_stale_peers(now_ms: u64, timeout_ms: u64) {
    let mut g = GRID.lock();
    let mut expired = 0u32;
    for peer in &mut g.peers {
        if peer.valid && now_ms.saturating_sub(peer.last_seen_ms) > timeout_ms {
            crate::arch::serial::write_line(b"[grid] peer expired");
            peer.valid = false;
            expired += 1;
        }
    }
    g.peer_count = g.peer_count.saturating_sub(expired as usize);
}

/// Copy peer list into a caller-provided buffer. Returns number of peers copied.
pub fn snapshot_peers(out: &mut [PeerNode]) -> usize {
    let g = GRID.lock();
    let mut count = 0;
    for peer in g.peers.iter().filter(|p| p.valid) {
        if count >= out.len() {
            break;
        }
        out[count] = *peer;
        count += 1;
    }
    count
}

/// Find the least-loaded peer that has the given capability bit set.
pub fn best_peer_for(capability: u8) -> Option<PeerNode> {
    let g = GRID.lock();
    let mut best: Option<&PeerNode> = None;
    for peer in g
        .peers
        .iter()
        .filter(|p| p.valid && (p.capabilities & capability) != 0)
    {
        match best {
            None => best = Some(peer),
            Some(b) if peer.ram_free_mib > b.ram_free_mib => best = Some(peer),
            _ => {}
        }
    }
    best.copied()
}
