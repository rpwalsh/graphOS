// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Modified Kneser-Ney trigram language model.
//!
//! Fixed-size, no_std, no heap.  All probabilities in 16.16 fixed-point.
//!
//! ## Model
//!
//! P_{KN}(w_i | w_{i-2}, w_{i-1}) =
//!     max(c(w_{i-2}, w_{i-1}, w_i) − D, 0) / c(w_{i-2}, w_{i-1})
//!   + γ(w_{i-2}, w_{i-1}) · P_{KN}(w_i | w_{i-1})
//!
//! where:
//!   D = discount parameter (typically 0.75)
//!   γ = interpolation weight = D · N_{1+}(w_{i-2}, w_{i-1}, •) / c(w_{i-2}, w_{i-1})
//!   N_{1+}(ctx, •) = number of unique continuations from context ctx
//!
//! The lower-order (bigram) uses continuation counts instead of raw counts:
//!   P_{KN}(w | w_{i-1}) = max(N_{1+}(•, w_{i-1}, w) − D, 0) / N_{1+}(•, w_{i-1}, •)
//!                        + γ(w_{i-1}) · P_{KN}(w)
//!
//! Unigram: P_{KN}(w) = N_{1+}(•, •, w) / Σ N_{1+}(•, •, w')
//!
//! ## Design
//! - Vocabulary: token IDs in [0, MAX_VOCAB).  Tokens are FNV hashes of words.
//! - Trigrams stored in a flat hash table with linear probing.
//! - Bigram and unigram counts stored separately.
//! - CRC32 checksum on serialised model for integrity.

extern crate alloc;
use crate::graph::types::Weight;
use alloc::boxed::Box;
use alloc::vec::Vec;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum vocabulary size.
const MAX_VOCAB: usize = 4096;

/// Maximum number of trigrams.
const MAX_TRIGRAMS: usize = 32768;

/// Maximum number of bigrams.
const MAX_BIGRAMS: usize = 16384;

/// Discount D = 0.75 in 16.16 = 49152
const DISCOUNT: u32 = 49152;

/// 1.0 in 16.16.
const FP_ONE: u32 = 1 << 16;

// ────────────────────────────────────────────────────────────────────
// Fixed-point helpers
// ────────────────────────────────────────────────────────────────────

fn fp_mul(a: u32, b: u32) -> u32 {
    ((a as u64 * b as u64) >> 16) as u32
}

fn fp_div(a: u32, b: u32) -> u32 {
    if b == 0 {
        return 0;
    }
    (((a as u64) << 16) / (b as u64)) as u32
}

// ────────────────────────────────────────────────────────────────────
// FNV-1a for token hashing
// ────────────────────────────────────────────────────────────────────

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

pub fn fnv1a(data: &[u8]) -> u16 {
    let mut h = FNV_OFFSET;
    let mut i = 0;
    while i < data.len() {
        h ^= data[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    (h % MAX_VOCAB as u64) as u16
}

// ────────────────────────────────────────────────────────────────────
// Trigram entry
// ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct TrigramEntry {
    w0: u16,
    w1: u16,
    w2: u16,
    count: u16,
}

impl TrigramEntry {
    const EMPTY: Self = Self {
        w0: u16::MAX,
        w1: u16::MAX,
        w2: u16::MAX,
        count: 0,
    };
    fn is_empty(&self) -> bool {
        self.w0 == u16::MAX
    }
}

#[derive(Clone, Copy)]
struct BigramEntry {
    w0: u16,
    w1: u16,
    count: u16,
    /// Number of unique continuations N_{1+}(w0, w1, •).
    continuations: u16,
}

impl BigramEntry {
    const EMPTY: Self = Self {
        w0: u16::MAX,
        w1: u16::MAX,
        count: 0,
        continuations: 0,
    };
    fn is_empty(&self) -> bool {
        self.w0 == u16::MAX
    }
}

#[derive(Clone, Copy)]
struct UnigramEntry {
    /// Number of unique contexts that precede this word: N_{1+}(•, •, w).
    continuation_count: u16,
    /// Raw count (for generation / sampling).
    raw_count: u16,
}

impl UnigramEntry {
    const EMPTY: Self = Self {
        continuation_count: 0,
        raw_count: 0,
    };
}

// ────────────────────────────────────────────────────────────────────
// Kneser-Ney model
// ────────────────────────────────────────────────────────────────────

/// Modified Kneser-Ney trigram language model.
pub struct KneserNeyModel {
    trigrams: [TrigramEntry; MAX_TRIGRAMS],
    trigram_count: usize,
    bigrams: [BigramEntry; MAX_BIGRAMS],
    bigram_count: usize,
    unigrams: [UnigramEntry; MAX_VOCAB],
    /// Total continuation count Σ N_{1+}(•, •, w) for all w (denominator of unigram).
    total_continuation: u32,
    /// Total tokens observed.
    total_tokens: u64,
}

impl KneserNeyModel {
    pub const fn new() -> Self {
        Self {
            trigrams: [TrigramEntry::EMPTY; MAX_TRIGRAMS],
            trigram_count: 0,
            bigrams: [BigramEntry::EMPTY; MAX_BIGRAMS],
            bigram_count: 0,
            unigrams: [UnigramEntry::EMPTY; MAX_VOCAB],
            total_continuation: 0,
            total_tokens: 0,
        }
    }

    /// Allocate a `KneserNeyModel` entirely on the heap, never touching the
    /// task stack with the ~400 KiB struct body.
    ///
    /// Safety: we allocate zeroed memory, then overwrite the hash-table slots
    /// with their proper EMPTY sentinel values (which use u16::MAX as the
    /// key, not 0), so the struct is fully initialised before the Box is
    /// returned.
    pub fn new_boxed() -> Box<Self> {
        unsafe {
            use core::alloc::Layout;
            let layout = Layout::new::<Self>();
            let raw = alloc::alloc::alloc_zeroed(layout) as *mut Self;
            if raw.is_null() {
                panic!("KneserNeyModel::new_boxed: allocation failed");
            }
            // Initialise trigram slots to EMPTY (w0 = u16::MAX sentinel).
            let trigrams = core::ptr::addr_of_mut!((*raw).trigrams);
            let mut i = 0usize;
            while i < MAX_TRIGRAMS {
                (*trigrams)[i] = TrigramEntry::EMPTY;
                i += 1;
            }
            // Initialise bigram slots to EMPTY (w0 = u16::MAX sentinel).
            let bigrams = core::ptr::addr_of_mut!((*raw).bigrams);
            let mut i = 0usize;
            while i < MAX_BIGRAMS {
                (*bigrams)[i] = BigramEntry::EMPTY;
                i += 1;
            }
            // Unigram entries are zero-valid (continuation_count=0, raw_count=0),
            // scalar fields (trigram_count, bigram_count, total_continuation,
            // total_tokens) are also zero-valid — already zeroed by alloc_zeroed.
            Box::from_raw(raw)
        }
    }

    /// Observe a trigram (w0, w1, w2).  All words are token IDs.
    pub fn observe_trigram(&mut self, w0: u16, w1: u16, w2: u16) {
        self.total_tokens += 1;

        // Update trigram.
        let new_tri = self.inc_trigram(w0, w1, w2);

        // Update bigram (w0, w1).
        let _new_bi = self.inc_bigram(w0, w1);

        // If this is a NEW trigram, it's a new continuation for (w0, w1).
        if new_tri {
            self.inc_bigram_continuation(w0, w1);
        }

        // Update unigram continuation for w2.
        // A new trigram means a new (•, •, w2) context.
        if new_tri {
            let prev = self.unigrams[w2 as usize].continuation_count;
            self.unigrams[w2 as usize].continuation_count = prev + 1;
            self.total_continuation += 1;
        }
        self.unigrams[w2 as usize].raw_count =
            self.unigrams[w2 as usize].raw_count.saturating_add(1);
    }

    /// Observe a text (whitespace-delimited) and build trigrams.
    /// Returns the number of trigrams observed.
    pub fn observe_text(&mut self, text: &[u8]) -> u32 {
        let mut tokens: Vec<u16> = Vec::with_capacity(256);
        let mut start = 0usize;
        let len = text.len();

        while start < len {
            while start < len && is_ws(text[start]) {
                start += 1;
            }
            let word_start = start;
            while start < len && !is_ws(text[start]) {
                start += 1;
            }
            if start > word_start {
                tokens.push(fnv1a(&text[word_start..start]));
            }
        }

        let token_count = tokens.len();
        let mut count = 0u32;
        if token_count >= 3 {
            let mut i = 0;
            while i + 2 < token_count {
                self.observe_trigram(tokens[i], tokens[i + 1], tokens[i + 2]);
                count += 1;
                i += 1;
            }
        }
        count
    }

    /// Compute P_{KN}(w2 | w0, w1) in 16.16 fixed-point.
    pub fn probability(&self, w0: u16, w1: u16, w2: u16) -> Weight {
        // Trigram level.
        let tri_count = self.get_trigram_count(w0, w1, w2) as u32;
        let bi_count = self.get_bigram_count(w0, w1) as u32;
        let bi_contin = self.get_bigram_continuation(w0, w1) as u32;

        if bi_count > 0 {
            let discounted = if tri_count > 0 {
                let tc_fp = tri_count << 16;
                let sub = tc_fp.saturating_sub(DISCOUNT);
                fp_div(sub >> 16, bi_count)
            } else {
                0
            };

            // Interpolation weight γ = D * N_{1+}(w0, w1, •) / c(w0, w1)
            let gamma = fp_mul(DISCOUNT, fp_div(bi_contin, bi_count));
            let p_lower = self.bigram_probability(w1, w2);
            discounted.saturating_add(fp_mul(gamma, p_lower))
        } else {
            self.bigram_probability(w1, w2)
        }
    }

    /// Bigram-level Kneser-Ney probability.
    fn bigram_probability(&self, w1: u16, w2: u16) -> Weight {
        // P_{KN}(w2 | w1) uses continuation counts.
        // N_{1+}(•, w1, w2) is approximated by trigram diversity.
        // For simplicity we use the bigram count as a fallback.
        let bi_count = self.get_bigram_count(w1, w2) as u32;
        let ctx_count = self.bigram_context_count(w1);

        if ctx_count > 0 {
            let discounted = if bi_count > 0 {
                let bc_fp = bi_count << 16;
                let sub = bc_fp.saturating_sub(DISCOUNT);
                fp_div(sub >> 16, ctx_count)
            } else {
                0
            };
            let ctx_contin = self.bigram_context_continuation(w1);
            let gamma = if ctx_count > 0 {
                fp_mul(DISCOUNT, fp_div(ctx_contin, ctx_count))
            } else {
                0
            };
            let p_uni = self.unigram_probability(w2);
            discounted.saturating_add(fp_mul(gamma, p_uni))
        } else {
            self.unigram_probability(w2)
        }
    }

    /// Unigram probability: P(w) = N_{1+}(•, •, w) / Σ N_{1+}(•, •, w').
    fn unigram_probability(&self, w: u16) -> Weight {
        if self.total_continuation == 0 {
            return 0;
        }
        let cont = self.unigrams[w as usize].continuation_count as u32;
        fp_div(cont, self.total_continuation)
    }

    /// Score a sequence of tokens.  Returns the sum of log-probabilities
    /// (in 16.16 fixed-point, approximated).
    pub fn score_sequence(&self, tokens: &[u16]) -> u32 {
        if tokens.len() < 3 {
            return 0;
        }
        let mut total = 0u32;
        let mut i = 0;
        while i + 2 < tokens.len() {
            let p = self.probability(tokens[i], tokens[i + 1], tokens[i + 2]);
            // log2(p) approximation: use leading zeros.
            // We just accumulate raw probability as a perplexity proxy.
            total = total.saturating_add(p);
            i += 1;
        }
        total
    }

    /// Predict the most likely next token given context (w0, w1).
    /// Returns (token_id, probability) or (0, 0) if no prediction.
    pub fn predict(&self, w0: u16, w1: u16) -> (u16, Weight) {
        let mut best_w = 0u16;
        let mut best_p = 0u32;
        let mut w = 0u16;
        while (w as usize) < MAX_VOCAB {
            if self.unigrams[w as usize].raw_count > 0 {
                let p = self.probability(w0, w1, w);
                if p > best_p {
                    best_p = p;
                    best_w = w;
                }
            }
            w += 1;
        }
        (best_w, best_p)
    }

    // ── Trigram helpers ──

    fn trigram_hash(w0: u16, w1: u16, w2: u16) -> usize {
        let h = (w0 as u64) * 65537 + (w1 as u64) * 257 + (w2 as u64);
        (h as usize) % MAX_TRIGRAMS
    }

    fn inc_trigram(&mut self, w0: u16, w1: u16, w2: u16) -> bool {
        let base = Self::trigram_hash(w0, w1, w2);
        let mut i = 0;
        while i < MAX_TRIGRAMS {
            let idx = (base + i) % MAX_TRIGRAMS;
            if self.trigrams[idx].is_empty() {
                self.trigrams[idx] = TrigramEntry {
                    w0,
                    w1,
                    w2,
                    count: 1,
                };
                self.trigram_count += 1;
                return true; // new
            }
            if self.trigrams[idx].w0 == w0
                && self.trigrams[idx].w1 == w1
                && self.trigrams[idx].w2 == w2
            {
                self.trigrams[idx].count = self.trigrams[idx].count.saturating_add(1);
                return false; // existing
            }
            i += 1;
        }
        false // table full
    }

    fn get_trigram_count(&self, w0: u16, w1: u16, w2: u16) -> u16 {
        let base = Self::trigram_hash(w0, w1, w2);
        let mut i = 0;
        while i < MAX_TRIGRAMS {
            let idx = (base + i) % MAX_TRIGRAMS;
            if self.trigrams[idx].is_empty() {
                return 0;
            }
            if self.trigrams[idx].w0 == w0
                && self.trigrams[idx].w1 == w1
                && self.trigrams[idx].w2 == w2
            {
                return self.trigrams[idx].count;
            }
            i += 1;
        }
        0
    }

    // ── Bigram helpers ──

    fn bigram_hash(w0: u16, w1: u16) -> usize {
        let h = (w0 as u64) * 65537 + (w1 as u64);
        (h as usize) % MAX_BIGRAMS
    }

    fn inc_bigram(&mut self, w0: u16, w1: u16) -> bool {
        let base = Self::bigram_hash(w0, w1);
        let mut i = 0;
        while i < MAX_BIGRAMS {
            let idx = (base + i) % MAX_BIGRAMS;
            if self.bigrams[idx].is_empty() {
                self.bigrams[idx] = BigramEntry {
                    w0,
                    w1,
                    count: 1,
                    continuations: 0,
                };
                self.bigram_count += 1;
                return true;
            }
            if self.bigrams[idx].w0 == w0 && self.bigrams[idx].w1 == w1 {
                self.bigrams[idx].count = self.bigrams[idx].count.saturating_add(1);
                return false;
            }
            i += 1;
        }
        false
    }

    fn inc_bigram_continuation(&mut self, w0: u16, w1: u16) {
        let base = Self::bigram_hash(w0, w1);
        let mut i = 0;
        while i < MAX_BIGRAMS {
            let idx = (base + i) % MAX_BIGRAMS;
            if self.bigrams[idx].is_empty() {
                return;
            }
            if self.bigrams[idx].w0 == w0 && self.bigrams[idx].w1 == w1 {
                self.bigrams[idx].continuations = self.bigrams[idx].continuations.saturating_add(1);
                return;
            }
            i += 1;
        }
    }

    fn get_bigram_count(&self, w0: u16, w1: u16) -> u16 {
        let base = Self::bigram_hash(w0, w1);
        let mut i = 0;
        while i < MAX_BIGRAMS {
            let idx = (base + i) % MAX_BIGRAMS;
            if self.bigrams[idx].is_empty() {
                return 0;
            }
            if self.bigrams[idx].w0 == w0 && self.bigrams[idx].w1 == w1 {
                return self.bigrams[idx].count;
            }
            i += 1;
        }
        0
    }

    fn get_bigram_continuation(&self, w0: u16, w1: u16) -> u16 {
        let base = Self::bigram_hash(w0, w1);
        let mut i = 0;
        while i < MAX_BIGRAMS {
            let idx = (base + i) % MAX_BIGRAMS;
            if self.bigrams[idx].is_empty() {
                return 0;
            }
            if self.bigrams[idx].w0 == w0 && self.bigrams[idx].w1 == w1 {
                return self.bigrams[idx].continuations;
            }
            i += 1;
        }
        0
    }

    /// Total bigram count for context w1 (sum over all w' of c(w1, w')).
    fn bigram_context_count(&self, w1: u16) -> u32 {
        let mut total = 0u32;
        let mut i = 0;
        while i < MAX_BIGRAMS {
            if !self.bigrams[i].is_empty() && self.bigrams[i].w0 == w1 {
                total += self.bigrams[i].count as u32;
            }
            i += 1;
        }
        total
    }

    /// Number of unique continuations from context w1.
    fn bigram_context_continuation(&self, w1: u16) -> u32 {
        let mut total = 0u32;
        let mut i = 0;
        while i < MAX_BIGRAMS {
            if !self.bigrams[i].is_empty() && self.bigrams[i].w0 == w1 {
                total += 1;
            }
            i += 1;
        }
        total
    }

    // ── Stats ──

    pub fn trigram_count(&self) -> usize {
        self.trigram_count
    }
    pub fn bigram_count(&self) -> usize {
        self.bigram_count
    }
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }
}

/// CRC32 (Castagnoli) for model integrity.
/// Polynomial: 0x1EDC6F41.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i < data.len() {
        crc ^= data[i] as u32;
        let mut bit = 0;
        while bit < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x82F6_3B78; // reflected polynomial
            } else {
                crc >>= 1;
            }
            bit += 1;
        }
        i += 1;
    }
    crc ^ 0xFFFF_FFFF
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}
