// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Kernel regression tests.
//!
//! These tests run during boot when enabled. They validate critical
//! algorithms and allocation paths that have previously caused bugs.
//!
//! ## Test categories
//!
//! - `indexing`: span/chunk extraction edge cases
//! - `alloc`: large structure heap allocation
//!
//! ## Usage
//!
//! Call `tests::run_all()` from the init task to execute all tests.
//! Results are reported via the `diag` module with PASS/FAIL status.

mod address_space_smoke;
mod alloc_large;
mod graphics_smoke;
mod heap_reuse;
mod indexing;
mod vfs_smoke;

// Host-compiled unit tests (feature = "host-test").
#[cfg(feature = "host-test")]
pub mod net_tests;
#[cfg(feature = "host-test")]
pub mod nvme_tests;
#[cfg(feature = "host-test")]
pub mod sched_tests;
#[cfg(feature = "host-test")]
pub mod security;

use crate::diag;

/// Test result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestResult {
    Pass,
    Fail,
}

/// Run all regression tests and report results.
/// Returns the number of failures.
pub fn run_all() -> u32 {
    diag::test_info(b"=== Regression Test Suite ===");

    let mut failures = 0u32;

    // Indexing tests
    failures += indexing::run_tests();

    // Large allocation tests
    failures += alloc_large::run_tests();

    // Heap reuse tests
    failures += heap_reuse::run_tests();

    // Graphics/compositor foundation tests
    failures += graphics_smoke::run_tests();

    // User address-space bring-up tests
    failures += address_space_smoke::run_tests();

    // VFS smoke tests
    failures += vfs_smoke::run_tests();

    // Summary
    if failures == 0 {
        diag::test_pass(b"All tests passed");
    } else {
        diag::emit_val(
            diag::Category::Test,
            diag::Level::Fail,
            b"Tests failed: ",
            failures as u64,
        );
    }

    diag::test_info(b"=== End Regression Tests ===");
    failures
}
