// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graph type definitions — nodes and edges for the kernel object graph.
//!
//! The kernel graph is GraphOS's central abstraction. Every schedulable
//! entity, memory region, device, service, and trust boundary is a node.
//! Relationships between them are typed, directed edges. Both carry
//! creation timestamps and creator identity for provenance.
//!
//! ## Mathematical foundation
//!
//! The graph implements a **heterogeneous temporal graph**:
//!
//!   H = (V, E, τ_V, τ_E, t)
//!
//! where τ_V : V → NodeKind, τ_E : E → EdgeKind, and t : E → Timestamp
//! assigns continuous-time creation events to every edge.
//!
//! This is the native substrate for the **unified temporal operator**:
//!
//!   S_{ττ'}(h, Δt) = exp(−λ_{ττ'} · Δt) · W_{ττ'} · h
//!
//! which the PowerWalk Duality Theorem proves equivalent across random-walk
//! sampling and TGNN message passing. The per-type-pair decay λ_{ττ'} is
//! stored in the `TypePairParams` matrix (see `temporal.rs`).
//!
//! The edge `weight` field supports:
//! - Laplacian construction: L = D − A, 𝓛 = I − D^{−1/2} A D^{−1/2}
//! - Spectral analysis: Fiedler value λ₂, spectral gaps γ_k = λ_{k+1} − λ_k
//! - Cheeger inequality: λ₂/2 ≤ h(G) ≤ √(2λ₂)
//! - Weyl perturbation bounds: |λ_k(L+ΔL) − λ_k(L)| ≤ ‖ΔL‖₂
//! - Temporal walk transition probabilities with recency decay
//!
//! ## Design constraints
//! - All types are `#[repr(C)]` and `Copy` so they can live in static arrays
//!   with no heap.
//! - `NodeId` and `EdgeId` are 64-bit, monotonically increasing, never reused.
//!   This is a correctness invariant: a stale ID can never accidentally alias
//!   a new object.
//! - Timestamps are boot-relative ticks (no wall clock yet). Resolution
//!   improves when a real timer source (HPET/TSC) is calibrated.
//! - Weights are fixed-point u32 with 16.16 format (see `Weight`).
//! - `creator` is a `NodeId` — the node that caused this mutation. During
//!   early boot this is `NODE_ID_KERNEL` (1). Once tasks exist, it is
//!   the calling task's graph node.

// ────────────────────────────────────────────────────────────────────
// Scalar types
// ────────────────────────────────────────────────────────────────────

/// Unique identity for a graph node. Monotonically increasing, never reused.
pub type NodeId = u64;

/// Unique identity for a graph edge. Monotonically increasing, never reused.
pub type EdgeId = u64;

/// Boot-relative timestamp in abstract ticks.
/// Before timer calibration, this is the mutation sequence number.
/// Resolution: initially 1 tick = 1 mutation; after HPET/TSC calibration
/// 1 tick = 1 nanosecond. The continuous-time requirement from the
/// heterogeneous temporal graph formalism H = (V, E, τ_V, τ_E, t) is
/// satisfied once calibrated.
pub type Timestamp = u64;

/// Fixed-point edge weight: 16.16 unsigned format.
///
/// The integer part occupies the upper 16 bits, the fractional part the
/// lower 16 bits. `WEIGHT_ONE` represents 1.0. This supports:
/// - Laplacian degree accumulation: d(v) = Σ w(v,u)
/// - Transition probability: P(u→v) ∝ w(u,v) · exp(−λ·Δt)
/// - Spectral eigenvalue computation (integer arithmetic, no FPU required)
///
/// Range: [0, 65535.99998] with resolution ≈ 1.5×10⁻⁵.
pub type Weight = u32;

/// Weight value representing 1.0 in 16.16 fixed-point.
pub const WEIGHT_ONE: Weight = 1 << 16;

/// Weight value representing 0.0 (absence of connection — but for live
/// edges this should normally be non-zero).
pub const WEIGHT_ZERO: Weight = 0;

/// Well-known node ID for the kernel itself. Always node 1.
pub const NODE_ID_KERNEL: NodeId = 1;

// ────────────────────────────────────────────────────────────────────
// Node kind
// ────────────────────────────────────────────────────────────────────

/// Maximum number of distinct node kinds. Used to size type-pair matrices.
pub const NODE_KIND_COUNT: usize = 38;

/// Kernel-level node kinds.
///
/// Each variant corresponds to a class of system object that participates
/// in the graph. The type function τ_V : V → NodeKind is central to the
/// heterogeneous temporal graph formalism.
///
/// Variants are `repr(u16)` for compact storage and stable ABI across
/// serialization boundaries.
///
/// ## Expressiveness hierarchy (from TGNN proof)
///   1-WL ⊊ TGN ⊊ per-type-TGNN ⊊ 2-WL
///
/// Per-type distinction is what lifts the system above the Temporal Score
/// Collapse blind spot of homogeneous TGN. Every NodeKind matters.
///
/// ## SCCE provenance extensions (20–31)
///
/// Variants 20–31 are the SCCE cognitive runtime's provenance and
/// knowledge graph node types. These make the GraphOS graph natively
/// capable of hosting SCCE's document→span→chunk provenance chain,
/// entity/relation knowledge graph, and cognitive pipeline artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum NodeKind {
    // ── System objects (0–11) ────────────────────────────────────────
    /// The kernel as an entity (singleton, node 1).
    Kernel = 0,
    /// A physical CPU core.
    CpuCore = 1,
    /// A schedulable task (kernel-mode or user-mode).
    Task = 2,
    /// An address space (page table root).
    AddressSpace = 3,
    /// A physical memory region (from the UEFI map or frame allocator).
    MemoryRegion = 4,
    /// A hardware device.
    Device = 5,
    /// A kernel or userspace service.
    Service = 6,
    /// A file or file-like object.
    File = 7,
    /// An interrupt vector.
    Interrupt = 8,
    /// A timer source.
    Timer = 9,
    /// A display surface.
    DisplaySurface = 10,
    /// A reserved physical range (registered in mm::reserved).
    ReservedRange = 11,

    // ── Graph-theory first-class objects (12–19) ─────────────────────
    /// An IPC channel endpoint (for typed message-passing edges).
    Channel = 12,
    /// A graph partition (from METIS multilevel or Fennel streaming).
    /// Nodes within a partition share locality for walk sampling and
    /// spectral computation. Re-partition triggers: imbalance > 1.20,
    /// communication volume +25%, node loss ≥ 10%.
    Partition = 13,
    /// A spectral snapshot: frozen eigenvalue vector at a generation.
    /// Enables Fiedler drift detection: Δλ₂/Δt ≤ −0.1 sustained 5
    /// steps predicts partition within 5 steps (precision ≥ 0.82).
    SpectralSnapshot = 14,
    /// A discovered structural pattern (subgraph motif).
    /// From MDL compression: L(G,M) = L(M) + L(G|M).
    /// Pattern size cap: 10–20 edges (gSpan canonical DFS code).
    Pattern = 15,
    /// An anomaly event detected by the graph substrate.
    /// Triggered by: spectral CUSUM on λ₂(t), tensor reconstruction
    /// error > 3σ above 90-day rolling baseline, or pattern compression
    /// ratio > 0.85.
    Anomaly = 16,
    /// A causal model node (SCM structural equation).
    /// Supports Pearl's do-calculus for interventional reasoning.
    CausalModel = 17,
    /// A user or principal identity for the 4D audit tensor
    /// T ∈ ℝ^{U×R×T×A} (user × resource × time × action).
    Principal = 18,
    /// A workflow stage in a predictive pipeline.
    WorkflowStage = 19,

    // ── SCCE provenance & knowledge graph objects (20–31) ────────────
    //
    // These types translate the SCCE 2.0 data model into native graph
    // nodes. Together they implement the provenance chain:
    //   Document → Span → Chunk (retrieval unit)
    // and the knowledge graph:
    //   Entity → Relation (via edges, not a node type)
    //
    /// An ingested document — the root of a provenance chain.
    /// Metadata: blob_hash, doc_type, policy_class, path, mtime, size.
    /// From SCCE: DocumentRecord.
    Document = 20,
    /// A byte range within a document — finest provenance unit.
    /// Metadata: byte_start, byte_len, page_num, sheet_name, code_path.
    /// From SCCE: SpanRecord.
    Span = 21,
    /// A retrieval unit derived from one or more spans.
    /// Carries normalized text, SimHash64, token count estimate.
    /// From SCCE: ChunkRecord.
    Chunk = 22,
    /// A correlated knowledge entity (person, concept, term).
    /// Resolved from mentions via LSH + Hamming distance clustering.
    /// From SCCE: EntityRecord.
    Entity = 23,
    /// A background job (indexing, correlation, spectral refresh).
    /// Tracks status, progress, cost, verifier score.
    /// From SCCE: JobRecord.
    Job = 24,
    /// A generated artifact (answer bundle, report, export).
    /// Carries content hash, artifact type, size.
    /// From SCCE: ArtifactRecord.
    Artifact = 25,
    /// A conversation context (query session).
    /// From SCCE: ConversationRecord.
    Conversation = 26,
    /// A spectral model snapshot for the knowledge graph.
    /// Stores operator type, dimension k, drift metric.
    /// From SCCE: SpectralModelRecord.
    KnowledgeSpectral = 27,
    /// A table extracted from a spreadsheet document.
    /// From SCCE: TableRecord.
    Table = 28,
    /// A code unit (file/module within a code project).
    /// From SCCE: CodeUnitRecord.
    CodeUnit = 29,
    /// A mention surface form before entity resolution.
    /// From SCCE: MentionRecord.
    Mention = 30,
    /// Reserved for future SCCE expansion.
    ScceReserved = 31,
    /// An installed driver package (kernel-verified, signed manifest).
    DriverPackage = 32,
    /// A network socket (TCP or UDP endpoint).
    Socket = 33,
    /// A WebAssembly sandbox module instance.
    WasmSandbox = 34,
    /// A TPM 2.0 hardware security module node (singleton per machine).
    TpmDevice = 35,
    /// An enrolled FIDO2 / CTAP2 credential (authenticator-backed).
    FidoCredential = 36,
    /// An A/B OTA boot slot managed by the update subsystem.
    BootSlot = 37,
}

impl NodeKind {
    /// Convert to `usize` for indexing into type-pair matrices.
    /// Panics in debug if the variant exceeds `NODE_KIND_COUNT`.
    pub const fn index(self) -> usize {
        let v = self as usize;
        // Compile-time safety net: if someone adds a variant ≥ NODE_KIND_COUNT,
        // this will panic in debug builds and wrap in release.
        debug_assert!(v < NODE_KIND_COUNT);
        v
    }

    /// Convert a raw u16 to a NodeKind, or `None` if out of range.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::Kernel),
            1 => Some(Self::CpuCore),
            2 => Some(Self::Task),
            3 => Some(Self::AddressSpace),
            4 => Some(Self::MemoryRegion),
            5 => Some(Self::Device),
            6 => Some(Self::Service),
            7 => Some(Self::File),
            8 => Some(Self::Interrupt),
            9 => Some(Self::Timer),
            10 => Some(Self::DisplaySurface),
            11 => Some(Self::ReservedRange),
            12 => Some(Self::Channel),
            13 => Some(Self::Partition),
            14 => Some(Self::SpectralSnapshot),
            15 => Some(Self::Pattern),
            16 => Some(Self::Anomaly),
            17 => Some(Self::CausalModel),
            18 => Some(Self::Principal),
            19 => Some(Self::WorkflowStage),
            20 => Some(Self::Document),
            21 => Some(Self::Span),
            22 => Some(Self::Chunk),
            23 => Some(Self::Entity),
            24 => Some(Self::Job),
            25 => Some(Self::Artifact),
            26 => Some(Self::Conversation),
            27 => Some(Self::KnowledgeSpectral),
            28 => Some(Self::Table),
            29 => Some(Self::CodeUnit),
            30 => Some(Self::Mention),
            31 => Some(Self::ScceReserved),
            32 => Some(Self::DriverPackage),
            33 => Some(Self::Socket),
            34 => Some(Self::WasmSandbox),
            35 => Some(Self::TpmDevice),
            36 => Some(Self::FidoCredential),
            37 => Some(Self::BootSlot),
            _ => None,
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Edge kind
// ────────────────────────────────────────────────────────────────────

/// Maximum number of distinct edge kinds. Used to size per-edge-type arrays.
pub const EDGE_KIND_COUNT: usize = 36;

/// Kernel-level edge kinds.
///
/// Edges are directed: `from → to`. The type function τ_E : E → EdgeKind
/// is central to the heterogeneous temporal graph formalism.
///
/// Multiple edges of different kinds can connect the same pair of nodes.
/// The PowerWalk transition model P(x | s, v, t) conditions on both the
/// source node type and the edge type through per-type-pair parameters
/// (p_{ττ'}, q_{ττ'}, λ_{ττ'}).
///
/// ## SCCE provenance extensions (20–31)
///
/// Edge kinds 20–31 implement the SCCE data model's relationship types:
/// provenance chain links, knowledge graph relations, retrieval scoring
/// edges, and job/artifact provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum EdgeKind {
    // ── System relationships (1–9) ───────────────────────────────────
    /// `from` executes on `to` (task → CPU core).
    RunsOn = 1,
    /// `from` owns `to` (kernel → device, task → address space).
    Owns = 2,
    /// `from` maps `to` (address space → memory region).
    Maps = 3,
    /// `from` is waiting on `to` (task → task, task → device).
    WaitsOn = 4,
    /// `from` delivers interrupts to `to` (device → interrupt → task).
    Interrupts = 5,
    /// `from` communicates with `to` (task → task via IPC).
    CommunicatesWith = 6,
    /// `from` contains `to` (kernel → reserved range).
    Contains = 7,
    /// `from` depends on `to` (service → service).
    DependsOn = 8,
    /// `from` created `to` (provenance: which entity caused this node to exist).
    Created = 9,

    // ── Temporal / causal / analytic relationships (10–19) ───────────
    /// Temporal precedence: event at `from` happened-before event at `to`.
    /// Establishes causal ordering t₀ ≤ t₁ ≤ … ≤ tₖ for temporal walks.
    /// Grounded in Lamport clock semantics.
    TemporalPrecedes = 10,
    /// Granger-causal link: past values of `from` predict `to`.
    /// The weight encodes the F-test significance (1 − p-value in 16.16).
    /// Discovered via VAR-based F-test, O(n²T), with Benjamini-Hochberg
    /// FDR correction at q = 0.1.
    GrangerCauses = 11,
    /// Transfer entropy link: model-free directional information flow.
    /// T_{X→Y} = conditional mutual information.
    /// Weight encodes normalized transfer entropy in 16.16.
    TransferEntropy = 12,
    /// `from` triggers `to` (event-driven activation).
    Triggers = 13,
    /// `from` and `to` are the same logical entity across time slices
    /// or across partitions. Identity-preserving link.
    SameEntity = 14,
    /// `from` is a member of partition `to`.
    MemberOf = 15,
    /// `from` pattern instance matches `to` pattern definition.
    /// For temporal subgraph isomorphism with Δ-constraint:
    /// |τ(f(u)) − τ(f(v))| ≤ Δ.
    MatchesPattern = 16,
    /// `from` anomaly was detected by `to` detector.
    DetectedBy = 17,
    /// Spectral similarity (DeltaCon affinity) above threshold.
    SpectralSimilar = 18,
    /// `from` accesses `to` resource (for 4D audit tensor construction).
    Accesses = 19,

    // ── SCCE provenance & knowledge graph edges (20–31) ──────────────
    //
    // These edges implement the SCCE document→span→chunk provenance
    // chain and the entity/relation knowledge graph within the native
    // GraphOS graph.
    /// Document → Span: provenance link. A document contains this span.
    /// Weight: 1.0 (structural link, not a relevance score).
    HasSpan = 20,
    /// Span → Chunk: derivation link. This span contributed to this chunk.
    /// Weight: 1.0 (structural).
    DerivedChunk = 21,
    /// Entity → Entity: knowledge graph relation (typed by weight range).
    /// The `weight` field encodes relation strength (16.16 fixed-point).
    /// The relation type (IS_A, CAUSES, RELATED_TO, etc.) is stored in
    /// the edge's metadata u32 field (rel_type_id hash).
    KnowledgeRelation = 22,
    /// Mention → Entity: resolution link. This mention resolved to this entity.
    /// Weight: 1.0 - (hamming_distance / MAX_HAMMING) in 16.16.
    MentionResolves = 23,
    /// Chunk → Entity: co-occurrence. This chunk mentions this entity.
    /// Weight: TF-IDF or BM25 score in 16.16.
    ChunkMentions = 24,
    /// Conversation → Chunk: retrieval link. This chunk was retrieved
    /// as evidence for this conversation.
    /// Weight: fused retrieval score in 16.16.
    RetrievedEvidence = 25,
    /// Job → Document: a job operated on this document.
    /// Weight: 1.0 (structural).
    JobTarget = 26,
    /// Artifact → Conversation: this artifact was produced by this query.
    /// Weight: confidence score in 16.16.
    ArtifactFrom = 27,
    /// Entity → Entity: tensor edge with time and context dimensions.
    /// Extends KnowledgeRelation with temporal decay.
    /// From SCCE: TensorEdgeRecord.
    TensorRelation = 28,
    /// Chunk → Chunk: SimHash near-duplicate link.
    /// Weight: 1.0 - (hamming_distance / 64) in 16.16.
    SimHashNearDup = 29,
    /// Document → Table: document contains this extracted table.
    HasTable = 30,
    /// Document → CodeUnit: document contains this code unit.
    HasCodeUnit = 31,
    /// Device → DriverPackage: a driver package is attached to this device.
    DriverAttached = 32,
    /// Task → Socket: this task owns (or has open) this socket.
    Binds = 33,
    /// Task/Service → WasmSandbox: this entity hosts a WASM module sandbox.
    Hosts = 34,
    /// Watchdog → Service node: watchdog is monitoring this service.
    Monitors = 35,
}

impl EdgeKind {
    /// Convert to `usize` for indexing into per-edge-type arrays.
    pub const fn index(self) -> usize {
        let v = self as usize;
        debug_assert!(v < EDGE_KIND_COUNT);
        v
    }

    /// Convert a raw u16 to an EdgeKind, or `None` if out of range.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::RunsOn),
            2 => Some(Self::Owns),
            3 => Some(Self::Maps),
            4 => Some(Self::WaitsOn),
            5 => Some(Self::Interrupts),
            6 => Some(Self::CommunicatesWith),
            7 => Some(Self::Contains),
            8 => Some(Self::DependsOn),
            9 => Some(Self::Created),
            10 => Some(Self::TemporalPrecedes),
            11 => Some(Self::GrangerCauses),
            12 => Some(Self::TransferEntropy),
            13 => Some(Self::Triggers),
            14 => Some(Self::SameEntity),
            15 => Some(Self::MemberOf),
            16 => Some(Self::MatchesPattern),
            17 => Some(Self::DetectedBy),
            18 => Some(Self::SpectralSimilar),
            19 => Some(Self::Accesses),
            20 => Some(Self::HasSpan),
            21 => Some(Self::DerivedChunk),
            22 => Some(Self::KnowledgeRelation),
            23 => Some(Self::MentionResolves),
            24 => Some(Self::ChunkMentions),
            25 => Some(Self::RetrievedEvidence),
            26 => Some(Self::JobTarget),
            27 => Some(Self::ArtifactFrom),
            28 => Some(Self::TensorRelation),
            29 => Some(Self::SimHashNearDup),
            30 => Some(Self::HasTable),
            31 => Some(Self::HasCodeUnit),
            32 => Some(Self::DriverAttached),
            33 => Some(Self::Binds),
            34 => Some(Self::Hosts),
            35 => Some(Self::Monitors),
            _ => None,
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Node flags (bitfield in Node.flags)
// ────────────────────────────────────────────────────────────────────

/// Node is trusted (verified provenance chain to kernel root).
pub const NODE_FLAG_TRUSTED: u32 = 1 << 0;
/// Node is pinned and must not be garbage-collected or evicted.
pub const NODE_FLAG_PINNED: u32 = 1 << 1;
/// Node participates in spectral tracking (eigenvalue monitoring).
pub const NODE_FLAG_SPECTRAL: u32 = 1 << 2;
/// Node is a walk sampling source (eligible as walk origin).
pub const NODE_FLAG_WALK_SOURCE: u32 = 1 << 3;
/// Node has been marked anomalous by a detector.
pub const NODE_FLAG_ANOMALOUS: u32 = 1 << 4;
/// Node is a causal root (Perron-Frobenius stationary distribution peak).
pub const NODE_FLAG_CAUSAL_ROOT: u32 = 1 << 5;
/// Node has been logically detached (hot-unplug or package removal).
/// Graph traversal should skip nodes with this flag set.
pub const NODE_FLAG_DETACHED: u32 = 1 << 6;

// ────────────────────────────────────────────────────────────────────
// Edge flags (bitfield in Edge.flags)
// ────────────────────────────────────────────────────────────────────

/// Edge participates in causal ordering constraints.
pub const EDGE_FLAG_CAUSAL: u32 = 1 << 0;
/// Edge was inferred (not directly observed) — e.g., Granger, transfer entropy.
pub const EDGE_FLAG_INFERRED: u32 = 1 << 1;
/// Edge weight has been calibrated (post-EM or post-MLE fitting).
pub const EDGE_FLAG_CALIBRATED: u32 = 1 << 2;
/// Edge is bidirectional (convenience: avoids inserting reverse edge).
pub const EDGE_FLAG_BIDIRECTIONAL: u32 = 1 << 3;
/// Edge is part of a discovered pattern (gSpan canonical DFS code).
pub const EDGE_FLAG_IN_PATTERN: u32 = 1 << 4;

// ────────────────────────────────────────────────────────────────────
// Node struct
// ────────────────────────────────────────────────────────────────────

/// A node in the kernel graph.
///
/// 64 bytes. Stable `repr(C)` layout for static array storage.
///
/// The `degree_out` / `degree_in` fields support O(1) Laplacian diagonal
/// access: d(v) = degree_out + degree_in (for undirected interpretation)
/// or just degree_out (for directed Laplacian).
///
/// `adj_head_out` / `adj_head_in` are indices into the edge array for
/// intrusive adjacency list traversal (arena-internal, set by arena ops).
///
/// `uuid` is the UUID-first identity of this node. For kernel bootstrap nodes
/// it defaults to `Uuid128::NIL` and may be set post-creation via
/// `arena::set_node_uuid()`. For nodes created by userspace services it should
/// be set at creation time using `arena::add_node_with_uuid()`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Node {
    /// Unique identifier. Never zero in a live node.
    pub id: NodeId,
    /// UUID-first identity. May be NIL for early bootstrap nodes.
    pub uuid: crate::uuid::Uuid128,
    /// Classification of this node (τ_V function).
    pub kind: NodeKind,
    /// Padding for alignment.
    pub _pad: u16,
    /// Per-node flags (trust, spectral, anomaly, etc.).
    pub flags: u32,
    /// Boot-relative tick when this node was created.
    pub created_at: Timestamp,
    /// NodeId of the entity that created this node.
    pub creator: NodeId,
    /// Weighted out-degree: Σ w(self→u) in 16.16 fixed-point.
    /// Maintained incrementally by arena::add_edge / arena::remove_edge.
    pub degree_out: Weight,
    /// Weighted in-degree: Σ w(u→self) in 16.16 fixed-point.
    pub degree_in: Weight,
    /// Index of first outgoing edge in the edge array (intrusive list head).
    /// `u32::MAX` means "no outgoing edges".
    pub adj_head_out: u32,
    /// Index of first incoming edge in the edge array (intrusive list head).
    /// `u32::MAX` means "no incoming edges".
    pub adj_head_in: u32,
}

/// Sentinel value: "no adjacent edge" in the intrusive adjacency list.
pub const ADJ_NONE: u32 = u32::MAX;

impl Node {
    /// An empty node slot. `id == 0` means the slot is free.
    pub const EMPTY: Self = Self {
        id: 0,
        uuid: crate::uuid::Uuid128::NIL,
        kind: NodeKind::Kernel,
        _pad: 0,
        flags: 0,
        created_at: 0,
        creator: 0,
        degree_out: 0,
        degree_in: 0,
        adj_head_out: ADJ_NONE,
        adj_head_in: ADJ_NONE,
    };

    /// Returns `true` if this slot is occupied.
    pub const fn is_live(&self) -> bool {
        self.id != 0
    }

    /// Total weighted degree (undirected interpretation).
    /// Used for normalized Laplacian: 𝓛 = I − D^{−1/2} A D^{−1/2}.
    pub const fn degree_total(&self) -> u64 {
        self.degree_out as u64 + self.degree_in as u64
    }
}

// ────────────────────────────────────────────────────────────────────
// Edge struct
// ────────────────────────────────────────────────────────────────────

/// A directed, typed, weighted, timestamped edge in the kernel graph.
///
/// 56 bytes. Stable `repr(C)` layout for static array storage.
///
/// The `weight` field in 16.16 fixed-point supports:
/// - Laplacian construction: A_{ij} = weight
/// - Temporal decay: w̃(u,v,t) = weight · exp(−λ_{ττ'} · (t_now − created_at))
/// - Walk transition: P(u→v) ∝ w̃(u,v,t) (with PowerWalk type-pair bias)
///
/// `next_out` / `next_in` form intrusive singly-linked adjacency lists
/// rooted at Node.adj_head_out / Node.adj_head_in. This gives O(degree)
/// neighbor enumeration without a separate index structure.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    /// Unique edge identifier.
    pub id: EdgeId,
    /// Source node.
    pub from: NodeId,
    /// Destination node.
    pub to: NodeId,
    /// Semantic relationship (τ_E function).
    pub kind: EdgeKind,
    /// Padding.
    pub _pad: u16,
    /// Per-edge flags (causal, inferred, calibrated, etc.).
    pub flags: u32,
    /// Edge weight in 16.16 fixed-point.
    /// For system edges (Owns, Contains, etc.) this is typically WEIGHT_ONE.
    /// For analytic edges (GrangerCauses, TransferEntropy, SpectralSimilar)
    /// this encodes the computed strength.
    pub weight: Weight,
    /// Boot-relative tick when this edge was created.
    pub created_at: Timestamp,
    /// Next outgoing edge index from the same source node (intrusive list).
    /// `u32::MAX` means end-of-list.
    pub next_out: u32,
    /// Next incoming edge index to the same destination node (intrusive list).
    /// `u32::MAX` means end-of-list.
    pub next_in: u32,
}

impl Edge {
    /// An empty edge slot. `id == 0` means the slot is free.
    pub const EMPTY: Self = Self {
        id: 0,
        from: 0,
        to: 0,
        kind: EdgeKind::Owns,
        _pad: 0,
        flags: 0,
        weight: 0,
        created_at: 0,
        next_out: ADJ_NONE,
        next_in: ADJ_NONE,
    };

    /// Returns `true` if this slot is occupied.
    pub const fn is_live(&self) -> bool {
        self.id != 0
    }

    /// Compute temporally-decayed weight using integer approximation.
    ///
    /// Returns w · exp(−λ · Δt) approximated as:
    ///   w · (1 − λ·Δt + (λ·Δt)²/2)  (second-order Taylor)
    ///
    /// All arithmetic is 16.16 fixed-point.
    ///
    /// `decay_fp` is λ in 16.16 format (from TypePairParams).
    /// `delta_t` is (t_now − self.created_at) in ticks.
    ///
    /// Returns 0 if the decay drives the weight below the representable
    /// minimum (full attenuation).
    pub const fn decayed_weight(&self, decay_fp: Weight, delta_t: u64) -> Weight {
        // λ·Δt in 16.16: multiply then shift back.
        // Clamp Δt to u32 to avoid overflow in intermediate products.
        let dt = if delta_t > 0xFFFF {
            0xFFFF
        } else {
            delta_t as u32
        };
        let lambda_dt: u64 = (decay_fp as u64 * dt as u64) >> 16;

        if lambda_dt >= (WEIGHT_ONE as u64) {
            // Full attenuation: exp(−x) ≈ 0 for x ≥ 1.0 in this approximation.
            return 0;
        }

        // (λΔt)² / 2 in 16.16
        let sq_half: u64 = (lambda_dt * lambda_dt) >> 17; // >>16 for multiply, >>1 for /2

        // 1 − λΔt + (λΔt)²/2, all in 16.16
        let one = WEIGHT_ONE as u64;
        let approx = one.saturating_sub(lambda_dt).saturating_add(sq_half);

        // w · approx >> 16
        let result = (self.weight as u64 * approx) >> 16;
        if result > u32::MAX as u64 {
            u32::MAX
        } else {
            result as u32
        }
    }
}
