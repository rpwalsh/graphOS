// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! artifactsd — generated artifact management service.
//!
//! artifactsd owns the lifecycle of all artifacts produced by GraphOS:
//! - Answer bundles (structured answers with provenance chains)
//! - Verification reports (audit of reasoning and evidence)
//! - Long-form documents (generated prose with citations)
//! - System bundles (exportable snapshots of graph + models)
//!
//! ## SCCE doctrine preserved
//!
//! - **Provenance-is-the-product**: every artifact carries a full
//!   provenance chain back to source documents, spans, and chunks.
//!   An answer without provenance is not an answer.
//! - **Content-addressed storage**: artifacts are identified by their
//!   BLAKE3 content hash. Duplicate content is never stored twice.
//! - **Audit trail**: every artifact creation is logged as an AuditEntry
//!   via sysd. Who created it, from what query, with what evidence.
//!
//! ## SCCE implementation discarded
//!
//! - ZIP file generation for webapps → deferred (webapp builder not ported)
//! - Express download routes → replaced by IPC queries
//! - Filesystem artifact storage → replaced by graph node + arena storage
//!
//! ## IPC protocol
//!
//! artifactsd listens on a dedicated IPC channel. It handles:
//! - MsgTag::ArtifactCreated (0x50)  → register a new artifact in the catalog
//! - MsgTag::ArtifactQuery (0x51)    → look up artifact by node ID
//! - MsgTag::Ping (0x01)            → reply Pong (0x02)
//! - MsgTag::Shutdown (0x04)        → clean exit
//!
//! artifactsd sends:
//! - MsgTag::ArtifactResponse (0x52) → artifact descriptor to requester
//! - MsgTag::RuntimeEvent (0x60)     → to sysd (creation events)
//! - MsgTag::AuditEntry (0x61)       → to sysd (audit trail)

// ════════════════════════════════════════════════════════════════════
// Configuration
// ════════════════════════════════════════════════════════════════════

#[path = "../../common/host_sys.rs"]
mod host_sys;
#[path = "../../common/ipc.rs"]
mod ipc;

/// artifactsd configuration.
pub struct ArtifactsdConfig {
    /// IPC channel this service listens on.
    pub listen_channel: u32,
    /// IPC channel for sending events to sysd.
    pub sysd_channel: u32,
    /// Maximum number of artifacts tracked.
    pub max_artifacts: usize,
}

impl ArtifactsdConfig {
    pub const fn default() -> Self {
        Self {
            listen_channel: 5,
            sysd_channel: 6,
            max_artifacts: 256,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Artifact catalog
// ════════════════════════════════════════════════════════════════════

/// Maximum tracked artifacts (static allocation, no heap).
const MAX_CATALOG: usize = 256;

/// Artifact type tag (mirrors scce_proto::ArtifactType).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ArtifactType {
    AnswerBundle = 0,
    LongformDoc = 1,
    VerificationReport = 2,
    SystemBundle = 3,
}

/// A catalog entry — compact metadata for one artifact.
#[derive(Clone, Copy)]
struct CatalogEntry {
    /// Graph node ID (0 = empty slot).
    node_id: u64,
    /// Content hash (first 16 bytes of BLAKE3).
    content_hash: [u8; 16],
    /// Artifact type.
    artifact_type: ArtifactType,
    /// Size in bytes.
    size: u32,
    /// Creation timestamp.
    created_at: u64,
    /// Whether this slot is occupied.
    occupied: bool,
}

impl CatalogEntry {
    const EMPTY: Self = Self {
        node_id: 0,
        content_hash: [0; 16],
        artifact_type: ArtifactType::AnswerBundle,
        size: 0,
        created_at: 0,
        occupied: false,
    };
}

/// Fixed-capacity artifact catalog.
struct ArtifactCatalog {
    entries: [CatalogEntry; MAX_CATALOG],
    count: usize,
}

impl ArtifactCatalog {
    const fn new() -> Self {
        Self {
            entries: [CatalogEntry::EMPTY; MAX_CATALOG],
            count: 0,
        }
    }

    /// Register a new artifact. Returns false if catalog is full.
    fn register(&mut self, entry: CatalogEntry) -> bool {
        if self.count >= MAX_CATALOG {
            return false;
        }
        for slot in self.entries.iter_mut() {
            if !slot.occupied {
                *slot = CatalogEntry {
                    occupied: true,
                    ..entry
                };
                self.count += 1;
                return true;
            }
        }
        false
    }

    /// Look up an artifact by graph node ID.
    fn find_by_node(&self, node_id: u64) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|e| e.occupied && e.node_id == node_id)
    }

    /// Look up by content hash (deduplication check).
    fn find_by_hash(&self, hash: &[u8; 16]) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|e| e.occupied && e.content_hash == *hash)
    }

    /// Total registered artifacts.
    fn count(&self) -> usize {
        self.count
    }
}

// ════════════════════════════════════════════════════════════════════
// Service
// ════════════════════════════════════════════════════════════════════

/// artifactsd service state.
pub struct ArtifactsdService {
    config: ArtifactsdConfig,
    catalog: ArtifactCatalog,
    /// Total events emitted.
    event_seq: u32,
    /// Whether the service is running.
    running: bool,
}

impl ArtifactsdService {
    pub fn new(config: ArtifactsdConfig) -> Self {
        Self {
            config,
            catalog: ArtifactCatalog::new(),
            event_seq: 0,
            running: false,
        }
    }

    /// Start the service event loop.
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
                0x50 => {
                    // MsgTag::ArtifactCreated
                    self.handle_register(&buf[..msg.payload_len], msg.reply_endpoint);
                }
                0x51 => {
                    // MsgTag::ArtifactQuery
                    self.handle_query(&buf[..msg.payload_len], msg.reply_endpoint);
                }
                0x01 => {
                    // MsgTag::Ping — reply Pong
                    sys_channel_send(msg.reply_endpoint, &[], 0x02);
                }
                0x04 => {
                    // MsgTag::Shutdown
                    break;
                }
                _ => {}
            }
        }

        self.running = false;
    }

    /// Handle artifact registration.
    /// Deserializes: node_id(8) + content_hash(16) + artifact_type(1) + size(4) + created_at(8) = 37 bytes.
    pub fn handle_register(&mut self, payload: &[u8], reply_endpoint: u32) -> bool {
        if payload.len() < 37 {
            return false;
        }

        let node_id = u64::from_le_bytes([
            payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
            payload[7],
        ]);
        let mut content_hash = [0u8; 16];
        content_hash.copy_from_slice(&payload[8..24]);
        let artifact_type = match payload[24] {
            0 => ArtifactType::AnswerBundle,
            1 => ArtifactType::LongformDoc,
            2 => ArtifactType::VerificationReport,
            3 => ArtifactType::SystemBundle,
            _ => ArtifactType::AnswerBundle,
        };
        let size = u32::from_le_bytes([payload[25], payload[26], payload[27], payload[28]]);
        let created_at = u64::from_le_bytes([
            payload[29],
            payload[30],
            payload[31],
            payload[32],
            payload[33],
            payload[34],
            payload[35],
            payload[36],
        ]);

        // Dedup: check if content hash already exists.
        if self.catalog.find_by_hash(&content_hash).is_some() {
            sys_channel_send(reply_endpoint, &[0x02], 0x03); // Error: duplicate
            return false;
        }

        let entry = CatalogEntry {
            node_id,
            content_hash,
            artifact_type,
            size,
            created_at,
            occupied: true,
        };
        let ok = self.catalog.register(entry);
        if ok {
            self.emit_event(40, node_id, size); // ArtifactCreated event
            self.emit_audit(created_at, reply_endpoint as u64, 1, 1, node_id); // action=create, outcome=success
        }
        ok
    }

    /// Handle artifact query — look up by node ID, reply with descriptor.
    /// Payload: node_id(8) = 8 bytes.
    pub fn handle_query(&self, payload: &[u8], reply_endpoint: u32) {
        if payload.len() < 8 {
            sys_channel_send(reply_endpoint, &[VFS_ERROR_NOT_FOUND], 0x03);
            return;
        }
        let node_id = u64::from_le_bytes([
            payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
            payload[7],
        ]);
        if let Some(entry) = self.catalog.find_by_node(node_id) {
            self.send_artifact_response(reply_endpoint, entry);
        } else {
            sys_channel_send(reply_endpoint, &[VFS_ERROR_NOT_FOUND], 0x03);
        }
    }

    /// Number of artifacts in the catalog.
    pub fn artifact_count(&self) -> usize {
        self.catalog.count()
    }
}

const VFS_ERROR_NOT_FOUND: u8 = 1;

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

impl ArtifactsdService {
    /// Send an ArtifactResponse (0x52) to a requester.
    fn send_artifact_response(&self, reply_endpoint: u32, entry: &CatalogEntry) {
        // Serialize: node_id(8) + hash(16) + type(1) + size(4) + created_at(8) = 37 bytes
        let mut buf = [0u8; 37];
        buf[0..8].copy_from_slice(&entry.node_id.to_le_bytes());
        buf[8..24].copy_from_slice(&entry.content_hash);
        buf[24] = entry.artifact_type as u8;
        buf[25..29].copy_from_slice(&entry.size.to_le_bytes());
        buf[29..37].copy_from_slice(&entry.created_at.to_le_bytes());
        sys_channel_send(reply_endpoint, &buf, 0x52);
    }

    /// Emit a RuntimeEvent (0x60) to sysd.
    fn emit_event(&mut self, event_type: u8, related_node: u64, payload: u32) {
        self.event_seq += 1;
        let mut buf = [0u8; 17];
        buf[0] = event_type;
        buf[1..9].copy_from_slice(&related_node.to_le_bytes());
        buf[9..13].copy_from_slice(&payload.to_le_bytes());
        buf[13..17].copy_from_slice(&self.event_seq.to_le_bytes());
        sys_channel_send(self.config.sysd_channel, &buf, 0x60);
    }

    /// Emit an AuditEntry (0x61) to sysd.
    fn emit_audit(&self, ts: u64, actor: u64, action: u8, outcome: u8, target: u64) {
        let mut buf = [0u8; 26];
        buf[0..8].copy_from_slice(&ts.to_le_bytes());
        buf[8..16].copy_from_slice(&actor.to_le_bytes());
        buf[16] = action;
        buf[17] = outcome;
        buf[18..26].copy_from_slice(&target.to_le_bytes());
        sys_channel_send(self.config.sysd_channel, &buf, 0x61);
    }
}

fn main() {
    let mut service = ArtifactsdService::new(ArtifactsdConfig::default());
    service.run();
}
