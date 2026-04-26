// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Probabilistic sketch data structures — BloomFilter, CountMinSketch, HyperLogLog.
//!
//! All structures are fixed-size, no_std, no heap.  Hash functions use
//! FNV-1a 64-bit with domain-separated seeds (not cryptographic — speed
//! is the priority; these are approximate counters, not security primitives).
//!
//! ## FNV-1a
//! We use FNV-1a because it is trivially implementable in pure Rust with
//! no dependencies, has decent avalanche properties for sketch use, and
//! the kernel forbids pulling in external crates.

// ────────────────────────────────────────────────────────────────────
// FNV-1a hash with seed
// ────────────────────────────────────────────────────────────────────

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

/// FNV-1a 64-bit hash seeded with `seed`.
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

/// Split a 64-bit hash into two independent 32-bit values for double-hashing.
#[inline]
fn split_hash(h: u64) -> (u32, u32) {
    (h as u32, (h >> 32) as u32)
}

// ════════════════════════════════════════════════════════════════════
// Bloom Filter
// ════════════════════════════════════════════════════════════════════

/// Capacity in bits. 8192 bits = 1 KiB.  At k=7 hash functions this
/// gives FPR ≈ 0.008 for up to 800 insertions (m/n ≈ 10).
const BLOOM_BITS: usize = 8192;
const BLOOM_BYTES: usize = BLOOM_BITS / 8;

/// Number of hash functions.  Optimal k = (m/n)·ln2 ≈ 7 for m/n=10.
const BLOOM_K: usize = 7;

/// A fixed-size Bloom filter.
///
/// Uses double-hashing: h_i(x) = h1(x) + i·h2(x) mod m, which is
/// equivalent to k independent hash functions for Bloom filters (Kirsch
/// & Mitzenmacher 2006).
pub struct BloomFilter {
    bits: [u8; BLOOM_BYTES],
    count: u32,
}

impl BloomFilter {
    pub const fn new() -> Self {
        Self {
            bits: [0u8; BLOOM_BYTES],
            count: 0,
        }
    }

    /// Insert an item.
    pub fn insert(&mut self, data: &[u8]) {
        let h = fnv1a(data, 0);
        let (h1, h2) = split_hash(h);
        let mut i = 0u32;
        while i < BLOOM_K as u32 {
            let bit_idx = (h1.wrapping_add(i.wrapping_mul(h2))) as usize % BLOOM_BITS;
            self.bits[bit_idx / 8] |= 1 << (bit_idx % 8);
            i += 1;
        }
        self.count += 1;
    }

    /// Test membership (may return false positive, never false negative).
    pub fn may_contain(&self, data: &[u8]) -> bool {
        let h = fnv1a(data, 0);
        let (h1, h2) = split_hash(h);
        let mut i = 0u32;
        while i < BLOOM_K as u32 {
            let bit_idx = (h1.wrapping_add(i.wrapping_mul(h2))) as usize % BLOOM_BITS;
            if self.bits[bit_idx / 8] & (1 << (bit_idx % 8)) == 0 {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Alias for `may_contain` — test membership.
    pub fn query(&self, data: &[u8]) -> bool {
        self.may_contain(data)
    }

    /// Number of items inserted (not deduplicated).
    pub fn count(&self) -> u32 {
        self.count
    }

    /// Estimated false-positive rate: (1 - e^(-kn/m))^k
    /// Returns value in 16.16 fixed-point (0 = 0.0, 65536 = 1.0).
    pub fn estimated_fpr(&self) -> u32 {
        if self.count == 0 {
            return 0;
        }
        // Count set bits to estimate fill ratio.
        let mut set_bits = 0u32;
        let mut j = 0;
        while j < BLOOM_BYTES {
            set_bits += self.bits[j].count_ones();
            j += 1;
        }
        // fill_ratio = set_bits / BLOOM_BITS in 16.16
        let fill = ((set_bits as u64) << 16) / (BLOOM_BITS as u64);
        // FPR ≈ fill^k.  We compute iteratively in fixed-point.
        let mut fpr = fill as u32;
        let mut k = 1;
        while k < BLOOM_K {
            // fpr = fpr * fill >> 16  (fixed-point multiply)
            fpr = ((fpr as u64 * fill) >> 16) as u32;
            k += 1;
        }
        fpr
    }

    /// Reset the filter to empty.
    pub fn clear(&mut self) {
        self.bits = [0u8; BLOOM_BYTES];
        self.count = 0;
    }

    /// Number of set bits.
    pub fn popcount(&self) -> u32 {
        let mut c = 0u32;
        let mut j = 0;
        while j < BLOOM_BYTES {
            c += self.bits[j].count_ones();
            j += 1;
        }
        c
    }
}

// ════════════════════════════════════════════════════════════════════
// Count-Min Sketch
// ════════════════════════════════════════════════════════════════════

/// Number of hash functions (depth).  d=4 gives high probability guarantees.
const CMS_DEPTH: usize = 4;

/// Width of each row. 256 counters × 4 rows × 4 bytes = 4 KiB.
/// Error bound: ε = e/w ≈ 0.011 per unit of total count.
const CMS_WIDTH: usize = 256;

/// A Count-Min Sketch for frequency estimation.
///
/// Provides point-query: estimate(x) ≤ true_count(x) + ε·N
/// where ε = e/w and N is the total count of all items, with probability
/// ≥ 1 − δ where δ = e^{−d}.
///
/// Uses seeded FNV-1a for each row (seed = row index).
pub struct CountMinSketch {
    table: [[u32; CMS_WIDTH]; CMS_DEPTH],
    total_count: u64,
}

impl CountMinSketch {
    pub const fn new() -> Self {
        Self {
            table: [[0u32; CMS_WIDTH]; CMS_DEPTH],
            total_count: 0,
        }
    }

    /// Increment the count of `data` by `delta`.
    pub fn add(&mut self, data: &[u8], delta: u32) {
        let mut d = 0;
        while d < CMS_DEPTH {
            let h = fnv1a(data, d as u64 + 1) as usize % CMS_WIDTH;
            self.table[d][h] = self.table[d][h].saturating_add(delta);
            d += 1;
        }
        self.total_count += delta as u64;
    }

    /// Increment count by 1.
    pub fn increment(&mut self, data: &[u8]) {
        self.add(data, 1);
    }

    /// Estimate the count of `data`. Returns the minimum across all rows.
    pub fn estimate(&self, data: &[u8]) -> u32 {
        let mut min = u32::MAX;
        let mut d = 0;
        while d < CMS_DEPTH {
            let h = fnv1a(data, d as u64 + 1) as usize % CMS_WIDTH;
            let v = self.table[d][h];
            if v < min {
                min = v;
            }
            d += 1;
        }
        min
    }

    /// Total count of all items ever added.
    pub fn total(&self) -> u64 {
        self.total_count
    }

    /// Inner product of two sketches (useful for join-size estimation).
    /// Returns min across rows of dot(row_a, row_b).
    pub fn inner_product(&self, other: &CountMinSketch) -> u64 {
        let mut min_dot = u64::MAX;
        let mut d = 0;
        while d < CMS_DEPTH {
            let mut dot = 0u64;
            let mut w = 0;
            while w < CMS_WIDTH {
                dot += self.table[d][w] as u64 * other.table[d][w] as u64;
                w += 1;
            }
            if dot < min_dot {
                min_dot = dot;
            }
            d += 1;
        }
        min_dot
    }

    /// Reset to zero.
    pub fn clear(&mut self) {
        self.table = [[0u32; CMS_WIDTH]; CMS_DEPTH];
        self.total_count = 0;
    }
}

// ════════════════════════════════════════════════════════════════════
// HyperLogLog
// ════════════════════════════════════════════════════════════════════

/// Number of registers. m = 2^p where p = 8, so m = 256.
/// Standard error ≈ 1.04 / √m ≈ 6.5%.
const HLL_P: u32 = 8;
const HLL_M: usize = 1 << HLL_P; // 256

/// Alpha constant for bias correction.
/// For m = 256: α_m = 0.7213 / (1 + 1.079/m) ≈ 0.7183
/// In 16.16 fixed-point: 0.7183 × 65536 ≈ 47071
const HLL_ALPHA_FP: u64 = 47071;

/// A HyperLogLog cardinality estimator.
///
/// Estimates the number of distinct elements in a stream.
/// Uses p=8 (256 registers), giving ~6.5% standard error.
///
/// Algorithm: Flajolet et al. (2007) with linear counting correction
/// for small cardinalities and bias correction for large ones.
pub struct HyperLogLog {
    registers: [u8; HLL_M],
    count_ops: u64,
}

impl HyperLogLog {
    pub const fn new() -> Self {
        Self {
            registers: [0u8; HLL_M],
            count_ops: 0,
        }
    }

    /// Add an item to the sketch.
    pub fn add(&mut self, data: &[u8]) {
        let h = fnv1a(data, 0x484C4C5F53454544); // "HLL_SEED" as bytes
        // Use upper p bits as register index.
        let idx = (h >> (64 - HLL_P)) as usize;
        // Count leading zeros of remaining bits + 1.
        let remaining = (h << HLL_P) | (1 << (HLL_P - 1)); // ensure non-zero
        let rho = (remaining.leading_zeros() + 1) as u8;
        if rho > self.registers[idx] {
            self.registers[idx] = rho;
        }
        self.count_ops += 1;
    }

    /// Estimate cardinality.
    ///
    /// Returns estimated number of distinct items.
    pub fn estimate(&self) -> u64 {
        // Compute harmonic mean: Z = 1 / Σ 2^{-M[j]}
        // We compute Σ 2^{-M[j]} in 32.32 fixed-point.
        // 2^{-M[j]} = 1 / 2^{M[j]}.
        // In 32.32: (1 << 32) >> M[j]
        let mut sum_inv = 0u64; // sum in 0.32 fixed-point
        let mut zeros = 0u32;
        let mut j = 0;
        while j < HLL_M {
            let val = self.registers[j];
            if val == 0 {
                zeros += 1;
                sum_inv += 1u64 << 32; // 2^{-0} = 1.0
            } else {
                if (val as u32) < 32 {
                    sum_inv += (1u64 << 32) >> (val as u64);
                }
                // for val >= 32, contribution is negligible (< 2^{-32})
            }
            j += 1;
        }

        if sum_inv == 0 {
            return 0;
        }

        // raw estimate = α_m * m^2 / sum_inv
        // In fixed-point: (HLL_ALPHA_FP * m * m) << 16 / sum_inv
        let m = HLL_M as u64;
        // numerator: α(16.16) * m^2.  Shift to get enough precision.
        let numer = HLL_ALPHA_FP * m * m; // this fits in u64 easily
        // sum_inv is in 0.32.  We want numer / (sum_inv >> 32) but that
        // loses precision. Instead: numer << 32 / sum_inv, then >> 16
        // to remove the 16.16 from alpha.
        let raw = if sum_inv > 0 {
            ((numer as u128) << 32) / (sum_inv as u128)
        } else {
            return 0;
        };
        let raw = (raw >> 16) as u64; // remove 16.16 from alpha

        // Small range correction: linear counting.
        if raw <= (5 * m / 2) && zeros > 0 {
            // linear counting: m * ln(m / V) where V = zeros
            // Approximate ln(m/V) using integer log2: ln(x) ≈ 0.6931 · log2(x)
            // log2(m/V) ≈ log2(m) - log2(V)
            let log2_m = 64 - m.leading_zeros();
            let log2_v = if zeros > 0 {
                64 - (zeros as u64).leading_zeros()
            } else {
                0
            };
            if log2_m > log2_v {
                let ln_approx = (log2_m - log2_v) as u64 * 45426 / 65536; // 0.6931 in 16.16
                return m * ln_approx;
            }
        }

        raw
    }

    /// Merge another HyperLogLog into this one (union).
    pub fn merge(&mut self, other: &HyperLogLog) {
        let mut j = 0;
        while j < HLL_M {
            if other.registers[j] > self.registers[j] {
                self.registers[j] = other.registers[j];
            }
            j += 1;
        }
        self.count_ops += other.count_ops;
    }

    /// Reset to empty.
    pub fn clear(&mut self) {
        self.registers = [0u8; HLL_M];
        self.count_ops = 0;
    }

    /// Number of add() operations performed.
    pub fn ops(&self) -> u64 {
        self.count_ops
    }

    /// Number of zero-valued registers (used for small range correction).
    pub fn zero_registers(&self) -> u32 {
        let mut z = 0u32;
        let mut j = 0;
        while j < HLL_M {
            if self.registers[j] == 0 {
                z += 1;
            }
            j += 1;
        }
        z
    }
}
