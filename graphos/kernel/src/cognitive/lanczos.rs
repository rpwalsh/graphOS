// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Lanczos iteration for symmetric eigenvalue decomposition.
//!
//! Computes the k smallest eigenvalues of the normalised graph Laplacian
//! 𝓛 = I − D^{−½} A D^{−½} using the Lanczos tridiagonalisation algorithm
//! followed by implicit QR on the tridiagonal matrix.
//!
//! All arithmetic is 16.16 fixed-point.  No floating point, no heap.
//!
//! ## Algorithm
//!
//! 1. **Lanczos recurrence**: Build a (m × m) tridiagonal matrix T from the
//!    n × n Laplacian, where m ≪ n.  T's eigenvalues approximate 𝓛's.
//!
//!   q_{j+1} = A·q_j − α_j·q_j − β_j·q_{j-1}
//!   α_j = q_j^T · A · q_j
//!   β_{j+1} = ‖q_{j+1}‖
//!
//! 2. **Tridiagonal QR**: Extract eigenvalues from T using implicit QR
//!    iteration with Wilkinson shifts.
//!
//! ## Capacity
//! - Max graph nodes for Lanczos: MAX_LANCZOS_N = 4096
//! - Krylov subspace dimension: MAX_LANCZOS_M = 64
//! - Eigenvalues extracted: up to SPECTRAL_K = 8

use crate::graph::types::Weight;

// ────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────

/// Maximum number of nodes supported in a single Lanczos run.
const MAX_N: usize = 4096;

/// Krylov subspace dimension (number of Lanczos steps).
/// m = 64 is generous for extracting k=8 eigenvalues.
const MAX_M: usize = 64;

/// 1.0 in 16.16.
const FP_ONE: u32 = 1 << 16;

// ────────────────────────────────────────────────────────────────────
// Fixed-point helpers
// ────────────────────────────────────────────────────────────────────

/// Signed 16.16 multiply.
fn sfp_mul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 16) as i32
}

/// Integer square root (floor).
fn isqrt(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Fixed-point square root: input and output in 16.16.
fn fp_sqrt(x: u32) -> u32 {
    // sqrt(x_fp) = sqrt(x * 2^16) = sqrt(x) * 2^8
    // So: upscale to 32.32, take isqrt, result is in 16.16.
    let x64 = (x as u64) << 16;
    isqrt(x64) as u32
}

/// Fixed-point signed square root (absolute value).
fn sfp_sqrt_abs(x: i32) -> i32 {
    let abs = if x < 0 { (-x) as u32 } else { x as u32 };
    fp_sqrt(abs) as i32
}

// ────────────────────────────────────────────────────────────────────
// Compact symmetric matrix representation (CSR-like)
// ────────────────────────────────────────────────────────────────────

/// Maximum non-zero entries in the Laplacian.
/// For each undirected edge we store 2 entries + N diagonal.
const MAX_NNZ: usize = 32768;

/// Row pointer for CSR format.
#[derive(Clone, Copy)]
struct CsrEntry {
    col: u16,
    val: i32, // signed 16.16 fixed-point
}

impl CsrEntry {
    const EMPTY: Self = Self { col: 0, val: 0 };
}

/// Compressed sparse row representation of the normalised Laplacian.
pub struct LaplacianCsr {
    /// CSR values array.
    entries: [CsrEntry; MAX_NNZ],
    /// Row pointers: row_ptr[i]..row_ptr[i+1] are the entries for row i.
    row_ptr: [u32; MAX_N + 1],
    /// Number of rows (= number of nodes).
    pub n: usize,
    /// Number of non-zeros.
    pub nnz: usize,
}

impl LaplacianCsr {
    pub const fn new() -> Self {
        Self {
            entries: [CsrEntry::EMPTY; MAX_NNZ],
            row_ptr: [0u32; MAX_N + 1],
            n: 0,
            nnz: 0,
        }
    }

    /// Build the normalised Laplacian from degree and adjacency data.
    ///
    /// `degrees[i]` = weighted degree of node i (16.16 fixed-point).
    /// `edges` = slice of (from, to, weight) triples (undirected: list each direction).
    /// `n` = number of nodes.
    ///
    /// 𝓛_{ij} = δ_{ij} − A_{ij} / sqrt(d_i · d_j)
    pub fn build(&mut self, n: usize, degrees: &[u32], edges: &[(u16, u16, u32)]) {
        let n = n.min(MAX_N);
        self.n = n;
        self.nnz = 0;

        // We'll build row by row.  For each row i, we need:
        //   diagonal: 1.0  (if degree > 0)
        //   off-diagonal: -w_{ij} / sqrt(d_i * d_j)

        // First pass: count entries per row.
        let mut row_counts = [0u32; MAX_N];
        // Every node with non-zero degree gets a diagonal entry.
        let mut i = 0;
        while i < n {
            if degrees[i] > 0 {
                row_counts[i] += 1; // diagonal
            }
            i += 1;
        }
        // Each edge adds an off-diagonal.
        let mut ei = 0;
        while ei < edges.len() {
            let (from, _to, _w) = edges[ei];
            if (from as usize) < n {
                row_counts[from as usize] += 1;
            }
            ei += 1;
        }

        // Build row_ptr.
        self.row_ptr[0] = 0;
        let mut i = 0;
        while i < n {
            self.row_ptr[i + 1] = self.row_ptr[i] + row_counts[i];
            i += 1;
        }
        let total = self.row_ptr[n] as usize;
        if total > MAX_NNZ {
            // Truncate — won't be exact but won't crash.
            self.n = 0;
            return;
        }
        self.nnz = total;

        // Reset row_counts to use as write cursors.
        let mut i = 0;
        while i < MAX_N {
            row_counts[i] = 0;
            i += 1;
        }

        // Fill diagonal entries.
        let mut i = 0;
        while i < n {
            if degrees[i] > 0 {
                let pos = self.row_ptr[i] as usize + row_counts[i] as usize;
                if pos < MAX_NNZ {
                    self.entries[pos] = CsrEntry {
                        col: i as u16,
                        val: FP_ONE as i32,
                    };
                    row_counts[i] += 1;
                }
            }
            i += 1;
        }

        // Fill off-diagonal: -w / sqrt(d_i * d_j).
        let mut ei = 0;
        while ei < edges.len() {
            let (from, to, w) = edges[ei];
            let fi = from as usize;
            let ti = to as usize;
            if fi < n && ti < n && degrees[fi] > 0 && degrees[ti] > 0 {
                // sqrt(d_i * d_j) in 16.16.
                // d_i and d_j are 16.16.  Product is 32.32, take sqrt → 16.16.
                let prod = (degrees[fi] as u64) * (degrees[ti] as u64); // 32.32
                let sqrt_prod = isqrt(prod) as u32; // 16.16
                let val = if sqrt_prod > 0 {
                    let ratio = (((w as u64) << 16) / (sqrt_prod as u64)) as i32;
                    -ratio
                } else {
                    0
                };
                let pos = self.row_ptr[fi] as usize + row_counts[fi] as usize;
                if pos < MAX_NNZ {
                    self.entries[pos] = CsrEntry { col: to, val };
                    row_counts[fi] += 1;
                }
            }
            ei += 1;
        }
    }

    /// Sparse matrix-vector multiply: y = L · x.
    /// x and y are length-n vectors in signed 16.16.
    fn spmv(&self, x: &[i32], y: &mut [i32]) {
        let mut i = 0;
        while i < self.n {
            let start = self.row_ptr[i] as usize;
            let end = self.row_ptr[i + 1] as usize;
            let mut acc = 0i64;
            let mut j = start;
            while j < end {
                let e = &self.entries[j];
                acc += (e.val as i64 * x[e.col as usize] as i64) >> 16;
                j += 1;
            }
            y[i] = acc as i32;
            i += 1;
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Lanczos engine
// ────────────────────────────────────────────────────────────────────

/// Lanczos tridiagonalisation + QR eigenvalue engine.
pub struct LanczosEngine {
    /// Tridiagonal diagonal elements α (signed 16.16).
    alpha: [i32; MAX_M],
    /// Tridiagonal sub-diagonal elements β (signed 16.16).
    beta: [i32; MAX_M],
    /// Number of Lanczos steps completed.
    m: usize,

    /// Lanczos vectors: q[j] is the j-th vector, each of length n.
    /// Stored flat: q_flat[j * MAX_N .. (j+1) * MAX_N].
    /// We only keep 3 vectors at a time (q_{j-1}, q_j, q_{j+1})
    /// to save memory.
    q_prev: [i32; MAX_N],
    q_curr: [i32; MAX_N],
    q_next: [i32; MAX_N],
    /// Temporary for spmv result.
    work: [i32; MAX_N],

    /// Computed eigenvalues (sorted ascending), signed 16.16.
    pub eigenvalues: [i32; MAX_M],
    pub eigenvalue_count: usize,
}

impl LanczosEngine {
    pub const fn new() -> Self {
        Self {
            alpha: [0i32; MAX_M],
            beta: [0i32; MAX_M],
            m: 0,
            q_prev: [0i32; MAX_N],
            q_curr: [0i32; MAX_N],
            q_next: [0i32; MAX_N],
            work: [0i32; MAX_N],
            eigenvalues: [0i32; MAX_M],
            eigenvalue_count: 0,
        }
    }

    /// Run Lanczos iteration on the given Laplacian.
    ///
    /// `steps` = number of Lanczos steps (capped at MAX_M).
    /// After this call, the tridiagonal matrix (alpha, beta) is populated.
    pub fn tridiagonalise(&mut self, laplacian: &LaplacianCsr, steps: usize) {
        let n = laplacian.n;
        if n == 0 {
            return;
        }
        let m = steps.min(MAX_M).min(n);

        // Initial vector q_1 = [1/sqrt(n), 1/sqrt(n), ...] in 16.16.
        // 1/sqrt(n): sqrt(n) in 16.16, then divide FP_ONE by it.
        let sqrt_n = fp_sqrt((n as u32) << 16);
        let q1_val = if sqrt_n > 0 {
            ((FP_ONE as u64) << 16) / (sqrt_n as u64)
        } else {
            FP_ONE as u64
        } as i32;

        let mut i = 0;
        while i < n {
            self.q_curr[i] = q1_val;
            self.q_prev[i] = 0;
            i += 1;
        }

        let mut j = 0;
        while j < m {
            // w = L * q_j
            laplacian.spmv(&self.q_curr[..n], &mut self.work[..n]);

            // α_j = q_j^T · w
            let mut dot = 0i64;
            let mut i = 0;
            while i < n {
                dot += (self.q_curr[i] as i64 * self.work[i] as i64) >> 16;
                i += 1;
            }
            self.alpha[j] = dot as i32;

            // w = w - α_j * q_j - β_j * q_{j-1}
            let mut i = 0;
            while i < n {
                self.work[i] -= sfp_mul(self.alpha[j], self.q_curr[i]);
                if j > 0 {
                    self.work[i] -= sfp_mul(self.beta[j], self.q_prev[i]);
                }
                i += 1;
            }

            // β_{j+1} = ||w||
            let mut norm_sq = 0i64;
            let mut i = 0;
            while i < n {
                norm_sq += (self.work[i] as i64 * self.work[i] as i64) >> 16;
                i += 1;
            }
            let norm = fp_sqrt(if norm_sq < 0 { 0 } else { norm_sq as u32 });

            if j + 1 < m {
                self.beta[j + 1] = norm as i32;
            }

            // If β ≈ 0, the Krylov subspace is exhausted (invariant subspace found).
            if norm < 4 {
                // ~6e-5 in 16.16
                self.m = j + 1;
                self.tridiag_qr();
                return;
            }

            // q_{j+1} = w / β_{j+1}
            let mut i = 0;
            while i < n {
                self.q_next[i] = (((self.work[i] as i64) << 16) / (norm as i64)) as i32;
                i += 1;
            }

            // Shift vectors: prev <- curr, curr <- next.
            let mut i = 0;
            while i < n {
                self.q_prev[i] = self.q_curr[i];
                self.q_curr[i] = self.q_next[i];
                i += 1;
            }

            j += 1;
        }

        self.m = m;
        self.tridiag_qr();
    }

    /// Extract eigenvalues from the tridiagonal matrix using implicit QR.
    ///
    /// This implements the symmetric tridiagonal QR algorithm with
    /// Wilkinson shift.  The eigenvalues converge on the diagonal of T.
    fn tridiag_qr(&mut self) {
        let m = self.m;
        if m == 0 {
            return;
        }

        // Copy alpha/beta into working arrays.
        let mut diag = [0i32; MAX_M];
        let mut offdiag = [0i32; MAX_M];
        let mut i = 0;
        while i < m {
            diag[i] = self.alpha[i];
            if i + 1 < m {
                offdiag[i] = self.beta[i + 1];
            }
            i += 1;
        }

        // QR iteration.
        let mut iter = 0;
        let max_iter = m * 30; // generous bound
        let mut end = m;

        while end > 1 && iter < max_iter {
            // Check for convergence on offdiag[end-2].
            let abs_off = if offdiag[end - 2] < 0 {
                -offdiag[end - 2]
            } else {
                offdiag[end - 2]
            };
            if abs_off < 4 {
                // converged
                end -= 1;
                continue;
            }

            // Wilkinson shift: eigenvalue of trailing 2×2 closer to d[end-1].
            let d_n = diag[end - 1];
            let d_nm1 = diag[end - 2];
            let e_nm1 = offdiag[end - 2];
            let delta = (d_nm1 - d_n) / 2;
            let e_sq = sfp_mul(e_nm1, e_nm1);
            let d_sq = sfp_mul(delta, delta);
            let disc = sfp_sqrt_abs(d_sq.saturating_add(e_sq));
            let shift = if delta >= 0 {
                d_n - sfp_mul(e_sq, Self::sfp_inv(delta.saturating_add(disc)))
            } else {
                d_n - sfp_mul(e_sq, Self::sfp_inv(delta.saturating_sub(disc)))
            };

            // Implicit QR step with shift.
            let mut x = diag[0] - shift;
            let mut z = offdiag[0];
            let mut k = 0;
            while k < end - 1 {
                // Givens rotation to zero out z.
                let (c, s) = Self::givens(x, z);
                if k > 0 {
                    offdiag[k - 1] = sfp_mul(c, x).saturating_add(sfp_mul(s, z));
                }

                let d0 = diag[k];
                let d1 = diag[k + 1];
                let e = offdiag[k];

                diag[k] = sfp_mul(c, sfp_mul(c, d0))
                    .saturating_add(sfp_mul(2 * FP_ONE as i32, sfp_mul(sfp_mul(c, s), e)))
                    .saturating_add(sfp_mul(s, sfp_mul(s, d1)));
                diag[k + 1] = sfp_mul(s, sfp_mul(s, d0))
                    .saturating_sub(sfp_mul(2 * FP_ONE as i32, sfp_mul(sfp_mul(c, s), e)))
                    .saturating_add(sfp_mul(c, sfp_mul(c, d1)));
                offdiag[k] = sfp_mul(c, sfp_mul(s, d1 - d0))
                    .saturating_add(sfp_mul(sfp_mul(c, c) - sfp_mul(s, s), e));

                if k + 2 < end {
                    let t = offdiag[k + 1];
                    x = offdiag[k];
                    z = sfp_mul(s, t);
                    offdiag[k + 1] = sfp_mul(c, t);
                } else {
                    x = offdiag[k]; // not used, but keeps the pattern
                    z = 0;
                }

                k += 1;
            }

            iter += 1;
        }

        // Sort eigenvalues ascending.
        // Simple insertion sort on m ≤ 64 elements.
        let mut i = 1;
        while i < m {
            let key = diag[i];
            let mut j = i;
            while j > 0 && diag[j - 1] > key {
                diag[j] = diag[j - 1];
                j -= 1;
            }
            diag[j] = key;
            i += 1;
        }

        let mut i = 0;
        while i < m {
            self.eigenvalues[i] = diag[i];
            i += 1;
        }
        self.eigenvalue_count = m;
    }

    /// Compute Givens rotation (c, s) such that [c s; -s c]^T [a; b] = [r; 0].
    /// Returns (c, s) in signed 16.16.
    fn givens(a: i32, b: i32) -> (i32, i32) {
        if b == 0 {
            return (FP_ONE as i32, 0);
        }
        let r_sq = sfp_mul(a, a).saturating_add(sfp_mul(b, b));
        let r = sfp_sqrt_abs(r_sq);
        if r == 0 {
            return (FP_ONE as i32, 0);
        }
        let c = (((a as i64) << 16) / (r as i64)) as i32;
        let s = (((b as i64) << 16) / (r as i64)) as i32;
        (c, s)
    }

    /// Signed fixed-point inverse: 1/x.
    fn sfp_inv(x: i32) -> i32 {
        if x == 0 {
            return i32::MAX;
        }
        (((FP_ONE as i64) << 16) / (x as i64)) as i32
    }

    /// Extract the first `k` eigenvalues (smallest) as unsigned 16.16.
    /// Negative eigenvalues (numerical noise) are clamped to 0.
    pub fn smallest_k(&self, out: &mut [Weight], k: usize) -> usize {
        let count = k.min(self.eigenvalue_count).min(out.len());
        let mut i = 0;
        while i < count {
            out[i] = if self.eigenvalues[i] > 0 {
                self.eigenvalues[i] as u32
            } else {
                0
            };
            i += 1;
        }
        count
    }
}
