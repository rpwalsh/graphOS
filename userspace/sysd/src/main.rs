// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! sysd — diagnostics, audit, telemetry, and event streaming service.
//!
//! sysd is the central observability and accountability service for GraphOS.
//! It receives events from all other services and maintains:
//! - A ring buffer of runtime events (replay-capable)
//! - An append-only audit log
//! - Aggregate diagnostic status (knowledge graph stats, job states, etc.)
//! - Cognitive pipeline status tracking
//!
//! ## SCCE doctrine preserved
//!
//! - **Auditability**: every significant system action produces an audit
//!   entry with actor, action, target, outcome, and timestamp. The audit
//!   log is append-only and exportable.
//! - **Event streaming**: runtime events are typed, sequenced, and
//!   replayable. New subscribers can replay the ring buffer to catch up.
//!   This replaces SCCE's SSE event bus.
//! - **System status**: sysd maintains an aggregate view of the system
//!   (document count, entity count, relation count, spectral state, job
//!   states, verifier scores) that shell3d polls for live display.
//! - **Cognitive pipeline tracing**: sysd receives CognitiveStatus
//!   messages from modeld, enabling step-by-step debugging of the
//!   think loop (perception → verification → planning → synthesis).
//!
//! ## SCCE implementation discarded
//!
//! - PostgreSQL audit_log table → replaced by in-memory ring buffer
//! - Express `/api/audit/export` → replaced by IPC query
//! - Express `/api/system/status` → replaced by IPC query
//! - SSE EventSource → replaced by IPC event channel
//! - 500-event-per-conversation buffer → replaced by global ring buffer
//!
//! ## IPC protocol
//!
//! sysd listens on a dedicated IPC channel. It handles:
//! - MsgTag::RuntimeEvent (0x60)     → store in event ring buffer
//! - MsgTag::AuditEntry (0x61)       → store in audit log
//! - MsgTag::DiagnosticQuery (0x62)  → reply DiagnosticResponse (0x63)
//! - MsgTag::CognitiveStatus (0x64)  → store latest pipeline status
//! - MsgTag::Ping (0x01)            → reply Pong (0x02)
//! - MsgTag::Shutdown (0x04)        → clean exit
//!
//! sysd sends:
//! - MsgTag::DiagnosticResponse (0x63) → to requester
//! - MsgTag::RuntimeEvent (0x60)       → forwarded to shell3d (if subscribed)

// ════════════════════════════════════════════════════════════════════
// Configuration
// ════════════════════════════════════════════════════════════════════

#[path = "../../common/host_sys.rs"]
mod host_sys;
#[path = "../../common/ipc.rs"]
mod ipc;

/// sysd configuration.
pub struct SysdConfig {
    /// IPC channel this service listens on.
    pub listen_channel: u32,
    /// IPC channel for forwarding events to shell3d (0 = disabled).
    pub shell_channel: u32,
}

impl SysdConfig {
    pub const fn default() -> Self {
        Self {
            listen_channel: 6,
            shell_channel: 0,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Event ring buffer
// ════════════════════════════════════════════════════════════════════

/// Capacity of the event ring buffer.
///
/// SCCE used 500 per conversation. We use a global buffer of 512
/// (power-of-two for fast modular indexing).
const EVENT_RING_CAP: usize = 512;
const EVENT_RING_MASK: usize = EVENT_RING_CAP - 1;

/// Compact event record stored in the ring buffer.
///
/// We store the raw IPC payload bytes (48 bytes for RuntimeEvent)
/// plus a global sequence number for replay ordering.
#[derive(Clone, Copy)]
struct EventSlot {
    /// Global sequence number (monotonically increasing).
    global_seq: u64,
    /// Event type tag (from scce_proto::EventType).
    event_type: u8,
    /// Related node ID.
    related_node: u64,
    /// Related node 2.
    related_node2: u64,
    /// Numeric payload.
    payload_u32: u32,
    /// Timestamp.
    timestamp: u64,
    /// Whether this slot has been written.
    valid: bool,
}

impl EventSlot {
    const EMPTY: Self = Self {
        global_seq: 0,
        event_type: 0,
        related_node: 0,
        related_node2: 0,
        payload_u32: 0,
        timestamp: 0,
        valid: false,
    };
}

/// Ring buffer for runtime events, supporting replay.
struct EventRing {
    slots: [EventSlot; EVENT_RING_CAP],
    /// Write cursor (wraps via mask).
    write_idx: usize,
    /// Global sequence counter.
    next_seq: u64,
}

impl EventRing {
    const fn new() -> Self {
        Self {
            slots: [EventSlot::EMPTY; EVENT_RING_CAP],
            write_idx: 0,
            next_seq: 1,
        }
    }

    /// Push an event into the ring buffer.
    fn push(
        &mut self,
        event_type: u8,
        related_node: u64,
        related_node2: u64,
        payload: u32,
        ts: u64,
    ) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let idx = self.write_idx & EVENT_RING_MASK;
        self.slots[idx] = EventSlot {
            global_seq: seq,
            event_type,
            related_node,
            related_node2,
            payload_u32: payload,
            timestamp: ts,
            valid: true,
        };
        self.write_idx = self.write_idx.wrapping_add(1);
    }

    /// Replay events since a given global sequence number.
    /// Returns up to `max` events in order.
    fn replay_since(&self, since_seq: u64, max: usize) -> ReplayIter<'_> {
        let total_pushed = self.total_pushed() as usize;
        let available = total_pushed.min(EVENT_RING_CAP);
        let start_idx = if total_pushed >= EVENT_RING_CAP {
            self.write_idx & EVENT_RING_MASK
        } else {
            0
        };

        ReplayIter {
            ring: self,
            start_idx,
            scanned: 0,
            available,
            since_seq,
            remaining: max,
        }
    }

    /// Total events pushed (including overwritten ones).
    fn total_pushed(&self) -> u64 {
        self.next_seq - 1
    }

    /// Fold a bounded replay window into a cheap digest so the replay path
    /// and stored fields stay exercised in host builds.
    fn recent_digest(&self, max: usize) -> u64 {
        self.replay_since(self.next_seq.saturating_sub(max as u64), max)
            .fold(0u64, |acc, slot| {
                acc ^ slot.global_seq
                    ^ ((slot.event_type as u64) << 56)
                    ^ slot.related_node
                    ^ slot.related_node2.rotate_left(7)
                    ^ (slot.payload_u32 as u64)
                    ^ slot.timestamp.rotate_left(13)
            })
    }
}

/// Iterator for replaying events from the ring buffer.
struct ReplayIter<'a> {
    ring: &'a EventRing,
    start_idx: usize,
    scanned: usize,
    available: usize,
    since_seq: u64,
    remaining: usize,
}

impl<'a> Iterator for ReplayIter<'a> {
    type Item = &'a EventSlot;

    fn next(&mut self) -> Option<Self::Item> {
        while self.scanned < self.available && self.remaining > 0 {
            let idx = (self.start_idx + self.scanned) & EVENT_RING_MASK;
            let slot = &self.ring.slots[idx];
            self.scanned += 1;
            if slot.valid && slot.global_seq >= self.since_seq {
                self.remaining -= 1;
                return Some(slot);
            }
        }
        None
    }
}

// ════════════════════════════════════════════════════════════════════
// Audit log
// ════════════════════════════════════════════════════════════════════

/// Capacity of the audit log ring buffer.
const AUDIT_LOG_CAP: usize = 1024;
const AUDIT_LOG_MASK: usize = AUDIT_LOG_CAP - 1;

/// Compact audit entry stored in the log.
#[derive(Clone, Copy)]
struct AuditSlot {
    /// Global audit sequence number.
    audit_seq: u64,
    /// Timestamp.
    timestamp: u64,
    /// Actor node ID.
    actor: u64,
    /// Action type (from scce_proto::AuditAction).
    action: u8,
    /// Outcome (from scce_proto::AuditOutcome).
    outcome: u8,
    /// Target node ID.
    target: u64,
    /// Whether this slot has been written.
    valid: bool,
}

impl AuditSlot {
    const EMPTY: Self = Self {
        audit_seq: 0,
        timestamp: 0,
        actor: 0,
        action: 0,
        outcome: 0,
        target: 0,
        valid: false,
    };
}

/// Append-only audit log (ring buffer, oldest entries overwritten).
struct AuditLog {
    slots: [AuditSlot; AUDIT_LOG_CAP],
    write_idx: usize,
    next_seq: u64,
}

impl AuditLog {
    const fn new() -> Self {
        Self {
            slots: [AuditSlot::EMPTY; AUDIT_LOG_CAP],
            write_idx: 0,
            next_seq: 1,
        }
    }

    fn append(&mut self, ts: u64, actor: u64, action: u8, outcome: u8, target: u64) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let idx = self.write_idx & AUDIT_LOG_MASK;
        self.slots[idx] = AuditSlot {
            audit_seq: seq,
            timestamp: ts,
            actor,
            action,
            outcome,
            target,
            valid: true,
        };
        self.write_idx = self.write_idx.wrapping_add(1);
    }

    fn total_entries(&self) -> u64 {
        self.next_seq - 1
    }

    /// Fold a bounded recent window into a digest so audit-slot metadata stays
    /// live even before export/query APIs are expanded.
    fn recent_digest(&self, max: usize) -> u64 {
        let total_written = self.total_entries() as usize;
        let available = total_written.min(AUDIT_LOG_CAP);
        let start_idx = if total_written >= AUDIT_LOG_CAP {
            self.write_idx & AUDIT_LOG_MASK
        } else {
            0
        };
        let start_scan = available.saturating_sub(max);

        self.slots
            .iter()
            .cycle()
            .skip(start_idx + start_scan)
            .take(available.saturating_sub(start_scan))
            .filter(|slot| slot.valid)
            .fold(0u64, |acc, slot| {
                acc ^ slot.audit_seq
                    ^ slot.timestamp.rotate_left(11)
                    ^ slot.actor
                    ^ ((slot.action as u64) << 40)
                    ^ ((slot.outcome as u64) << 48)
                    ^ slot.target.rotate_left(3)
            })
    }
}

// ════════════════════════════════════════════════════════════════════
// Aggregate diagnostic state
// ════════════════════════════════════════════════════════════════════

/// Aggregate system status, updated by incoming events.
///
/// This is the data behind DiagnosticStatus messages.
#[derive(Clone, Copy)]
pub struct AggregateStatus {
    pub doc_count: u32,
    pub entity_count: u32,
    pub relation_count: u32,
    pub chunk_count: u32,
    pub spectral_k: u16,
    pub index_running: bool,
    pub correlate_running: bool,
    pub spectral_running: bool,
    pub spectral_last_refresh: u64,
    pub avg_verifier_score: u32,
}

impl AggregateStatus {
    const fn new() -> Self {
        Self {
            doc_count: 0,
            entity_count: 0,
            relation_count: 0,
            chunk_count: 0,
            spectral_k: 0,
            index_running: false,
            correlate_running: false,
            spectral_running: false,
            spectral_last_refresh: 0,
            avg_verifier_score: 0,
        }
    }
}

/// Last cognitive pipeline status from modeld.
#[derive(Clone, Copy)]
pub struct CognitiveTrace {
    /// Current phase (0–9).
    pub phase: u8,
    /// Current round (0–2).
    pub round: u8,
    /// Evidence count.
    pub evidence_count: u8,
    /// Supported hypothesis count.
    pub supported_count: u8,
    /// Weak hypothesis count.
    pub weak_count: u8,
    /// Contradicted count.
    pub contradicted_count: u8,
    /// Elapsed microseconds.
    pub elapsed_us: u32,
    /// Query hash.
    pub query_hash: u64,
    /// Whether a pipeline is currently active.
    pub active: bool,
}

impl CognitiveTrace {
    const fn empty() -> Self {
        Self {
            phase: 0,
            round: 0,
            evidence_count: 0,
            supported_count: 0,
            weak_count: 0,
            contradicted_count: 0,
            elapsed_us: 0,
            query_hash: 0,
            active: false,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Service
// ════════════════════════════════════════════════════════════════════

/// sysd service state.
pub struct SysdService {
    config: SysdConfig,
    events: EventRing,
    audit: AuditLog,
    status: AggregateStatus,
    cognitive: CognitiveTrace,
    running: bool,
}

impl SysdService {
    pub fn new(config: SysdConfig) -> Self {
        Self {
            config,
            events: EventRing::new(),
            audit: AuditLog::new(),
            status: AggregateStatus::new(),
            cognitive: CognitiveTrace::empty(),
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
                0x60 => {
                    // MsgTag::RuntimeEvent
                    // Deserialize: event_type(1) + related_node(8) + related_node2(8) + payload(4) + ts(8) = 29 bytes
                    if msg.payload_len >= 29 {
                        let event_type = buf[0];
                        let related_node = u64::from_le_bytes([
                            buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8],
                        ]);
                        let related_node2 = u64::from_le_bytes([
                            buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16],
                        ]);
                        let payload = u32::from_le_bytes([buf[17], buf[18], buf[19], buf[20]]);
                        let ts = u64::from_le_bytes([
                            buf[21], buf[22], buf[23], buf[24], buf[25], buf[26], buf[27], buf[28],
                        ]);
                        self.handle_event(event_type, related_node, related_node2, payload, ts);
                    }
                }
                0x61 => {
                    // MsgTag::AuditEntry
                    // Deserialize: ts(8) + actor(8) + action(1) + outcome(1) + target(8) = 26 bytes
                    if msg.payload_len >= 26 {
                        let ts = u64::from_le_bytes([
                            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                        ]);
                        let actor = u64::from_le_bytes([
                            buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
                        ]);
                        let action = buf[16];
                        let outcome = buf[17];
                        let target = u64::from_le_bytes([
                            buf[18], buf[19], buf[20], buf[21], buf[22], buf[23], buf[24], buf[25],
                        ]);
                        self.handle_audit(ts, actor, action, outcome, target);
                    }
                }
                0x62 => {
                    // MsgTag::DiagnosticQuery
                    self.handle_diag_query(msg.reply_endpoint);
                }
                0x64 => {
                    // MsgTag::CognitiveStatus
                    // Deserialize: phase(1)+round(1)+evidence(1)+supported(1)+weak(1)+contradicted(1)+elapsed(4)+query_hash(8) = 18 bytes
                    if msg.payload_len >= 18 {
                        let phase = buf[0];
                        let round = buf[1];
                        let evidence = buf[2];
                        let supported = buf[3];
                        let weak = buf[4];
                        let contradicted = buf[5];
                        let elapsed = u32::from_le_bytes([buf[6], buf[7], buf[8], buf[9]]);
                        let query_hash = u64::from_le_bytes([
                            buf[10], buf[11], buf[12], buf[13], buf[14], buf[15], buf[16], buf[17],
                        ]);
                        self.handle_cognitive(
                            phase,
                            round,
                            evidence,
                            supported,
                            weak,
                            contradicted,
                            elapsed,
                            query_hash,
                        );
                    }
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

    /// Handle an incoming runtime event.
    /// Stores in ring buffer, updates aggregate status, forwards to shell3d.
    pub fn handle_event(
        &mut self,
        event_type: u8,
        related_node: u64,
        related_node2: u64,
        payload: u32,
        ts: u64,
    ) {
        self.events
            .push(event_type, related_node, related_node2, payload, ts);

        match event_type {
            0 => self.status.doc_count += 1,
            11 => self.status.entity_count += 1,
            12 => self.status.relation_count += 1,
            20 => {
                self.status.spectral_last_refresh = ts;
            }
            _ => {}
        }

        // Forward to shell3d if channel is configured.
        if self.config.shell_channel != 0 {
            let mut fwd = [0u8; 29];
            fwd[0] = event_type;
            fwd[1..9].copy_from_slice(&related_node.to_le_bytes());
            fwd[9..17].copy_from_slice(&related_node2.to_le_bytes());
            fwd[17..21].copy_from_slice(&payload.to_le_bytes());
            fwd[21..29].copy_from_slice(&ts.to_le_bytes());
            sys_channel_send(self.config.shell_channel, &fwd, 0x60);
        }
    }

    /// Handle an incoming audit entry.
    pub fn handle_audit(&mut self, ts: u64, actor: u64, action: u8, outcome: u8, target: u64) {
        self.audit.append(ts, actor, action, outcome, target);
    }

    /// Handle a diagnostic query — build and send DiagnosticStatus.
    pub fn handle_diag_query(&self, requester: u32) {
        let _activity_digest = self.events.recent_digest(8) ^ self.audit.recent_digest(8);
        // Serialize DiagnosticStatus (49 bytes)
        let mut buf = [0u8; 49];
        buf[0..4].copy_from_slice(&self.status.doc_count.to_le_bytes());
        buf[4..8].copy_from_slice(&self.status.entity_count.to_le_bytes());
        buf[8..12].copy_from_slice(&self.status.relation_count.to_le_bytes());
        buf[12..16].copy_from_slice(&self.status.chunk_count.to_le_bytes());
        buf[16..18].copy_from_slice(&self.status.spectral_k.to_le_bytes());
        let mut flags: u8 = 0;
        if self.status.index_running {
            flags |= 1;
        }
        if self.status.correlate_running {
            flags |= 2;
        }
        if self.status.spectral_running {
            flags |= 4;
        }
        buf[18] = flags;
        buf[19..27].copy_from_slice(&self.status.spectral_last_refresh.to_le_bytes());
        buf[27..31].copy_from_slice(&self.status.avg_verifier_score.to_le_bytes());
        buf[31..39].copy_from_slice(&self.events.total_pushed().to_le_bytes());
        buf[39..47].copy_from_slice(&self.audit.total_entries().to_le_bytes());
        buf[47] = if self.cognitive.active { 1 } else { 0 };
        buf[48] = self.cognitive.phase;
        sys_channel_send(requester, &buf, 0x63);
    }

    /// Handle a cognitive pipeline status update from modeld.
    pub fn handle_cognitive(
        &mut self,
        phase: u8,
        round: u8,
        evidence: u8,
        supported: u8,
        weak: u8,
        contradicted: u8,
        elapsed: u32,
        query_hash: u64,
    ) {
        self.cognitive = CognitiveTrace {
            phase,
            round,
            evidence_count: evidence,
            supported_count: supported,
            weak_count: weak,
            contradicted_count: contradicted,
            elapsed_us: elapsed,
            query_hash,
            active: phase < 10,
        };
    }

    /// Total events in the ring buffer.
    pub fn total_events(&self) -> u64 {
        self.events.total_pushed()
    }

    /// Total audit entries.
    pub fn total_audit_entries(&self) -> u64 {
        self.audit.total_entries()
    }

    /// Current aggregate status.
    pub fn status(&self) -> &AggregateStatus {
        &self.status
    }

    /// Current cognitive pipeline trace.
    pub fn cognitive_trace(&self) -> &CognitiveTrace {
        &self.cognitive
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
    let mut service = SysdService::new(SysdConfig::default());
    service.run();
}
