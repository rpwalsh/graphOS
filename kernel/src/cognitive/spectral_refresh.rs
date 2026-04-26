// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Spectral refresh pipeline — Lanczos-based eigenvalue snapshot generation.
//!
//! This module bridges the graph arena and the Lanczos engine to produce
//! `SpectralSnapshot` records for the spectral tracking subsystem.
//!
//! ## Pipeline
//!
//! 1. Extract a compact adjacency from the graph arena (nodes + weighted edges).
//! 2. Compute weighted degrees.
//! 3. Build the normalised Laplacian in CSR format.
//! 4. Run Lanczos iteration to extract the k smallest eigenvalues.
//! 5. Construct a SpectralSnapshot and record it.
//!
//! ## When to refresh
//!
//! The refresh should be triggered after significant graph mutations
//! (e.g. after an indexing batch, or when the generation counter has
//! advanced by a configurable delta).

use crate::cognitive::lanczos::{LanczosEngine, LaplacianCsr};
use crate::graph::spectral::{SPECTRAL_K, SpectralSnapshot};
use crate::graph::types::*;
use crate::graph::{arena, spectral};

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum nodes for a spectral refresh.
const MAX_REFRESH_NODES: usize = 4096;

/// Maximum edges for extraction.
const MAX_REFRESH_EDGES: usize = 16384;

/// Number of Lanczos steps.
const LANCZOS_STEPS: usize = 48;

// ────────────────────────────────────────────────────────────────────
// Refresh
// ────────────────────────────────────────────────────────────────────

/// Result of a spectral refresh.
pub struct RefreshResult {
    /// Whether the refresh succeeded.
    pub success: bool,
    /// Number of nodes in the subgraph.
    pub node_count: u32,
    /// Number of edges in the subgraph.
    pub edge_count: u32,
    /// Number of Lanczos steps performed.
    pub lanczos_steps: u32,
    /// Whether the CUSUM alarm fired.
    pub cusum_alarm: bool,
}

/// Run a spectral refresh on the current graph state.
///
/// Extracts the knowledge subgraph (Document, Span, Chunk, Entity, Mention,
/// Task node kinds), builds the normalised Laplacian, runs Lanczos, and
/// records a SpectralSnapshot.
///
/// `lanczos`: mutable Lanczos engine (caller-owned to avoid static allocation).
/// `laplacian`: mutable Laplacian CSR (caller-owned).
pub fn refresh(lanczos: &mut LanczosEngine, laplacian: &mut LaplacianCsr) -> RefreshResult {
    let mut result = RefreshResult {
        success: false,
        node_count: 0,
        edge_count: 0,
        lanczos_steps: 0,
        cusum_alarm: false,
    };

    // Step 1: Extract node IDs from the arena.
    // We take SCCE provenance node types plus Task.
    let interesting_kinds: [NodeKind; 6] = [
        NodeKind::Document,
        NodeKind::Span,
        NodeKind::Chunk,
        NodeKind::Entity,
        NodeKind::Mention,
        NodeKind::Task,
    ];

    let mut node_ids = [0u64; MAX_REFRESH_NODES];
    let mut node_count = 0usize;

    // Iterate all live nodes by index and filter by kind.
    let total_nodes = arena::node_count();
    let mut ni = 0;
    while ni < total_nodes && node_count < MAX_REFRESH_NODES {
        if let Some(kind) = arena::node_kind_at_index(ni) {
            let mut ki = 0;
            let mut matched = false;
            while ki < interesting_kinds.len() {
                if kind as u16 == interesting_kinds[ki] as u16 {
                    matched = true;
                    break;
                }
                ki += 1;
            }
            if matched && let Some(nid) = arena::node_id_at_index(ni) {
                node_ids[node_count] = nid;
                node_count += 1;
            }
        }
        ni += 1;
    }
    result.node_count = node_count as u32;

    if node_count < 2 {
        // Need at least 2 nodes for meaningful spectral analysis.
        return result;
    }

    // Build a compact ID → index mapping.
    // Simple linear scan (n ≤ 4096).
    let n = node_count;

    // Step 2: Extract edges and compute degrees.
    //
    // We cannot capture &mut locals inside the FnMut closure passed to
    // arena::edges_from (it borrows the arena lock).  Instead we collect
    // edges into a local buffer per-node by reading edges one node at a
    // time through the arena's public get_node + edges_from API.
    let mut edges = [(0u16, 0u16, 0u32); MAX_REFRESH_EDGES];
    let mut edge_count = 0usize;
    let mut degrees = [0u32; MAX_REFRESH_NODES];

    // For each source node, collect outgoing edges into a stack-local
    // mini-buffer, then copy into the main edges array.  This avoids
    // the borrow issue with the closure capturing &mut edge_count etc.
    let mut from_idx = 0;
    while from_idx < n {
        let from_id = node_ids[from_idx];

        // Collect up to 256 outgoing edges for this node.
        const PER_NODE_CAP: usize = 256;
        let mut local_to = [0u64; PER_NODE_CAP];
        let mut local_w = [0u32; PER_NODE_CAP];
        let mut local_count = 0usize;

        arena::edges_from(from_id, |edge| {
            if local_count < PER_NODE_CAP {
                local_to[local_count] = edge.to;
                local_w[local_count] = edge.weight;
                local_count += 1;
            }
        });

        // Map each collected edge to our compact index set.
        let mut li = 0;
        while li < local_count {
            let to_id = local_to[li];
            let weight = local_w[li];
            // Find to_id in our node set.
            let mut ti = 0;
            while ti < n {
                if node_ids[ti] == to_id {
                    if edge_count < MAX_REFRESH_EDGES {
                        edges[edge_count] = (from_idx as u16, ti as u16, weight);
                        edge_count += 1;
                        degrees[from_idx] = degrees[from_idx].saturating_add(weight);
                    }
                    break;
                }
                ti += 1;
            }
            li += 1;
        }

        from_idx += 1;
    }
    result.edge_count = edge_count as u32;

    if edge_count == 0 {
        return result;
    }

    // Step 3: Build normalised Laplacian.
    laplacian.build(n, &degrees[..n], &edges[..edge_count]);
    if laplacian.n == 0 {
        return result; // build failed (too many NNZ)
    }

    // Step 4: Run Lanczos.
    *lanczos = LanczosEngine::new();
    lanczos.tridiagonalise(laplacian, LANCZOS_STEPS);
    result.lanczos_steps = lanczos.eigenvalue_count as u32;

    // Step 5: Build SpectralSnapshot.
    let mut eigenvalues = [0u32; SPECTRAL_K];
    let k = lanczos.smallest_k(&mut eigenvalues, SPECTRAL_K);

    let mut gaps = [0u32; SPECTRAL_K];
    let mut gi = 0;
    while gi + 1 < k {
        gaps[gi] = eigenvalues[gi + 1].saturating_sub(eigenvalues[gi]);
        gi += 1;
    }

    let snapshot = SpectralSnapshot {
        generation: arena::generation(),
        eigenvalues,
        gaps,
        total_weight: arena::total_weight(),
        node_count: n as u32,
        edge_count: edge_count as u32,
    };

    result.cusum_alarm = spectral::record_snapshot(snapshot);
    result.success = true;
    result
}
