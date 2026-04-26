// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Temporal graph primitives — type-pair parameters and decay computation.
//!
//! This module implements the core temporal infrastructure required by the
//! **unified temporal operator**:
//!
//!   S_{ττ'}(h, Δt) = exp(−λ_{ττ'} · Δt) · W_{ττ'} · h
//!
//! The PowerWalk Duality Theorem proves that random-walk sampling and
//! per-type TGNN message passing compute the same operator when
//! parameterised by the same per-type-pair (p, q, λ) triple. This module
//! stores those parameters.
//!
//! ## Type-pair parameter matrix
//!
//! The `TypePairParams` matrix is indexed by (source NodeKind, dest NodeKind).
//! Each entry contains:
//! - `decay`: λ_{ττ'} — temporal decay rate for this type pair
//! - `return_p`: p_{ττ'} — return parameter (node2vec-style)
//! - `inout_q`: q_{ττ'} — in-out parameter (node2vec-style)
//!
//! These three scalars fully determine the PowerWalk transition distribution:
//!   P(x | s, v, t) = w̃(v,x,t) · α(s,x,p,q) / Z
//!
//! where w̃ is the temporally-decayed weight and α is the node2vec bias
//! function extended to heterogeneous types.
//!
//! ## Walk-length optimality
//!
//! From the spectral gap connection:
//!   L*(τ,τ',ε) = ⌈ log(1/ε) / γ_{ττ'} ⌉
//!
//! where γ_{ττ'} is the spectral gap of the type-pair-restricted transition
//! matrix. The `optimal_walk_length` function computes this.
//!
//! ## Design
//! - All values are 16.16 fixed-point (same as edge weights).
//! - The matrix is statically allocated: NODE_KIND_COUNT² entries.
//! - Defaults are conservative: λ = 0.01, p = 1.0, q = 1.0 (unbiased).
//! - Calibration will be performed by EM (Expectation-Maximization) once
//!   enough walk samples have been collected.
//!
//! ## Concurrency
//! Single `spin::Mutex`. Same model as the arena — acceptable for early boot,
//! will need sharding under SMP.

use spin::Mutex;

use crate::arch::serial;
use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Per-type-pair parameter entry
// ────────────────────────────────────────────────────────────────────

/// Parameters for a single (source-type, dest-type) pair.
///
/// 12 bytes, repr(C), Copy. Stored in a flat matrix indexed by
/// (NodeKind::index(), NodeKind::index()).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TypePairEntry {
    /// λ_{ττ'}: temporal decay rate in 16.16 fixed-point.
    /// Higher values mean faster forgetting. 0 = no decay (static edge).
    /// Default: 0.01 (≈ 655 in 16.16).
    pub decay: Weight,

    /// p_{ττ'}: return parameter in 16.16 fixed-point.
    /// Controls probability of revisiting the previous node.
    /// p > 1 discourages backtracking (BFS-like exploration).
    /// p < 1 encourages backtracking (local clustering).
    /// Default: 1.0 (WEIGHT_ONE = 65536).
    pub return_p: Weight,

    /// q_{ττ'}: in-out parameter in 16.16 fixed-point.
    /// Controls exploration vs. exploitation.
    /// q > 1 biases toward local neighborhood (BFS-like).
    /// q < 1 biases toward distant nodes (DFS-like).
    /// Default: 1.0 (WEIGHT_ONE = 65536).
    pub inout_q: Weight,
}

impl TypePairEntry {
    /// Default entry: unbiased walk with mild temporal decay.
    /// λ = 0.01, p = 1.0, q = 1.0.
    pub const DEFAULT: Self = Self {
        decay: 655,           // 0.01 × 65536 ≈ 655
        return_p: WEIGHT_ONE, // 1.0
        inout_q: WEIGHT_ONE,  // 1.0
    };

    /// Zero-decay entry for static structural edges.
    pub const STATIC: Self = Self {
        decay: 0,
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE,
    };
}

// ────────────────────────────────────────────────────────────────────
// Type-pair parameter matrix
// ────────────────────────────────────────────────────────────────────

/// The full NODE_KIND_COUNT × NODE_KIND_COUNT parameter matrix.
///
/// Indexed as `matrix[source_kind.index() * NODE_KIND_COUNT + dest_kind.index()]`.
///
/// This is the single source of truth for the per-type-pair parameters
/// that the PowerWalk Duality Theorem says must be shared between walk
/// sampling and TGNN message passing.
struct TypePairMatrix {
    entries: [TypePairEntry; NODE_KIND_COUNT * NODE_KIND_COUNT],
}

impl TypePairMatrix {
    const fn new() -> Self {
        Self {
            entries: [TypePairEntry::DEFAULT; NODE_KIND_COUNT * NODE_KIND_COUNT],
        }
    }

    /// Get the entry for a (source, dest) type pair.
    fn get(&self, src: NodeKind, dst: NodeKind) -> TypePairEntry {
        self.entries[src.index() * NODE_KIND_COUNT + dst.index()]
    }

    /// Set the entry for a (source, dest) type pair.
    fn set(&mut self, src: NodeKind, dst: NodeKind, entry: TypePairEntry) {
        self.entries[src.index() * NODE_KIND_COUNT + dst.index()] = entry;
    }
}

static TYPE_PAIR_MATRIX: Mutex<TypePairMatrix> = Mutex::new(TypePairMatrix::new());

// ────────────────────────────────────────────────────────────────────
// Public API
// ────────────────────────────────────────────────────────────────────

/// Initialise the type-pair parameter matrix with domain-appropriate defaults.
///
/// System-structural edges (Owns, Contains, Created) get zero decay since
/// they represent static relationships. Temporal edges (CommunicatesWith,
/// WaitsOn, Triggers) get higher decay rates to emphasise recency.
///
/// Call this after `arena::init()` and before any walk sampling or
/// spectral computation.
pub fn init() {
    let mut m = TYPE_PAIR_MATRIX.lock();

    // ── Static structural edges: zero decay ──
    // When a system edge like Kernel→Device (Owns) exists, the temporal
    // decay should be zero — the relationship doesn't weaken over time.
    // We set λ = 0 for all pairs involving Kernel as source.
    for dst in 0..NODE_KIND_COUNT {
        m.entries[NodeKind::Kernel.index() * NODE_KIND_COUNT + dst] = TypePairEntry::STATIC;
    }

    // ── Calibrated decay rate matrix — WTG paper 31× spread ──
    //
    // The PowerWalk paper specifies calibrated per-type-pair λ values with
    // a 31× range from λ_min ≈ 0.009/day to λ_max ≈ 0.28/day across the
    // empirically measured type-pair taxonomy.  In abstract ticks (where
    // 1 tick ≈ 1 scheduling quantum ≈ 10ms), these convert as:
    //
    //   λ/day → λ/tick: divide by (86400 / 0.01) = 8,640,000
    //   In 16.16 fixed-point (WEIGHT_ONE=65536), multiply λ/day by 65536
    //   then divide by ticks_per_day to get the 16.16 representation.
    //   For a coarser mapping used here, λ_fp ≈ λ_fraction × 65536.
    //
    // The table below covers the seven principal type-pair families from
    // Table 2 of the WTG paper, ordered from fastest (IPC bursts) to
    // slowest (structural ownership) decay.  Values are 16.16 fixed-point.

    let task_idx = NodeKind::Task.index();
    let svc_idx = NodeKind::Service.index();
    let cpu_idx = NodeKind::CpuCore.index();
    let dev_idx = NodeKind::Device.index();
    let chan_idx = NodeKind::Channel.index();
    let wasm_idx = NodeKind::WasmSandbox.index();
    let gpu_idx = NodeKind::DisplaySurface.index();
    let tpm_idx = NodeKind::TpmDevice.index();

    // 1. Task ↔ Task (IPC / CommunicatesWith): λ ≈ 0.28/day (fastest)
    //    Short-lived bursty IPC — edges become stale within hours.
    //    DFS bias (q=0.5): follow IPC chains, don't backtrack.
    //    Calibrated: 18350 ≈ 0.28 × 65536.
    m.entries[task_idx * NODE_KIND_COUNT + task_idx] = TypePairEntry {
        decay: 18350,             // 0.28 in 16.16
        return_p: WEIGHT_ONE / 2, // p=0.5: mild backtrack encouragement (ping-pong IPC)
        inout_q: WEIGHT_ONE / 2,  // q=0.5: DFS — follow IPC chains
    };

    // 2. Task → Channel (SendsTo): λ ≈ 0.18/day
    //    Channel memberships change frequently (task spawns, exits).
    //    Unbiased walk: p=1.0, q=1.0.
    m.entries[task_idx * NODE_KIND_COUNT + chan_idx] = TypePairEntry {
        decay: 11796, // 0.18 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE,
    };

    // 3. Task → CpuCore (RunsOn): λ ≈ 0.12/day
    //    Scheduling assignments shift over minutes–hours.
    //    BFS bias (q=2): explore across cores, don't stay local.
    m.entries[task_idx * NODE_KIND_COUNT + cpu_idx] = TypePairEntry {
        decay: 7864, // 0.12 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE * 2, // q=2.0: BFS — spread across cores
    };

    // 4. Task → WasmSandbox (Hosts): λ ≈ 0.08/day
    //    WASM app lifetimes are session-scoped (~hours).
    m.entries[task_idx * NODE_KIND_COUNT + wasm_idx] = TypePairEntry {
        decay: 5243, // 0.08 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE,
    };

    // 5. Service ↔ Service (DependsOn / Triggers): λ ≈ 0.05/day
    //    Service dependency chains are long-lived but can be reconfigured.
    //    Strong DFS bias (q=4): follow cascade chains deep.
    m.entries[svc_idx * NODE_KIND_COUNT + svc_idx] = TypePairEntry {
        decay: 3277, // 0.05 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE * 4, // q=4.0: strongly discourage backtracking
    };

    // 6. Task → Device / GpuSurface (Writes / Renders): λ ≈ 0.02/day
    //    Device ownership changes rarely within a session.
    m.entries[task_idx * NODE_KIND_COUNT + dev_idx] = TypePairEntry {
        decay: 1311, // 0.02 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE,
    };
    m.entries[task_idx * NODE_KIND_COUNT + gpu_idx] = TypePairEntry {
        decay: 1311, // 0.02 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE,
    };

    // 7. Service → TpmDevice (Attests): λ ≈ 0.009/day (slowest)
    //    TPM attestation links are quasi-permanent for the lifetime of an
    //    installation.  Slowest decay in the calibrated spread (31× floor).
    m.entries[svc_idx * NODE_KIND_COUNT + tpm_idx] = TypePairEntry {
        decay: 590, // 0.009 in 16.16
        return_p: WEIGHT_ONE,
        inout_q: WEIGHT_ONE,
    };

    // Symmetric reverse directions at same decay rates.
    m.entries[chan_idx * NODE_KIND_COUNT + task_idx] =
        m.entries[task_idx * NODE_KIND_COUNT + chan_idx];
    m.entries[dev_idx * NODE_KIND_COUNT + task_idx] =
        m.entries[task_idx * NODE_KIND_COUNT + dev_idx];
    m.entries[gpu_idx * NODE_KIND_COUNT + task_idx] =
        m.entries[task_idx * NODE_KIND_COUNT + gpu_idx];
    m.entries[tpm_idx * NODE_KIND_COUNT + svc_idx] = m.entries[svc_idx * NODE_KIND_COUNT + tpm_idx];

    serial::write_line(b"[graph] type-pair parameter matrix initialised (31x spread: 0.009-0.28)");
}

/// Look up the decay rate λ_{ττ'} for a (source-type, dest-type) pair.
///
/// Returns 16.16 fixed-point. Used by `Edge::decayed_weight()`.
pub fn get_decay(src_kind: NodeKind, dst_kind: NodeKind) -> Weight {
    TYPE_PAIR_MATRIX.lock().get(src_kind, dst_kind).decay
}

/// Look up the full parameter triple (λ, p, q) for a type pair.
pub fn get_params(src_kind: NodeKind, dst_kind: NodeKind) -> TypePairEntry {
    TYPE_PAIR_MATRIX.lock().get(src_kind, dst_kind)
}

/// Set the parameter triple for a specific type pair.
///
/// Used by EM calibration or manual tuning from the service layer.
pub fn set_params(src_kind: NodeKind, dst_kind: NodeKind, entry: TypePairEntry) {
    TYPE_PAIR_MATRIX.lock().set(src_kind, dst_kind, entry);
}

/// Compute the optimal walk length L*(τ,τ',ε) from the spectral gap.
///
/// L* = ⌈ log(1/ε) / γ_{ττ'} ⌉
///
/// `spectral_gap_fp` is γ_{ττ'} in 16.16 fixed-point.
/// `epsilon_fp` is ε in 16.16 fixed-point (convergence tolerance).
///
/// Returns the walk length as a u32. Clamped to [1, 256] for safety.
///
/// If the spectral gap is zero (disconnected or degenerate graph),
/// returns the maximum (256).
pub fn optimal_walk_length(spectral_gap_fp: Weight, epsilon_fp: Weight) -> u32 {
    if spectral_gap_fp == 0 || epsilon_fp == 0 {
        return 256;
    }

    // log(1/ε) in 16.16: we approximate using integer log2.
    // log(1/ε) = log(WEIGHT_ONE / ε) = log2(WEIGHT_ONE/ε) × ln(2)
    // For 16.16 fixed-point where WEIGHT_ONE = 65536:
    //   If ε = 0.01 (≈655), then 1/ε ≈ 100, log(100) ≈ 4.6
    //   We approximate: find highest bit of (WEIGHT_ONE / ε).

    let inv_eps = if epsilon_fp > 0 {
        (WEIGHT_ONE as u64 * WEIGHT_ONE as u64) / epsilon_fp as u64
    } else {
        return 256;
    };

    // log2 approximation: position of highest set bit.
    let log2_inv_eps = 63u32.saturating_sub(inv_eps.leading_zeros());
    // Convert to natural log (×0.693) in fixed-point: multiply by 45426/65536.
    let ln_inv_eps = (log2_inv_eps as u64 * 45426) >> 16;

    // L* = ln(1/ε) / γ, both in fixed-point-ish integers.
    // ln_inv_eps is roughly in integer units now, γ is 16.16.
    // We want L* as an integer, so: L* = (ln_inv_eps << 16) / γ.
    let l_star = if spectral_gap_fp > 0 {
        ((ln_inv_eps << 16) / spectral_gap_fp as u64) as u32
    } else {
        256
    };

    // Clamp to [1, 256].
    if l_star == 0 {
        1
    } else if l_star > 256 {
        256
    } else {
        l_star
    }
}

/// Compute timestamp delta, saturating at u64::MAX.
///
/// Returns (t_now − t_edge) if t_now ≥ t_edge, else 0.
/// This is the Δt in the unified operator S_{ττ'}(h, Δt).
pub const fn delta_t(t_now: Timestamp, t_edge: Timestamp) -> u64 {
    t_now.saturating_sub(t_edge)
}

// ────────────────────────────────────────────────────────────────────
// EM calibration — PowerWalk Duality Theorem parameter fitting
// ────────────────────────────────────────────────────────────────────

/// Accumulated statistics for one (source-type, dest-type) pair.
///
/// These are the sufficient statistics for the EM M-step. They are
/// collected during walk sampling and cleared after each EM update.
///
/// Matches Table 1 of the WTG papers: the per-type-pair data required
/// to fit (λ_{ττ'}, p_{ττ'}, q_{ττ'}) by maximum likelihood.
///
/// 32 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TypePairStats {
    /// Number of observed transitions of this type pair.
    pub transitions: u32,
    /// Sum of Δt for all transitions (abstract ticks, wrapping).
    /// E[Δt] = sum_delta_t / transitions.  MLE: λ = 1/E[Δt].
    pub sum_delta_t: u64,
    /// Transitions that were "return" steps (walk returned to prev node).
    pub return_count: u32,
    /// Transitions to a node adjacent to the previous node (inout).
    pub inout_count: u32,
}

impl TypePairStats {
    pub const ZERO: Self = Self {
        transitions: 0,
        sum_delta_t: 0,
        return_count: 0,
        inout_count: 0,
    };

    /// True if this cell has enough observations to produce a reliable estimate.
    /// The WTG paper uses n ≥ 30 as the minimum sample threshold.
    pub const fn is_calibrated(&self) -> bool {
        self.transitions >= 30
    }
}

/// Full walk statistics matrix (NODE_KIND_COUNT²).  Updated by walk sampling.
struct WalkStatsMatrix {
    cells: [TypePairStats; NODE_KIND_COUNT * NODE_KIND_COUNT],
    /// Total EM epochs completed since boot.
    epoch: u32,
}

impl WalkStatsMatrix {
    const fn new() -> Self {
        Self {
            cells: [TypePairStats::ZERO; NODE_KIND_COUNT * NODE_KIND_COUNT],
            epoch: 0,
        }
    }
}

static WALK_STATS: Mutex<WalkStatsMatrix> = Mutex::new(WalkStatsMatrix::new());

/// Minimum and maximum bounds for fitted parameters (16.16 fixed-point).
/// Prevents degenerate solutions that destabilise the walk distribution.
const MIN_DECAY: Weight = 33; // 0.0005 — edges don't vanish instantly
const MAX_DECAY: Weight = 32768; // 0.5    — half-life ≥ 2 ticks
const MIN_P: Weight = 8192; // 0.125  — some return probability
const MAX_P: Weight = 262144; // 4.0    — heavy DFS bias
const MIN_Q: Weight = 8192; // 0.125  — some exploration
const MAX_Q: Weight = 262144; // 4.0    — heavy BFS bias

/// Record a single transition observation from a temporal walk step.
///
/// Call this from the walk sampler immediately after each accepted step.
///
/// # Parameters
/// - `src_kind`, `dst_kind` — NodeKind of the edge endpoints.
/// - `delta_t`              — Time elapsed since the edge was created (Δt).
/// - `is_return`            — True if the destination is the previous node.
/// - `is_inout`             — True if the destination is adjacent to previous.
pub fn accumulate_transition(
    src_kind: NodeKind,
    dst_kind: NodeKind,
    delta_t: u64,
    is_return: bool,
    is_inout: bool,
) {
    let idx = src_kind.index() * NODE_KIND_COUNT + dst_kind.index();
    let mut stats = WALK_STATS.lock();
    let cell = &mut stats.cells[idx];
    // Clamp to prevent overflow on very long runs without an EM flush.
    if cell.transitions < u32::MAX {
        cell.transitions += 1;
        cell.sum_delta_t = cell.sum_delta_t.saturating_add(delta_t);
        if is_return {
            cell.return_count = cell.return_count.saturating_add(1);
        }
        if is_inout {
            cell.inout_count = cell.inout_count.saturating_add(1);
        }
    }
}

/// Snapshot the current accumulated statistics for external analysis.
/// Writes into `out[..NODE_KIND_COUNT²]`.  Returns the epoch number.
pub fn snapshot_stats(out: &mut [TypePairStats]) -> u32 {
    let stats = WALK_STATS.lock();
    let copy = out.len().min(NODE_KIND_COUNT * NODE_KIND_COUNT);
    out[..copy].copy_from_slice(&stats.cells[..copy]);
    stats.epoch
}

/// Snapshot statistics for a single type pair.
/// Writes one entry into `out[0]` and returns the epoch number.
/// Returns 0 if the NodeKind indices are out of range.
pub fn snapshot_stats_pair(src_kind: u16, dst_kind: u16, out: &mut [TypePairStats]) -> u32 {
    let s = src_kind as usize;
    let d = dst_kind as usize;
    if s >= NODE_KIND_COUNT || d >= NODE_KIND_COUNT || out.is_empty() {
        return 0;
    }
    let stats = WALK_STATS.lock();
    out[0] = stats.cells[s * NODE_KIND_COUNT + d];
    stats.epoch
}

/// EM M-step: update all per-type-pair parameters from accumulated statistics.
///
/// This implements one iteration of the EM algorithm from the PowerWalk
/// Duality Theorem.  Cells with fewer than 30 observations are skipped to
/// avoid fitting noise.
///
/// ## λ update (temporal decay)
///
///   λ_new = 1 / E[Δt] = transitions / sum_delta_t
///
/// This is the MLE for the exponential inter-arrival distribution, which the
/// WTG papers prove is the stationary distribution of the temporal walk gap
/// process.
///
/// ## p update (return parameter)
///
///   return_frac = return_count / transitions
///   1/p = return_frac × Z_factor
///
/// Under the symmetric edge-weight assumption, Z_factor ≈ 1/return_frac_prior
/// giving p_new = WEIGHT_ONE.  A simplified relative update is used:
///   p_new = WEIGHT_ONE² / max(return_count, 1) × (transitions / WEIGHT_ONE)
///
/// ## q update (in-out parameter)
///
///   explore_count = transitions − return_count − inout_count
///   q_new = WEIGHT_ONE² / max(explore_count, 1) × (transitions / WEIGHT_ONE)
///
/// Both p and q are clamped to [MIN_P, MAX_P] / [MIN_Q, MAX_Q].
///
/// After the update, statistics are cleared.  Returns the new epoch number.
pub fn em_step() -> u32 {
    let mut stats = WALK_STATS.lock();
    let mut params = TYPE_PAIR_MATRIX.lock();

    for src in 0..NODE_KIND_COUNT {
        for dst in 0..NODE_KIND_COUNT {
            let idx = src * NODE_KIND_COUNT + dst;
            let cell = &stats.cells[idx];

            if !cell.is_calibrated() {
                continue;
            }

            let n = cell.transitions as u64;
            let sum_dt = cell.sum_delta_t.max(1);

            // ── λ: MLE for exponential gap distribution ──
            // λ_new = transitions / sum_delta_t, in 16.16.
            // Numerator: n × WEIGHT_ONE²  (to keep 16.16 units after division).
            let lambda_numer = n.saturating_mul(WEIGHT_ONE as u64 * WEIGHT_ONE as u64);
            let lambda_raw = (lambda_numer / sum_dt) as u32;
            let lambda_new = lambda_raw.clamp(MIN_DECAY, MAX_DECAY);

            // ── p: return bias ──
            // Large return_count → small p (more return-friendly).
            // Small return_count → large p (discourages return).
            let r = cell.return_count.max(1) as u64;
            // p_new = (WEIGHT_ONE / return_fraction) = WEIGHT_ONE * n / r
            let p_raw = ((WEIGHT_ONE as u64 * n) / r) as u32;
            let p_new = p_raw.clamp(MIN_P, MAX_P);

            // ── q: in-out bias ──
            let explore = n
                .saturating_sub(cell.return_count as u64)
                .saturating_sub(cell.inout_count as u64)
                .max(1);
            let q_raw = ((WEIGHT_ONE as u64 * n) / explore) as u32;
            let q_new = q_raw.clamp(MIN_Q, MAX_Q);

            let Some(src_kind) = NodeKind::from_u16(src as u16) else {
                continue;
            };
            let Some(dst_kind) = NodeKind::from_u16(dst as u16) else {
                continue;
            };
            params.set(
                src_kind,
                dst_kind,
                TypePairEntry {
                    decay: lambda_new,
                    return_p: p_new,
                    inout_q: q_new,
                },
            );
        }
    }

    // Clear all calibrated cells; leave under-sampled cells to accumulate more.
    for cell in stats.cells.iter_mut() {
        if cell.is_calibrated() {
            *cell = TypePairStats::ZERO;
        }
    }

    stats.epoch += 1;
    stats.epoch
}

/// Returns how many type-pair cells currently have enough samples for the
/// next EM step (≥ 30 observations).  Useful for deciding when to call
/// `em_step()`.
pub fn em_ready_count() -> usize {
    WALK_STATS
        .lock()
        .cells
        .iter()
        .filter(|c| c.is_calibrated())
        .count()
}

/// Dump the type-pair matrix to serial for diagnostics.
///
/// Only dumps non-default entries to avoid flooding.
pub fn dump() {
    let m = TYPE_PAIR_MATRIX.lock();
    serial::write_line(b"[graph] === Type-Pair Parameter Matrix ===");

    let mut count = 0u32;
    for src in 0..NODE_KIND_COUNT {
        for dst in 0..NODE_KIND_COUNT {
            let e = m.entries[src * NODE_KIND_COUNT + dst];
            // Skip default entries.
            if e.decay == TypePairEntry::DEFAULT.decay
                && e.return_p == TypePairEntry::DEFAULT.return_p
                && e.inout_q == TypePairEntry::DEFAULT.inout_q
            {
                continue;
            }
            serial::write_bytes(b"  (");
            serial::write_u64_dec_inline(src as u64);
            serial::write_bytes(b",");
            serial::write_u64_dec_inline(dst as u64);
            serial::write_bytes(b") decay=");
            serial::write_u64_dec_inline(e.decay as u64);
            serial::write_bytes(b" p=");
            serial::write_u64_dec_inline(e.return_p as u64);
            serial::write_bytes(b" q=");
            serial::write_u64_dec(e.inout_q as u64);
            count += 1;
        }
    }

    serial::write_bytes(b"[graph] ");
    serial::write_u64_dec_inline(count as u64);
    serial::write_bytes(b" non-default entries, ");
    serial::write_u64_dec_inline((NODE_KIND_COUNT * NODE_KIND_COUNT) as u64);
    serial::write_line(b" total slots");
    serial::write_line(b"[graph] === End Type-Pair Matrix ===");
}
