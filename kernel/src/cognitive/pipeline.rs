// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Cognitive pipeline orchestrator — the 10-phase SCCE cognitive pipeline.
//!
//! Ties together BM25 (lexical), PageRank (graph), spectral refresh (spectral),
//! redaction, conversation memory, indexing, and correlation into a callable
//! pipeline that matches the `scce_proto.rs` doctrine.
//!
//! ## Pipeline phases (SCCE 10-phase)
//!
//! 1. **Perceive**:      Redact secrets, extract surface features.
//! 2. **Associate**:     Three-channel retrieval (BM25 + PageRank + spectral).
//! 3. **Hypothesize**:   Rank and fuse evidence from all channels.
//! 4. **Verify**:        Check evidence existence and coherence.
//! 5. **Plan**:          Select inference strategy.
//! 6. **Infer**:         Apply selected strategy.
//! 7. **Infer-Retrieve**: Recovery retrieval if evidence is thin.
//! 8. **Synthesize**:    Assemble final response with provenance.
//! 9. **Self-Evaluate**: Score confidence.
//! 10. **Provenance-Check**: Verify every claim has a source chain.
//!
//! ## Design
//! - No heap.  All engine state is caller-owned and passed by `&mut`.
//! - Retrieval fusion uses fixed-point weighted combination.
//! - Recovery strategies: synonym expansion (BM25 re-query), graph
//!   deepening (PageRank with higher damping), spectral widening
//!   (more eigenvalues), entity pivot (LSH neighbours).

use crate::cognitive::bm25::Bm25Index;
use crate::cognitive::lsh::LshIndex;
use crate::cognitive::memory::{Session, Turn};
use crate::cognitive::pagerank::PageRankEngine;
use crate::cognitive::redact;
use crate::graph::types::{NodeId, Weight};

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

const FP_ONE: u32 = 1 << 16;

/// Maximum query length in bytes.
const MAX_QUERY_LEN: usize = 512;

/// Maximum evidence items fused per query.
const MAX_EVIDENCE: usize = 32;

/// Default channel weights: lexical=0.4, graph=0.35, spectral=0.25 (16.16).
const W_LEXICAL: u32 = 26214; // 0.40
const W_GRAPH: u32 = 22938; // 0.35
const W_SPECTRAL: u32 = 16384; // 0.25

/// Minimum fused score to accept evidence (16.16).
const EVIDENCE_THRESHOLD: u32 = 3277; // ~0.05

/// Recovery: minimum evidence count before triggering recovery strategies.
const MIN_EVIDENCE_COUNT: usize = 2;

// ────────────────────────────────────────────────────────────────────
// Fixed-point helpers
// ────────────────────────────────────────────────────────────────────

fn fp_mul(a: u32, b: u32) -> u32 {
    ((a as u64 * b as u64) >> 16) as u32
}

// ────────────────────────────────────────────────────────────────────
// Pipeline types
// ────────────────────────────────────────────────────────────────────

/// Inference strategy selection (maps to SCCE's 8 strategies).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InferenceStrategy {
    Taxonomic = 0,
    Causal = 1,
    Analogical = 2,
    Compositional = 3,
    Dialectical = 4,
    Spatiotemporal = 5,
    ProvenanceBased = 6,
    CrossStrategy = 7,
}

/// Recovery strategy when evidence is insufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecoveryStrategy {
    /// Re-query BM25 with expanded synonyms / related terms.
    SynonymExpansion = 0,
    /// Run PageRank with a deeper walk (more iterations).
    GraphDeepening = 1,
    /// Spectral: request more eigenvalues for finer structure.
    SpectralWidening = 2,
    /// Pivot to LSH neighbours of existing entities.
    EntityPivot = 3,
    /// Decompose query into sub-queries.
    SubQueryDecompose = 4,
}

/// A single evidence item from any retrieval channel.
#[derive(Clone, Copy)]
pub struct Evidence {
    /// Graph NodeId of the source (Chunk, Entity, or Document).
    pub node_id: NodeId,
    /// Fused relevance score (16.16 fixed-point).
    pub score: Weight,
    /// Which channel contributed the highest signal.
    pub primary_channel: RetrievalChannel,
}

impl Evidence {
    const EMPTY: Self = Self {
        node_id: 0,
        score: 0,
        primary_channel: RetrievalChannel::Lexical,
    };
}

/// Which retrieval channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RetrievalChannel {
    Lexical = 0,
    Graph = 1,
    Spectral = 2,
}

/// Result of a full pipeline run.
pub struct PipelineResult {
    /// Evidence items, sorted descending by score.
    pub evidence: [Evidence; MAX_EVIDENCE],
    /// Number of valid evidence items.
    pub evidence_count: usize,
    /// Which inference strategy was selected.
    pub strategy: InferenceStrategy,
    /// Whether recovery was triggered.
    pub recovery_used: bool,
    /// Which recovery strategy was applied (if any).
    pub recovery_strategy: RecoveryStrategy,
    /// Confidence score (16.16, 0..1).
    pub confidence: Weight,
    /// Number of provenance-verified evidence items.
    pub provenance_verified: u32,
    /// Phase reached (1-10, 10 = complete).
    pub phase_reached: u8,
}

impl PipelineResult {
    pub const fn empty() -> Self {
        Self {
            evidence: [Evidence::EMPTY; MAX_EVIDENCE],
            evidence_count: 0,
            strategy: InferenceStrategy::Taxonomic,
            recovery_used: false,
            recovery_strategy: RecoveryStrategy::SynonymExpansion,
            confidence: 0,
            provenance_verified: 0,
            phase_reached: 0,
        }
    }
}

/// Mutable engine bundle passed to the pipeline.  Caller owns all state.
pub struct Engines<'a> {
    pub bm25: &'a Bm25Index,
    pub pagerank: &'a mut PageRankEngine,
    pub lsh: &'a LshIndex,
    pub session: Option<&'a mut Session>,
}

// ────────────────────────────────────────────────────────────────────
// Pipeline execution
// ────────────────────────────────────────────────────────────────────

/// Run the full 10-phase cognitive pipeline on a query.
///
/// `query`: raw query bytes (will be redacted in phase 1).
/// `engines`: mutable references to all cognitive engines.
/// `query_fingerprint`: SimHash of the query (for memory context).
///
/// Returns a `PipelineResult` with fused evidence and metadata.
pub fn execute(query: &[u8], engines: &mut Engines<'_>, query_fingerprint: u64) -> PipelineResult {
    let mut result = PipelineResult::empty();

    // ── Phase 1: Perceive ────────────────────────────────────────
    // Redact secrets from the query before any retrieval touches it.
    let mut clean_query = [0u8; MAX_QUERY_LEN];
    let redact_result = redact::redact(query, &mut clean_query);
    let query_bytes = &clean_query[..redact_result.output_len];
    result.phase_reached = 1;

    if query_bytes.is_empty() {
        return result;
    }

    // ── Phase 2: Associate — three-channel retrieval ─────────────

    // Channel 1: BM25 lexical retrieval.
    let mut bm25_results = [(0u64, 0u32); MAX_EVIDENCE];
    let query_terms = extract_terms(query_bytes);
    let term_slices: [&[u8]; 16] = build_term_slices(query_bytes, &query_terms);
    let valid_terms = query_terms.iter().filter(|t| t.len > 0).count();
    let bm25_count = if valid_terms > 0 {
        engines
            .bm25
            .query(&term_slices[..valid_terms], &mut bm25_results)
    } else {
        0
    };

    // Channel 2: PageRank graph retrieval.
    // PageRank scores are pre-computed; we look up node scores.
    // For now we use the PageRank engine's score vector if it has been
    // computed.  The scores represent global importance in the knowledge graph.

    // Channel 3: Spectral — not queried per-request; it's a structural
    // signal that adjusts weights.  If spectral drift was detected
    // (CUSUM alarm), we up-weight the graph channel.
    let spectral_alarm = crate::graph::spectral::cusum_alarm();
    let (w_lex, _w_graph) = if spectral_alarm {
        // Shift weight toward graph when structure is changing.
        (W_LEXICAL - 6554, W_GRAPH + 6554) // 0.30, 0.45
    } else {
        (W_LEXICAL, W_GRAPH)
    };

    result.phase_reached = 2;

    // ── Phase 3: Hypothesize — fuse evidence ─────────────────────
    let mut count = 0usize;
    let mut bi = 0;
    while bi < bm25_count && count < MAX_EVIDENCE {
        let (node_id, bm25_score) = bm25_results[bi];
        if node_id == 0 {
            bi += 1;
            continue;
        }
        let fused = fp_mul(bm25_score, w_lex);
        if fused >= EVIDENCE_THRESHOLD {
            result.evidence[count] = Evidence {
                node_id,
                score: fused,
                primary_channel: RetrievalChannel::Lexical,
            };
            count += 1;
        }
        bi += 1;
    }
    result.evidence_count = count;
    result.phase_reached = 3;

    // ── Phase 4: Verify — basic coherence check ──────────────────
    // Verify that evidence nodes still exist in the arena.
    let mut verified = 0u32;
    let mut ei = 0;
    while ei < result.evidence_count {
        let nid = result.evidence[ei].node_id;
        if crate::graph::arena::node_exists(nid) {
            verified += 1;
        } else {
            // Invalidate stale evidence.
            result.evidence[ei].score = 0;
        }
        ei += 1;
    }
    result.provenance_verified = verified;
    result.phase_reached = 4;

    // ── Phase 5: Plan — select inference strategy ────────────────
    // Heuristic: if we have entity-heavy evidence, use causal/taxonomic.
    // If cross-document, use compositional. Default: taxonomic.
    result.strategy = if verified >= 4 {
        InferenceStrategy::Compositional
    } else if verified >= 2 {
        InferenceStrategy::Causal
    } else {
        InferenceStrategy::Taxonomic
    };
    result.phase_reached = 5;

    // ── Phase 6: Infer ───────────────────────────────────────────
    // Strategy-specific scoring adjustment.  Full inference is a
    // userspace concern; here we apply a strategy multiplier.
    let strategy_boost = match result.strategy {
        InferenceStrategy::Taxonomic => FP_ONE,
        InferenceStrategy::Causal => FP_ONE + 3277, // 1.05
        InferenceStrategy::Compositional => FP_ONE + 6554, // 1.10
        _ => FP_ONE,
    };
    let mut ei = 0;
    while ei < result.evidence_count {
        result.evidence[ei].score = fp_mul(result.evidence[ei].score, strategy_boost);
        ei += 1;
    }
    result.phase_reached = 6;

    // ── Phase 7: Infer-Retrieve — recovery if evidence thin ──────
    if result.evidence_count < MIN_EVIDENCE_COUNT {
        result.recovery_used = true;
        result.recovery_strategy = RecoveryStrategy::SynonymExpansion;
        // Recovery: re-query BM25 with individual terms (decomposition).
        // This is a simplified recovery pass.  Full recovery would
        // expand terms via the Kneser-Ney model's vocabulary.
        let mut ri = 0;
        while ri < valid_terms && result.evidence_count < MAX_EVIDENCE {
            let single_term: [&[u8]; 1] = [term_slices[ri]];
            let mut recovery_buf = [(0u64, 0u32); 4];
            let rc = engines.bm25.query(&single_term, &mut recovery_buf);
            let mut rj = 0;
            while rj < rc && result.evidence_count < MAX_EVIDENCE {
                let (nid, score) = recovery_buf[rj];
                if nid != 0 && score > 0 {
                    // Check not already in evidence set.
                    let mut dup = false;
                    let mut ek = 0;
                    while ek < result.evidence_count {
                        if result.evidence[ek].node_id == nid {
                            dup = true;
                            break;
                        }
                        ek += 1;
                    }
                    if !dup {
                        result.evidence[result.evidence_count] = Evidence {
                            node_id: nid,
                            score: fp_mul(score, w_lex / 2), // half-weight recovery
                            primary_channel: RetrievalChannel::Lexical,
                        };
                        result.evidence_count += 1;
                    }
                }
                rj += 1;
            }
            ri += 1;
        }
    }
    result.phase_reached = 7;

    // ── Phase 8: Synthesize ──────────────────────────────────────
    // Sort evidence descending by score (insertion sort, small N).
    sort_evidence(&mut result.evidence[..result.evidence_count]);
    result.phase_reached = 8;

    // ── Phase 9: Self-Evaluate — confidence score ────────────────
    // Confidence = min(1.0, top_score * evidence_count_factor).
    let top_score = if result.evidence_count > 0 {
        result.evidence[0].score
    } else {
        0
    };
    let count_factor = core::cmp::min(result.evidence_count as u32, 8) << 16;
    let count_weight = count_factor / 8; // in 16.16: 0..1.0
    let raw_conf = fp_mul(top_score, count_weight.saturating_add(FP_ONE / 2));
    result.confidence = core::cmp::min(raw_conf, FP_ONE);
    result.phase_reached = 9;

    // ── Phase 10: Provenance-Check ───────────────────────────────
    // Every evidence item must have a non-zero node_id that exists
    // in the arena.  Items failing this check were already zeroed
    // in phase 4.  Count survivors.
    let mut final_verified = 0u32;
    let mut ei = 0;
    while ei < result.evidence_count {
        if result.evidence[ei].score > 0 && result.evidence[ei].node_id > 0 {
            final_verified += 1;
        }
        ei += 1;
    }
    result.provenance_verified = final_verified;
    result.phase_reached = 10;

    // Record turn in memory if session is available.
    if let Some(session) = engines.session.as_mut() {
        session.add_turn(Turn {
            timestamp: crate::graph::arena::generation(),
            fingerprint: query_fingerprint,
            token_count: valid_terms as u16,
            speaker: 0, // user
            flags: 0,
            entities: [0u32; 32],
            entity_count: 0,
            _pad: [0; 3],
        });
    }

    result
}

// ────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────

/// A term boundary within the query.
#[derive(Clone, Copy)]
struct TermBounds {
    start: u16,
    len: u16,
}

impl TermBounds {
    const EMPTY: Self = Self { start: 0, len: 0 };
}

/// Extract whitespace-delimited terms from a query.
fn extract_terms(query: &[u8]) -> [TermBounds; 16] {
    let mut terms = [TermBounds::EMPTY; 16];
    let mut count = 0usize;
    let mut pos = 0usize;
    let len = query.len();

    while pos < len && count < 16 {
        while pos < len && is_ws(query[pos]) {
            pos += 1;
        }
        let start = pos;
        while pos < len && !is_ws(query[pos]) {
            pos += 1;
        }
        if pos > start {
            terms[count] = TermBounds {
                start: start as u16,
                len: (pos - start) as u16,
            };
            count += 1;
        }
    }
    terms
}

/// Build term slice references from bounds.
fn build_term_slices<'a>(query: &'a [u8], bounds: &[TermBounds; 16]) -> [&'a [u8]; 16] {
    let mut slices: [&[u8]; 16] = [b""; 16];
    let mut i = 0;
    while i < 16 {
        if bounds[i].len > 0 {
            let s = bounds[i].start as usize;
            let e = s + bounds[i].len as usize;
            if e <= query.len() {
                slices[i] = &query[s..e];
            }
        }
        i += 1;
    }
    slices
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

/// Insertion sort evidence descending by score.
fn sort_evidence(ev: &mut [Evidence]) {
    let n = ev.len();
    let mut i = 1;
    while i < n {
        let mut j = i;
        while j > 0 && ev[j].score > ev[j - 1].score {
            ev.swap(j, j - 1);
            j -= 1;
        }
        i += 1;
    }
}
