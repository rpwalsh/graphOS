// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Large allocation regression tests.
//!
//! Tests that verify heap-backed construction of large cognitive
//! structures works correctly without stack overflow.

extern crate alloc;

use crate::cognitive::bm25::Bm25Index;
use crate::cognitive::kneser_ney::KneserNeyModel;
use crate::cognitive::lsh::LshIndex;
use crate::cognitive::pagerank::PageRankEngine;
use crate::diag;
use alloc::boxed::Box;

/// Run all large allocation tests. Returns number of failures.
pub fn run_tests() -> u32 {
    let mut failures = 0u32;

    // Test 1: KneserNeyModel heap allocation
    if !test_kneser_ney_alloc() {
        failures += 1;
    }

    // Test 2: LshIndex heap allocation
    if !test_lsh_alloc() {
        failures += 1;
    }

    // Test 3: Bm25Index heap allocation
    if !test_bm25_alloc() {
        failures += 1;
    }

    // Test 4: PageRankEngine heap allocation
    if !test_pagerank_alloc() {
        failures += 1;
    }

    // Test 5: Multiple large allocations
    if !test_multiple_large_allocs() {
        failures += 1;
    }

    failures
}

fn test_kneser_ney_alloc() -> bool {
    // KneserNeyModel is ~400 KiB - must use new_boxed()
    let model = KneserNeyModel::new_boxed();

    // Verify it's initialized correctly by checking default state
    if model.trigram_count() == 0 && model.total_tokens() == 0 {
        diag::test_pass(b"alloc: KneserNeyModel::new_boxed()");
        true
    } else {
        diag::test_fail(b"alloc: KneserNeyModel not properly initialized");
        false
    }
}

fn test_lsh_alloc() -> bool {
    // LshIndex is ~134 KiB - must use new_boxed()
    let index = LshIndex::new_boxed();

    // Verify by inserting and querying
    let mut idx = index;
    let inserted = idx.insert(0x123456789ABCDEF0, 42);

    if inserted {
        diag::test_pass(b"alloc: LshIndex::new_boxed()");
        true
    } else {
        diag::test_fail(b"alloc: LshIndex insert failed");
        false
    }
}

fn test_bm25_alloc() -> bool {
    // Bm25Index uses alloc_zeroed
    let bm25 = unsafe {
        let raw =
            alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<Bm25Index>()) as *mut Bm25Index;
        if raw.is_null() {
            diag::test_fail(b"alloc: Bm25Index allocation failed");
            return false;
        }
        Box::from_raw(raw)
    };

    // Verify basic state
    if bm25.doc_count() == 0 {
        diag::test_pass(b"alloc: Bm25Index zeroed heap");
        true
    } else {
        diag::test_fail(b"alloc: Bm25Index not zeroed");
        false
    }
}

fn test_pagerank_alloc() -> bool {
    // PageRankEngine uses alloc_zeroed
    let pr = unsafe {
        let raw = alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<PageRankEngine>())
            as *mut PageRankEngine;
        if raw.is_null() {
            diag::test_fail(b"alloc: PageRankEngine allocation failed");
            return false;
        }
        Box::from_raw(raw)
    };

    // Just verify we can access it without faulting
    let _ = &*pr;
    diag::test_pass(b"alloc: PageRankEngine zeroed heap");
    true
}

fn test_multiple_large_allocs() -> bool {
    // Allocate multiple large structures to verify heap capacity
    let kn = KneserNeyModel::new_boxed();
    let lsh = LshIndex::new_boxed();

    // Both should be usable simultaneously
    let kn_ok = kn.trigram_count() == 0;

    let mut lsh_mut = lsh;
    let lsh_ok = lsh_mut.insert(0xDEADBEEF, 99);

    if kn_ok && lsh_ok {
        diag::test_pass(b"alloc: multiple large structures");
        true
    } else {
        diag::test_fail(b"alloc: multiple large structures failed");
        false
    }
}
