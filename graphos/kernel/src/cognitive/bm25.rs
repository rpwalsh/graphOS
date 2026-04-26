// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! BM25 lexical retrieval engine — inverted index with Okapi BM25 scoring.
//!
//! Fixed-size, no_std, no heap. All scoring is 16.16 fixed-point.
//!
//! ## BM25 formula
//!
//!   score(q, d) = Σ_{t ∈ q} IDF(t) · (tf(t,d) · (k1 + 1)) / (tf(t,d) + k1 · (1 − b + b · |d| / avgdl))
//!
//! where:
//!   IDF(t) = ln((N − df(t) + 0.5) / (df(t) + 0.5) + 1)
//!   k1 = 1.2, b = 0.75
//!
//! ## Design
//! - Vocabulary: up to MAX_TERMS unique terms (hashed to 32-bit fingerprints).
//! - Documents: up to MAX_DOCS.
//! - Postings: up to MAX_POSTINGS (term, doc, tf) triples.
//! - Terms are stored as FNV-1a hashes, not raw strings.

use crate::graph::types::Weight;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

const MAX_TERMS: usize = 4096;
const MAX_DOCS: usize = 2048;
const MAX_POSTINGS: usize = 32768;

/// k1 = 1.2 in 16.16 = 78643
const K1: u32 = 78643;
/// b = 0.75 in 16.16 = 49152
const B: u32 = 49152;
/// 1.0 in 16.16
const FP_ONE: u32 = 1 << 16;

// ────────────────────────────────────────────────────────────────────
// FNV-1a (same as sketch.rs — duplicated to avoid cross-module dep)
// ────────────────────────────────────────────────────────────────────

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

fn fnv1a(data: &[u8]) -> u32 {
    let mut h = FNV_OFFSET;
    let mut i = 0;
    while i < data.len() {
        h ^= data[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h as u32
}

// ────────────────────────────────────────────────────────────────────
// Fixed-point math helpers
// ────────────────────────────────────────────────────────────────────

/// Fixed-point multiply: (a * b) >> 16
fn fp_mul(a: u32, b: u32) -> u32 {
    ((a as u64 * b as u64) >> 16) as u32
}

/// Fixed-point divide: (a << 16) / b
fn fp_div(a: u32, b: u32) -> u32 {
    if b == 0 {
        return u32::MAX;
    }
    (((a as u64) << 16) / (b as u64)) as u32
}

/// Integer natural log approximation in 16.16 fixed-point.
/// Uses ln(x) ≈ (log2(x)) × ln(2), where log2 is computed from
/// leading zeros and ln(2) ≈ 0.6931 = 45426 in 16.16.
///
/// Input: x in 16.16 fixed-point (must be > 0).
/// Output: ln(x) in 16.16 fixed-point.
fn fp_ln(x: u32) -> u32 {
    if x <= FP_ONE {
        return 0; // ln(x) ≤ 0 for x ≤ 1; clamp to 0 for positive scoring
    }
    // log2(x_fp) = log2(x_int << 16) = log2(x_int) + 16
    // But x is in 16.16, so real value = x / 65536.
    // log2(real) = log2(x) - 16.
    let bits = 32 - x.leading_zeros(); // floor(log2(x)) + 1
    // log2(real) ≈ bits - 1 - 16 = bits - 17
    if bits <= 17 {
        return 0;
    }
    let log2_int = bits - 17;
    // Fractional part: use one bit of sub-integer precision.
    // Check if the bit below is set for +0.5 refinement.
    let frac = if bits > 1 && (x & (1 << (bits - 2))) != 0 {
        32768u32
    } else {
        0u32
    };
    let log2_fp = (log2_int << 16) + frac;
    // ln(x) = log2(x) * ln(2) = log2_fp * 45426 >> 16
    fp_mul(log2_fp, 45426)
}

// ────────────────────────────────────────────────────────────────────
// Term dictionary
// ────────────────────────────────────────────────────────────────────

/// A term in the vocabulary, identified by its FNV hash.
#[derive(Clone, Copy)]
struct TermEntry {
    hash: u32,
    /// Document frequency — number of distinct documents containing this term.
    df: u32,
    /// First posting index in the postings array.
    first_posting: u32,
    /// Number of postings for this term.
    posting_count: u32,
}

impl TermEntry {
    const EMPTY: Self = Self {
        hash: 0,
        df: 0,
        first_posting: 0,
        posting_count: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// Posting
// ────────────────────────────────────────────────────────────────────

/// A (term → document) posting with term frequency.
#[derive(Clone, Copy)]
struct Posting {
    term_idx: u16,
    doc_id: u16,
    tf: u16, // raw term frequency in this document
    _pad: u16,
}

impl Posting {
    const EMPTY: Self = Self {
        term_idx: 0,
        doc_id: 0,
        tf: 0,
        _pad: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// Document metadata
// ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct DocMeta {
    /// Total number of terms in this document (for length normalisation).
    length: u32,
    /// An opaque ID that the caller uses (e.g. a graph NodeId).
    external_id: u64,
}

impl DocMeta {
    const EMPTY: Self = Self {
        length: 0,
        external_id: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// BM25 Index
// ────────────────────────────────────────────────────────────────────

/// A fixed-size BM25 inverted index.
pub struct Bm25Index {
    terms: [TermEntry; MAX_TERMS],
    term_count: usize,
    postings: [Posting; MAX_POSTINGS],
    posting_count: usize,
    docs: [DocMeta; MAX_DOCS],
    doc_count: usize,
    /// Sum of all document lengths (for avgdl computation).
    total_doc_length: u64,
}

impl Bm25Index {
    pub const fn new() -> Self {
        Self {
            terms: [TermEntry::EMPTY; MAX_TERMS],
            term_count: 0,
            postings: [Posting::EMPTY; MAX_POSTINGS],
            posting_count: 0,
            docs: [DocMeta::EMPTY; MAX_DOCS],
            doc_count: 0,
            total_doc_length: 0,
        }
    }

    /// Register a new document.  Returns the internal doc_id (0-based),
    /// or `None` if the index is full.
    pub fn add_document(&mut self, external_id: u64) -> Option<u16> {
        if self.doc_count >= MAX_DOCS {
            return None;
        }
        let id = self.doc_count as u16;
        self.docs[self.doc_count] = DocMeta {
            length: 0,
            external_id,
        };
        self.doc_count += 1;
        Some(id)
    }

    /// Index a single term occurrence in a document.
    ///
    /// `term` is the raw term bytes (will be hashed).
    /// `doc_id` is the internal doc_id from `add_document`.
    ///
    /// If the (term, doc) pair already has a posting, increments tf.
    /// Otherwise creates a new posting.
    pub fn index_term(&mut self, term: &[u8], doc_id: u16) -> bool {
        let hash = fnv1a(term);
        let term_idx = self.find_or_create_term(hash);
        let term_idx = match term_idx {
            Some(i) => i,
            None => return false,
        };

        // Try to find existing posting for (term, doc).
        let te = &self.terms[term_idx];
        let base = te.first_posting as usize;
        let count = te.posting_count as usize;
        let mut found = false;
        let mut j = 0;
        while j < count {
            let pi = base + j;
            if pi < MAX_POSTINGS && self.postings[pi].doc_id == doc_id {
                self.postings[pi].tf += 1;
                found = true;
                break;
            }
            j += 1;
        }

        if !found {
            // New posting.
            if self.posting_count >= MAX_POSTINGS {
                return false;
            }
            // If this term's postings are not contiguous at the end, we
            // append and update. Since we only ever append in order this
            // is contiguous as long as terms are indexed in batches.
            let pi = self.posting_count;
            if count == 0 {
                self.terms[term_idx].first_posting = pi as u32;
            }
            self.postings[pi] = Posting {
                term_idx: term_idx as u16,
                doc_id,
                tf: 1,
                _pad: 0,
            };
            self.posting_count += 1;
            self.terms[term_idx].posting_count += 1;
            self.terms[term_idx].df += 1;
        }

        // Update document length.
        self.docs[doc_id as usize].length += 1;
        self.total_doc_length += 1;
        true
    }

    /// Batch-index a sequence of whitespace-separated terms for a document.
    /// Returns the number of terms indexed.
    pub fn index_text(&mut self, text: &[u8], doc_id: u16) -> u32 {
        let mut count = 0u32;
        let mut start = 0usize;
        let len = text.len();
        while start < len {
            // Skip whitespace.
            while start < len && is_whitespace(text[start]) {
                start += 1;
            }
            let word_start = start;
            // Find end of word.
            while start < len && !is_whitespace(text[start]) {
                start += 1;
            }
            if start > word_start && self.index_term(&text[word_start..start], doc_id) {
                count += 1;
            }
        }
        count
    }

    /// Score a query against all documents. Returns the top-K results
    /// sorted by descending BM25 score.
    ///
    /// `query_terms` is a slice of raw term byte slices.
    /// `out` is a buffer for results. Returns the number of results written.
    ///
    /// Each result is (external_id, score_fp) where score is 16.16 fixed-point.
    pub fn query(&self, query_terms: &[&[u8]], out: &mut [(u64, Weight)]) -> usize {
        if self.doc_count == 0 || out.is_empty() {
            return 0;
        }

        let n = self.doc_count as u32;
        let avgdl = if n > 0 {
            fp_div(self.total_doc_length as u32, n)
        } else {
            FP_ONE
        };

        // Score accumulator per document.  We use `out` as scratch space
        // since MAX_DOCS can be large.  Instead, iterate query terms and
        // accumulate scores in a small fixed buffer.
        // For simplicity we cap scoring to the first 2048 docs.
        const SCORE_CAP: usize = 2048;
        let mut scores = [0u32; SCORE_CAP];
        let scoring_docs = core::cmp::min(self.doc_count, SCORE_CAP);

        let mut qi = 0;
        while qi < query_terms.len() {
            let hash = fnv1a(query_terms[qi]);
            if let Some(term_idx) = self.find_term(hash) {
                let te = &self.terms[term_idx];
                let idf = self.compute_idf(te.df, n);

                // Walk postings.
                let base = te.first_posting as usize;
                let cnt = te.posting_count as usize;
                let mut pi = 0;
                while pi < cnt {
                    let idx = base + pi;
                    if idx < MAX_POSTINGS {
                        let p = &self.postings[idx];
                        let did = p.doc_id as usize;
                        if did < scoring_docs {
                            let tf_fp = (p.tf as u32) << 16; // tf in 16.16
                            let dl = self.docs[did].length;
                            let dl_fp = dl << 16;
                            let score = self.bm25_term_score(tf_fp, dl_fp, avgdl, idf);
                            scores[did] = scores[did].saturating_add(score);
                        }
                    }
                    pi += 1;
                }
            }
            qi += 1;
        }

        // Extract top-K by repeated scan (K is small — out.len()).
        let k = out.len();
        let mut written = 0;
        let mut used = [false; SCORE_CAP];
        while written < k {
            let mut best_idx = usize::MAX;
            let mut best_score = 0u32;
            let mut d = 0;
            while d < scoring_docs {
                if !used[d] && scores[d] > best_score {
                    best_score = scores[d];
                    best_idx = d;
                }
                d += 1;
            }
            if best_idx == usize::MAX || best_score == 0 {
                break;
            }
            used[best_idx] = true;
            out[written] = (self.docs[best_idx].external_id, best_score);
            written += 1;
        }
        written
    }

    /// Compute IDF(t) in 16.16 fixed-point.
    /// IDF(t) = ln((N - df + 0.5) / (df + 0.5) + 1)
    fn compute_idf(&self, df: u32, n: u32) -> u32 {
        // (N - df + 0.5) / (df + 0.5) + 1
        // In integer: ((N - df) * 2 + 1) and (df * 2 + 1) to avoid 0.5
        let numer = (n.saturating_sub(df)) * 2 + 1;
        let denom = df * 2 + 1;
        // ratio in 16.16
        let ratio = fp_div(numer, denom);
        // +1 in 16.16
        let arg = ratio.saturating_add(FP_ONE);
        fp_ln(arg)
    }

    /// BM25 per-term score for one document.
    /// All inputs in 16.16 fixed-point.
    fn bm25_term_score(&self, tf: u32, dl: u32, avgdl: u32, idf: u32) -> u32 {
        // numerator = tf * (k1 + 1)
        let k1_plus_1 = K1.saturating_add(FP_ONE);
        let numer = fp_mul(tf, k1_plus_1);

        // denominator = tf + k1 * (1 - b + b * dl / avgdl)
        let dl_over_avgdl = fp_div(dl >> 16, avgdl >> 16).min(FP_ONE * 10); // cap at 10x
        let b_dl = fp_mul(B, dl_over_avgdl); // b * dl/avgdl
        let one_minus_b = FP_ONE.saturating_sub(B); // 1 - b
        let norm = one_minus_b.saturating_add(b_dl); // (1-b + b*dl/avgdl)
        let k1_norm = fp_mul(K1, norm); // k1 * (...)
        let denom = tf.saturating_add(k1_norm);

        if denom == 0 {
            return 0;
        }

        // score = idf * numer / denom
        let ratio = fp_div(numer >> 16, denom >> 16);
        fp_mul(idf, ratio)
    }

    // ── Internal helpers ──

    fn find_term(&self, hash: u32) -> Option<usize> {
        let mut i = 0;
        while i < self.term_count {
            if self.terms[i].hash == hash {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn find_or_create_term(&mut self, hash: u32) -> Option<usize> {
        if let Some(i) = self.find_term(hash) {
            return Some(i);
        }
        if self.term_count >= MAX_TERMS {
            return None;
        }
        let idx = self.term_count;
        self.terms[idx] = TermEntry {
            hash,
            df: 0,
            first_posting: 0,
            posting_count: 0,
        };
        self.term_count += 1;
        Some(idx)
    }

    /// Number of indexed documents.
    pub fn doc_count(&self) -> usize {
        self.doc_count
    }

    /// Number of unique terms.
    pub fn term_count(&self) -> usize {
        self.term_count
    }

    /// Number of postings.
    pub fn posting_count(&self) -> usize {
        self.posting_count
    }
}

fn is_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}
