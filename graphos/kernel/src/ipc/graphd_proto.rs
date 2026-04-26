// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphd IPC protocol — typed temporal graph query/mutation payloads.
//!
//! These types define the message payloads carried over IPC channels when
//! communicating with graphd. They match `MsgTag` values 0x10–0x1F.
//!
//! All types are `#[repr(C)]` and fixed-size for zero-copy channel reads.
//! The payload is serialised by simple byte-copy (`as_bytes` / `from_bytes`),
//! not by a separate serialisation framework.
//!
//! ## Paper doctrine alignment
//!
//! These types make the IPC substrate **typed-temporal-graph-native**:
//! - **Local Operator Engine**: `GraphQuery` can request scored/ranked
//!   node neighborhoods. `NodeScore` carries urgency, relevance, risk,
//!   trust, and recency — the five scoring dimensions from the papers.
//! - **Structural Engine**: `GraphQuery::SubgraphByKind` enables structural
//!   queries for compression/clustering. `SummaryResult` provides compressed
//!   graph digests for shell3d and modeld context assembly.
//! - **Predictive Engine**: `GraphQuery::DriftReport` asks for temporal
//!   drift metrics. `DriftEntry` carries eigenvalue changes and trend.
//! - **Causal Decision Engine**: `GraphQuery::CausalAncestors` supports
//!   root-cause queries. Responses carry provenance chains.

use crate::graph::types::{EdgeKind, NodeId, NodeKind, Timestamp, Weight};

// ────────────────────────────────────────────────────────────────────
// Graph query request (MsgTag::GraphQuery = 0x10)
// ────────────────────────────────────────────────────────────────────

/// Maximum number of result nodes returned in a single query response.
pub const MAX_QUERY_RESULTS: usize = 8;

/// Maximum depth for neighborhood queries.
pub const MAX_QUERY_DEPTH: u8 = 4;

/// The kind of graph query being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GraphQueryKind {
    /// Get a single node by ID.
    NodeById = 0,
    /// Get all edges from a node (1-hop neighborhood).
    EdgesFrom = 1,
    /// Get all edges to a node (reverse 1-hop).
    EdgesTo = 2,
    /// Get nodes of a specific kind (up to MAX_QUERY_RESULTS).
    NodesByKind = 3,
    /// Get a scored neighborhood: nodes ranked by the local operator
    /// scoring dimensions (urgency, relevance, risk, trust, recency).
    ScoredNeighborhood = 4,
    /// Get a structural subgraph filtered by node/edge kind.
    SubgraphByKind = 5,
    /// Request a compressed structural summary (for shell3d or modeld).
    StructuralSummary = 6,
    /// Request temporal drift metrics for the graph or a subgraph.
    DriftReport = 7,
    /// Get causal ancestors of a node (for root-cause analysis).
    CausalAncestors = 8,
    /// Get the current graph generation (sequence number).
    Generation = 9,
    /// Count nodes/edges (for diagnostics).
    Stats = 10,
}

/// Graph query request payload.
///
/// Fits in 32 bytes. Sent with `MsgTag::GraphQuery`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphQuery {
    /// What kind of query.
    pub kind: GraphQueryKind,
    /// For kind-filtered queries: the node kind to match.
    pub node_kind_filter: u8,
    /// For kind-filtered queries: the edge kind to match.
    pub edge_kind_filter: u8,
    /// Maximum depth for neighborhood/causal queries.
    pub max_depth: u8,
    /// Maximum number of results to return.
    pub max_results: u8,
    /// Padding for alignment.
    pub _pad: [u8; 3],
    /// The target node ID (for node-specific queries).
    pub target_node: NodeId,
    /// Minimum timestamp filter (0 = no filter).
    pub since_timestamp: Timestamp,
    /// Minimum score threshold (16.16 fixed-point, 0 = no filter).
    pub min_score: Weight,
    /// Reserved for future use.
    pub _reserved: u32,
}

impl GraphQuery {
    /// Create a NodeById query.
    pub const fn node_by_id(id: NodeId) -> Self {
        Self {
            kind: GraphQueryKind::NodeById,
            node_kind_filter: 0,
            edge_kind_filter: 0,
            max_depth: 0,
            max_results: 1,
            _pad: [0; 3],
            target_node: id,
            since_timestamp: 0,
            min_score: 0,
            _reserved: 0,
        }
    }

    /// Create an EdgesFrom query.
    pub const fn edges_from(id: NodeId) -> Self {
        Self {
            kind: GraphQueryKind::EdgesFrom,
            node_kind_filter: 0,
            edge_kind_filter: 0,
            max_depth: 1,
            max_results: MAX_QUERY_RESULTS as u8,
            _pad: [0; 3],
            target_node: id,
            since_timestamp: 0,
            min_score: 0,
            _reserved: 0,
        }
    }

    /// Create a NodesByKind query.
    pub const fn nodes_by_kind(kind: NodeKind, max: u8) -> Self {
        Self {
            kind: GraphQueryKind::NodesByKind,
            node_kind_filter: kind as u8,
            edge_kind_filter: 0,
            max_depth: 0,
            max_results: max,
            _pad: [0; 3],
            target_node: 0,
            since_timestamp: 0,
            min_score: 0,
            _reserved: 0,
        }
    }

    /// Create a ScoredNeighborhood query.
    pub const fn scored_neighborhood(id: NodeId, depth: u8, min_score: Weight) -> Self {
        Self {
            kind: GraphQueryKind::ScoredNeighborhood,
            node_kind_filter: 0,
            edge_kind_filter: 0,
            max_depth: depth,
            max_results: MAX_QUERY_RESULTS as u8,
            _pad: [0; 3],
            target_node: id,
            since_timestamp: 0,
            min_score,
            _reserved: 0,
        }
    }

    /// Create a DriftReport query.
    pub const fn drift_report(since: Timestamp) -> Self {
        Self {
            kind: GraphQueryKind::DriftReport,
            node_kind_filter: 0,
            edge_kind_filter: 0,
            max_depth: 0,
            max_results: MAX_QUERY_RESULTS as u8,
            _pad: [0; 3],
            target_node: 0,
            since_timestamp: since,
            min_score: 0,
            _reserved: 0,
        }
    }

    /// Create a CausalAncestors query.
    pub const fn causal_ancestors(id: NodeId, depth: u8) -> Self {
        Self {
            kind: GraphQueryKind::CausalAncestors,
            node_kind_filter: 0,
            edge_kind_filter: 0,
            max_depth: depth,
            max_results: MAX_QUERY_RESULTS as u8,
            _pad: [0; 3],
            target_node: id,
            since_timestamp: 0,
            min_score: 0,
            _reserved: 0,
        }
    }

    /// Create a Stats query.
    pub const fn stats() -> Self {
        Self {
            kind: GraphQueryKind::Stats,
            node_kind_filter: 0,
            edge_kind_filter: 0,
            max_depth: 0,
            max_results: 0,
            _pad: [0; 3],
            target_node: 0,
            since_timestamp: 0,
            min_score: 0,
            _reserved: 0,
        }
    }

    /// Interpret self as a byte slice for IPC payload.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    /// Interpret a byte slice as a GraphQuery. Returns None if too small.
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        // SAFETY: We've verified the length. GraphQuery is repr(C) and all
        // bit patterns are valid for its numeric fields.
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Graph query response (MsgTag::GraphQueryResult = 0x11)
// ────────────────────────────────────────────────────────────────────

/// Status code for query responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QueryStatus {
    /// Query succeeded.
    Ok = 0,
    /// Node not found.
    NotFound = 1,
    /// Invalid query parameters.
    InvalidQuery = 2,
    /// Result truncated (more results than max_results).
    Truncated = 3,
    /// Internal error.
    InternalError = 4,
}

/// A single node entry in a query result.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ResultNode {
    /// Node ID.
    pub id: NodeId,
    /// Node kind.
    pub kind: u16,
    /// Flags from the node.
    pub flags: u16,
    /// Weight/score (16.16 fixed-point).
    pub weight: Weight,
}

impl ResultNode {
    pub const EMPTY: Self = Self {
        id: 0,
        kind: 0,
        flags: 0,
        weight: 0,
    };
}

/// A single edge entry in a query result.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ResultEdge {
    /// Source node ID.
    pub from: NodeId,
    /// Target node ID.
    pub to: NodeId,
    /// Edge kind.
    pub kind: u16,
    /// Padding.
    pub _pad: u16,
    /// Edge weight (16.16 fixed-point).
    pub weight: Weight,
    /// Edge creation timestamp.
    pub created_at: Timestamp,
}

impl ResultEdge {
    pub const EMPTY: Self = Self {
        from: 0,
        to: 0,
        kind: 0,
        _pad: 0,
        weight: 0,
        created_at: 0,
    };
}

/// Graph query response payload.
///
/// Contains up to MAX_QUERY_RESULTS node entries. Edges are returned as
/// separate follow-up responses (MsgTag::GraphEvent) if needed.
/// Sent with `MsgTag::GraphQueryResult`.
///
/// Size: 1 + 1 + 2 + 8 + (8 × 12) = 108 bytes. Fits in MAX_MSG_BYTES (256).
/// If edge results are needed, graphd sends a separate edge-typed response.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphQueryResult {
    /// Status code.
    pub status: QueryStatus,
    /// Number of result entries populated.
    pub result_count: u8,
    /// Padding.
    pub _pad: [u8; 2],
    /// Current graph generation at time of query.
    pub generation: Timestamp,
    /// Result nodes (for node queries).
    pub nodes: [ResultNode; MAX_QUERY_RESULTS],
}

impl GraphQueryResult {
    pub const EMPTY: Self = Self {
        status: QueryStatus::Ok,
        result_count: 0,
        _pad: [0; 2],
        generation: 0,
        nodes: [ResultNode::EMPTY; MAX_QUERY_RESULTS],
    };

    /// Interpret self as a byte slice for IPC payload.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    /// Interpret a byte slice as a GraphQueryResult. Returns None if too small.
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Graph mutation request (MsgTag::GraphMutate = 0x12)
// ────────────────────────────────────────────────────────────────────

/// Kind of graph mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MutationKind {
    /// Add a new node.
    AddNode = 0,
    /// Add a new edge.
    AddEdge = 1,
    /// Update a node's weight/flags.
    UpdateNode = 2,
    /// Update an edge's weight.
    UpdateEdge = 3,
    /// Mark a node inactive (soft-delete).
    DeactivateNode = 4,
    /// Mark an edge inactive (soft-delete).
    DeactivateEdge = 5,
}

/// Graph mutation request payload.
///
/// Fits in 48 bytes. Sent with `MsgTag::GraphMutate`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GraphMutation {
    /// What kind of mutation.
    pub kind: MutationKind,
    /// For AddNode: the node kind.
    pub node_kind: u8,
    /// For AddEdge: the edge kind.
    pub edge_kind: u8,
    /// Padding.
    pub _pad: u8,
    /// Weight/score to assign (16.16 fixed-point).
    pub weight: Weight,
    /// For AddEdge: source node.
    pub from_node: NodeId,
    /// For AddEdge: target node. For AddNode: parent/creator node.
    pub to_node: NodeId,
    /// For UpdateNode/Edge: the target node/edge ID.
    pub target_id: NodeId,
    /// Flags to set.
    pub flags: u16,
    /// Padding.
    pub _pad2: [u8; 6],
}

impl GraphMutation {
    /// Create an AddNode mutation.
    pub const fn add_node(kind: NodeKind, weight: Weight, parent: NodeId) -> Self {
        Self {
            kind: MutationKind::AddNode,
            node_kind: kind as u8,
            edge_kind: 0,
            _pad: 0,
            weight,
            from_node: 0,
            to_node: parent,
            target_id: 0,
            flags: 0,
            _pad2: [0; 6],
        }
    }

    /// Create an AddEdge mutation.
    pub const fn add_edge(kind: EdgeKind, from: NodeId, to: NodeId, weight: Weight) -> Self {
        Self {
            kind: MutationKind::AddEdge,
            node_kind: 0,
            edge_kind: kind as u8,
            _pad: 0,
            weight,
            from_node: from,
            to_node: to,
            target_id: 0,
            flags: 0,
            _pad2: [0; 6],
        }
    }

    /// Interpret self as a byte slice for IPC payload.
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    /// Interpret a byte slice as a GraphMutation. Returns None if too small.
    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Graph mutation acknowledgement (MsgTag::GraphMutateAck = 0x13)
// ────────────────────────────────────────────────────────────────────

/// Graph mutation ack payload. 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MutationAck {
    /// Status: 0 = success, non-zero = error code.
    pub status: u8,
    /// Padding.
    pub _pad: [u8; 3],
    /// The graph generation after this mutation.
    pub generation: Timestamp,
    /// For AddNode: the assigned node ID. For AddEdge: the assigned edge ID.
    pub assigned_id: u64,
}

impl MutationAck {
    pub const fn success(generation: Timestamp, assigned_id: u64) -> Self {
        Self {
            status: 0,
            _pad: [0; 3],
            generation,
            assigned_id,
        }
    }

    pub const fn error(code: u8) -> Self {
        Self {
            status: code,
            _pad: [0; 3],
            generation: 0,
            assigned_id: 0,
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Node scoring — Local Operator Engine types
// ────────────────────────────────────────────────────────────────────

/// Per-node score vector used by the Local Operator Engine.
///
/// These five dimensions come from the temporal scoring / typed-relation
/// papers. They drive:
/// - Process urgency (scheduler hints)
/// - Service health score (diagnostics)
/// - File/workspace relevance (shell ranking)
/// - Package risk (update decisions)
/// - Trust/anomaly (security surface)
///
/// All values are 16.16 fixed-point.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NodeScore {
    /// Urgency: how time-critical is this node's current state?
    /// Higher = needs attention sooner.
    pub urgency: Weight,
    /// Relevance: how central is this node to the user's current context?
    /// Computed from graph centrality + recency.
    pub relevance: Weight,
    /// Risk: how likely is this node to cause a failure or degradation?
    /// From predictive engine drift metrics.
    pub risk: Weight,
    /// Trust: how well-established is this node's provenance chain?
    /// From causal/provenance analysis.
    pub trust: Weight,
    /// Recency: time-decayed activity score.
    /// From temporal operator: exp(-lambda * delta_t).
    pub recency: Weight,
}

impl NodeScore {
    pub const ZERO: Self = Self {
        urgency: 0,
        relevance: 0,
        risk: 0,
        trust: 0,
        recency: 0,
    };

    /// Composite score: weighted sum of all dimensions.
    /// Uses equal weights for now. Returns 16.16 fixed-point.
    pub const fn composite(&self) -> u64 {
        let sum = self.urgency as u64
            + self.relevance as u64
            + self.risk as u64
            + self.trust as u64
            + self.recency as u64;
        sum / 5
    }

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Structural summary — Structural Engine types
// ────────────────────────────────────────────────────────────────────

/// Compressed structural summary of a subgraph.
///
/// This is what modeld and shell3d consume instead of raw graph dumps.
/// The Structural Engine produces these using MDL compression ideas:
///   L(G,M) = L(M) + L(G|M)
///
/// Fits in 64 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StructuralSummary {
    /// Number of nodes in the summarised subgraph.
    pub node_count: u32,
    /// Number of edges in the summarised subgraph.
    pub edge_count: u32,
    /// Number of distinct node kinds present.
    pub kind_count: u8,
    /// Number of detected clusters/partitions.
    pub cluster_count: u8,
    /// Number of detected motifs/patterns.
    pub motif_count: u8,
    /// Padding.
    pub _pad: u8,
    /// Fiedler value (lambda_2) of the graph Laplacian, 16.16 fixed-point.
    /// Low values indicate near-disconnection.
    pub fiedler_value: Weight,
    /// Graph density: edges / (nodes * (nodes-1)), 16.16 fixed-point.
    pub density: Weight,
    /// Compression ratio from MDL model, 16.16 fixed-point.
    /// Higher = more compressible = more regular structure.
    pub compression_ratio: Weight,
    /// Graph generation at summary time.
    pub generation: Timestamp,
    /// Reserved.
    pub _reserved: [u8; 24],
}

impl StructuralSummary {
    pub const EMPTY: Self = Self {
        node_count: 0,
        edge_count: 0,
        kind_count: 0,
        cluster_count: 0,
        motif_count: 0,
        _pad: 0,
        fiedler_value: 0,
        density: 0,
        compression_ratio: 0,
        generation: 0,
        _reserved: [0; 24],
    };

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Drift report — Predictive Engine types
// ────────────────────────────────────────────────────────────────────

/// Maximum drift entries per report.
pub const MAX_DRIFT_ENTRIES: usize = 4;

/// A single drift metric entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DriftEntry {
    /// What is drifting (node ID, or 0 for global metric).
    pub source_node: NodeId,
    /// Metric kind (0=Fiedler, 1=degree_variance, 2=edge_churn, 3=score_divergence).
    pub metric: u8,
    /// Trend direction: 0=stable, 1=increasing, 2=decreasing.
    pub trend: u8,
    /// Padding.
    pub _pad: [u8; 2],
    /// Current value, 16.16 fixed-point.
    pub current: Weight,
    /// Delta since last report, 16.16 signed as i32.
    pub delta: i32,
}

/// Drift report response. Fits in ~128 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DriftReport {
    /// Number of drift entries populated.
    pub entry_count: u8,
    /// Overall health: 0=healthy, 1=warning, 2=critical.
    pub overall_health: u8,
    /// Padding.
    pub _pad: [u8; 2],
    /// Graph generation at report time.
    pub generation: Timestamp,
    /// Drift entries.
    pub entries: [DriftEntry; MAX_DRIFT_ENTRIES],
}

impl DriftReport {
    pub const EMPTY: Self = Self {
        entry_count: 0,
        overall_health: 0,
        _pad: [0; 2],
        generation: 0,
        entries: [DriftEntry {
            source_node: 0,
            metric: 0,
            trend: 0,
            _pad: [0; 2],
            current: 0,
            delta: 0,
        }; MAX_DRIFT_ENTRIES],
    };

    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<&Self> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return None;
        }
        let ptr = bytes.as_ptr() as *const Self;
        Some(unsafe { &*ptr })
    }
}

// ────────────────────────────────────────────────────────────────────
// Compile-time size assertions
// ────────────────────────────────────────────────────────────────────
// Every payload type must fit inside MAX_MSG_BYTES (256).

const _: () = {
    assert!(core::mem::size_of::<GraphQuery>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<GraphQueryResult>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<GraphMutation>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<MutationAck>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<NodeScore>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<StructuralSummary>() <= super::msg::MAX_MSG_BYTES);
    assert!(core::mem::size_of::<DriftReport>() <= super::msg::MAX_MSG_BYTES);
};
