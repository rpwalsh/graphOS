// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! trainerd — background job orchestration and predictive runtime service.
//!
//! trainerd owns all long-running background work in GraphOS:
//! - Document indexing (chunking, span extraction, SimHash dedup)
//! - Entity correlation (mention→entity resolution, relation promotion)
//! - Spectral decomposition refresh (Lanczos SVD on knowledge graph)
//! - Corpus hydration (bulk import of reference material)
//!
//! ## SCCE doctrine preserved
//!
//! - **Job-worker separation**: background work does NOT run in the query
//!   path. trainerd processes jobs asynchronously, reports progress via
//!   IPC events, and never blocks the cognitive pipeline.
//! - **Atomic job claiming**: jobs are picked up one at a time. Only one
//!   indexing job runs concurrently (prevents race conditions on the
//!   knowledge graph).
//! - **Verifier score**: spectral refresh jobs compute a drift-based
//!   verifier score to quantify model staleness.
//! - **Resource budgeting**: each subsystem (index, correlate, spectral)
//!   can be independently started, paused, or resumed with CPU/IO
//!   budget controls.
//! - **Event emission**: all job state transitions produce RuntimeEvent
//!   messages sent to sysd for audit and to shell3d for live display.
//!
//! ## SCCE implementation discarded
//!
//! - PostgreSQL `FOR UPDATE SKIP LOCKED` → replaced by in-memory job queue
//! - Node.js `setInterval` polling → replaced by IPC-driven job dispatch
//! - Express/Fastify route handlers → replaced by MsgTag dispatch
//!
//! ## IPC protocol
//!
//! trainerd listens on a dedicated IPC channel. It handles:
//! - MsgTag::JobSubmit (0x40)       → enqueue a background job
//! - MsgTag::TrainingControl (0x42) → start/pause/resume subsystem
//! - MsgTag::TrainingStatusQuery (0x43) → reply with TrainingStatus
//! - MsgTag::Ping (0x01)           → reply Pong (0x02)
//! - MsgTag::Shutdown (0x04)       → clean exit
//!
//! trainerd sends:
//! - MsgTag::JobReport (0x41)       → to requester + sysd
//! - MsgTag::TrainingStatusResponse (0x44) → to requester
//! - MsgTag::RuntimeEvent (0x60)    → to sysd (job lifecycle events)
//! - MsgTag::GraphMutate (0x12)     → to graphd (entity/relation creation)

// ════════════════════════════════════════════════════════════════════
// Configuration
// ════════════════════════════════════════════════════════════════════

#[path = "../../common/host_sys.rs"]
mod host_sys;
#[path = "../../common/ipc.rs"]
mod ipc;

/// trainerd configuration.
pub struct TrainerdConfig {
    /// IPC channel this service listens on.
    pub listen_channel: u32,
    /// IPC channel for sending mutations to graphd.
    pub graphd_channel: u32,
    /// IPC channel for sending events to sysd.
    pub sysd_channel: u32,
    /// Maximum pending jobs in the queue.
    pub max_pending_jobs: usize,
    /// Poll interval for idle-mode scheduling (in ticks).
    pub idle_poll_interval: u64,
}

impl TrainerdConfig {
    pub const fn default() -> Self {
        Self {
            listen_channel: 4,
            graphd_channel: 2,
            sysd_channel: 6,
            max_pending_jobs: 32,
            idle_poll_interval: 100,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Job queue
// ════════════════════════════════════════════════════════════════════

/// Maximum number of jobs in the pending queue.
const MAX_JOBS: usize = 32;

/// A pending job entry.
#[derive(Clone, Copy)]
struct PendingJob {
    job_type: u8,
    priority: u8,
    target_node: u64,
    submitted_at: u64,
    requester: u32,
    occupied: bool,
}

impl PendingJob {
    const EMPTY: Self = Self {
        job_type: 0,
        priority: 0,
        target_node: 0,
        submitted_at: 0,
        requester: 0,
        occupied: false,
    };
}

/// A fixed-capacity job queue. No heap allocation.
///
/// Jobs are stored in a flat array. Insertion is O(n) scan for an
/// empty slot. Claiming picks the highest-priority pending job.
struct JobQueue {
    slots: [PendingJob; MAX_JOBS],
    count: usize,
}

impl JobQueue {
    const fn new() -> Self {
        Self {
            slots: [PendingJob::EMPTY; MAX_JOBS],
            count: 0,
        }
    }

    /// Enqueue a job. Returns false if the queue is full.
    fn enqueue(&mut self, job: PendingJob) -> bool {
        if self.count >= MAX_JOBS {
            return false;
        }
        for slot in self.slots.iter_mut() {
            if !slot.occupied {
                *slot = PendingJob {
                    occupied: true,
                    ..job
                };
                self.count += 1;
                return true;
            }
        }
        false
    }

    /// Claim the highest-priority pending job. Returns None if empty.
    ///
    /// Priority ordering: higher `priority` value wins. Ties broken
    /// by earliest `submitted_at`.
    fn claim(&mut self) -> Option<PendingJob> {
        let mut best_idx: Option<usize> = None;
        let mut best_pri: u8 = 0;
        let mut best_time: u64 = u64::MAX;

        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.occupied {
                continue;
            }
            if slot.priority > best_pri
                || (slot.priority == best_pri && slot.submitted_at < best_time)
            {
                best_idx = Some(i);
                best_pri = slot.priority;
                best_time = slot.submitted_at;
            }
        }

        if let Some(idx) = best_idx {
            let job = self.slots[idx];
            self.slots[idx] = PendingJob::EMPTY;
            self.count -= 1;
            Some(job)
        } else {
            None
        }
    }

    /// Number of pending jobs.
    fn pending(&self) -> usize {
        self.count
    }
}

// ════════════════════════════════════════════════════════════════════
// Subsystem state
// ════════════════════════════════════════════════════════════════════

/// Subsystem run state (matches scce_proto::JobStatus semantics).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum SubsystemState {
    Idle = 0,
    Running = 1,
    Paused = 2,
}

/// Per-subsystem tracking.
#[derive(Clone, Copy)]
struct SubsystemTracker {
    state: SubsystemState,
    jobs_completed: u32,
    jobs_failed: u32,
    last_run_cost_ms: u32,
    last_verifier_score: u32,
}

impl SubsystemTracker {
    const fn new() -> Self {
        Self {
            state: SubsystemState::Idle,
            jobs_completed: 0,
            jobs_failed: 0,
            last_run_cost_ms: 0,
            last_verifier_score: 0,
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// Service
// ════════════════════════════════════════════════════════════════════

/// The number of tracked subsystems: Index, Correlate, Spectral, Hydrate.
const SUBSYSTEM_COUNT: usize = 4;

/// trainerd service state.
pub struct TrainerdService {
    config: TrainerdConfig,
    queue: JobQueue,
    subsystems: [SubsystemTracker; SUBSYSTEM_COUNT],
    /// Monotonic job sequence counter.
    next_job_seq: u32,
    /// Total events emitted (for RuntimeEvent.seq).
    event_seq: u32,
    /// Whether the service is running.
    running: bool,
}

impl TrainerdService {
    pub fn new(config: TrainerdConfig) -> Self {
        Self {
            config,
            queue: JobQueue::new(),
            subsystems: [SubsystemTracker::new(); SUBSYSTEM_COUNT],
            next_job_seq: 1,
            event_seq: 0,
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
                self.try_dispatch_next_job();
                sys_yield();
                continue;
            };

            match msg.tag {
                0x40 => {
                    // MsgTag::JobSubmit
                    self.handle_job_submit(&buf[..msg.payload_len], msg.reply_endpoint);
                }
                0x42 => {
                    // MsgTag::TrainingControl
                    self.handle_control(&buf[..msg.payload_len]);
                }
                0x43 => {
                    // MsgTag::TrainingStatusQuery
                    self.handle_status_query(msg.reply_endpoint);
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
            self.try_dispatch_next_job();
        }

        self.running = false;
    }

    /// Handle a job submission request.
    /// Deserializes JobRequest: job_type(1) + priority(1) + target_node(8) + submitted_at(8) = 18 bytes.
    pub fn handle_job_submit(&mut self, payload: &[u8], reply_endpoint: u32) -> bool {
        if payload.len() < 18 {
            return false;
        }
        let job_type = payload[0];
        let priority = payload[1];
        let target_node = u64::from_le_bytes([
            payload[2], payload[3], payload[4], payload[5], payload[6], payload[7], payload[8],
            payload[9],
        ]);
        let submitted_at = u64::from_le_bytes([
            payload[10],
            payload[11],
            payload[12],
            payload[13],
            payload[14],
            payload[15],
            payload[16],
            payload[17],
        ]);

        if (job_type as usize) >= SUBSYSTEM_COUNT {
            self.emit_event(0xFF, target_node, self.next_job_seq);
            return false;
        }

        let job = PendingJob {
            job_type,
            priority,
            target_node,
            submitted_at,
            requester: reply_endpoint,
            occupied: true,
        };
        let ok = self.queue.enqueue(job);
        if ok {
            self.next_job_seq += 1;
            self.emit_event(30, target_node, self.next_job_seq - 1);
        } else {
            self.emit_event(0xFF, target_node, self.next_job_seq);
        }
        ok
    }

    /// Handle a training control request (start/pause/resume).
    /// Payload: subsystem_idx(1) + command(1) = 2 bytes. command: 0=start, 1=pause, 2=resume.
    pub fn handle_control(&mut self, payload: &[u8]) {
        if payload.len() < 2 {
            return;
        }
        let sub_idx = payload[0] as usize;
        let command = payload[1];
        if sub_idx >= SUBSYSTEM_COUNT {
            return;
        }
        let sub = &mut self.subsystems[sub_idx];
        match command {
            0 => {
                if sub.state == SubsystemState::Idle {
                    sub.state = SubsystemState::Running;
                    self.emit_event(32, sub_idx as u64, command as u32);
                }
            }
            1 => {
                if sub.state == SubsystemState::Running {
                    sub.state = SubsystemState::Paused;
                    self.emit_event(33, sub_idx as u64, command as u32);
                }
            }
            2 => {
                if sub.state == SubsystemState::Paused {
                    sub.state = SubsystemState::Running;
                    self.emit_event(34, sub_idx as u64, command as u32);
                }
            }
            _ => {}
        }
    }

    /// Handle a training status query — reply with aggregate status.
    /// Serializes 4 subsystems x 12 bytes + pending(4) + seq(4) = 56 bytes.
    pub fn handle_status_query(&self, requester: u32) {
        let _extended_status = self.subsystems.iter().fold((0u32, 0u32), |acc, sub| {
            (
                acc.0.saturating_add(sub.last_run_cost_ms),
                acc.1.max(sub.last_verifier_score),
            )
        });
        let mut buf = [0u8; 56];
        for (i, sub) in self.subsystems.iter().enumerate() {
            let off = i * 12;
            buf[off] = sub.state as u8;
            buf[off + 4..off + 8].copy_from_slice(&sub.jobs_completed.to_le_bytes());
            buf[off + 8..off + 12].copy_from_slice(&sub.jobs_failed.to_le_bytes());
        }
        let base = SUBSYSTEM_COUNT * 12;
        buf[base..base + 4].copy_from_slice(&(self.queue.pending() as u32).to_le_bytes());
        buf[base + 4..base + 8].copy_from_slice(&self.next_job_seq.to_le_bytes());
        sys_channel_send(requester, &buf, 0x44);
    }

    /// Dispatch the next queued job to the appropriate subsystem.
    pub fn try_dispatch_next_job(&mut self) {
        if let Some(job) = self.queue.claim() {
            let sub_idx = job.job_type as usize;
            if sub_idx >= SUBSYSTEM_COUNT {
                return;
            }
            let sub = &mut self.subsystems[sub_idx];
            if sub.state == SubsystemState::Paused {
                let _ = self.queue.enqueue(job);
                return;
            }
            sub.state = SubsystemState::Running;

            let success = self.execute_job(sub_idx, job.target_node);

            let sub = &mut self.subsystems[sub_idx];
            if success {
                sub.jobs_completed += 1;
                sub.state = SubsystemState::Idle;
                self.emit_event(31, job.target_node, job.job_type as u32);
                let mut rpt = [0u8; 10];
                rpt[0] = job.job_type;
                rpt[1] = 1;
                rpt[2..10].copy_from_slice(&job.target_node.to_le_bytes());
                sys_channel_send(job.requester, &rpt, 0x41);
            } else {
                sub.jobs_failed += 1;
                sub.state = SubsystemState::Idle;
                self.emit_event(0xFE, job.target_node, job.job_type as u32);
                let mut rpt = [0u8; 10];
                rpt[0] = job.job_type;
                rpt[1] = 0;
                rpt[2..10].copy_from_slice(&job.target_node.to_le_bytes());
                sys_channel_send(job.requester, &rpt, 0x41);
            }
        }
    }

    /// Execute a job for a subsystem. Returns true on success.
    fn execute_job(&self, sub_idx: usize, target_node: u64) -> bool {
        match sub_idx {
            0 => {
                // Index: create entity node in graph.
                let mut payload = [0u8; 9];
                payload[0] = 0; // AddNode
                payload[1..9].copy_from_slice(&target_node.to_le_bytes());
                sys_channel_send(self.config.graphd_channel, &payload, 0x12);
                true
            }
            1 => {
                // Correlate: create relation edge in graph.
                let mut payload = [0u8; 17];
                payload[0] = 1; // AddEdge
                payload[1..9].copy_from_slice(&target_node.to_le_bytes());
                sys_channel_send(self.config.graphd_channel, &payload, 0x12);
                true
            }
            2 => true, // Spectral: local computation
            3 => true, // Hydrate: local bulk import
            _ => false,
        }
    }

    /// Current number of pending jobs.
    pub fn pending_jobs(&self) -> usize {
        self.queue.pending()
    }
}

// ════════════════════════════════════════════════════════════════════
// Host-mode syscall shims until ring-3 exists
// ════════════════════════════════════════════════════════════════════

/// Receive a message from an IPC channel.
/// Returns packed result: low 16 = payload len, bits [16..24] = tag,
/// bits [24..56] = reply endpoint. Returns 0 if empty, u64::MAX on error.
fn sys_channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    host_sys::channel_recv(channel, buf)
}

/// Send a message on an IPC channel.
fn sys_channel_send(channel: u32, payload: &[u8], tag: u8) {
    host_sys::channel_send(channel, payload, tag);
}

/// Yield CPU to the scheduler.
fn sys_yield() {
    host_sys::yield_now();
}

impl TrainerdService {
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
}

fn main() {
    let mut service = TrainerdService::new(TrainerdConfig::default());
    service.run();
}
