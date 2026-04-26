// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Cognitive runtime engines — the algorithmic core of GraphOS's SCCE subsystem.
//!
//! Every module here is `no_std`, `no_alloc`, fixed-size, integer-only.
//! No floating-point. All "real" values are 16.16 fixed-point (`Weight`).
//!
//! ## Modules
//! - `sketch`:      BloomFilter, CountMinSketch, HyperLogLog
//! - `bm25`:        BM25 inverted index with TF-IDF scoring
//! - `pagerank`:    Iterative power-method PageRank on the knowledge subgraph
//! - `lanczos`:     Lanczos iteration for symmetric eigenvalue decomposition
//! - `lsh`:         SimHash + locality-sensitive hashing for entity resolution
//! - `redact`:      Secret/credential redaction via manual pattern matching
//! - `kneser_ney`:  Modified Kneser-Ney trigram language model
//! - `memory`:      Conversation memory / multi-turn context tracking
//! - `indexing`:    Document → span → chunk → graph mutation pipeline
//! - `correlation`: Mention → entity → relation promotion pipeline
//! - `spectral_refresh`: Lanczos-based spectral snapshot refresh pipeline

pub mod bm25;
pub mod correlation;
pub mod engine;
pub mod indexing;
pub mod kneser_ney;
pub mod lanczos;
pub mod lsh;
pub mod memory;
pub mod pagerank;
pub mod pipeline;
pub mod redact;
pub mod sketch;
pub mod spectral_refresh;
