// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! In-kernel graph subsystem — the core abstraction of GraphOS.
//!
//! The graph is the shared substrate for scheduling, diagnostics, trust
//! enforcement, predictive reasoning, and bounded AI participation.
//! Every kernel object that matters is a node. Relationships are typed,
//! directed, weighted, timestamped edges with creator provenance.
//!
//! ## Mathematical foundation
//!
//! The graph implements a **heterogeneous temporal graph** H = (V, E, τ_V, τ_E, t)
//! with the **unified temporal operator**:
//!
//!   S_{ττ'}(h, Δt) = exp(−λ_{ττ'} · Δt) · W_{ττ'} · h
//!
//! proven equivalent across random-walk sampling (PowerWalk) and per-type
//! TGNN message passing by the Duality Theorem.
//!
//! ## Expressiveness hierarchy
//!   1-WL ⊊ TGN ⊊ per-type-TGNN ⊊ 2-WL
//!
//! The per-type distinction in NodeKind/EdgeKind lifts the kernel above
//! the Temporal Score Collapse blind spot of homogeneous TGN.
//!
//! ## Modules
//! - `types`:    Node, Edge, NodeKind, EdgeKind, Weight, flags, ADJ_NONE
//! - `arena`:    Static graph store with adjacency lists, weighted edges, temporal queries
//! - `seed`:     Boot-time graph population from BootInfo and hardware state
//! - `temporal`: Per-type-pair decay matrix (λ, p, q), timestamp utilities
//! - `spectral`: Eigenvalue tracking, CUSUM detector, Fiedler drift monitoring
//! - `walk`:     PowerWalk-grounded walk state, transition scoring, causal ordering
//! - `pattern`:  Subgraph isomorphism types, MDL compression metrics, WL labels
//! - `causal`:   Causal graph (happened-before ordering, DAG constraints)
//! - `tensor`:   4D audit tensor types (CP decomposition, PPMI, anomaly detection)

pub mod arena;
pub mod bootstrap;
pub mod causal;
pub mod handles;
pub mod pattern;
pub mod seed;
pub mod spectral;
pub mod temporal;
pub mod tensor;
pub mod twin;
pub mod types;
pub mod walk;
pub mod walsh;
pub mod wtg;
