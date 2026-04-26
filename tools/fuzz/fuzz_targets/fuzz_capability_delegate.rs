// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Fuzz target: capability delegation bitmask logic.
//!
//! Capability delegation must obey the rule: a delegating process cannot
//! grant capabilities it does not itself hold (confused-deputy prevention).
//! This target verifies that the bitmask logic
//! (replicated from kernel/src/capability/mod.rs) holds for all inputs.

#![no_main]

use libfuzzer_sys::fuzz_target;

// ── Replicated capability bitmask logic ───────────────────────────────────────

/// A process capability set — 64 individual capability bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CapSet(u64);

impl CapSet {
    fn has_all(self, required: CapSet) -> bool {
        self.0 & required.0 == required.0
    }

    /// Delegate `requested` caps to a child process.
    /// Returns `Ok(CapSet)` only if `self` holds all requested caps.
    fn delegate(self, requested: CapSet) -> Result<CapSet, ()> {
        if !self.has_all(requested) {
            return Err(());
        }
        Ok(requested)
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }
    let parent_raw = u64::from_le_bytes(data[0..8].try_into().unwrap());
    let request_raw = u64::from_le_bytes(data[8..16].try_into().unwrap());

    let parent = CapSet(parent_raw);
    let requested = CapSet(request_raw);

    match parent.delegate(requested) {
        Ok(granted) => {
            // Granted caps must be a subset of parent's caps.
            assert!(
                parent.has_all(granted),
                "BUG: granted caps not a subset of parent caps"
            );
            // Granted caps must equal requested (no amplification).
            assert_eq!(
                granted, requested,
                "BUG: granted caps differ from requested"
            );
        }
        Err(()) => {
            // Delegation must fail iff requested has bits parent does not.
            assert!(
                !parent.has_all(requested),
                "BUG: delegation rejected but parent holds all requested caps"
            );
        }
    }
});
