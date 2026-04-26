// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Locality-Sensitive Hashing (SimHash) for entity resolution.
//!
//! ## Algorithm
//!
//! **SimHash** (Charikar 2002): For a document/entity represented as a set of
//! weighted features (terms), compute a b-bit hash such that cosine similarity
//! between documents corresponds to Hamming distance between hashes.
//!
//! For each bit position i ∈ [0, b):
//!   1. Sum weighted hash-bit contributions: V[i] = Σ_t (w_t if h_i(t)=1, else −w_t)
//!   2. bit i = 1 if V[i] > 0, else 0
//!
//! **Entity Resolution**: Two entities are candidate duplicates if their
//! SimHash Hamming distance ≤ threshold (typically 3-6 for 64-bit hashes).
//!
//! **LSH Index**: Partition the hash into bands. Two hashes match in the
//! LSH index if any band is identical (band-OR amplification).
//!
//! ## Design
//! - 64-bit SimHash fingerprints.
//! - LSH index with B=8 bands of R=8 bits each.
//! - Hash table per band with chaining (fixed-size bucket arrays).
//! - No heap, no_std, all fixed-size.

// ────────────────────────────────────────────────────────────────────
// FNV-1a
// ────────────────────────────────────────────────────────────────────

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

fn fnv1a(data: &[u8], seed: u64) -> u64 {
    let mut h = FNV_OFFSET ^ seed;
    let mut i = 0;
    while i < data.len() {
        h ^= data[i] as u64;
        h = h.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    h
}

// ────────────────────────────────────────────────────────────────────
// SimHash computation
// ────────────────────────────────────────────────────────────────────

/// Number of bits in the SimHash fingerprint.
const SIMHASH_BITS: usize = 64;

/// Compute a 64-bit SimHash fingerprint from a set of weighted features.
///
/// `features` is a slice of (term_bytes, weight) pairs.
/// Each term is hashed with a per-bit seed to produce independent projections.
///
/// The standard SimHash uses a single hash per feature and accumulates
/// bit-by-bit. We use the full 64 bits of FNV-1a directly.
pub fn simhash(features: &[(&[u8], i32)]) -> u64 {
    let mut v = [0i64; SIMHASH_BITS];

    let mut fi = 0;
    while fi < features.len() {
        let (term, weight) = features[fi];
        let h = fnv1a(term, 0);
        let w = weight as i64;
        let mut bit = 0;
        while bit < SIMHASH_BITS {
            if (h >> bit) & 1 == 1 {
                v[bit] += w;
            } else {
                v[bit] -= w;
            }
            bit += 1;
        }
        fi += 1;
    }

    let mut fingerprint = 0u64;
    let mut bit = 0;
    while bit < SIMHASH_BITS {
        if v[bit] > 0 {
            fingerprint |= 1u64 << bit;
        }
        bit += 1;
    }
    fingerprint
}

/// Compute SimHash from whitespace-delimited text (all terms weight 1).
pub fn simhash_text(text: &[u8]) -> u64 {
    let mut v = [0i64; SIMHASH_BITS];
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
            let h = fnv1a(&text[word_start..start], 0);
            let mut bit = 0;
            while bit < SIMHASH_BITS {
                if (h >> bit) & 1 == 1 {
                    v[bit] += 1;
                } else {
                    v[bit] -= 1;
                }
                bit += 1;
            }
        }
    }

    let mut fp = 0u64;
    let mut bit = 0;
    while bit < SIMHASH_BITS {
        if v[bit] > 0 {
            fp |= 1u64 << bit;
        }
        bit += 1;
    }
    fp
}

/// Hamming distance between two 64-bit hashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

// ────────────────────────────────────────────────────────────────────
// LSH Index (banded)
// ────────────────────────────────────────────────────────────────────

/// Number of bands. Each band = 8 bits of the 64-bit hash.
const NUM_BANDS: usize = 8;
/// Bits per band.
const BITS_PER_BAND: usize = 8;
/// Buckets per band (2^8 = 256).
const BUCKETS_PER_BAND: usize = 1 << BITS_PER_BAND;
/// Max entries per bucket.
const MAX_BUCKET_ENTRIES: usize = 16;
/// Max total entities in the index.
const MAX_LSH_ENTITIES: usize = 4096;

/// An entry in an LSH bucket.
#[derive(Clone, Copy)]
struct BucketEntry {
    entity_idx: u16,
    _pad: u16,
}

impl BucketEntry {
    const EMPTY: Self = Self {
        entity_idx: 0,
        _pad: 0,
    };
}

/// A single band's hash table.
struct BandTable {
    /// buckets[bucket_id][entry_idx] = entity index.
    buckets: [[BucketEntry; MAX_BUCKET_ENTRIES]; BUCKETS_PER_BAND],
    counts: [u8; BUCKETS_PER_BAND],
}

impl BandTable {
    const EMPTY_BUCKET: [BucketEntry; MAX_BUCKET_ENTRIES] =
        [BucketEntry::EMPTY; MAX_BUCKET_ENTRIES];

    const fn new() -> Self {
        Self {
            buckets: [Self::EMPTY_BUCKET; BUCKETS_PER_BAND],
            counts: [0u8; BUCKETS_PER_BAND],
        }
    }

    fn insert(&mut self, bucket: usize, entity_idx: u16) {
        let c = self.counts[bucket] as usize;
        if c < MAX_BUCKET_ENTRIES {
            self.buckets[bucket][c] = BucketEntry {
                entity_idx,
                _pad: 0,
            };
            self.counts[bucket] += 1;
        }
    }
}

/// Entity record stored in the index.
#[derive(Clone, Copy)]
struct EntityRecord {
    fingerprint: u64,
    external_id: u64,
}

impl EntityRecord {
    const EMPTY: Self = Self {
        fingerprint: 0,
        external_id: 0,
    };
}

/// LSH entity resolution index.
pub struct LshIndex {
    bands: [BandTable; NUM_BANDS],
    entities: [EntityRecord; MAX_LSH_ENTITIES],
    entity_count: usize,
}

impl LshIndex {
    pub const fn new() -> Self {
        const EMPTY_BAND: BandTable = BandTable::new();
        Self {
            bands: [EMPTY_BAND; NUM_BANDS],
            entities: [EntityRecord::EMPTY; MAX_LSH_ENTITIES],
            entity_count: 0,
        }
    }

    /// Allocate a `LshIndex` on the heap without constructing it on the stack.
    /// `LshIndex` is zero-valid: all fields initialise correctly from zeroed memory.
    pub fn new_boxed() -> alloc::boxed::Box<Self> {
        unsafe {
            use core::alloc::Layout;
            let raw = alloc::alloc::alloc_zeroed(Layout::new::<Self>()) as *mut Self;
            if raw.is_null() {
                panic!("LshIndex::new_boxed: allocation failed");
            }
            alloc::boxed::Box::from_raw(raw)
        }
    }

    /// Insert an entity with its SimHash fingerprint.
    pub fn insert(&mut self, fingerprint: u64, external_id: u64) -> bool {
        if self.entity_count >= MAX_LSH_ENTITIES {
            return false;
        }
        let idx = self.entity_count as u16;
        self.entities[self.entity_count] = EntityRecord {
            fingerprint,
            external_id,
        };
        self.entity_count += 1;

        // Insert into each band's hash table.
        let mut b = 0;
        while b < NUM_BANDS {
            let band_val = ((fingerprint >> (b * BITS_PER_BAND)) & 0xFF) as usize;
            self.bands[b].insert(band_val, idx);
            b += 1;
        }
        true
    }

    /// Find candidate matches for a query fingerprint.
    ///
    /// Returns entities that share at least one band value with the query.
    /// `max_hamming` further filters by exact Hamming distance.
    /// `out` is filled with (external_id, hamming_distance) pairs.
    /// Returns the number of results.
    pub fn query(&self, fingerprint: u64, max_hamming: u32, out: &mut [(u64, u32)]) -> usize {
        if out.is_empty() || self.entity_count == 0 {
            return 0;
        }

        // Collect candidate set via band-OR.
        // Use a bitset to deduplicate (MAX_LSH_ENTITIES / 8 = 512 bytes).
        const BITSET_SIZE: usize = MAX_LSH_ENTITIES / 8 + 1;
        let mut seen = [0u8; BITSET_SIZE];

        let mut result_count = 0usize;

        let mut b = 0;
        while b < NUM_BANDS {
            let band_val = ((fingerprint >> (b * BITS_PER_BAND)) & 0xFF) as usize;
            let count = self.bands[b].counts[band_val] as usize;
            let mut j = 0;
            while j < count {
                let eidx = self.bands[b].buckets[band_val][j].entity_idx as usize;
                let byte_idx = eidx / 8;
                let bit_idx = eidx % 8;
                if byte_idx < BITSET_SIZE && (seen[byte_idx] & (1 << bit_idx)) == 0 {
                    seen[byte_idx] |= 1 << bit_idx;
                    // Check Hamming distance.
                    let dist = hamming(fingerprint, self.entities[eidx].fingerprint);
                    if dist <= max_hamming && result_count < out.len() {
                        out[result_count] = (self.entities[eidx].external_id, dist);
                        result_count += 1;
                    }
                }
                j += 1;
            }
            b += 1;
        }

        result_count
    }

    /// Exact nearest-neighbour search (brute force over all entities).
    /// Returns (external_id, hamming_distance) of the closest match,
    /// or None if the index is empty or no match within max_hamming.
    pub fn nearest(&self, fingerprint: u64, max_hamming: u32) -> Option<(u64, u32)> {
        let mut best_dist = u32::MAX;
        let mut best_id = 0u64;
        let mut i = 0;
        while i < self.entity_count {
            let d = hamming(fingerprint, self.entities[i].fingerprint);
            if d < best_dist {
                best_dist = d;
                best_id = self.entities[i].external_id;
            }
            i += 1;
        }
        if best_dist <= max_hamming {
            Some((best_id, best_dist))
        } else {
            None
        }
    }

    pub fn entity_count(&self) -> usize {
        self.entity_count
    }
}
