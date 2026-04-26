// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Indexing pipeline — document → span → chunk → graph mutations.
//!
//! This module implements the SCCE provenance-preserving indexing pipeline
//! that transforms raw documents into graph nodes and BM25 index entries.
//!
//! ## Pipeline stages
//!
//! 1. **Document registration**: Create a Document node in the graph.
//! 2. **Span extraction**: Split document into paragraph-level spans.
//!    Each span becomes a Span node linked to its parent Document.
//! 3. **Chunking**: Split spans into fixed-size token chunks with overlap.
//!    Each chunk becomes a Chunk node linked to its parent Span.
//! 4. **BM25 indexing**: Index chunk text into the BM25 inverted index.
//! 5. **Entity extraction (stub)**: Identify entity mentions in chunks.
//!    Creates Mention nodes linked to Chunk nodes.
//!
//! ## Design
//! - No heap.  All structures are fixed-size.
//! - Chunk overlap for context preservation.
//! - Graph mutations use the arena API directly.
//! - BM25 index is the primary retrieval path.

use crate::graph::arena;
use crate::graph::types::*;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum chunk size in bytes.
const MAX_CHUNK_SIZE: usize = 512;

/// Overlap between consecutive chunks in bytes.
const CHUNK_OVERLAP: usize = 64;

/// Maximum number of spans per document.
const MAX_SPANS: usize = 256;

/// Maximum number of chunks per span.
const MAX_CHUNKS_PER_SPAN: usize = 64;

/// Maximum number of entity mentions per chunk.
const MAX_MENTIONS_PER_CHUNK: usize = 16;

// ────────────────────────────────────────────────────────────────────
// Pipeline state
// ────────────────────────────────────────────────────────────────────

/// A span boundary (start, length) within the document.
#[derive(Clone, Copy)]
pub struct SpanBounds {
    pub start: u32,
    pub len: u32,
}

/// A chunk boundary within a span.
#[derive(Clone, Copy)]
pub struct ChunkBounds {
    pub span_offset: u32, // byte offset within the span
    pub len: u32,
}

/// Result of indexing a single document.
pub struct IndexResult {
    /// Graph NodeId of the Document node.
    pub doc_node_id: NodeId,
    /// Number of spans created.
    pub span_count: u32,
    /// Number of chunks created.
    pub chunk_count: u32,
    /// Number of BM25 terms indexed.
    pub terms_indexed: u32,
    /// Whether the operation succeeded.
    pub success: bool,
}

/// Index a document into the graph and BM25 index.
///
/// `doc_text`: raw document bytes.
/// `doc_type`: classification of the document (maps to a flag byte).
/// `creator`: NodeId of the entity that provided this document.
/// `bm25`: mutable reference to the BM25 index for term insertion.
///
/// Returns an `IndexResult` with statistics.
pub fn index_document(
    doc_text: &[u8],
    doc_type: u8,
    creator: NodeId,
    bm25: &mut crate::cognitive::bm25::Bm25Index,
) -> IndexResult {
    let mut result = IndexResult {
        doc_node_id: 0,
        span_count: 0,
        chunk_count: 0,
        terms_indexed: 0,
        success: false,
    };

    // Stage 1: Create Document node.
    let doc_node = match arena::add_node(NodeKind::Document, doc_type as u32, creator) {
        Some(id) => id,
        None => return result,
    };
    result.doc_node_id = doc_node;

    // Stage 2: Extract spans (paragraph-level segmentation).
    let mut spans = [SpanBounds { start: 0, len: 0 }; MAX_SPANS];
    let span_count = extract_spans(doc_text, &mut spans);

    let mut si = 0;
    while si < span_count {
        let span = spans[si];
        let span_start = span.start as usize;
        let span_len = span.len as usize;
        if span_start + span_len > doc_text.len() {
            si += 1;
            continue;
        }
        let span_text = &doc_text[span_start..span_start + span_len];

        // Create Span node and link to Document.
        let span_node = match arena::add_node(NodeKind::Span, 0, creator) {
            Some(id) => id,
            None => break,
        };
        arena::add_edge_weighted(doc_node, span_node, EdgeKind::HasSpan, 0, WEIGHT_ONE);
        result.span_count += 1;

        // Stage 3: Chunk the span.
        let mut chunks = [ChunkBounds {
            span_offset: 0,
            len: 0,
        }; MAX_CHUNKS_PER_SPAN];
        let chunk_count = extract_chunks(span_text, &mut chunks);

        let mut ci = 0;
        while ci < chunk_count {
            let chunk = chunks[ci];
            let c_start = chunk.span_offset as usize;
            let c_len = chunk.len as usize;
            if c_start + c_len > span_text.len() {
                ci += 1;
                continue;
            }
            let chunk_text = &span_text[c_start..c_start + c_len];

            // Create Chunk node and link to Span.
            let chunk_node = match arena::add_node(NodeKind::Chunk, 0, creator) {
                Some(id) => id,
                None => break,
            };
            arena::add_edge_weighted(span_node, chunk_node, EdgeKind::DerivedChunk, 0, WEIGHT_ONE);
            result.chunk_count += 1;

            // Stage 4: BM25 indexing.
            if let Some(doc_id) = bm25.add_document(chunk_node) {
                let terms = bm25.index_text(chunk_text, doc_id);
                result.terms_indexed += terms;
            }

            ci += 1;
        }

        si += 1;
    }

    result.success = true;
    result
}

/// Split text into paragraph-level spans.
/// Paragraphs are separated by blank lines (two or more consecutive newlines).
/// Returns the number of spans found.
fn extract_spans(text: &[u8], out: &mut [SpanBounds]) -> usize {
    let len = text.len();
    let max_spans = out.len();
    let mut count = 0usize;
    let mut pos = 0usize;

    while pos < len && count < max_spans {
        // Skip leading blank lines.
        while pos < len && text[pos] == b'\n' {
            pos += 1;
        }
        let start = pos;

        // Find end of paragraph: two consecutive newlines or end of text.
        let mut end = pos;
        while end < len {
            if end + 1 < len && text[end] == b'\n' && text[end + 1] == b'\n' {
                break;
            }
            end += 1;
        }

        if end > start {
            out[count] = SpanBounds {
                start: start as u32,
                len: (end - start) as u32,
            };
            count += 1;
        }
        // Advance past the current span. If we stopped on a blank-line separator,
        // skip both newline bytes. Otherwise advance to end. As a final safety
        // net, force progress by one byte if nothing moved.
        let prev_pos = pos;
        pos = end;
        if pos + 1 < len && text[pos] == b'\n' && text[pos + 1] == b'\n' {
            pos += 2;
        }
        if pos <= prev_pos {
            pos = prev_pos + 1;
        }
    }

    count
}

/// Split a span into overlapping fixed-size chunks.
/// Returns the number of chunks.
fn extract_chunks(text: &[u8], out: &mut [ChunkBounds]) -> usize {
    let len = text.len();
    let max_chunks = out.len();
    let mut count = 0usize;
    let mut pos = 0usize;

    while pos < len && count < max_chunks {
        let remaining = len - pos;
        let chunk_len = if remaining <= MAX_CHUNK_SIZE {
            remaining
        } else {
            // Try to break at a whitespace boundary.
            let mut end = pos + MAX_CHUNK_SIZE;
            while end > pos + MAX_CHUNK_SIZE - 64 {
                if end < len && (text[end] == b' ' || text[end] == b'\n') {
                    break;
                }
                end -= 1;
            }
            if end <= pos + MAX_CHUNK_SIZE - 64 {
                end = pos + MAX_CHUNK_SIZE; // no good break point found
            }
            end - pos
        };

        out[count] = ChunkBounds {
            span_offset: pos as u32,
            len: chunk_len as u32,
        };
        count += 1;

        // Advance with overlap.
        if chunk_len >= CHUNK_OVERLAP {
            pos += chunk_len - CHUNK_OVERLAP;
        } else {
            pos += chunk_len;
        }

        // Don't create a chunk that's just the overlap remainder.
        if len - pos < CHUNK_OVERLAP && pos < len {
            // Include the tail in the last chunk if small.
            if count > 0 {
                let prev = &mut out[count - 1];
                prev.len = (len - prev.span_offset as usize) as u32;
            }
            break;
        }
    }

    count
}
