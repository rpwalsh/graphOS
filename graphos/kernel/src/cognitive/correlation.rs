// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Correlation pipeline — mention → entity → relation promotion.
//!
//! This module implements the SCCE entity resolution and relation
//! extraction pipeline that promotes surface-level mentions into
//! first-class graph entities and typed relations.
//!
//! ## Pipeline stages
//!
//! 1. **Mention detection**: Identify entity mentions in chunk text
//!    (named entities, noun phrases, code identifiers).
//! 2. **Entity resolution**: Match mentions to existing entities using
//!    LSH SimHash + Hamming distance.  If no match within threshold,
//!    create a new Entity node.
//! 3. **Relation extraction**: For co-occurring entity pairs within
//!    a chunk window, propose typed relations based on syntactic
//!    proximity patterns.
//! 4. **Relation promotion**: Create relation edges in the graph after
//!    evidence accumulation (BloomFilter for dedup, CMS for frequency).
//!
//! ## Design
//! - Uses the LSH index for candidate matching.
//! - Uses BloomFilter to track already-processed mention pairs.
//! - Uses CountMinSketch for co-occurrence frequency estimation.
//! - All structures are fixed-size, no heap.

use crate::cognitive::lsh::{LshIndex, simhash_text};
use crate::cognitive::sketch::{BloomFilter, CountMinSketch};
use crate::graph::arena;
use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum Hamming distance for entity match (6 out of 64 bits = ~91% similarity).
const MATCH_THRESHOLD: u32 = 6;

/// Minimum co-occurrence count before promoting a relation to the graph.
const PROMOTION_THRESHOLD: u32 = 3;

/// Maximum mentions per chunk.
const MAX_MENTIONS_PER_CHUNK: usize = 32;

/// Maximum length of a mention surface form.
const MAX_MENTION_LEN: usize = 64;

// ────────────────────────────────────────────────────────────────────
// Mention extraction
// ────────────────────────────────────────────────────────────────────

/// A detected mention in chunk text.
#[derive(Clone, Copy)]
pub struct Mention {
    /// Byte offset within the chunk.
    pub offset: u16,
    /// Length of the surface form.
    pub len: u16,
    /// SimHash fingerprint of the surface form.
    pub fingerprint: u64,
}

impl Mention {
    const EMPTY: Self = Self {
        offset: 0,
        len: 0,
        fingerprint: 0,
    };
}

/// Extract entity mentions from chunk text.
///
/// Strategy: Capitalised word sequences (proper nouns) and
/// backtick-delimited identifiers are candidate mentions.
///
/// Returns the number of mentions found.
pub fn extract_mentions(text: &[u8], out: &mut [Mention]) -> usize {
    let len = text.len();
    let max_out = out.len().min(MAX_MENTIONS_PER_CHUNK);
    let mut count = 0usize;
    let mut pos = 0usize;

    while pos < len && count < max_out {
        // Strategy 1: Capitalised word sequences (at least 2 chars, starts uppercase).
        if is_upper(text[pos]) {
            let start = pos;
            // Consume capitalised words separated by spaces.
            let mut end = pos;
            while end < len && (is_alpha(text[end]) || text[end] == b' ') {
                if text[end] == b' ' {
                    // Check if next char is uppercase (continuing the NP).
                    if end + 1 < len && is_upper(text[end + 1]) {
                        end += 1;
                        continue;
                    } else {
                        break;
                    }
                }
                end += 1;
            }
            let mention_len = end - start;
            if (2..=MAX_MENTION_LEN).contains(&mention_len) {
                let surface = &text[start..end];
                out[count] = Mention {
                    offset: start as u16,
                    len: mention_len as u16,
                    fingerprint: simhash_text(surface),
                };
                count += 1;
            }
            pos = end;
            continue;
        }

        // Strategy 2: Backtick-delimited identifiers.
        if text[pos] == b'`' {
            let start = pos + 1;
            let mut end = start;
            while end < len && text[end] != b'`' {
                end += 1;
            }
            let mention_len = end - start;
            if (2..=MAX_MENTION_LEN).contains(&mention_len) && end < len {
                let surface = &text[start..end];
                out[count] = Mention {
                    offset: start as u16,
                    len: mention_len as u16,
                    fingerprint: simhash_text(surface),
                };
                count += 1;
                pos = end + 1;
                continue;
            }
        }

        pos += 1;
    }

    count
}

// ────────────────────────────────────────────────────────────────────
// Entity resolution
// ────────────────────────────────────────────────────────────────────

/// Resolve a mention against the LSH index.
///
/// If a matching entity exists (Hamming ≤ threshold), returns its NodeId.
/// Otherwise creates a new Entity node in the graph and inserts into the
/// LSH index.
pub fn resolve_entity(mention: &Mention, lsh: &mut LshIndex, creator: NodeId) -> Option<NodeId> {
    // Query LSH index.
    let mut candidates = [(0u64, 0u32); 8];
    let n = lsh.query(mention.fingerprint, MATCH_THRESHOLD, &mut candidates);

    if n > 0 {
        // Return the best match (lowest Hamming distance).
        let mut best_id = candidates[0].0;
        let mut best_dist = candidates[0].1;
        let mut i = 1;
        while i < n {
            if candidates[i].1 < best_dist {
                best_dist = candidates[i].1;
                best_id = candidates[i].0;
            }
            i += 1;
        }
        Some(best_id)
    } else {
        // Create a new Entity node.
        let entity_node = arena::add_node(NodeKind::Entity, 0, creator)?;
        lsh.insert(mention.fingerprint, entity_node);
        Some(entity_node)
    }
}

// ────────────────────────────────────────────────────────────────────
// Relation extraction and promotion
// ────────────────────────────────────────────────────────────────────

/// Process a chunk: extract mentions, resolve entities, detect co-occurrences,
/// and promote relations that exceed the frequency threshold.
///
/// `chunk_text`: the raw chunk bytes.
/// `chunk_node`: NodeId of the Chunk node in the graph.
/// `creator`: NodeId of the pipeline task.
/// `lsh`: mutable LSH index for entity resolution.
/// `cooccur`: mutable CMS for co-occurrence counting.
/// `seen_pairs`: BloomFilter for deduplicating pair processing.
///
/// Returns (mentions_found, entities_resolved, relations_promoted).
pub fn process_chunk(
    chunk_text: &[u8],
    chunk_node: NodeId,
    creator: NodeId,
    lsh: &mut LshIndex,
    cooccur: &mut CountMinSketch,
    seen_pairs: &mut BloomFilter,
) -> (u32, u32, u32) {
    let mut mentions = [Mention::EMPTY; MAX_MENTIONS_PER_CHUNK];
    let mention_count = extract_mentions(chunk_text, &mut mentions);

    let mut entity_ids = [0u64; MAX_MENTIONS_PER_CHUNK];
    let mut entities_resolved = 0u32;

    // Resolve each mention to an entity.
    let mut mi = 0;
    while mi < mention_count {
        if let Some(eid) = resolve_entity(&mentions[mi], lsh, creator) {
            entity_ids[mi] = eid;
            entities_resolved += 1;

            // Link Mention → Entity and Chunk → Mention.
            if let Some(mention_node) = arena::add_node(NodeKind::Mention, 0, creator) {
                arena::add_edge_weighted(
                    chunk_node,
                    mention_node,
                    EdgeKind::ChunkMentions,
                    0,
                    WEIGHT_ONE,
                );
                arena::add_edge_weighted(
                    mention_node,
                    eid,
                    EdgeKind::MentionResolves,
                    0,
                    WEIGHT_ONE,
                );
            }
        }
        mi += 1;
    }

    // Co-occurrence: for every pair of distinct entities in this chunk,
    // increment co-occurrence count and check for promotion.
    let mut relations_promoted = 0u32;
    let mut i = 0;
    while i < mention_count {
        if entity_ids[i] == 0 {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < mention_count {
            if entity_ids[j] == 0 || entity_ids[j] == entity_ids[i] {
                j += 1;
                continue;
            }

            // Canonical pair key for the CMS and bloom filter.
            let (a, b) = if entity_ids[i] < entity_ids[j] {
                (entity_ids[i], entity_ids[j])
            } else {
                (entity_ids[j], entity_ids[i])
            };
            let mut pair_key = [0u8; 16];
            pair_key[0..8].copy_from_slice(&a.to_le_bytes());
            pair_key[8..16].copy_from_slice(&b.to_le_bytes());

            // Increment co-occurrence.
            cooccur.increment(&pair_key);
            let freq = cooccur.estimate(&pair_key);

            // Promote to a Relation edge if threshold met and not already promoted.
            if freq >= PROMOTION_THRESHOLD && !seen_pairs.may_contain(&pair_key) {
                seen_pairs.insert(&pair_key);

                // Create RelatedTo edge weighted by co-occurrence frequency.
                let weight = freq << 16; // frequency as 16.16
                arena::add_edge_weighted(a, b, EdgeKind::KnowledgeRelation, 0, weight);
                arena::add_edge_weighted(b, a, EdgeKind::KnowledgeRelation, 0, weight);
                relations_promoted += 1;
            }

            j += 1;
        }
        i += 1;
    }

    (mention_count as u32, entities_resolved, relations_promoted)
}

fn is_upper(b: u8) -> bool {
    b.is_ascii_uppercase()
}
fn is_alpha(b: u8) -> bool {
    b.is_ascii_alphabetic()
}
