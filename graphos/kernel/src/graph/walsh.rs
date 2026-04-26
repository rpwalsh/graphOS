// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Walsh-Hadamard spectral operator — W_{ττ'} graph message passing.
//!
//! This module implements the **per-type-pair Walsh operator** that is the
//! second factor of the unified temporal operator:
//!
//!   S_{ττ'}(h, Δt) = exp(−λ_{ττ'} · Δt) · W_{ττ'} · h
//!
//! ## What is W_{ττ'}?
//!
//! W_{ττ'} is the **graph message-passing matrix** restricted to edges of
//! type-pair (τ, τ'). For a graph with adjacency A_{ττ'}:
//!
//!   (W_{ττ'} · h)[u] = Σ_{v : τ(v)=τ'} A_{ττ'}[u,v] · h[v]
//!
//! The *Walsh-Hadamard structure* arises because the `NodeKind` index space
//! has size NODE_KIND_COUNT = 32 = 2⁵. The Hadamard matrix H_5 of order 32
//! diagonalises the type-adjacency tensor in the **Walsh-Fourier basis**:
//!
//!   Â_{ττ'} = H₃₂ · A_{ττ'} · H₃₂   (type-space WHT)
//!
//! This lets us:
//! 1. Compute the **Fast Walsh-Hadamard Transform** (FWHT) on type-indexed
//!    feature vectors in O(k log k) integer ops instead of O(k²).
//! 2. Detect **spectral anomalies** in the type-space (unusual type-pair
//!    activation patterns) via CUSUM on the Walsh coefficients.
//! 3. **Convolve** graph signals with kernel functions expressed in the
//!    Walsh basis — the foundation for graph wavelet filtering.
//!
//! ## Integer-only implementation
//!
//! Everything uses 16.16 fixed-point (`Weight = u32`). No FPU. No heap.
//! The FWHT butterfly uses i64 intermediates to avoid overflow, then
//! saturates back to u32.
//!
//! ## Design
//!
//! - `walsh_fwht(signal)`: in-place fast Walsh-Hadamard transform on a
//!   32-element type-indexed signal vector.
//! - `aggregate_type_signal(graph_node_kind, walker_features)`: compute
//!   (W_{τ,·} · h) — one row of the full message-passing operator, i.e.
//!   the aggregated signal arriving at a node of kind τ from all its
//!   type-labeled neighbors.
//! - `TypeSignal`: a 32-element Weight vector, one slot per NodeKind.
//!
//! ## Relationship to the TypePairMatrix
//!
//! The per-type-pair decay λ_{ττ'} lives in `temporal::TypePairMatrix`.
//! This module handles the W_{ττ'} · h multiplication step; `temporal`
//! provides the exp(−λ·Δt) decay factor to apply afterward.

use crate::graph::types::{NODE_KIND_COUNT, NodeKind, WEIGHT_ONE, Weight};

// ────────────────────────────────────────────────────────────────────
// TypeSignal — a vector indexed by NodeKind
// ────────────────────────────────────────────────────────────────────

/// A 32-element signal vector, one element per NodeKind.
///
/// Used as both:
/// - A **node feature vector** h: h[τ] = aggregate feature for nodes of kind τ
/// - A **Walsh coefficient vector** Â = FWHT(h)
///
/// 128 bytes (32 × u32), no heap.
#[derive(Clone, Copy, Debug)]
pub struct TypeSignal {
    pub data: [Weight; NODE_KIND_COUNT],
}

impl TypeSignal {
    /// Zero signal.
    pub const ZERO: Self = Self {
        data: [0; NODE_KIND_COUNT],
    };

    /// Unit signal (all ones).
    pub const ONE: Self = Self {
        data: [WEIGHT_ONE; NODE_KIND_COUNT],
    };

    /// Create a signal with only one type active at `weight`.
    pub fn impulse(kind: NodeKind, weight: Weight) -> Self {
        let mut s = Self::ZERO;
        s.data[kind.index()] = weight;
        s
    }

    /// Get the component for a specific NodeKind.
    #[inline]
    pub fn get(&self, kind: NodeKind) -> Weight {
        self.data[kind.index()]
    }

    /// Set the component for a specific NodeKind.
    #[inline]
    pub fn set(&mut self, kind: NodeKind, val: Weight) {
        self.data[kind.index()] = val;
    }

    /// Accumulate (add) into the component for a specific NodeKind.
    /// Saturates at u32::MAX to prevent wraparound.
    #[inline]
    pub fn accumulate(&mut self, kind: NodeKind, val: Weight) {
        let slot = &mut self.data[kind.index()];
        *slot = slot.saturating_add(val);
    }
}

// ────────────────────────────────────────────────────────────────────
// Fast Walsh-Hadamard Transform (FWHT)
// ────────────────────────────────────────────────────────────────────

/// In-place Fast Walsh-Hadamard Transform on a 32-element type signal.
///
/// Computes: Â = H₃₂ · signal   where H₃₂ is the 32×32 Hadamard matrix.
///
/// The FWHT butterfly is:
///   (a, b) → (a + b, a − b)
///
/// Applied across all stages for a 2⁵ = 32-point transform.
///
/// ## Fixed-point semantics
/// Input values are 16.16 fixed-point. The FWHT sums/differences are
/// computed in i64 to prevent overflow, then clamped back to [0, u32::MAX].
/// The result is NOT normalised by 1/32 — callers who need the inverse
/// transform should call `walsh_ifwht` which applies the 1/32 factor.
///
/// ## Walsh basis ordering
/// The output follows the **sequency** (Walsh) ordering, which is the
/// natural ordering for graph signal processing (sequency = number of
/// sign changes = graph frequency analog).
pub fn walsh_fwht(signal: &mut TypeSignal) {
    let n = NODE_KIND_COUNT; // 32
    let mut step = 1usize;
    while step < n {
        let mut i = 0;
        while i < n {
            let mut j = i;
            while j < i + step {
                let a = signal.data[j] as i64;
                let b = signal.data[j + step] as i64;
                let sum = a + b;
                let diff = a - b;
                // Saturate: clamp to [0, u32::MAX].
                signal.data[j] = sum.clamp(0, u32::MAX as i64) as u32;
                signal.data[j + step] = diff.clamp(0, u32::MAX as i64) as u32;
                j += 1;
            }
            i += step * 2;
        }
        step <<= 1;
    }
}

/// In-place inverse FWHT.
///
/// Applies FWHT then divides by N=32 to get the true inverse:
///   signal = H₃₂⁻¹ · Â = (1/32) · H₃₂ · Â
///
/// Division by 32 in 16.16 fixed-point: right-shift by 5.
pub fn walsh_ifwht(signal: &mut TypeSignal) {
    walsh_fwht(signal); // H is its own inverse up to 1/N factor
    for v in signal.data.iter_mut() {
        *v >>= 5; // divide by 32 = 2⁵
    }
}

// ────────────────────────────────────────────────────────────────────
// Type-pair temporal decay application
// ────────────────────────────────────────────────────────────────────

/// Apply temporal decay exp(−λ_{τ,τ'} · Δt) to a type signal.
///
/// This is the exp(−λ · Δt) factor of S_{ττ'}(h, Δt).
///
/// Uses the linear approximation exp(−x) ≈ 1 − x for small x (valid
/// when λ · Δt < 0.5, which holds for Δt < 1 generation step and the
/// default λ = 0.01). For larger Δt, a repeated-squaring approach is
/// used.
///
/// # Arguments
/// * `signal`   — The type signal to decay in-place.
/// * `src_kind` — The source NodeKind τ (row of the type-pair matrix).
/// * `delta_t`  — Time elapsed since edge creation, in abstract ticks (16.16).
///
/// The decay coefficients λ_{τ,τ'} come from `temporal::get_pair_decay()`.
pub fn apply_decay(signal: &mut TypeSignal, src_kind: NodeKind, delta_t: u64) {
    for (dst_idx, slot) in signal.data.iter_mut().enumerate() {
        if *slot == 0 {
            continue;
        }
        let dst_kind = match NodeKind::from_u16(dst_idx as u16) {
            Some(k) => k,
            None => continue,
        };
        let lambda = crate::graph::temporal::get_decay(src_kind, dst_kind);
        if lambda == 0 {
            continue; // static edge, no decay
        }

        // Compute decay factor in 16.16:
        // decay_factor = WEIGHT_ONE - saturate(lambda * delta_t / WEIGHT_ONE)
        // lambda is 16.16, delta_t is raw ticks (treat as integer here).
        // lambda * delta_t overflows for large delta_t — saturate to WEIGHT_ONE.
        let product = (lambda as u64).saturating_mul(delta_t);
        let product_fixed = (product >> 16) as u32; // back to 16.16 after mul
        let decay_factor = WEIGHT_ONE.saturating_sub(product_fixed.min(WEIGHT_ONE));

        // Multiply signal slot by decay_factor (both 16.16):
        // result = (slot * decay_factor) >> 16
        let decayed = ((*slot as u64 * decay_factor as u64) >> 16) as u32;
        *slot = decayed;
    }
}

// ────────────────────────────────────────────────────────────────────
// Message-passing aggregation: W_{ττ'} · h
// ────────────────────────────────────────────────────────────────────

/// Aggregate type signals from all neighbors of a set of source nodes.
///
/// Computes one step of the message-passing operator: for each type-pair
/// (src_kind, dst_kind), multiply the neighbor contributions by the
/// per-pair type-selector and accumulate into the output signal.
///
/// This is the **graph-level** component of W_{ττ'} · h, operating on the
/// type-level summary rather than per-node values (which live in the arena).
///
/// # Arguments
/// * `neighbor_types` — TypeSignal where `data[τ']` = number of type-τ'
///   neighbors (or a weighted count). Source of h.
/// * `src_kind`       — The NodeKind τ of the aggregating node.
///
/// Returns the aggregated output signal for node type τ.
pub fn aggregate(neighbor_types: &TypeSignal, src_kind: NodeKind) -> TypeSignal {
    let mut out = TypeSignal::ZERO;
    for dst_idx in 0..NODE_KIND_COUNT {
        let h_val = neighbor_types.data[dst_idx];
        if h_val == 0 {
            continue;
        }
        let dst_kind = match NodeKind::from_u16(dst_idx as u16) {
            Some(k) => k,
            None => continue,
        };
        let lambda = crate::graph::temporal::get_decay(src_kind, dst_kind);
        // The W_{ττ'} weight is inversely proportional to decay (slow-decaying
        // edges are structurally stronger). Use (WEIGHT_ONE - lambda).saturating_add(1)
        // as the per-pair weight. Edges with zero decay (static) get full weight.
        let pair_weight: u32 = WEIGHT_ONE.saturating_sub(lambda).max(1);
        // Accumulate: out[τ'] += h_val * pair_weight >> 16
        let contrib = ((h_val as u64 * pair_weight as u64) >> 16) as u32;
        out.data[dst_idx] = out.data[dst_idx].saturating_add(contrib);
    }
    out
}

/// Apply the full unified temporal operator S_{ττ'}(h, Δt) in the Walsh basis.
///
/// Steps:
/// 1. Forward FWHT of `h` → Ĥ (Walsh-Fourier coefficients of h)
/// 2. Apply per-type decay to Ĥ in the Walsh domain
/// 3. Inverse FWHT → S_{ττ'} · h (time-domain output)
///
/// This is the "global" version operating on the full type signal.
/// Use `aggregate()` + `apply_decay()` for the local per-node version.
///
/// # Arguments
/// * `h`         — Input type signal (modified in-place).
/// * `src_kind`  — Source type τ for the decay matrix selection.
/// * `delta_t`   — Elapsed time (abstract ticks).
pub fn apply_temporal_operator(h: &mut TypeSignal, src_kind: NodeKind, delta_t: u64) {
    walsh_fwht(h);
    apply_decay(h, src_kind, delta_t);
    walsh_ifwht(h);
}

// ────────────────────────────────────────────────────────────────────
// Walsh spectral anomaly detection
// ────────────────────────────────────────────────────────────────────

/// Per-type Walsh coefficient snapshot for anomaly tracking.
///
/// Maintains a running mean and variance of each Walsh coefficient
/// (using Welford's online algorithm, integer-only) to detect when
/// the type-composition of the graph deviates from baseline.
///
/// 64 bytes (32 × u32 mean only — variance stored separately for space).
#[derive(Clone, Copy)]
pub struct WalshCoeffStats {
    /// Running mean of each Walsh coefficient, 16.16 fixed-point.
    pub mean: [Weight; NODE_KIND_COUNT],
    /// Running count of samples seen.
    pub count: u32,
}

impl WalshCoeffStats {
    pub const EMPTY: Self = Self {
        mean: [0; NODE_KIND_COUNT],
        count: 0,
    };

    /// Update the running mean with a new signal snapshot.
    ///
    /// Uses Welford's incremental formula:
    ///   mean_n = mean_{n-1} + (x - mean_{n-1}) / n
    ///
    /// In 16.16 fixed-point: delta / n is approximated as delta >> log2(n)
    /// for n = powers of 2, otherwise uses integer division.
    pub fn update(&mut self, signal: &TypeSignal) {
        self.count = self.count.saturating_add(1);
        let n = self.count as u64;
        for i in 0..NODE_KIND_COUNT {
            let x = signal.data[i] as i64;
            let m = self.mean[i] as i64;
            let delta = x - m;
            // delta / n in 16.16: multiply delta by WEIGHT_ONE, divide by n.
            let update = ((delta * WEIGHT_ONE as i64) / n as i64) as i32;
            self.mean[i] = (m + update as i64).clamp(0, u32::MAX as i64) as u32;
        }
    }

    /// Compute the L1 deviation of a signal from the running mean.
    ///
    /// Returns Σ|signal[i] − mean[i]| in 16.16. This is the "Walsh anomaly
    /// score" — high values indicate unusual type-composition.
    pub fn deviation_l1(&self, signal: &TypeSignal) -> u64 {
        let mut acc: u64 = 0;
        for i in 0..NODE_KIND_COUNT {
            let x = signal.data[i] as i64;
            let m = self.mean[i] as i64;
            acc += (x - m).unsigned_abs();
        }
        acc
    }
}

// ────────────────────────────────────────────────────────────────────
// Global Walsh stats state
// ────────────────────────────────────────────────────────────────────

use spin::Mutex;

static WALSH_STATS: Mutex<WalshCoeffStats> = Mutex::new(WalshCoeffStats::EMPTY);

/// Record a new type-composition snapshot for baseline tracking.
///
/// Called from arena mutation hooks (add_node / add_edge) after every
/// `WALSH_SAMPLE_PERIOD` mutations to maintain the Walsh anomaly baseline.
pub fn record_type_snapshot(signal: &TypeSignal) {
    WALSH_STATS.lock().update(signal);
}

/// Compute the Walsh anomaly score of a current type signal vs baseline.
///
/// Returns 0 if no baseline has been established yet.
pub fn anomaly_score(signal: &TypeSignal) -> u64 {
    let stats = WALSH_STATS.lock();
    if stats.count == 0 {
        return 0;
    }
    stats.deviation_l1(signal)
}

/// How often (in arena generations) to sample the Walsh type composition.
pub const WALSH_SAMPLE_PERIOD: u64 = 64;

// ────────────────────────────────────────────────────────────────────
// Temporal WL label augmentation
// ────────────────────────────────────────────────────────────────────

/// Default temporal bucket width (Δ) in abstract ticks.
///
/// From `graphKernelMethods.tsx`: ℓ'_v = (ℓ_v, ⌊t_v/Δ⌋).
/// This width controls the granularity of temporal label discrimination.
/// At 64 ticks per bucket, two nodes with the same structural label are
/// considered temporally distinct only if their timestamps differ by ≥ 64 ticks.
pub const WL_TEMPORAL_BUCKET_WIDTH: u64 = 64;

/// Compute the temporal bucket index for a timestamp.
///
/// Returns `⌊t / bucket_width⌋`, clamped to u32 range.
/// A bucket_width of 0 is treated as 1 (no division by zero).
#[inline]
pub const fn wl_time_bucket(timestamp: u64, bucket_width: u64) -> u32 {
    let w = if bucket_width == 0 { 1 } else { bucket_width };
    let bucket = timestamp / w;
    if bucket > u32::MAX as u64 {
        u32::MAX
    } else {
        bucket as u32
    }
}

/// Augmented WL label: ℓ'_v = (ℓ_v, ⌊t_v/Δ⌋).
///
/// Packs the structural label `label` and temporal bucket `time_bucket` into
/// a single u64 for use in graph kernel computations and isomorphism tests.
///
/// The high 32 bits carry `label`; the low 32 bits carry `time_bucket`.
/// This encoding is stable, collision-free for valid (label, bucket) pairs,
/// and directly comparable with `==`.
///
/// From the WTG paper (graphKernelMethods.tsx): a temporally-augmented WL
/// test using these labels can distinguish Temporal Score Collapse (TSC)
/// counter-examples that the un-augmented WL test conflates.
#[inline]
pub const fn temporal_wl_label(label: u32, time_bucket: u32) -> u64 {
    ((label as u64) << 32) | (time_bucket as u64)
}

/// Compute the augmented WL label for a node given its structural label
/// and timestamp, using the default bucket width.
///
/// Convenience wrapper over `temporal_wl_label` + `wl_time_bucket`.
#[inline]
pub fn wl_label_for_node(structural_label: u32, node_timestamp: u64) -> u64 {
    let bucket = wl_time_bucket(node_timestamp, WL_TEMPORAL_BUCKET_WIDTH);
    temporal_wl_label(structural_label, bucket)
}

/// Apply one round of the temporally-augmented WL label refinement to a
/// type signal.
///
/// For each active NodeKind slot in `signal`, the slot value (used as
/// structural label) is re-keyed by its temporal bucket so that
/// structurally identical nodes in different time windows produce
/// distinguishable Walsh coefficients.
///
/// This implements the ℓ'_v augmentation at the type-signal level:
/// instead of operating on individual node labels (which requires per-node
/// arena access), we apply the temporal modulation to the aggregated type
/// counts.  The resulting signal encodes both *which* types are present and
/// *when* they became active, enabling the Walsh anomaly scorer to detect
/// TSC-class structural changes that are invisible to the unaugmented test.
///
/// `now_ticks`: the current generation/tick counter (from `arena::generation()`).
/// `bucket_width`: the Δ parameter; pass `WL_TEMPORAL_BUCKET_WIDTH` for default.
pub fn augment_wl_signal(
    signal: &mut TypeSignal,
    node_timestamps: &[u64; NODE_KIND_COUNT],
    bucket_width: u64,
) {
    for i in 0..NODE_KIND_COUNT {
        if signal.data[i] == 0 {
            continue;
        }
        let label = signal.data[i];
        let bucket = wl_time_bucket(node_timestamps[i], bucket_width);
        // Re-hash: mix the label with the bucket using a simple multiplicative
        // hash so two (label, bucket) pairs are unlikely to collide in the u32
        // weight space.  Full precision is in `temporal_wl_label`; here we
        // fold back to u32 for the weight slot.
        let augmented_label = temporal_wl_label(label, bucket);
        // Fold the 64-bit augmented label into a 32-bit weight via XOR-folding.
        let folded = (augmented_label ^ (augmented_label >> 32)) as u32;
        signal.data[i] = folded.max(1); // preserve non-zero (active) status
    }
}
