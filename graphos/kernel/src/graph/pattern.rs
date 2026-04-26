// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Structural pattern primitives — subgraph matching and MDL compression.
//!
//! This module provides the type-level infrastructure for:
//!
//! ## Temporal subgraph isomorphism
//!
//! A temporal subgraph isomorphism is an injection f: V(P) → V(G) such that:
//! - Structure is preserved: (u,v) ∈ E(P) ⟹ (f(u),f(v)) ∈ E(G)
//! - Types are preserved: τ(f(u)) = τ(u) for all u
//! - Temporal constraint: |t(f(u),f(v)) − t(f(u'),f(v'))| ≤ Δ
//!
//! The Δ-constraint bounds how far apart in time matched edges can be,
//! enabling detection of attack patterns (MITRE ATT&CK, precision 0.82,
//! recall 0.91) and workflow sequences.
//!
//! Algorithms: VF2/VF3 tree-search, STMatch GPU acceleration (8× speedup).
//!
//! ## MDL graph compression
//!
//!   L(G, M) = L(M) + L(G|M)
//!
//! The Minimum Description Length principle: find the model M (set of
//! patterns) that minimises total description length. Patterns are
//! discovered via gSpan mining with canonical DFS codes.
//!
//! - Pattern size cap: 10–20 edges (beyond this, diminishing returns).
//! - Mining trigger: compression ratio > 0.85 (graph is becoming repetitive).
//! - Grammar-based compression for recursive structure.
//!
//! ## Graph kernel support (WL subtree kernel)
//!
//! The Weisfeiler-Leman subtree kernel operates by iterative label
//! refinement:
//!   ℓ'(v) = hash(ℓ(v), {ℓ(u) : u ∈ N(v)})
//!
//! The temporal extension adds time-bucketed labels:
//!   ℓ'(v) = (ℓ(v), ⌊t_v / Δ⌋)
//!
//! This gives 1-WL expressiveness. The TGNN expressiveness proof shows:
//!   1-WL ⊊ TGN ⊊ per-type-TGNN ⊊ 2-WL
//!
//! ## Design
//! - PatternGraph is a compact representation of a small subgraph template.
//! - Fixed-size arrays, no heap. Max 16 nodes, 20 edges per pattern.
//! - Patterns are identified by a 64-bit hash of their canonical DFS code.
//! - Match results are stored as MatchResult structs referencing the
//!   pattern and the set of matched node IDs.

use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum nodes in a pattern template.
pub const PATTERN_MAX_NODES: usize = 16;

/// Maximum edges in a pattern template (gSpan cap: 10–20).
pub const PATTERN_MAX_EDGES: usize = 20;

/// Maximum match results stored per pattern query.
pub const MAX_MATCHES: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Pattern node / edge
// ────────────────────────────────────────────────────────────────────

/// A node constraint within a pattern template.
///
/// 4 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PatternNode {
    /// Required NodeKind. The isomorphism must map this to a graph node
    /// of the same kind.
    pub kind: NodeKind,

    /// Required flags mask. The matched node must have all these flags set.
    /// 0 = no flag constraint.
    pub required_flags: u16,
}

impl PatternNode {
    pub const EMPTY: Self = Self {
        kind: NodeKind::Kernel,
        required_flags: 0,
    };
}

/// An edge constraint within a pattern template.
///
/// 8 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PatternEdge {
    /// Index into the pattern's node array for the source.
    pub from_idx: u8,
    /// Index into the pattern's node array for the destination.
    pub to_idx: u8,
    /// Required EdgeKind.
    pub kind: EdgeKind,
    /// Minimum weight in 16.16 (0 = no minimum).
    pub min_weight: Weight,
}

impl PatternEdge {
    pub const EMPTY: Self = Self {
        from_idx: 0,
        to_idx: 0,
        kind: EdgeKind::Owns,
        min_weight: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// Pattern graph
// ────────────────────────────────────────────────────────────────────

/// A structural pattern template for subgraph isomorphism queries.
///
/// Compact representation of a small directed typed graph that can be
/// matched against the kernel arena. The canonical DFS code hash uniquely
/// identifies the pattern topology.
///
/// The Δ-constraint for temporal matching is stored in `temporal_delta`:
/// |t(matched_edge_i) − t(matched_edge_j)| ≤ temporal_delta for all
/// edge pairs in the pattern.
pub struct PatternGraph {
    /// Canonical DFS code hash — unique structural identifier.
    /// Computed via gSpan's DFS code minimisation.
    pub canonical_hash: u64,

    /// Pattern nodes (type constraints).
    pub nodes: [PatternNode; PATTERN_MAX_NODES],
    /// Number of active nodes in the pattern.
    pub node_count: u8,

    /// Pattern edges (type + direction constraints).
    pub edges: [PatternEdge; PATTERN_MAX_EDGES],
    /// Number of active edges in the pattern.
    pub edge_count: u8,

    /// Temporal Δ-constraint: maximum allowed timestamp difference
    /// between any two matched edges. 0 = no temporal constraint
    /// (purely structural matching).
    pub temporal_delta: Timestamp,

    /// Human-readable label (e.g., "lateral-movement", "cascade-failure").
    pub label: [u8; 32],
    /// Length of the label.
    pub label_len: u8,
}

impl PatternGraph {
    /// Create an empty pattern.
    pub const fn empty() -> Self {
        Self {
            canonical_hash: 0,
            nodes: [PatternNode::EMPTY; PATTERN_MAX_NODES],
            node_count: 0,
            edges: [PatternEdge::EMPTY; PATTERN_MAX_EDGES],
            edge_count: 0,
            temporal_delta: 0,
            label: [0u8; 32],
            label_len: 0,
        }
    }

    /// Add a node constraint to the pattern.
    /// Returns the node index, or `None` if full.
    pub fn add_node(&mut self, kind: NodeKind, required_flags: u16) -> Option<u8> {
        if self.node_count as usize >= PATTERN_MAX_NODES {
            return None;
        }
        let idx = self.node_count;
        self.nodes[idx as usize] = PatternNode {
            kind,
            required_flags,
        };
        self.node_count += 1;
        Some(idx)
    }

    /// Add an edge constraint between two pattern nodes.
    /// Returns `true` if added, `false` if full or indices out of range.
    pub fn add_edge(
        &mut self,
        from_idx: u8,
        to_idx: u8,
        kind: EdgeKind,
        min_weight: Weight,
    ) -> bool {
        if self.edge_count as usize >= PATTERN_MAX_EDGES {
            return false;
        }
        if from_idx >= self.node_count || to_idx >= self.node_count {
            return false;
        }
        self.edges[self.edge_count as usize] = PatternEdge {
            from_idx,
            to_idx,
            kind,
            min_weight,
        };
        self.edge_count += 1;
        true
    }

    /// Set the pattern label from a byte slice.
    pub fn set_label(&mut self, name: &[u8]) {
        let len = if name.len() > 32 { 32 } else { name.len() };
        self.label[..len].copy_from_slice(&name[..len]);
        self.label_len = len as u8;
    }
}

// ────────────────────────────────────────────────────────────────────
// Match result
// ────────────────────────────────────────────────────────────────────

/// The result of a successful subgraph isomorphism match.
///
/// Maps each pattern node index to a concrete NodeId in the arena.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MatchResult {
    /// The canonical hash of the matched pattern.
    pub pattern_hash: u64,
    /// Arena generation when the match was found.
    pub generation: u64,
    /// Mapped node IDs: mapping[pattern_idx] = arena NodeId.
    pub mapping: [NodeId; PATTERN_MAX_NODES],
    /// Number of nodes in the mapping (= pattern.node_count).
    pub mapping_len: u8,
    /// Temporal span: (earliest edge timestamp, latest edge timestamp)
    /// in the matched subgraph.
    pub time_min: Timestamp,
    pub time_max: Timestamp,
}

impl MatchResult {
    pub const EMPTY: Self = Self {
        pattern_hash: 0,
        generation: 0,
        mapping: [0; PATTERN_MAX_NODES],
        mapping_len: 0,
        time_min: 0,
        time_max: 0,
    };

    /// Temporal span of the match.
    pub const fn temporal_span(&self) -> u64 {
        self.time_max.saturating_sub(self.time_min)
    }
}

// ────────────────────────────────────────────────────────────────────
// Compression metrics (MDL)
// ────────────────────────────────────────────────────────────────────

/// MDL compression accounting for a set of patterns.
///
/// L(G, M) = L(M) + L(G|M)
///
/// - L(M) = total description length of the pattern model
/// - L(G|M) = residual graph length after pattern substitution
///
/// The compression ratio = L(G,M) / L(G,∅) should decrease as useful
/// patterns are discovered. Mining is triggered when ratio > 0.85.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CompressionMetrics {
    /// L(G,∅): description length of the raw graph (no patterns).
    /// Approximated as (node_count × node_bits) + (edge_count × edge_bits).
    pub raw_length: u64,

    /// L(M): total description length of discovered patterns.
    pub model_length: u64,

    /// L(G|M): residual graph length after pattern substitution.
    pub residual_length: u64,

    /// Number of discovered patterns.
    pub pattern_count: u32,

    /// Total instances matched across all patterns.
    pub total_instances: u32,
}

impl CompressionMetrics {
    pub const ZERO: Self = Self {
        raw_length: 0,
        model_length: 0,
        residual_length: 0,
        pattern_count: 0,
        total_instances: 0,
    };

    /// Total compressed length: L(M) + L(G|M).
    pub const fn compressed_length(&self) -> u64 {
        self.model_length + self.residual_length
    }

    /// Compression ratio: L(G,M) / L(G,∅).
    /// Returns 16.16 fixed-point. 65536 = 1.0 (no compression).
    /// Lower is better.
    pub const fn ratio_fp(&self) -> Weight {
        if self.raw_length == 0 {
            return WEIGHT_ONE;
        }
        let numer = self.compressed_length() * WEIGHT_ONE as u64;
        (numer / self.raw_length) as u32
    }

    /// Returns `true` if the ratio exceeds the mining trigger threshold (0.85).
    /// 0.85 × 65536 ≈ 55706.
    pub const fn should_trigger_mining(&self) -> bool {
        self.ratio_fp() > 55706
    }
}

// ────────────────────────────────────────────────────────────────────
// WL label (for graph kernel computation)
// ────────────────────────────────────────────────────────────────────

/// A Weisfeiler-Leman label for a node at a specific iteration depth.
///
/// The temporal WL extension:
///   ℓ'(v) = (ℓ(v), ⌊t_v / Δ⌋)
///
/// adds a time bucket to the label, giving temporal discrimination.
///
/// 16 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WlLabel {
    /// The refined label hash at the current WL iteration.
    pub hash: u64,
    /// Time bucket: ⌊t / Δ⌋. For iteration 0, this is the node's
    /// creation timestamp divided by the bucket width.
    pub time_bucket: u32,
    /// WL iteration depth at which this label was computed.
    pub iteration: u16,
    /// The NodeKind of the labeled node (base label at iteration 0).
    pub kind: NodeKind,
}

impl WlLabel {
    pub const EMPTY: Self = Self {
        hash: 0,
        time_bucket: 0,
        iteration: 0,
        kind: NodeKind::Kernel,
    };

    /// Create the initial (iteration 0) label for a node.
    ///
    /// `bucket_width` is Δ in ticks. If 0, time bucketing is disabled.
    pub const fn initial(kind: NodeKind, created_at: Timestamp, bucket_width: u64) -> Self {
        let time_bucket = match created_at.checked_div(bucket_width) {
            Some(bucket) => bucket as u32,
            None => 0,
        };
        // Initial hash: combine kind and time bucket.
        let hash = (kind as u64) ^ ((time_bucket as u64) << 32);
        Self {
            hash,
            time_bucket,
            iteration: 0,
            kind,
        }
    }
}

/// Simple FNV-1a hash for combining WL neighbor labels.
///
/// Used in the WL refinement step:
///   ℓ'(v) = hash(ℓ(v), multiset{ℓ(u) : u ∈ N(v)})
///
/// This is a 64-bit FNV-1a — not cryptographic, but sufficient for
/// label discrimination in the WL test.
pub const fn fnv1a_combine(base: u64, value: u64) -> u64 {
    const FNV_PRIME: u64 = 0x00000100000001B3;
    let mut h = base;
    // Feed each byte of `value`.
    let bytes = value.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}
