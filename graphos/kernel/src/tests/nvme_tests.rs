// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! NVMe and block-layer host tests.

#![cfg(feature = "host-test")]

use crate::drivers::nvme;

/// Submitting a read to the stub NVMe driver must return false (no device).
#[test]
fn nvme_stub_read_returns_false() {
    let mut buf = [0u8; 512];
    let ok = nvme::read_lba(0, &mut buf);
    assert!(!ok, "stub NVMe read_lba must return false");
}

/// The NVMe stub must not panic on any LBA value.
#[test]
fn nvme_stub_large_lba_no_panic() {
    let mut buf = [0u8; 512];
    for lba in [0u64, u32::MAX as u64, u64::MAX / 2, u64::MAX] {
        let _ = nvme::read_lba(lba, &mut buf);
    }
}

/// NVMe probe must return NoController (stub) — not panic.
#[test]
fn nvme_probe_no_panic() {
    let _ = nvme::probe();
}
