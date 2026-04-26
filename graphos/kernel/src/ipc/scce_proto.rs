// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! SCCE doctrine protocol — native IPC types for the cognitive runtime.
//!
//! This module translates the SCCE 2.0 architecture's data model and
//! message shapes into native `#[repr(C)]` GraphOS IPC payloads. It does
//! NOT port SCCE's implementation (Fastify, PostgreSQL, Node workers).
//! It preserves the *architecture*: provenance-first retrieval, typed
//! evidence handling, graph construction, spectral reasoning, cognitive
//! pipeline, job orchestration, event streaming, and audit posture.
//!
//! ## SCCE concepts preserved
//!
//! - **Provenance chain**: Document → Span → Chunk (three-level provenance)
//! - **Knowledge graph**: Entity → Relation → TensorEdge
//! - **Three-channel retrieval**: Lexical (BM25), Graph (PageRank), Spectral (SVD)
//! - **Cognitive pipeline**: Perceive → Associate → Hypothesize → Verify → Plan
//! - **Inference engine**: 8 strategies (taxonomic, causal, analogical, etc.)
//! - **Evidence synthesis**: Constrained generation with provenance attribution
//! - **Job orchestration**: INDEX, CORRELATE, SPECTRAL, HYDRATE
//! - **Event streaming**: Typed events with sequence numbers and replay
//! - **Audit trail**: Actor + action + detail logging
//! - **Policy classification**: NORMAL, SECRET, QUARANTINED, BINARY_UNPARSEABLE
//!
//! ## SCCE implementation discarded
//!
//! - Fastify HTTP routes → replaced by IPC message dispatch
//! - PostgreSQL tables → replaced by graph arena + typed IPC payloads
//! - Node.js EventEmitter → replaced by IPC event channel
//! - React frontend → replaced by shell3d native consumption
//! - pnpm/npm tooling → irrelevant to native kernel
//!
//! ## Service ownership
//!
//! | Struct family             | Owning service | MsgTag range |
//! |--------------------------|----------------|--------------|
//! | Provenance/Graph queries | graphd         | 0x10–0x1F    |
//! | Cognitive pipeline       | modeld         | 0x20–0x2F    |
//! | Service lifecycle        | servicemgr     | 0x30–0x3F    |
//! | Training/background jobs | trainerd       | 0x40–0x4F    |
//! | Artifact management      | artifactsd     | 0x50–0x5F    |
//! | Diagnostics/audit        | sysd           | 0x60–0x6F    |

use crate::graph::types::{NodeId, Timestamp, Weight};
use crate::ipc::msg::MAX_MSG_BYTES;

// ════════════════════════════════════════════════════════════════════
// § 1. PROVENANCE MODEL — Document / Span / Chunk
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: every piece of retrieved evidence traces back to a
// (document, span, chunk) triple. This is non-negotiable.
//
// In SCCE/TypeScript: DocumentRecord, SpanRecord, ChunkRecord.
// In GraphOS: these become graph nodes with typed edges, but the IPC
// payloads carry compact descriptors for cross-service communication.

/// Document type — what kind of source file was ingested.
///
/// Maps 1:1 to SCCE's DocType enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DocType {
    Pdf = 0,
    Xlsx = 1,
    Docx = 2,
    Pptx = 3,
    Markdown = 4,
    PlainText = 5,
    Html = 6,
    Code = 7,
    Image = 8,
    Unknown = 255,
}

/// Security policy classification for ingested documents.
///
/// Maps 1:1 to SCCE's PolicyClass. Documents classified as SECRET
/// or QUARANTINED are ingested but their content is never exposed
/// through retrieval channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PolicyClass {
    /// Normal document — fully searchable and retrievable.
    Normal = 0,
    /// Secret material (.env, .pem, credentials). Indexed for
    /// provenance but content is redacted in all retrieval results.
    Secret = 1,
    /// Quarantined — flagged by policy but not yet reviewed.
    Quarantined = 2,
    /// Binary file that could not be parsed. Metadata only.
    BinaryUnparseable = 3,
}

/// Compact document descriptor for IPC.
///
/// This is NOT the full document content — it's the metadata that
/// identifies a document in the provenance chain. The actual content
/// lives in graphd's storage arena.
///
/// 64 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DocumentDesc {
    /// Graph node ID for this document (assigned by graphd).
    pub node_id: NodeId,
    /// BLAKE3 hash of the original file content (first 16 bytes).
    pub blob_hash: [u8; 16],
    /// Document type.
    pub doc_type: DocType,
    /// Security policy classification.
    pub policy: PolicyClass,
    /// Padding.
    pub _pad: [u8; 2],
    /// File size in bytes.
    pub size: u32,
    /// Modification time (boot-relative or epoch, depending on VFS state).
    pub mtime: Timestamp,
    /// Path hash (first 8 bytes of BLAKE3 of the path string).
    /// The full path is stored in the graph node's metadata.
    pub path_hash: u64,
    /// Number of spans extracted from this document.
    pub span_count: u16,
    /// Number of chunks derived from this document.
    pub chunk_count: u16,
    /// Reserved.
    pub _reserved: u32,
}

/// Compact span descriptor — a byte range within a document.
///
/// SCCE doctrine: spans are the finest-grained provenance unit.
/// A span identifies exactly where in a source file a piece of
/// evidence came from: page number, sheet name, cell address,
/// code path, byte offset and length.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SpanDesc {
    /// Graph node ID for this span (or 0 if not yet assigned).
    pub node_id: NodeId,
    /// Parent document's graph node ID.
    pub doc_node_id: NodeId,
    /// Byte offset within the document.
    pub byte_start: u32,
    /// Length in bytes.
    pub byte_len: u32,
    /// Page number (0 = N/A, 1-based for PDF/PPTX).
    pub page_num: u16,
    /// Language identifier (0 = natural language, 1+ = code language IDs).
    pub language_id: u8,
    /// Span modality.
    pub modality: ChunkModality,
    /// SimHash64 of normalized text (first 4 bytes for quick dedup).
    pub simhash_prefix: u32,
}

/// Chunk modality — what kind of content this chunk contains.
///
/// Maps to SCCE's chunk modality enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChunkModality {
    Text = 0,
    Table = 1,
    Code = 2,
    Image = 3,
}

/// Compact chunk descriptor — a retrieval unit with provenance.
///
/// Chunks are the unit of retrieval. Each chunk references one or
/// more spans, carries a SimHash for dedup, and a token count
/// estimate for context budget management.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ChunkDesc {
    /// Graph node ID for this chunk.
    pub node_id: NodeId,
    /// Parent document's graph node ID.
    pub doc_node_id: NodeId,
    /// Content modality.
    pub modality: ChunkModality,
    /// Padding.
    pub _pad: [u8; 1],
    /// Estimated token count (for context budget).
    pub token_count_est: u16,
    /// SimHash64 of normalized text (full 8 bytes).
    pub simhash64: u64,
}

// ════════════════════════════════════════════════════════════════════
// § 2. KNOWLEDGE GRAPH SUBSTRATE — Entity / Relation / Mention
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: entities are extracted from mentions, correlated
// via LSH + Hamming distance, promoted to the knowledge graph.
// Relations connect entities with typed, weighted edges.
// TensorEdges add time and context dimensions.

/// Entity descriptor for IPC. Represents a correlated knowledge entity.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EntityDesc {
    /// Graph node ID for this entity.
    pub node_id: NodeId,
    /// Entity type hash (first 4 bytes of BLAKE3 of type string).
    /// Well-known types: 0 = TERM, 1 = PERSON, 2 = ORG, etc.
    pub type_id: u32,
    /// Number of distinct mentions that resolved to this entity.
    pub mention_count: u16,
    /// Number of relations this entity participates in.
    pub relation_count: u16,
    /// Canonical name hash (first 8 bytes of BLAKE3).
    pub name_hash: u64,
}

/// Relation descriptor for IPC. An edge in the knowledge graph.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RelationDesc {
    /// Source entity graph node ID.
    pub src_entity: NodeId,
    /// Destination entity graph node ID.
    pub dst_entity: NodeId,
    /// Relation type hash (first 4 bytes of BLAKE3 of rel_type string).
    pub rel_type_id: u32,
    /// Relation weight (16.16 fixed-point).
    pub weight: Weight,
}

// ════════════════════════════════════════════════════════════════════
// § 3. THREE-CHANNEL RETRIEVAL — SCCE's core retrieval fusion
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: retrieval fuses three independent channels:
// 1. Lexical (BM25) — keyword matching with fallback chain
// 2. Graph expansion — entity→relation traversal with PageRank
// 3. Spectral (SVD) — cosine similarity in projected space
//
// Weights are query-adaptive: factual→boost lexical, explanatory→
// boost graph, conceptual→boost spectral.

/// Task classification for query perception.
///
/// Maps 1:1 to SCCE's TaskType. Drives adaptive retrieval weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskType {
    /// Direct fact lookup — boost lexical channel.
    Factual = 0,
    /// Causal/explanatory reasoning — boost graph channel.
    Analytical = 1,
    /// Code-related query — boost code-specific retrieval.
    Code = 2,
    /// Broad topic exploration — boost spectral channel.
    Exploratory = 3,
}

/// Evidence source channel identifier.
///
/// Maps to SCCE's evidence source tags on EvidenceItem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EvidenceChannel {
    /// Full-text search (BM25 lexical).
    Lexical = 0,
    /// Knowledge graph expansion (PageRank).
    Graph = 1,
    /// Spectral projection (SVD cosine similarity).
    Spectral = 2,
    /// Probabilistic sketch (BloomFilter/HyperLogLog/SimHash).
    Sketch = 3,
}

/// A scored evidence chunk — the output of multi-channel retrieval.
///
/// Each ScoredEvidence carries scores from all three channels plus
/// provenance back to the source chunk/span/document.
///
/// 48 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ScoredEvidence {
    /// Chunk graph node ID (provenance root for this evidence).
    pub chunk_node_id: NodeId,
    /// Document graph node ID (grandparent in provenance chain).
    pub doc_node_id: NodeId,
    /// Lexical (BM25) score — 16.16 fixed-point.
    pub lex_score: Weight,
    /// Graph expansion score — 16.16 fixed-point.
    pub graph_score: Weight,
    /// Spectral similarity score — 16.16 fixed-point.
    pub spec_score: Weight,
    /// Fused total score — 16.16 fixed-point.
    pub total_score: Weight,
    /// Which channel contributed the strongest signal.
    pub primary_channel: EvidenceChannel,
    /// Content modality of the chunk.
    pub modality: ChunkModality,
    /// Number of retrieval channels that found this chunk (1–3).
    pub channel_count: u8,
    /// Padding.
    pub _pad: u8,
    /// Cost in microseconds to retrieve this evidence.
    pub cost_us: u32,
}

/// Retrieval request — sent from modeld to graphd/retrieval subsystem.
///
/// 48 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RetrievalRequest {
    /// Perceived task type (drives adaptive channel weights).
    pub task_type: TaskType,
    /// Maximum results to return.
    pub max_results: u8,
    /// Maximum depth for graph expansion.
    pub max_graph_depth: u8,
    /// Padding.
    pub _pad: u8,
    /// Minimum fused score threshold (16.16 fixed-point).
    pub min_score: Weight,
    /// Lexical channel weight override (16.16, 0 = use adaptive default).
    pub lex_weight: Weight,
    /// Graph channel weight override (16.16, 0 = use adaptive default).
    pub graph_weight: Weight,
    /// Spectral channel weight override (16.16, 0 = use adaptive default).
    pub spec_weight: Weight,
    /// Query text hash (first 8 bytes of BLAKE3).
    pub query_hash: u64,
    /// Conversation context node ID (0 = none).
    pub conversation_node: NodeId,
}

// ════════════════════════════════════════════════════════════════════
// § 4. COGNITIVE PIPELINE — The Thinker
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: the cognitive pipeline is NOT an LLM. It is a
// deterministic multi-stage reasoning process:
// Perceive → Associate → Hypothesize → Verify → Plan (recovery)
//
// The think loop runs max 3 rounds. Each round can trigger
// inference-guided retrieval for additional evidence.

/// Verification status for a hypothesis atom.
///
/// Maps 1:1 to SCCE's VerificationStatus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VerificationStatus {
    /// Supported by ≥2 independent channels or score ≥ 0.3.
    Supported = 0,
    /// Evidence exists but weak (single channel, low score).
    Weak = 1,
    /// Evidence contradicts the claim (negation/antonym detected).
    Contradicted = 2,
}

/// Recovery strategy for the planning stage.
///
/// Maps 1:1 to SCCE's PlanStep.strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecoveryStrategy {
    /// Re-query with synonyms/alternate terms.
    SynonymExpansion = 0,
    /// Follow more hops from known entities.
    GraphDeepening = 1,
    /// Increase spectral projection k or lower threshold.
    SpectralWidening = 2,
    /// Use discovered entities as new query seeds.
    EntityPivot = 3,
    /// Decompose compound question into sub-queries.
    SubQuery = 4,
}

/// Inference strategy — one of SCCE's 8 reasoning strategies.
///
/// Each strategy operates over the pre-loaded entity-relation graph
/// and produces DerivedFact entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InferenceStrategy {
    /// IS-A / HAS-A hierarchy reasoning.
    Taxonomic = 0,
    /// CAUSES / PREVENTS chain following.
    Causal = 1,
    /// Structural similarity across domains.
    Analogical = 2,
    /// Combining partial facts into composite claims.
    Compositional = 3,
    /// Thesis/antithesis/synthesis reasoning.
    Dialectical = 4,
    /// Time/location-aware reasoning.
    SpatioTemporal = 5,
    /// Provenance-chain reasoning (tracing evidence origins).
    Provenance = 6,
    /// Second-order reasoning across first-order strategy outputs.
    CrossStrategy = 7,
}

/// Cognitive pipeline phase — tracks where we are in the think loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CognitivePhase {
    /// Initial perception: normalize query, extract entities, classify task.
    Perception = 0,
    /// Gather evidence from all retrieval channels.
    Association = 1,
    /// Cluster evidence into hypothesis atoms.
    Hypothesis = 2,
    /// Cross-channel verification of each hypothesis atom.
    Verification = 3,
    /// Recovery planning when evidence is insufficient.
    Planning = 4,
    /// Multi-strategy inference over entity-relation graph.
    Inference = 5,
    /// Inference-guided retrieval (sub-queries from discovered entities).
    InferenceRetrieval = 6,
    /// Evidence synthesis (constrained generation).
    Synthesis = 7,
    /// Self-evaluation of synthesis quality.
    SelfEvaluation = 8,
    /// Final provenance verification.
    ProvenanceCheck = 9,
}

/// Cognitive pipeline status message.
///
/// Sent from modeld to sysd (and optionally shell3d) to report
/// progress through the think loop. Enables live debugging of
/// the reasoning process.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CognitiveStatus {
    /// Current phase in the pipeline.
    pub phase: CognitivePhase,
    /// Current think-loop round (0–2).
    pub round: u8,
    /// Number of evidence items gathered so far.
    pub evidence_count: u8,
    /// Number of hypothesis atoms formed.
    pub hypothesis_count: u8,
    /// Number of SUPPORTED atoms.
    pub supported_count: u8,
    /// Number of WEAK atoms.
    pub weak_count: u8,
    /// Number of CONTRADICTED atoms.
    pub contradicted_count: u8,
    /// Number of recovery steps planned.
    pub plan_step_count: u8,
    /// Inference strategies activated (bitmask, bit i = strategy i).
    pub inference_mask: u8,
    /// Number of entities discovered by inference.
    pub discovered_entities: u8,
    /// Padding.
    pub _pad: [u8; 2],
    /// Elapsed time in microseconds since pipeline start.
    pub elapsed_us: u32,
    /// Query hash (matches RetrievalRequest.query_hash).
    pub query_hash: u64,
}

// ════════════════════════════════════════════════════════════════════
// § 5. EVENT STREAMING — typed event bus for runtime state
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: all runtime events are typed, sequenced, and
// replayable. The event bus carries: document ingestion events,
// correlation events, spectral refresh events, cognitive pipeline
// progress, synthesis completion, and error conditions.

/// Event type — what kind of runtime event occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    // ── Ingestion events ──
    /// A document was ingested.
    DocumentIngested = 0,
    /// A document was classified as SECRET/QUARANTINED.
    DocumentRestricted = 1,
    /// Duplicate document detected (SimHash match).
    DuplicateDetected = 2,

    // ── Correlation events ──
    /// Entity extraction completed for a document.
    CorrelationComplete = 10,
    /// New entity created in the knowledge graph.
    EntityCreated = 11,
    /// New relation created between entities.
    RelationCreated = 12,

    // ── Spectral events ──
    /// Spectral decomposition refreshed.
    SpectralRefreshed = 20,
    /// Spectral drift detected above threshold.
    SpectralDrift = 21,

    // ── Cognitive pipeline events ──
    /// Cognitive pipeline started for a query.
    CognitivePipelineStart = 30,
    /// Think-loop round completed.
    ThinkRoundComplete = 31,
    /// Synthesis completed.
    SynthesisComplete = 32,
    /// Cognitive pipeline completed (full answer ready).
    CognitivePipelineDone = 33,

    // ── Job events ──
    /// Background job started.
    JobStarted = 40,
    /// Background job completed.
    JobCompleted = 41,
    /// Background job failed.
    JobFailed = 42,
    /// Background job progress update.
    JobProgress = 43,

    // ── System events ──
    /// Service registered with servicemgr.
    ServiceRegistered = 50,
    /// Service health check failed.
    ServiceUnhealthy = 51,
    /// Audit entry recorded.
    AuditEntry = 60,
    /// System error.
    SystemError = 70,
}

/// A runtime event message.
///
/// Sent from any service to sysd (primary event sink) and optionally
/// to shell3d (for live UI updates). Events are sequenced per-service
/// and globally ordered by timestamp for replay.
///
/// 48 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RuntimeEvent {
    /// What kind of event.
    pub event_type: EventType,
    /// Padding.
    pub _pad: [u8; 3],
    /// Monotonic sequence number (per-service).
    pub seq: u32,
    /// Boot-relative timestamp.
    pub timestamp: Timestamp,
    /// Related graph node ID (document, entity, job, etc.), or 0.
    pub related_node: NodeId,
    /// Secondary related node (e.g., dst_entity for relation events), or 0.
    pub related_node2: NodeId,
    /// Numeric payload (interpretation depends on event_type).
    /// Examples: progress percentage, error code, entity count.
    pub payload_u32: u32,
    /// Weight/score payload (16.16 fixed-point).
    pub payload_weight: Weight,
}

// ════════════════════════════════════════════════════════════════════
// § 6. JOB ORCHESTRATION — background work scheduling
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: long-running work (indexing, entity correlation,
// spectral refresh, corpus hydration) runs as background jobs.
// Jobs are claimed atomically, tracked with progress, and produce
// audit-trail entries on completion/failure.

/// Background job type.
///
/// Maps to SCCE's JobType enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JobType {
    /// Document indexing (chunking, span extraction, dedup).
    Index = 0,
    /// Entity correlation (mention→entity resolution, relation promotion).
    Correlate = 1,
    /// Spectral decomposition refresh (Lanczos SVD).
    Spectral = 2,
    /// Corpus hydration (Wikipedia or other bulk import).
    Hydrate = 3,
    /// Artifact build (webapp or bundle generation).
    ArtifactBuild = 4,
}

/// Background job status.
///
/// Maps to SCCE's JobStatus enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JobStatus {
    Pending = 0,
    Running = 1,
    Paused = 2,
    Completed = 3,
    Failed = 4,
}

/// Job submission request — sent to trainerd.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JobRequest {
    /// What kind of background job.
    pub job_type: JobType,
    /// Requested priority (0 = normal, 1 = high, 2 = low).
    pub priority: u8,
    /// Padding.
    pub _pad: [u8; 2],
    /// Target node ID (e.g., document to index, or 0 for bulk jobs).
    pub target_node: NodeId,
    /// Timestamp of request.
    pub submitted_at: Timestamp,
    /// Requester's task/service node ID.
    pub requester: NodeId,
}

/// Job status report — sent from trainerd to requester and sysd.
///
/// 48 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JobReport {
    /// Job type.
    pub job_type: JobType,
    /// Current status.
    pub status: JobStatus,
    /// Progress (0–100).
    pub progress: u8,
    /// Padding.
    pub _pad: u8,
    /// Job sequence ID (assigned by trainerd).
    pub job_seq: u32,
    /// Target node ID (from the original request).
    pub target_node: NodeId,
    /// Start timestamp.
    pub started_at: Timestamp,
    /// Completion timestamp (0 if still running).
    pub completed_at: Timestamp,
    /// Cost in milliseconds (0 if still running).
    pub cost_ms: u32,
    /// Verifier score on completion (16.16 fixed-point, 0 if N/A).
    pub verifier_score: Weight,
}

/// Training subsystem control request.
///
/// Maps to SCCE's TrainingControlRequest. Allows starting, pausing,
/// or resuming individual training subsystems.
///
/// 16 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrainingControl {
    /// Which subsystem to control.
    pub target: JobType,
    /// Action to perform.
    pub action: TrainingAction,
    /// CPU budget preset.
    pub cpu_budget: ResourceBudget,
    /// IO budget preset.
    pub io_budget: ResourceBudget,
    /// Schedule mode.
    pub schedule: ScheduleMode,
    /// Padding.
    pub _pad: [u8; 3],
    /// Timestamp.
    pub timestamp: Timestamp,
}

/// Training control action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrainingAction {
    Start = 0,
    Pause = 1,
    Resume = 2,
}

/// Resource budget level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResourceBudget {
    Low = 0,
    Medium = 1,
    High = 2,
}

/// Schedule mode for background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ScheduleMode {
    /// Run only when system is idle.
    Idle = 0,
    /// Run continuously.
    Always = 1,
}

// ════════════════════════════════════════════════════════════════════
// § 7. ARTIFACT MANAGEMENT
// ════════════════════════════════════════════════════════════════════

/// Artifact type.
///
/// Maps to SCCE's ArtifactRecord.artifact_type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ArtifactType {
    /// Structured answer with provenance (JSON-equivalent).
    AnswerBundle = 0,
    /// Long-form generated document.
    LongformDoc = 1,
    /// Verification/audit report.
    VerificationReport = 2,
    /// Full system bundle (exportable snapshot).
    SystemBundle = 3,
}

/// Artifact descriptor.
///
/// 48 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArtifactDesc {
    /// Graph node ID for this artifact.
    pub node_id: NodeId,
    /// Artifact type.
    pub artifact_type: ArtifactType,
    /// Padding.
    pub _pad: [u8; 3],
    /// Size in bytes.
    pub size: u32,
    /// BLAKE3 hash of the artifact content (first 16 bytes).
    pub content_hash: [u8; 16],
    /// Creation timestamp.
    pub created_at: Timestamp,
}

// ════════════════════════════════════════════════════════════════════
// § 8. DIAGNOSTICS AND AUDIT
// ════════════════════════════════════════════════════════════════════
//
// SCCE doctrine: every significant action is auditable. The audit
// trail records actor, action, and structured details.

/// Audit event — immutable record of a system action.
///
/// 48 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AuditEntry {
    /// Timestamp of the action.
    pub timestamp: Timestamp,
    /// Actor node ID (task, service, or kernel).
    pub actor: NodeId,
    /// Action type.
    pub action: AuditAction,
    /// Padding.
    pub _pad: [u8; 3],
    /// Target node ID (what was acted upon).
    pub target: NodeId,
    /// Outcome.
    pub outcome: AuditOutcome,
    /// Padding.
    pub _pad2: [u8; 3],
    /// Numeric detail (interpretation depends on action).
    pub detail_u32: u32,
    /// Weight detail (16.16 fixed-point).
    pub detail_weight: Weight,
}

/// Audit action classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AuditAction {
    /// Document was ingested.
    Ingest = 0,
    /// Entity was created or modified.
    EntityMutation = 1,
    /// Query was executed.
    Query = 2,
    /// Inference was performed.
    Inference = 3,
    /// Tool was invoked.
    ToolInvocation = 4,
    /// Job was submitted.
    JobSubmit = 5,
    /// Job completed.
    JobComplete = 6,
    /// Configuration was changed.
    ConfigChange = 7,
    /// Service registered or deregistered.
    ServiceLifecycle = 8,
    /// Policy classification was applied.
    PolicyApplied = 9,
    /// Secret was redacted.
    SecretRedacted = 10,
    /// Artifact was created.
    ArtifactCreated = 11,
}

/// Audit outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AuditOutcome {
    Success = 0,
    Failure = 1,
    Denied = 2,
    Partial = 3,
}

/// System diagnostic status.
///
/// Sent from sysd to shell3d (and logged internally).
///
/// 64 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DiagnosticStatus {
    /// Boot-relative timestamp.
    pub timestamp: Timestamp,
    /// Total documents ingested.
    pub doc_count: u32,
    /// Total entities in knowledge graph.
    pub entity_count: u32,
    /// Total relations in knowledge graph.
    pub relation_count: u32,
    /// Total chunks available for retrieval.
    pub chunk_count: u32,
    /// Spectral model dimension k (0 = no model).
    pub spectral_k: u16,
    /// Index subsystem status.
    pub index_status: JobStatus,
    /// Correlate subsystem status.
    pub correlate_status: JobStatus,
    /// Spectral subsystem status.
    pub spectral_status: JobStatus,
    /// Padding.
    pub _pad: [u8; 1],
    /// Last spectral refresh timestamp (0 = never).
    pub spectral_last_refresh: Timestamp,
    /// Average verifier score across recent jobs (16.16).
    pub avg_verifier_score: Weight,
    /// Audit entries recorded since boot.
    pub audit_count: u32,
}

/// Training status view — aggregate training state for shell3d.
///
/// Maps to SCCE's TrainingStatusView.
///
/// 32 bytes, `#[repr(C)]`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrainingStatus {
    /// Documents processed by ingestion pipeline.
    pub docs_processed: u32,
    /// Entities in knowledge graph.
    pub entity_count: u32,
    /// Relations in knowledge graph.
    pub relation_count: u32,
    /// Spectral model dimension k.
    pub spectral_k: u16,
    /// Padding.
    pub _pad: u16,
    /// Planner template count.
    pub planner_templates: u16,
    /// Average planner success rate (0–100).
    pub planner_success_pct: u8,
    /// Average provenance coverage (0–100).
    pub provenance_coverage_pct: u8,
    /// Last spectral refresh timestamp.
    pub spectral_last_refresh: Timestamp,
}

// ════════════════════════════════════════════════════════════════════
// § 9. COMPILE-TIME SIZE ASSERTIONS
// ════════════════════════════════════════════════════════════════════
//
// Every IPC-carried struct must fit within MAX_MSG_BYTES (256 bytes).

const _: () = {
    assert!(core::mem::size_of::<DocumentDesc>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<SpanDesc>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ChunkDesc>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<EntityDesc>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<RelationDesc>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ScoredEvidence>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<RetrievalRequest>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<CognitiveStatus>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<RuntimeEvent>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<JobRequest>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<JobReport>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<TrainingControl>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<ArtifactDesc>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<AuditEntry>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<DiagnosticStatus>() <= MAX_MSG_BYTES);
    assert!(core::mem::size_of::<TrainingStatus>() <= MAX_MSG_BYTES);
};

// ════════════════════════════════════════════════════════════════════
// § 10. BYTE SERIALIZATION HELPERS
// ════════════════════════════════════════════════════════════════════

macro_rules! impl_ipc_bytes {
    ($ty:ty) => {
        impl $ty {
            /// Interpret self as a byte slice for IPC payload.
            pub fn as_bytes(&self) -> &[u8] {
                unsafe {
                    core::slice::from_raw_parts(
                        self as *const Self as *const u8,
                        core::mem::size_of::<Self>(),
                    )
                }
            }

            /// Interpret a byte slice as this type. Returns None if too small.
            pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
                if bytes.len() < core::mem::size_of::<Self>() {
                    return None;
                }
                Some(unsafe { &*(bytes.as_ptr() as *const Self) })
            }
        }
    };
}

impl_ipc_bytes!(DocumentDesc);
impl_ipc_bytes!(SpanDesc);
impl_ipc_bytes!(ChunkDesc);
impl_ipc_bytes!(EntityDesc);
impl_ipc_bytes!(RelationDesc);
impl_ipc_bytes!(ScoredEvidence);
impl_ipc_bytes!(RetrievalRequest);
impl_ipc_bytes!(CognitiveStatus);
impl_ipc_bytes!(RuntimeEvent);
impl_ipc_bytes!(JobRequest);
impl_ipc_bytes!(JobReport);
impl_ipc_bytes!(TrainingControl);
impl_ipc_bytes!(ArtifactDesc);
impl_ipc_bytes!(AuditEntry);
impl_ipc_bytes!(DiagnosticStatus);
impl_ipc_bytes!(TrainingStatus);
