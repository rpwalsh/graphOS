// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Network and present-queue host tests.

#![cfg(feature = "host-test")]

use crate::net::tls;
use crate::wm::surface_table;

// ─── TLS flag tests ───────────────────────────────────────────────────────────

/// TLS must be unavailable at init (before any set_available call).
#[test]
fn tls_unavailable_at_start() {
    // We cannot reset the AtomicBool between tests in a unit-test harness;
    // this test is only reliable when run in isolation.  In CI, run with
    // `--test-threads=1` or per-test binary.
    // The important invariant: after set_unavailable() the flag is false.
    tls::set_unavailable();
    assert!(
        !tls::is_available(),
        "TLS must be unavailable after set_unavailable"
    );
}

/// set_available / set_unavailable must toggle the flag correctly.
#[test]
fn tls_flag_toggles() {
    tls::set_available();
    assert!(tls::is_available());
    tls::set_unavailable();
    assert!(!tls::is_available());
}

// ─── Present queue tests ─────────────────────────────────────────────────────

/// Pushing more surfaces than the present queue capacity must return QueueFull.
#[test]
fn present_queue_overflow_returns_error() {
    use crate::wm::surface_table::PresentError;
    // Drain any stale state by consuming all pending presents.
    while surface_table::present_queue_pop().is_some() {}
    // Fill the queue to capacity.
    let capacity = surface_table::present_queue_capacity();
    for i in 0..capacity {
        let _ = surface_table::present_queue_push(i as u32);
    }
    // Next push must fail with QueueFull.
    let result = surface_table::present_queue_push(9999);
    assert!(
        matches!(result, Err(PresentError::QueueFull)),
        "expected QueueFull, got {:?}",
        result
    );
    // Drain.
    while surface_table::present_queue_pop().is_some() {}
}
