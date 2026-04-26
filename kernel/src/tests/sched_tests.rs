// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Scheduler host tests.

#![cfg(feature = "host-test")]

use crate::sched;

/// Sleeping more tasks than the primary sleep table can hold must not panic.
/// Tasks beyond `SLEEP_CAPACITY` spill to the overflow ring.
#[test]
fn sleep_overflow_does_not_panic() {
    // This test just verifies that calling sleep_for_ticks does not panic
    // even when called many times.  The actual scheduling is no-op in
    // host-test mode since we have no CPU context, but the data-structure
    // code path must be sound.
    for i in 0..300u64 {
        // Each call must return without panicking.
        sched::sleep_for_ticks(i % 100 + 1);
    }
}

/// tick_advance must not panic when the overflow queue is empty.
#[test]
fn tick_advance_empty_overflow() {
    for _ in 0..1000 {
        sched::tick_advance();
    }
}
