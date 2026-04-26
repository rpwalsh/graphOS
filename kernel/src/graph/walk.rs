// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Temporal walk primitives — PowerWalk-grounded walk state and sampling.
//!
//! This module provides the in-kernel walk sampling infrastructure required
//! by the unified temporal operator and the PowerWalk framework.
//!
//! ## Walk hierarchy (strict containment, proven in the papers)
//!
//! DeepWalk ⊊ node2vec ⊊ TempNode2vec ⊊ PowerWalk
//!
//! PowerWalk is the most expressive walk model: it conditions each
//! transition on the (source-type, dest-type) pair through per-type
//! parameters (p_{ττ'}, q_{ττ'}, λ_{ττ'}), recovering all lower models
//! as special cases.
//!
//! ## Transition model
//!
//! P(x | s, v, t) = w̃(v,x,t) · α(s,x,p_{ττ'},q_{ττ'}) / Z
//!
//! where:
//! - w̃(v,x,t) = w(v,x) · exp(−λ_{ττ'} · (t − t_{(v,x)}))
//!   is the temporally-decayed edge weight
//! - α is the node2vec bias function extended to heterogeneous types
//! - Z is the normalisation constant
//! - s is the previous node (for second-order bias)
//! - τ, τ' are the types of v, x respectively
//!
//! ## Causal ordering invariant
//!
//! A valid temporal walk w = (v₀, t₀), (v₁, t₁), …, (vₖ, tₖ) must satisfy:
//!   t₀ ≤ t₁ ≤ … ≤ tₖ   (causal monotonicity)
//!
//! The `WalkState` enforces this as a hard invariant.
//!
//! ## Walk-length optimality
//!
//!   L*(τ,τ',ε) = ⌈ log(1/ε) / γ_{ττ'} ⌉
//!
//! from the spectral gap of the type-pair-restricted transition matrix.
//! See `temporal::optimal_walk_length()`.
//!
//! ## Design
//! - WalkState is stack-allocated, fixed-size, no heap.
//! - Maximum walk length is 64 steps (sufficient for all papers' experiments).
//! - Walk buffers are ephemeral — they do not persist in the arena.
//! - The arena's adjacency lists provide O(degree) neighbor enumeration.

use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum walk length (number of steps, not nodes — walk has len+1 nodes).
pub const MAX_WALK_LEN: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Walk step
// ────────────────────────────────────────────────────────────────────

/// A single step in a temporal walk: the node visited and the timestamp
/// at the time of the transition.
///
/// 16 bytes, repr(C), Copy.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WalkStep {
    /// The node visited at this step.
    pub node: NodeId,
    /// The timestamp when this step was taken.
    /// Must satisfy causal ordering: t[i] ≤ t[i+1].
    pub timestamp: Timestamp,
}

impl WalkStep {
    pub const EMPTY: Self = Self {
        node: 0,
        timestamp: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// Walk state
// ────────────────────────────────────────────────────────────────────

/// The state of an in-progress or completed temporal walk.
///
/// Stack-allocated, fixed-size. Enforces causal ordering.
///
/// The walk records both the node sequence and the edge used at each
/// transition (for edge-type-conditioned analysis).
///
/// Size: ~2.1 KiB (stack-friendly for kernel use).
pub struct WalkState {
    /// The sequence of (node, timestamp) pairs.
    pub steps: [WalkStep; MAX_WALK_LEN + 1],

    /// The EdgeKind used at each transition (steps[i] → steps[i+1]).
    /// Index i corresponds to the edge between step i and step i+1.
    pub edge_kinds: [EdgeKind; MAX_WALK_LEN],

    /// The edge weight (16.16) at each transition.
    pub edge_weights: [Weight; MAX_WALK_LEN],

    /// Current walk length (number of transitions completed).
    /// The walk contains `len + 1` nodes.
    pub len: usize,

    /// The NodeKind of the walk origin (for type-conditioned analysis).
    pub origin_kind: NodeKind,
}

impl WalkState {
    /// Create a new walk starting at `origin` at time `t`.
    pub fn new(origin: NodeId, origin_kind: NodeKind, t: Timestamp) -> Self {
        let mut state = Self {
            steps: [WalkStep::EMPTY; MAX_WALK_LEN + 1],
            edge_kinds: [EdgeKind::Owns; MAX_WALK_LEN],
            edge_weights: [0; MAX_WALK_LEN],
            len: 0,
            origin_kind,
        };
        state.steps[0] = WalkStep {
            node: origin,
            timestamp: t,
        };
        state
    }

    /// Attempt to extend the walk by one step.
    ///
    /// Returns `true` if the step was added, `false` if:
    /// - The walk is at maximum length, or
    /// - The causal ordering invariant would be violated (t < last timestamp).
    pub fn push(
        &mut self,
        node: NodeId,
        timestamp: Timestamp,
        edge_kind: EdgeKind,
        edge_weight: Weight,
    ) -> bool {
        if self.len >= MAX_WALK_LEN {
            return false;
        }

        // Causal ordering invariant: t[i] ≤ t[i+1].
        let last_t = self.steps[self.len].timestamp;
        if timestamp < last_t {
            return false;
        }

        self.edge_kinds[self.len] = edge_kind;
        self.edge_weights[self.len] = edge_weight;
        self.len += 1;
        self.steps[self.len] = WalkStep { node, timestamp };
        true
    }

    /// The most recently visited node.
    pub fn current_node(&self) -> NodeId {
        self.steps[self.len].node
    }

    /// The previous node (for second-order bias computation).
    /// Returns `None` if the walk has zero transitions.
    pub fn previous_node(&self) -> Option<NodeId> {
        if self.len == 0 {
            None
        } else {
            Some(self.steps[self.len - 1].node)
        }
    }

    /// The timestamp of the most recent step.
    pub fn current_timestamp(&self) -> Timestamp {
        self.steps[self.len].timestamp
    }

    /// The walk origin node.
    pub fn origin(&self) -> NodeId {
        self.steps[0].node
    }

    /// Elapsed time from walk start to current position.
    pub fn elapsed(&self) -> u64 {
        self.steps[self.len]
            .timestamp
            .saturating_sub(self.steps[0].timestamp)
    }

    /// Returns `true` if the walk has visited `node` (cycle detection).
    pub fn has_visited(&self, node: NodeId) -> bool {
        for i in 0..=self.len {
            if self.steps[i].node == node {
                return true;
            }
        }
        false
    }

    /// Count how many times a specific NodeKind appears in the walk.
    /// Requires a lookup function since we don't store kinds per step.
    ///
    /// `kind_of` maps NodeId → NodeKind (caller provides from arena).
    pub fn count_kind(
        &self,
        target: NodeKind,
        kind_of: impl Fn(NodeId) -> Option<NodeKind>,
    ) -> usize {
        let mut count = 0;
        for i in 0..=self.len {
            if let Some(k) = kind_of(self.steps[i].node)
                && k == target
            {
                count += 1;
            }
        }
        count
    }
}

// ────────────────────────────────────────────────────────────────────
// Walk scoring (PowerWalk bias function)
// ────────────────────────────────────────────────────────────────────

/// Compute the PowerWalk second-order bias α(s, x, p, q) for a candidate
/// next node `x`, given:
/// - `prev` (s): the previous node in the walk
/// - `current` (v): the current node
/// - `candidate` (x): the candidate next node
/// - `return_p`: p parameter in 16.16
/// - `inout_q`: q parameter in 16.16
/// - `prev_is_candidate`: true if x == s (return to previous)
/// - `candidate_adjacent_to_prev`: true if edge (s, x) exists
///
/// Returns the bias multiplier in 16.16 fixed-point.
///
/// From node2vec:
///   α(s, x) = 1/p  if x == s       (return)
///   α(s, x) = 1    if d(s,x) == 1  (neighbor of prev)
///   α(s, x) = 1/q  otherwise        (explore)
///
/// In 16.16: 1/p = WEIGHT_ONE² / p = (65536 * 65536) / p.
pub const fn powerwalk_bias(
    prev_is_candidate: bool,
    candidate_adjacent_to_prev: bool,
    return_p: Weight,
    inout_q: Weight,
) -> Weight {
    if prev_is_candidate {
        // α = 1/p: return bias
        if return_p == 0 {
            return WEIGHT_ONE; // Degenerate: treat as unbiased.
        }
        // (WEIGHT_ONE * WEIGHT_ONE) / p, capped at u32::MAX.
        let numer = WEIGHT_ONE as u64 * WEIGHT_ONE as u64;
        let result = numer / return_p as u64;
        if result > u32::MAX as u64 {
            u32::MAX
        } else {
            result as u32
        }
    } else if candidate_adjacent_to_prev {
        // α = 1: neighbor of previous node.
        WEIGHT_ONE
    } else {
        // α = 1/q: exploration bias.
        if inout_q == 0 {
            return WEIGHT_ONE;
        }
        let numer = WEIGHT_ONE as u64 * WEIGHT_ONE as u64;
        let result = numer / inout_q as u64;
        if result > u32::MAX as u64 {
            u32::MAX
        } else {
            result as u32
        }
    }
}

/// Compute the unnormalised PowerWalk transition score for a candidate edge.
///
/// score = w̃(v,x,t) · α(s,x,p,q)
///
/// where w̃ is the temporally-decayed weight of edge (v→x).
///
/// `decayed_weight`: the output of Edge::decayed_weight() for this edge.
/// `bias`: the output of powerwalk_bias() for this candidate.
///
/// Returns the score in 16.16 fixed-point.
pub const fn transition_score(decayed_weight: Weight, bias: Weight) -> Weight {
    let product = decayed_weight as u64 * bias as u64;
    let result = product >> 16;
    if result > u32::MAX as u64 {
        u32::MAX
    } else {
        result as u32
    }
}
