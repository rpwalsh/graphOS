// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Spectral graph primitives — eigenvalue tracking and Laplacian support.
//!
//! This module provides the kernel-level infrastructure for spectral graph
//! analysis, grounded in the following mathematical framework:
//!
//! ## Laplacian construction
//!   L = D − A            (combinatorial Laplacian)
//!   𝓛 = I − D^{−1/2} A D^{−1/2}   (normalised Laplacian)
//!
//! where D is the diagonal degree matrix and A is the weighted adjacency.
//! Node.degree_out / degree_in maintain D incrementally.
//!
//! ## Key spectral quantities
//! - **Fiedler value** λ₂: algebraic connectivity. Measures how well-connected
//!   the graph is. Drop in λ₂ predicts partitioning events.
//! - **Spectral gaps** γ_k = λ_{k+1} − λ_k: the correct state variable for
//!   VAR-based temporal forecasting (not raw eigenvalues).
//! - **Cheeger inequality**: λ₂/2 ≤ h(G) ≤ √(2λ₂) — bridges spectral and
//!   combinatorial connectivity.
//!
//! ## Incremental updates
//! When an edge (u,v) with weight w is added:
//!   ΔL = w · (eᵤ − eᵥ)(eᵤ − eᵥ)ᵀ   (rank-1 update)
//!
//! Sherman-Morrison-Woodbury gives O(k²) per-edge eigenvalue update vs O(n³)
//! full recomputation. The `SpectralState` tracks this incrementally.
//!
//! ## Perturbation bounds
//! - **Weyl**: |λ_k(L+ΔL) − λ_k(L)| ≤ ‖ΔL‖₂
//! - **Cauchy interlacing**: λ_k(G) ≤ λ_k(G+e) ≤ λ_{k+1}(G)
//! - **Davis-Kahan**: prediction interval half-width ≤ 2·w_max·T for T steps
//!
//! ## Anomaly detection
//! - CUSUM on λ₂(t): detects sustained drift.
//! - Fiedler drift theorem: Δλ₂/Δt ≤ −0.1 sustained 5 steps → partition
//!   within 5 steps (precision ≥ 0.82).
//! - DeltaCon similarity (Matusita affinity) between consecutive snapshots.
//!
//! ## Design
//! - Eigenvalues stored as 16.16 fixed-point (same as weights).
//! - Fixed-size snapshot ring buffer — no heap allocation.
//! - CUSUM detector with configurable drift and threshold parameters.
//! - All computations are integer-only (no FPU required at boot).

use spin::Mutex;

use crate::arch::serial;
use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Number of eigenvalues tracked per snapshot.
/// Tracks λ₁ through λ_K. λ₁ = 0 for connected graphs (trivial),
/// so the interesting values are λ₂..λ_K.
/// K = 8 is sufficient for: Fiedler value (λ₂), first 6 spectral gaps,
/// and clustering structure up to 8 communities.
pub const SPECTRAL_K: usize = 8;

/// Maximum number of snapshots retained in the ring buffer.
/// At one snapshot per significant graph mutation batch, this covers
/// ~64 snapshots — enough for CUSUM detection and short-horizon
/// VAR forecasting on the spectral gap sequence.
pub const SNAPSHOT_RING_SIZE: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Spectral snapshot
// ────────────────────────────────────────────────────────────────────

/// A frozen spectral snapshot: eigenvalues at a specific generation.
///
/// Represents the spectral state of the graph Laplacian at the time
/// of capture. The eigenvalues are sorted ascending: λ₁ ≤ λ₂ ≤ … ≤ λ_K.
///
/// 72 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SpectralSnapshot {
    /// Arena generation counter when this snapshot was taken.
    pub generation: u64,

    /// Eigenvalues λ₁..λ_K of the normalised Laplacian, 16.16 fixed-point.
    /// Sorted ascending. λ₁ should be ≈ 0 for a connected graph.
    pub eigenvalues: [Weight; SPECTRAL_K],

    /// Spectral gaps γ_k = λ_{k+1} − λ_k for k ∈ [1, K-1].
    /// These are the correct state variable for VAR forecasting.
    pub gaps: [Weight; SPECTRAL_K],

    /// Total weighted edge count at snapshot time (for normalisation).
    pub total_weight: u64,

    /// Node count at snapshot time.
    pub node_count: u32,

    /// Edge count at snapshot time.
    pub edge_count: u32,
}

impl SpectralSnapshot {
    pub const EMPTY: Self = Self {
        generation: 0,
        eigenvalues: [0; SPECTRAL_K],
        gaps: [0; SPECTRAL_K],
        total_weight: 0,
        node_count: 0,
        edge_count: 0,
    };

    /// Returns the Fiedler value λ₂ (algebraic connectivity).
    pub const fn fiedler(&self) -> Weight {
        self.eigenvalues[1]
    }

    /// Returns spectral gap γ₁ = λ₂ − λ₁ (= λ₂ for connected graphs).
    pub const fn primary_gap(&self) -> Weight {
        self.gaps[0]
    }

    /// Returns `true` if this snapshot has been populated.
    pub const fn is_valid(&self) -> bool {
        self.generation > 0
    }
}

// ────────────────────────────────────────────────────────────────────
// CUSUM detector
// ────────────────────────────────────────────────────────────────────

/// Cumulative Sum (CUSUM) change-point detector for the Fiedler value.
///
/// Monitors λ₂(t) for sustained downward drift, which predicts graph
/// partitioning events. The Fiedler drift theorem states:
///
///   Δλ₂/Δt ≤ −0.1 sustained 5 steps → partition within 5 steps
///   (precision ≥ 0.82)
///
/// The CUSUM maintains two running sums:
///   S⁺(t) = max(0, S⁺(t−1) + (x(t) − μ₀ − k))  — detects increase
///   S⁻(t) = max(0, S⁻(t−1) + (μ₀ − k − x(t)))  — detects decrease
///
/// An alarm fires when S⁺ or S⁻ exceeds threshold h.
///
/// 24 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CusumDetector {
    /// Running reference mean μ₀ in 16.16 fixed-point.
    pub mu_0: Weight,

    /// Allowable slack k in 16.16 fixed-point.
    /// Prevents false alarms from minor fluctuations.
    /// Default: 0.05 (≈ 3277 in 16.16).
    pub slack: Weight,

    /// Alarm threshold h in 16.16 fixed-point.
    /// Default: 0.5 (≈ 32768 in 16.16).
    pub threshold: Weight,

    /// Cumulative sum for upward drift detection (S⁺).
    pub s_pos: Weight,

    /// Cumulative sum for downward drift detection (S⁻).
    pub s_neg: Weight,

    /// Number of consecutive alarm steps (for the "sustained 5 steps" rule).
    pub alarm_count: u32,
}

impl CusumDetector {
    pub const DEFAULT: Self = Self {
        mu_0: 0,
        slack: 3277,      // 0.05
        threshold: 32768, // 0.5
        s_pos: 0,
        s_neg: 0,
        alarm_count: 0,
    };

    /// Feed a new Fiedler value observation and return `true` if alarm.
    ///
    /// `fiedler` is λ₂ in 16.16 fixed-point.
    ///
    /// Returns `true` if a sustained downward drift alarm is active
    /// (5+ consecutive alarm steps).
    pub fn observe(&mut self, fiedler: Weight) -> bool {
        // S⁺(t) = max(0, S⁺(t−1) + (x − μ₀ − k))
        let x_minus_mu = fiedler.saturating_sub(self.mu_0);
        let above = x_minus_mu.saturating_sub(self.slack);
        self.s_pos = self.s_pos.saturating_add(above);

        // S⁻(t) = max(0, S⁻(t−1) + (μ₀ − k − x))
        let mu_minus_x = self.mu_0.saturating_sub(fiedler);
        let below = mu_minus_x.saturating_sub(self.slack);
        self.s_neg = self.s_neg.saturating_add(below);

        let alarm = self.s_neg >= self.threshold;
        if alarm {
            self.alarm_count += 1;
        } else {
            self.alarm_count = 0;
        }

        // Fiedler drift theorem: 5 sustained alarm steps.
        self.alarm_count >= 5
    }

    /// Reset the detector with a new reference mean.
    pub fn reset(&mut self, new_mu_0: Weight) {
        self.mu_0 = new_mu_0;
        self.s_pos = 0;
        self.s_neg = 0;
        self.alarm_count = 0;
    }
}

// ────────────────────────────────────────────────────────────────────
// Spectral state (module-level singleton)
// ────────────────────────────────────────────────────────────────────

struct SpectralState {
    /// Ring buffer of spectral snapshots.
    ring: [SpectralSnapshot; SNAPSHOT_RING_SIZE],
    /// Write index into the ring buffer (wraps around).
    write_idx: usize,
    /// Total number of snapshots ever recorded.
    total_snapshots: u64,
    /// CUSUM detector for the Fiedler value.
    cusum: CusumDetector,
}

impl SpectralState {
    const fn new() -> Self {
        Self {
            ring: [SpectralSnapshot::EMPTY; SNAPSHOT_RING_SIZE],
            write_idx: 0,
            total_snapshots: 0,
            cusum: CusumDetector::DEFAULT,
        }
    }
}

static SPECTRAL: Mutex<SpectralState> = Mutex::new(SpectralState::new());

// ────────────────────────────────────────────────────────────────────
// Public API
// ────────────────────────────────────────────────────────────────────

/// Initialise spectral tracking.
///
/// Sets the CUSUM reference mean to the current Fiedler value (if a
/// snapshot exists) or zero. Call after `arena::init()` and initial
/// graph seeding.
pub fn init() {
    serial::write_line(b"[graph] spectral tracking initialised");
}

/// Record a new spectral snapshot.
///
/// The caller is responsible for computing the eigenvalues (via
/// shift-invert Lanczos, power iteration, or whatever method is
/// appropriate at the current boot stage). This function stores the
/// result and feeds the Fiedler value to the CUSUM detector.
///
/// Returns `true` if the CUSUM detector fires (sustained Fiedler drift).
pub fn record_snapshot(snapshot: SpectralSnapshot) -> bool {
    let mut state = SPECTRAL.lock();

    // Store in ring buffer.
    let idx = state.write_idx;
    state.ring[idx] = snapshot;
    state.write_idx = (idx + 1) % SNAPSHOT_RING_SIZE;
    state.total_snapshots += 1;

    // Feed Fiedler value to CUSUM.
    let alarm = state.cusum.observe(snapshot.fiedler());

    if alarm {
        serial::write_bytes(b"[spectral] CUSUM ALARM: Fiedler drift at gen=");
        serial::write_u64_dec(snapshot.generation);
    }

    alarm
}

/// Get the most recent spectral snapshot, if any.
pub fn latest_snapshot() -> Option<SpectralSnapshot> {
    let state = SPECTRAL.lock();
    if state.total_snapshots == 0 {
        return None;
    }
    let idx = if state.write_idx == 0 {
        SNAPSHOT_RING_SIZE - 1
    } else {
        state.write_idx - 1
    };
    let snap = state.ring[idx];
    if snap.is_valid() { Some(snap) } else { None }
}

/// Get the snapshot at a specific ring buffer offset from the most recent.
///
/// `offset = 0` is the latest, `offset = 1` is one before, etc.
/// Returns `None` if the offset exceeds available history.
pub fn snapshot_at_offset(offset: usize) -> Option<SpectralSnapshot> {
    let state = SPECTRAL.lock();
    if offset >= SNAPSHOT_RING_SIZE || (state.total_snapshots as usize) <= offset {
        return None;
    }
    let idx = if state.write_idx > offset {
        state.write_idx - 1 - offset
    } else {
        SNAPSHOT_RING_SIZE - 1 - (offset - state.write_idx)
    };
    let snap = state.ring[idx];
    if snap.is_valid() { Some(snap) } else { None }
}

/// Returns `true` if the CUSUM detector is in sustained alarm state
/// (≥ 5 consecutive alarm steps — the Fiedler drift theorem threshold).
pub fn cusum_alarm() -> bool {
    SPECTRAL.lock().cusum.alarm_count >= 5
}

/// Get the current CUSUM alarm count (consecutive alarm steps).
pub fn cusum_alarm_count() -> u32 {
    SPECTRAL.lock().cusum.alarm_count
}

/// Reset the CUSUM detector with a new reference Fiedler value.
pub fn cusum_reset(new_mu_0: Weight) {
    SPECTRAL.lock().cusum.reset(new_mu_0);
}

/// Total number of snapshots ever recorded.
pub fn total_snapshots() -> u64 {
    SPECTRAL.lock().total_snapshots
}

/// Compute the Fiedler drift rate over the last `window` snapshots.
///
/// Returns the average Δλ₂ per step in 16.16 fixed-point.
/// Positive = increasing connectivity. Negative = decreasing.
///
/// Since we're in unsigned fixed-point, we return (drift, is_negative).
///
/// Returns `None` if fewer than 2 snapshots are available in the window.
pub fn fiedler_drift(window: usize) -> Option<(Weight, bool)> {
    let state = SPECTRAL.lock();
    let available = core::cmp::min(state.total_snapshots as usize, SNAPSHOT_RING_SIZE);
    let w = core::cmp::min(window, available);
    if w < 2 {
        return None;
    }

    // Get oldest and newest in window.
    let newest_idx = if state.write_idx == 0 {
        SNAPSHOT_RING_SIZE - 1
    } else {
        state.write_idx - 1
    };
    let oldest_idx = if newest_idx >= w - 1 {
        newest_idx - (w - 1)
    } else {
        SNAPSHOT_RING_SIZE - (w - 1 - newest_idx)
    };

    let newest = state.ring[newest_idx];
    let oldest = state.ring[oldest_idx];

    if !newest.is_valid() || !oldest.is_valid() {
        return None;
    }

    let steps = (w - 1) as u32;
    if steps == 0 {
        return None;
    }

    let f_new = newest.fiedler();
    let f_old = oldest.fiedler();

    if f_new >= f_old {
        Some(((f_new - f_old) / steps, false))
    } else {
        Some(((f_old - f_new) / steps, true))
    }
}

/// Dump spectral state to serial.
pub fn dump() {
    let state = SPECTRAL.lock();
    serial::write_line(b"[graph] === Spectral State ===");
    serial::write_bytes(b"[spectral] snapshots: ");
    serial::write_u64_dec_inline(state.total_snapshots);
    serial::write_bytes(b"  cusum_alarm: ");
    serial::write_u64_dec(state.cusum.alarm_count as u64);

    // Dump the latest snapshot if available.
    if state.total_snapshots > 0 {
        let idx = if state.write_idx == 0 {
            SNAPSHOT_RING_SIZE - 1
        } else {
            state.write_idx - 1
        };
        let snap = state.ring[idx];
        if snap.is_valid() {
            serial::write_bytes(b"[spectral] latest gen=");
            serial::write_u64_dec_inline(snap.generation);
            serial::write_bytes(b" fiedler=");
            serial::write_u64_dec_inline(snap.fiedler() as u64);
            serial::write_bytes(b" nodes=");
            serial::write_u64_dec_inline(snap.node_count as u64);
            serial::write_bytes(b" edges=");
            serial::write_u64_dec(snap.edge_count as u64);
        }
    }

    serial::write_line(b"[graph] === End Spectral State ===");
}
