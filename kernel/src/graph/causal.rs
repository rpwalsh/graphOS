// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Causal inference primitives — Granger causality, transfer entropy, RCA.
//!
//! This module provides the type-level infrastructure for causal analysis
//! over the kernel graph's temporal edge stream.
//!
//! ## Granger causality (VAR-based F-test)
//!
//! Time series X Granger-causes Y if past values of X improve prediction
//! of Y beyond what Y's own past provides. Discovered via:
//! 1. Fit VAR(p) restricted model: Y_t = Σ_{k=1}^p A_k Y_{t-k} + ε
//! 2. Fit VAR(p) unrestricted model: Y_t = Σ A_k Y_{t-k} + Σ B_k X_{t-k} + ε'
//! 3. F-test: F = ((RSS_r − RSS_u)/p) / (RSS_u/(T−2p−1))
//! 4. Benjamini-Hochberg FDR correction at q = 0.1 for O(n²) tests
//!
//! Complexity: O(n²T) for all pairs.
//!
//! ## Transfer entropy (model-free)
//!
//!   T_{X→Y} = H(Y_t | Y_{<t}) − H(Y_t | Y_{<t}, X_{<t})
//!
//! Conditional mutual information — detects non-linear causal relationships.
//! Complexity: O(n²T²).
//!
//! ## Root Cause Analysis (Perron-Frobenius)
//!
//! Given a causal DAG, the Perron-Frobenius stationary distribution π
//! over the causal transition matrix identifies root cause nodes:
//! nodes with high π(v) are disproportionately upstream of observed effects.
//!
//! ## PCMCI (for autocorrelated telemetry)
//!
//! PC algorithm + Momentary Conditional Independence — removes
//! autocorrelation confounds that cause spurious Granger results.
//!
//! ## Design
//! - CausalLink stores a directed causal relationship with strength/lag.
//! - CausalGraph is a small adjacency matrix for n ≤ 32 variables.
//! - VarAccumulator collects time-series observations for VAR fitting.
//! - All fixed-size, no heap.

use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum number of causal variables tracked simultaneously.
/// 32 is sufficient for subsystem-level causal analysis (CPU, memory,
/// disk, network, service dependencies).
pub const MAX_CAUSAL_VARS: usize = 32;

/// Maximum VAR lag order (p in VAR(p)).
pub const MAX_VAR_LAG: usize = 8;

/// Maximum time-series history length per variable.
pub const MAX_HISTORY: usize = 256;

// ────────────────────────────────────────────────────────────────────
// Causal link
// ────────────────────────────────────────────────────────────────────

/// A directed causal relationship between two graph nodes.
///
/// Discovered by Granger F-test or transfer entropy analysis.
/// The link is stored in the arena as a GrangerCauses or TransferEntropy
/// edge; this struct captures the analysis metadata.
///
/// 32 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CausalLink {
    /// Source node (the cause).
    pub from: NodeId,
    /// Destination node (the effect).
    pub to: NodeId,
    /// Strength of the causal link in 16.16 fixed-point.
    /// For Granger: 1 − p-value (higher = more significant).
    /// For transfer entropy: normalised T_{X→Y}.
    pub strength: Weight,
    /// Optimal lag (in ticks) at which the causal effect is strongest.
    pub lag: u32,
}

impl CausalLink {
    pub const EMPTY: Self = Self {
        from: 0,
        to: 0,
        strength: 0,
        lag: 0,
    };

    /// Returns `true` if this link passes the significance threshold.
    /// Default: strength ≥ 0.9 (i.e., p-value ≤ 0.1 after FDR correction).
    /// 0.9 × 65536 ≈ 58982.
    pub const fn is_significant(&self) -> bool {
        self.strength >= 58982
    }
}

// ────────────────────────────────────────────────────────────────────
// Causal adjacency matrix
// ────────────────────────────────────────────────────────────────────

/// A small causal graph represented as an adjacency matrix.
///
/// `matrix[i][j]` = strength of causal link from variable i to variable j.
/// 0 = no link. Variables are mapped to NodeIds via `var_nodes`.
pub struct CausalGraph {
    /// Maps variable index → NodeId.
    pub var_nodes: [NodeId; MAX_CAUSAL_VARS],
    /// Number of active variables.
    pub var_count: u8,
    /// Causal adjacency matrix: matrix[from][to] = strength (16.16).
    pub matrix: [[Weight; MAX_CAUSAL_VARS]; MAX_CAUSAL_VARS],
    /// Lag matrix: lag[from][to] = optimal lag in ticks.
    pub lag: [[u32; MAX_CAUSAL_VARS]; MAX_CAUSAL_VARS],
}

impl CausalGraph {
    /// Create an empty causal graph.
    pub const fn empty() -> Self {
        Self {
            var_nodes: [0; MAX_CAUSAL_VARS],
            var_count: 0,
            matrix: [[0; MAX_CAUSAL_VARS]; MAX_CAUSAL_VARS],
            lag: [[0; MAX_CAUSAL_VARS]; MAX_CAUSAL_VARS],
        }
    }

    /// Register a variable (NodeId) in the causal graph.
    /// Returns the variable index, or `None` if full.
    pub fn add_variable(&mut self, node: NodeId) -> Option<u8> {
        if self.var_count as usize >= MAX_CAUSAL_VARS {
            return None;
        }
        let idx = self.var_count;
        self.var_nodes[idx as usize] = node;
        self.var_count += 1;
        Some(idx)
    }

    /// Set a causal link.
    pub fn set_link(&mut self, from_idx: u8, to_idx: u8, strength: Weight, optimal_lag: u32) {
        let f = from_idx as usize;
        let t = to_idx as usize;
        if f < MAX_CAUSAL_VARS && t < MAX_CAUSAL_VARS {
            self.matrix[f][t] = strength;
            self.lag[f][t] = optimal_lag;
        }
    }

    /// Get the causal strength from variable i to variable j.
    pub fn get_strength(&self, from_idx: u8, to_idx: u8) -> Weight {
        let f = from_idx as usize;
        let t = to_idx as usize;
        if f < MAX_CAUSAL_VARS && t < MAX_CAUSAL_VARS {
            self.matrix[f][t]
        } else {
            0
        }
    }

    /// Count the number of significant causal links (strength ≥ threshold).
    pub fn significant_link_count(&self, threshold: Weight) -> usize {
        let mut count = 0;
        for i in 0..self.var_count as usize {
            for j in 0..self.var_count as usize {
                if i != j && self.matrix[i][j] >= threshold {
                    count += 1;
                }
            }
        }
        count
    }
}

// ────────────────────────────────────────────────────────────────────
// VAR time-series accumulator
// ────────────────────────────────────────────────────────────────────

/// Ring-buffer accumulator for a single variable's time-series observations.
///
/// Stores the most recent MAX_HISTORY values. Used as input for
/// Granger causality VAR fitting.
///
/// Values are 16.16 fixed-point.
pub struct VarAccumulator {
    /// Observation ring buffer.
    pub values: [Weight; MAX_HISTORY],
    /// Write index (wraps around).
    pub write_idx: usize,
    /// Total observations ever recorded.
    pub total: u64,
}

impl VarAccumulator {
    pub const fn new() -> Self {
        Self {
            values: [0; MAX_HISTORY],
            write_idx: 0,
            total: 0,
        }
    }

    /// Push a new observation.
    pub fn push(&mut self, value: Weight) {
        self.values[self.write_idx] = value;
        self.write_idx = (self.write_idx + 1) % MAX_HISTORY;
        self.total += 1;
    }

    /// Number of available observations (capped at MAX_HISTORY).
    pub fn available(&self) -> usize {
        if self.total >= MAX_HISTORY as u64 {
            MAX_HISTORY
        } else {
            self.total as usize
        }
    }

    /// Get the observation at offset `steps_back` from the most recent.
    /// 0 = most recent, 1 = one before, etc.
    /// Returns `None` if offset exceeds available history.
    pub fn get(&self, steps_back: usize) -> Option<Weight> {
        if steps_back >= self.available() {
            return None;
        }
        let idx = if self.write_idx > steps_back {
            self.write_idx - 1 - steps_back
        } else {
            MAX_HISTORY - 1 - (steps_back - self.write_idx)
        };
        Some(self.values[idx])
    }

    /// Compute the mean of the last `window` observations in 16.16.
    pub fn mean(&self, window: usize) -> Weight {
        let n = core::cmp::min(window, self.available());
        if n == 0 {
            return 0;
        }
        let mut sum: u64 = 0;
        for i in 0..n {
            if let Some(v) = self.get(i) {
                sum += v as u64;
            }
        }
        (sum / n as u64) as Weight
    }
}

// ────────────────────────────────────────────────────────────────────
// Benjamini-Hochberg FDR correction
// ────────────────────────────────────────────────────────────────────

/// Apply Benjamini-Hochberg FDR correction to a set of p-values.
///
/// `p_values` is an array of (1 − strength) values in 16.16 fixed-point.
/// `count` is the number of valid entries.
/// `q` is the FDR threshold in 16.16 (default: 0.1 ≈ 6554).
///
/// Returns a bitmask where bit `i` is set if hypothesis `i` survives
/// correction. Supports up to 64 hypotheses.
///
/// The BH procedure:
/// 1. Sort p-values ascending.
/// 2. Find largest k such that p_(k) ≤ k·q/m.
/// 3. Reject hypotheses 1..k.
pub fn benjamini_hochberg(p_values: &[Weight], count: usize, q_fp: Weight) -> u64 {
    if count == 0 || count > 64 {
        return 0;
    }

    // Create index-sorted pairs (p_value, original_index).
    let mut sorted: [(Weight, usize); 64] = [(0, 0); 64];
    for i in 0..count {
        sorted[i] = (p_values[i], i);
    }
    // Simple insertion sort (count ≤ 64).
    for i in 1..count {
        let key = sorted[i];
        let mut j = i;
        while j > 0 && sorted[j - 1].0 > key.0 {
            sorted[j] = sorted[j - 1];
            j -= 1;
        }
        sorted[j] = key;
    }

    // Find largest k such that p_(k) ≤ k·q/m.
    let m = count as u64;
    let mut last_significant = 0usize; // 0 means none found.
    for k in 1..=count {
        // Threshold: k·q/m in 16.16.
        let threshold = (k as u64 * q_fp as u64) / m;
        if (sorted[k - 1].0 as u64) <= threshold {
            last_significant = k;
        }
    }

    // Build rejection bitmask.
    let mut mask: u64 = 0;
    for (_, original_idx) in sorted.iter().take(last_significant) {
        mask |= 1u64 << original_idx;
    }
    mask
}
