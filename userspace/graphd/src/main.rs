// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphd — the userspace graph runtime service.
//!
//! graphd owns the typed temporal graph that represents system truth.
//! It is the primary home of the heterogeneous temporal graph H = (V, E, tau_V, tau_E, t).
//!
//! ## Paper doctrine engines served
//!
//! - **Local Operator Engine**: graphd computes per-node scores (urgency,
//!   relevance, risk, trust, recency) from graph topology and temporal
//!   metadata. It answers scored-neighborhood queries for modeld and shell3d.
//!
//! - **Structural Engine**: graphd produces compressed graph summaries using
//!   MDL ideas: L(G,M) = L(M) + L(G|M). It detects motifs, clusters
//!   nodes by kind, and reports Fiedler value / spectral gaps.
//!
//! - **Predictive Engine**: graphd tracks temporal drift metrics (eigenvalue
//!   changes, edge churn, score divergence) and serves DriftReport responses.
//!
//! - **Causal Decision Engine**: graphd answers causal-ancestor queries
//!   by walking GrangerCauses/TransferEntropy edges.
//!
//! ## IPC protocol
//!
//! graphd listens on a dedicated IPC channel. It handles:
//! - MsgTag::GraphQuery (0x10) -> dispatches to query handler, replies GraphQueryResult (0x11)
//! - MsgTag::GraphMutate (0x12) -> dispatches to mutation handler, replies GraphMutateAck (0x13)
//! - MsgTag::GraphSubscribe (0x14) -> registers for event notifications
//! - MsgTag::Ping (0x01) -> replies Pong (0x02)
//! - MsgTag::Shutdown (0x04) -> clean exit
//!
//! ## Design constraints
//!
//! - graphd does NOT run in ring 0. It communicates with the kernel graph
//!   arena through IPC, not through direct memory access.
//! - All queries and mutations are journaled for provenance.
//! - Graph state is the single source of truth for all services.
//!
//! ## Current status
//!
//! This host-build service scaffolding shares the real IPC/schema contract
//! and query/mutation logic shape, but it is not yet kernel-launched.
//! Until live ring-3 execution and userspace ELF loading exist, the
//! in-kernel graph arena remains the authoritative runtime.
//!
//! The IPC message types are defined in kernel/src/ipc/graphd_proto.rs
//! and are shared between kernel and userspace via a stable ABI contract.

#[path = "../../common/host_sys.rs"]
mod host_sys;
#[path = "../../common/ipc.rs"]
mod ipc;

/// graphd configuration.
pub struct GraphdConfig {
    /// IPC channel this service listens on.
    pub listen_channel: u32,
    /// Maximum nodes in the userspace graph replica.
    pub max_nodes: usize,
    /// Maximum edges in the userspace graph replica.
    pub max_edges: usize,
    /// Whether to journal all mutations.
    pub journal_mutations: bool,
    /// Structural summary refresh interval (in ticks).
    pub summary_interval: u64,
    /// Drift report refresh interval (in ticks).
    pub drift_interval: u64,
}

impl GraphdConfig {
    pub const fn default() -> Self {
        Self {
            listen_channel: 2,
            max_nodes: 4096,
            max_edges: 16384,
            journal_mutations: true,
            summary_interval: 100,
            drift_interval: 50,
        }
    }
}

/// graphd service state.
pub struct GraphdService {
    config: GraphdConfig,
    /// Number of queries processed.
    queries_processed: u64,
    /// Number of mutations processed.
    mutations_processed: u64,
    /// Number of errors.
    errors: u64,
    /// Current generation (mutation sequence number).
    generation: u64,
    /// Whether the service is running.
    running: bool,
}

impl GraphdService {
    pub fn new(config: GraphdConfig) -> Self {
        Self {
            config,
            queries_processed: 0,
            mutations_processed: 0,
            errors: 0,
            generation: 0,
            running: false,
        }
    }

    /// Start the service event loop.
    pub fn run(&mut self) {
        self.running = true;
        let mut buf = [0u8; 256];

        loop {
            let result = sys_channel_recv(self.config.listen_channel, &mut buf);
            let Some(msg) = ipc::decode_recv_result(result) else {
                sys_yield();
                continue;
            };

            match msg.tag {
                0x10 => {
                    // MsgTag::GraphQuery
                    self.handle_query(&buf[..msg.payload_len], msg.reply_endpoint);
                }
                0x12 => {
                    // MsgTag::GraphMutate
                    self.handle_mutation(&buf[..msg.payload_len], msg.reply_endpoint);
                }
                0x14 => {
                    // MsgTag::GraphSubscribe
                    sys_channel_send(msg.reply_endpoint, &[], 0x02);
                }
                0x01 => {
                    sys_channel_send(msg.reply_endpoint, &[], 0x02);
                }
                0x04 => {
                    break;
                }
                _ => {
                    self.errors += 1;
                }
            }
        }

        self.running = false;
    }

    /// Handle a graph query request. Dispatches by query_kind.
    /// Payload: query_kind(1) + target_node(8) + filter(4) = 13 bytes min.
    pub fn handle_query(&mut self, payload: &[u8], reply_endpoint: u32) {
        self.queries_processed += 1;
        if payload.len() < 13 {
            sys_channel_send(reply_endpoint, &[0xFF], 0x03);
            self.errors += 1;
            return;
        }
        let query_kind = payload[0];
        let target_node = u64::from_le_bytes([
            payload[1], payload[2], payload[3], payload[4], payload[5], payload[6], payload[7],
            payload[8],
        ]);
        let filter = u32::from_le_bytes([payload[9], payload[10], payload[11], payload[12]]);

        let mut resp = [0u8; 29];
        resp[0] = query_kind;
        resp[1..9].copy_from_slice(&target_node.to_le_bytes());
        let result_count: u32 = match query_kind {
            0 => 1,
            1 | 2 => 0,
            3 => 0,
            4 => 0,
            5 => 1,
            6 => 1,
            7 => 0,
            8 => 1,
            _ => 0,
        };
        resp[9..13].copy_from_slice(&result_count.to_le_bytes());
        resp[13..21].copy_from_slice(&self.generation.to_le_bytes());
        if query_kind == 8 {
            resp[21..25].copy_from_slice(&(self.queries_processed as u32).to_le_bytes());
            resp[25..29].copy_from_slice(&(self.mutations_processed as u32).to_le_bytes());
        }
        let _ = filter;
        sys_channel_send(reply_endpoint, &resp, 0x11);
    }

    /// Handle a graph mutation request.
    /// Payload: mutation_kind(1) + variable args.
    pub fn handle_mutation(&mut self, payload: &[u8], reply_endpoint: u32) {
        self.mutations_processed += 1;
        self.generation += 1;
        if payload.is_empty() {
            sys_channel_send(reply_endpoint, &[0xFF], 0x03);
            self.errors += 1;
            return;
        }
        let mutation_kind = payload[0];
        let valid = match mutation_kind {
            0 => payload.len() >= 9,
            1 => payload.len() >= 17,
            2 => payload.len() >= 13,
            3 => payload.len() >= 21,
            4 => payload.len() >= 9,
            5 => payload.len() >= 17,
            _ => false,
        };
        let mut ack = [0u8; 10];
        ack[0] = mutation_kind;
        ack[1..9].copy_from_slice(&self.generation.to_le_bytes());
        ack[9] = if valid { 1 } else { 0 };
        if !valid {
            self.errors += 1;
        }
        sys_channel_send(reply_endpoint, &ack, 0x13);
    }
}

// ════════════════════════════════════════════════════════════════════
// Host-mode syscall shims until ring-3 exists
// ════════════════════════════════════════════════════════════════════

fn sys_channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    host_sys::channel_recv(channel, buf)
}

fn sys_channel_send(channel: u32, payload: &[u8], tag: u8) {
    host_sys::channel_send(channel, payload, tag);
}

fn sys_yield() {
    host_sys::yield_now();
}

fn main() {
    let mut service = GraphdService::new(GraphdConfig::default());
    service.run();
}
