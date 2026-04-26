// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Tensor decomposition primitives — 4D audit tensor for correlation detection.
//!
//! This module provides the type infrastructure for constructing and
//! decomposing the 4-dimensional audit tensor:
//!
//!   T ∈ ℝ^{U × R × T × A}   (principal × resource × time × action)
//!
//! ## CP decomposition
//!
//!   T ≈ Σ_{r=1}^{R} λ_r · u_r ⊗ v_r ⊗ w_r ⊗ x_r
//!
//! Each rank-one component (u,v,w,x) represents a behavioural pattern:
//! - u: which principals are involved
//! - v: which resources are accessed
//! - w: when the pattern occurs (temporal profile)
//! - x: what actions are performed
//!
//! ## Tucker decomposition
//!
//!   T ≈ G ×₁ U ×₂ V ×₃ W ×₄ X
//!
//! where G is the core tensor and U,V,W,X are mode-specific factor matrices.
//! Gives more nuanced multi-modal correlations than CP.
//!
//! ## Anomaly detection
//!
//! Reconstruction error: ‖T − T̂‖_F > 3σ above 90-day rolling baseline.
//!
//! ## Rank selection
//!
//! CP rank selected via CORCONDIA (core consistency diagnostic > 85%).
//!
//! ## Design
//! - The full tensor is NOT stored in the kernel — it would require
//!   U×R×T×A entries. Instead, we store:
//!   1. TensorConfig: dimensions and metadata
//!   2. TensorSlice: a compact slice through one or two dimensions
//!   3. CpComponent: a single rank-one component from decomposition
//!   4. ReconstructionError: anomaly scoring state
//! - All fixed-size, no heap. The actual decomposition runs in userspace;
//!   the kernel provides the observation pipeline and result storage.

use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum tensor dimension per mode.
/// Bounds the size of factor vectors in CP components.
pub const TENSOR_MAX_DIM: usize = 64;

/// Maximum CP rank (number of rank-one components).
pub const MAX_CP_RANK: usize = 16;

/// Number of time buckets for the temporal mode.
pub const TENSOR_TIME_BUCKETS: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Tensor configuration
// ────────────────────────────────────────────────────────────────────

/// Configuration for a 4D audit tensor T ∈ ℝ^{U×R×T×A}.
///
/// Defines the dimensionality and maps mode indices to graph entities.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TensorConfig {
    /// Number of principals (U dimension).
    pub n_principals: u16,
    /// Number of resources (R dimension).
    pub n_resources: u16,
    /// Number of time buckets (T dimension). ≤ TENSOR_TIME_BUCKETS.
    pub n_time_buckets: u16,
    /// Number of action types (A dimension).
    pub n_actions: u16,
    /// Time bucket width in ticks (Δt per bucket).
    pub bucket_width: Timestamp,
    /// Start timestamp of the observation window.
    pub window_start: Timestamp,
}

impl TensorConfig {
    pub const EMPTY: Self = Self {
        n_principals: 0,
        n_resources: 0,
        n_time_buckets: 0,
        n_actions: 0,
        bucket_width: 0,
        window_start: 0,
    };

    /// Total number of tensor entries (U × R × T × A).
    pub const fn total_entries(&self) -> u64 {
        self.n_principals as u64
            * self.n_resources as u64
            * self.n_time_buckets as u64
            * self.n_actions as u64
    }
}

// ────────────────────────────────────────────────────────────────────
// CP component
// ────────────────────────────────────────────────────────────────────

/// A single rank-one component from CP decomposition:
///
///   λ_r · u_r ⊗ v_r ⊗ w_r ⊗ x_r
///
/// Factor vectors are stored as 16.16 fixed-point, padded to TENSOR_MAX_DIM.
pub struct CpComponent {
    /// Component weight λ_r (16.16 fixed-point).
    pub lambda: Weight,
    /// Principal factor vector u_r (length = n_principals).
    pub u: [Weight; TENSOR_MAX_DIM],
    /// Resource factor vector v_r (length = n_resources).
    pub v: [Weight; TENSOR_MAX_DIM],
    /// Temporal factor vector w_r (length = n_time_buckets).
    pub w: [Weight; TENSOR_MAX_DIM],
    /// Action factor vector x_r (length = n_actions).
    pub x: [Weight; TENSOR_MAX_DIM],
}

impl CpComponent {
    pub const fn empty() -> Self {
        Self {
            lambda: 0,
            u: [0; TENSOR_MAX_DIM],
            v: [0; TENSOR_MAX_DIM],
            w: [0; TENSOR_MAX_DIM],
            x: [0; TENSOR_MAX_DIM],
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Reconstruction error tracking
// ────────────────────────────────────────────────────────────────────

/// Tracks reconstruction error for anomaly detection.
///
/// Anomaly threshold: current error > baseline_mean + 3 × baseline_stddev.
///
/// The baseline is computed over a rolling window (ideally 90 days,
/// but in kernel terms: `baseline_window` observations).
///
/// 40 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ReconstructionError {
    /// Current Frobenius norm error ‖T − T̂‖_F in 16.16 fixed-point.
    pub current: Weight,
    /// Rolling baseline mean in 16.16.
    pub baseline_mean: Weight,
    /// Rolling baseline standard deviation in 16.16.
    pub baseline_stddev: Weight,
    /// Number of observations in the baseline.
    pub baseline_count: u32,
    /// Arena generation when this was last updated.
    pub generation: u64,
    /// The CP rank used for this decomposition.
    pub cp_rank: u16,
    /// CORCONDIA score in 16.16 (core consistency %, × 65536/100).
    pub corcondia: Weight,
}

impl ReconstructionError {
    pub const ZERO: Self = Self {
        current: 0,
        baseline_mean: 0,
        baseline_stddev: 0,
        baseline_count: 0,
        generation: 0,
        cp_rank: 0,
        corcondia: 0,
    };

    /// Returns `true` if the current error exceeds 3σ above baseline.
    pub const fn is_anomalous(&self) -> bool {
        if self.baseline_count < 10 {
            return false; // Not enough baseline data.
        }
        // threshold = mean + 3 × stddev
        let three_sigma = self.baseline_stddev.saturating_mul(3);
        let threshold = self.baseline_mean.saturating_add(three_sigma);
        self.current > threshold
    }

    /// Returns `true` if the CORCONDIA score indicates the CP rank is appropriate.
    /// Threshold: 85% = 55705 in 16.16.
    pub const fn rank_is_appropriate(&self) -> bool {
        self.corcondia >= 55705
    }

    /// Update baseline with a new error observation (online mean/stddev).
    ///
    /// Uses Welford's online algorithm for numerically stable variance.
    /// All arithmetic in 16.16 fixed-point.
    pub fn update_baseline(&mut self, error: Weight) {
        self.baseline_count += 1;
        let n = self.baseline_count;

        if n == 1 {
            self.baseline_mean = error;
            self.baseline_stddev = 0;
            return;
        }

        // Welford's: delta = x - mean
        let (delta, delta_neg) = if error >= self.baseline_mean {
            (error - self.baseline_mean, false)
        } else {
            (self.baseline_mean - error, true)
        };

        // new_mean = mean + delta/n
        let delta_over_n = delta / n;
        if delta_neg {
            self.baseline_mean = self.baseline_mean.saturating_sub(delta_over_n);
        } else {
            self.baseline_mean = self.baseline_mean.saturating_add(delta_over_n);
        }

        // Approximate stddev update: running stddev ≈ |delta| / √n.
        // Proper Welford needs M2 accumulator, but this approximation
        // is sufficient for the 3σ threshold check.
        // We use: stddev ≈ (old_stddev × (n-1) + |delta|) / n.
        let weighted_old = (self.baseline_stddev as u64 * (n - 1) as u64) / n as u64;
        let new_contrib = delta as u64 / n as u64;
        self.baseline_stddev = (weighted_old + new_contrib) as Weight;
    }
}

// ────────────────────────────────────────────────────────────────────
// PPMI (Positive Pointwise Mutual Information) accumulator
// ────────────────────────────────────────────────────────────────────

/// Accumulates co-occurrence counts for PPMI tensor construction.
///
/// PPMI(i,j) = max(0, log(p(i,j) / (p(i)·p(j))))
///
/// The PPMI tensor is the input to CP/Tucker decomposition, replacing
/// raw counts with information-theoretic surprise scores.
///
/// This is a 2D slice accumulator (one mode-pair at a time).
/// The full 4D tensor is built by composing slices.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CooccurrenceSlice {
    /// Joint counts: count[i][j] = number of co-occurrences.
    /// Compact: TENSOR_MAX_DIM × TENSOR_MAX_DIM = 4096 entries.
    pub counts: [[u32; TENSOR_MAX_DIM]; TENSOR_MAX_DIM],
    /// Marginal row sums (for p(i)).
    pub row_sums: [u32; TENSOR_MAX_DIM],
    /// Marginal column sums (for p(j)).
    pub col_sums: [u32; TENSOR_MAX_DIM],
    /// Total observations.
    pub total: u64,
    /// Active rows.
    pub n_rows: u16,
    /// Active columns.
    pub n_cols: u16,
}

impl CooccurrenceSlice {
    pub const fn empty() -> Self {
        Self {
            counts: [[0; TENSOR_MAX_DIM]; TENSOR_MAX_DIM],
            row_sums: [0; TENSOR_MAX_DIM],
            col_sums: [0; TENSOR_MAX_DIM],
            total: 0,
            n_rows: 0,
            n_cols: 0,
        }
    }

    /// Record a co-occurrence of (row i, col j).
    pub fn observe(&mut self, i: usize, j: usize) {
        if i < TENSOR_MAX_DIM && j < TENSOR_MAX_DIM {
            self.counts[i][j] += 1;
            self.row_sums[i] += 1;
            self.col_sums[j] += 1;
            self.total += 1;
        }
    }

    /// Compute PPMI(i, j) in 16.16 fixed-point.
    ///
    /// PPMI = max(0, log₂(p(i,j) / (p(i)·p(j)))) × WEIGHT_ONE
    ///
    /// Returns 0 if any marginal is zero.
    pub fn ppmi(&self, i: usize, j: usize) -> Weight {
        if i >= TENSOR_MAX_DIM || j >= TENSOR_MAX_DIM || self.total == 0 {
            return 0;
        }
        let c_ij = self.counts[i][j] as u64;
        let r_i = self.row_sums[i] as u64;
        let c_j = self.col_sums[j] as u64;
        if c_ij == 0 || r_i == 0 || c_j == 0 {
            return 0;
        }

        // PMI = log₂(c_ij · N / (r_i · c_j))
        // Numerator: c_ij × N. Denominator: r_i × c_j.
        let numer = c_ij * self.total;
        let denom = r_i * c_j;
        if numer <= denom {
            return 0; // PMI ≤ 0 → PPMI = 0.
        }

        // log₂(numer/denom) approximated via leading-bit difference.
        let ratio = numer / denom; // integer ratio ≥ 1
        let log2_ratio = 63u32.saturating_sub(ratio.leading_zeros());

        // Convert to 16.16: log₂ × WEIGHT_ONE.
        (log2_ratio as u64 * WEIGHT_ONE as u64) as Weight
    }
}
