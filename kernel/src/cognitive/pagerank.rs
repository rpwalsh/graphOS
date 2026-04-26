// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! PageRank via iterative power method on the knowledge subgraph.
//!
//! All arithmetic is 16.16 fixed-point.  No floating point.
//!
//! ## Algorithm
//!
//!   PR(v) = (1 − d)/N + d · Σ_{u→v} PR(u) / out_degree(u)
//!
//! where d = 0.85 is the damping factor.
//!
//! We iterate until convergence (L1 change < ε) or max iterations.
//!
//! ## Design
//! - Operates on a compact adjacency extracted from the kernel graph arena.
//! - Fixed-size buffers: up to MAX_RANK_NODES nodes and MAX_RANK_EDGES edges.
//! - Two score vectors (current + next) for in-place iteration.
//! - Returns scores in 16.16 fixed-point.

use crate::graph::types::Weight;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum number of nodes in the PageRank subgraph.
const MAX_RANK_NODES: usize = 4096;

/// Maximum number of edges in the PageRank subgraph.
const MAX_RANK_EDGES: usize = 16384;

/// Damping factor d = 0.85 in 16.16 = 55706
const DAMPING: u32 = 55706;

/// 1 - d = 0.15 in 16.16 = 9830
const ONE_MINUS_D: u32 = 9830;

/// 1.0 in 16.16
const FP_ONE: u32 = 1 << 16;

/// Convergence threshold: L1 total delta < ε.
/// ε = 0.001 in 16.16 = 66
const EPSILON: u32 = 66;

/// Maximum iterations.
const MAX_ITERATIONS: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Fixed-point helpers
// ────────────────────────────────────────────────────────────────────

fn fp_mul(a: u32, b: u32) -> u32 {
    ((a as u64 * b as u64) >> 16) as u32
}

fn fp_div(a: u32, b: u32) -> u32 {
    if b == 0 {
        return 0;
    }
    (((a as u64) << 16) / (b as u64)) as u32
}

// ────────────────────────────────────────────────────────────────────
// Compact adjacency for PageRank
// ────────────────────────────────────────────────────────────────────

/// A directed edge in the compact adjacency list.
#[derive(Clone, Copy)]
struct RankEdge {
    from: u16,
    to: u16,
    weight: Weight, // edge weight (unused in basic PageRank, used in weighted variant)
}

impl RankEdge {
    const EMPTY: Self = Self {
        from: 0,
        to: 0,
        weight: 0,
    };
}

/// PageRank computation engine with fixed-size buffers.
pub struct PageRankEngine {
    /// Compact edge list.
    edges: [RankEdge; MAX_RANK_EDGES],
    edge_count: usize,

    /// Out-degree of each node (needed for transition probability).
    out_degree: [u32; MAX_RANK_NODES],

    /// External node IDs mapped to compact indices.
    node_ids: [u64; MAX_RANK_NODES],
    node_count: usize,

    /// Current PageRank score vector (16.16 fixed-point).
    scores: [Weight; MAX_RANK_NODES],

    /// Scratch vector for next iteration.
    next_scores: [Weight; MAX_RANK_NODES],

    /// Number of iterations performed in last run.
    pub last_iterations: u32,

    /// Final L1 delta of last run (16.16).
    pub last_delta: Weight,
}

impl PageRankEngine {
    pub const fn new() -> Self {
        Self {
            edges: [RankEdge::EMPTY; MAX_RANK_EDGES],
            edge_count: 0,
            out_degree: [0u32; MAX_RANK_NODES],
            node_ids: [0u64; MAX_RANK_NODES],
            node_count: 0,
            scores: [0; MAX_RANK_NODES],
            next_scores: [0; MAX_RANK_NODES],
            last_iterations: 0,
            last_delta: 0,
        }
    }

    /// Reset the engine.
    pub fn clear(&mut self) {
        self.edge_count = 0;
        self.node_count = 0;
        let mut i = 0;
        while i < MAX_RANK_NODES {
            self.out_degree[i] = 0;
            self.scores[i] = 0;
            self.next_scores[i] = 0;
            self.node_ids[i] = 0;
            i += 1;
        }
        self.last_iterations = 0;
        self.last_delta = 0;
    }

    /// Add a node. Returns the compact index, or None if full.
    pub fn add_node(&mut self, external_id: u64) -> Option<u16> {
        // Check if already present.
        let mut i = 0;
        while i < self.node_count {
            if self.node_ids[i] == external_id {
                return Some(i as u16);
            }
            i += 1;
        }
        if self.node_count >= MAX_RANK_NODES {
            return None;
        }
        let idx = self.node_count as u16;
        self.node_ids[self.node_count] = external_id;
        self.node_count += 1;
        Some(idx)
    }

    /// Add a directed edge. Both endpoints must already be added.
    pub fn add_edge(&mut self, from: u16, to: u16, weight: Weight) -> bool {
        if self.edge_count >= MAX_RANK_EDGES {
            return false;
        }
        if from as usize >= self.node_count || to as usize >= self.node_count {
            return false;
        }
        self.edges[self.edge_count] = RankEdge { from, to, weight };
        self.edge_count += 1;
        self.out_degree[from as usize] += 1;
        true
    }

    /// Run PageRank iteration.  Returns the number of iterations performed.
    pub fn compute(&mut self) -> u32 {
        let n = self.node_count;
        if n == 0 {
            return 0;
        }

        // Initialise scores to 1/N.
        let init_score = fp_div(FP_ONE, n as u32);
        let mut i = 0;
        while i < n {
            self.scores[i] = init_score;
            i += 1;
        }

        let teleport = fp_div(ONE_MINUS_D, n as u32); // (1-d)/N

        let mut iter = 0u32;
        while (iter as usize) < MAX_ITERATIONS {
            // Zero next_scores.
            let mut i = 0;
            while i < n {
                self.next_scores[i] = teleport;
                i += 1;
            }

            // Distribute score along edges.
            let mut ei = 0;
            while ei < self.edge_count {
                let e = &self.edges[ei];
                let from = e.from as usize;
                let to = e.to as usize;
                let od = self.out_degree[from];
                if od > 0 {
                    // contribution = d * PR(from) / out_degree(from)
                    let share = fp_div(self.scores[from], od);
                    let contrib = fp_mul(DAMPING, share);
                    self.next_scores[to] = self.next_scores[to].saturating_add(contrib);
                }
                ei += 1;
            }

            // Handle dangling nodes (out_degree = 0): redistribute their
            // mass equally.  dangling_sum = d * Σ PR(v) for v with od=0.
            let mut dangling_sum = 0u64;
            let mut i = 0;
            while i < n {
                if self.out_degree[i] == 0 {
                    dangling_sum += self.scores[i] as u64;
                }
                i += 1;
            }
            if dangling_sum > 0 {
                // dangling_share = d * dangling_sum / N
                let ds = ((dangling_sum * DAMPING as u64) >> 16) / (n as u64);
                let ds32 = ds as u32;
                let mut i = 0;
                while i < n {
                    self.next_scores[i] = self.next_scores[i].saturating_add(ds32);
                    i += 1;
                }
            }

            // Compute L1 delta and swap.
            let mut delta = 0u64;
            let mut i = 0;
            while i < n {
                let d = self.next_scores[i].abs_diff(self.scores[i]);
                delta += d as u64;
                self.scores[i] = self.next_scores[i];
                i += 1;
            }

            iter += 1;
            let delta32 = if delta > u32::MAX as u64 {
                u32::MAX
            } else {
                delta as u32
            };
            self.last_delta = delta32;

            if delta32 < EPSILON {
                break;
            }
        }

        self.last_iterations = iter;
        iter
    }

    /// Get the PageRank score for compact node index `i` (16.16).
    pub fn score(&self, i: usize) -> Weight {
        if i < self.node_count {
            self.scores[i]
        } else {
            0
        }
    }

    /// Get the external node ID for compact index `i`.
    pub fn external_id(&self, i: usize) -> u64 {
        if i < self.node_count {
            self.node_ids[i]
        } else {
            0
        }
    }

    /// Fill `out` with top-K nodes sorted by descending score.
    /// Returns the number of results.
    pub fn top_k(&self, out: &mut [(u64, Weight)]) -> usize {
        let k = out.len();
        let mut written = 0;
        let mut used = [false; MAX_RANK_NODES];
        while written < k {
            let mut best_idx = usize::MAX;
            let mut best_score = 0u32;
            let mut i = 0;
            while i < self.node_count {
                if !used[i] && self.scores[i] > best_score {
                    best_score = self.scores[i];
                    best_idx = i;
                }
                i += 1;
            }
            if best_idx == usize::MAX || best_score == 0 {
                break;
            }
            used[best_idx] = true;
            out[written] = (self.node_ids[best_idx], best_score);
            written += 1;
        }
        written
    }

    pub fn node_count(&self) -> usize {
        self.node_count
    }
    pub fn edge_count(&self) -> usize {
        self.edge_count
    }
}
