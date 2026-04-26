// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! HPET — High Precision Event Timer driver.
//!
//! Parses the HPET ACPI table (signature `"HPET"`), maps the MMIO base,
//! and provides a monotonically increasing sub-millisecond timestamp source.
//!
//! The HPET is used as the high-resolution timer when the PIT (≈1 ms
//! resolution) is insufficient.  Kernel callers use `read_fs()` to obtain
//! a femtosecond timestamp, or `read_ns()` for nanoseconds.

use core::sync::atomic::{AtomicU64, Ordering};

// ── HPET register offsets (from MMIO base) ───────────────────────────────────

const HPET_CAP_ID: u64 = 0x000; // General Capabilities and ID
const HPET_CONFIG: u64 = 0x010; // General Configuration
const HPET_INT_STATUS: u64 = 0x020; // General Interrupt Status
const HPET_MAIN_CTR: u64 = 0x0F0; // Main Counter Value

const HPET_CFG_ENABLE: u64 = 1 << 0; // ENABLE_CNT

// ── State ─────────────────────────────────────────────────────────────────────

/// MMIO base address of the HPET registers (0 = not yet initialised).
static HPET_BASE: AtomicU64 = AtomicU64::new(0);
/// Counter period in femtoseconds (read from CAP_ID[63:32]).
static HPET_PERIOD_FS: AtomicU64 = AtomicU64::new(0);

// ── MMIO helpers ──────────────────────────────────────────────────────────────

#[inline]
unsafe fn read64(base: u64, offset: u64) -> u64 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u64) }
}

#[inline]
unsafe fn write64(base: u64, offset: u64, val: u64) {
    unsafe {
        core::ptr::write_volatile((base + offset) as *mut u64, val);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the HPET from the ACPI-supplied MMIO base address.
///
/// `mmio_base`: physical address of the HPET register block (from the
/// ACPI HPET table, `base_address.address` field).
///
/// # Safety
/// The MMIO base must be identity-mapped in the kernel page table before
/// this function is called.
pub unsafe fn init(mmio_base: u64) {
    if mmio_base == 0 {
        crate::arch::serial::write_line(b"[hpet] no MMIO base; HPET unavailable");
        return;
    }
    unsafe {
        let cap = read64(mmio_base, HPET_CAP_ID);
        let period_fs = cap >> 32; // femtoseconds per tick
        if period_fs == 0 {
            crate::arch::serial::write_line(b"[hpet] invalid period; HPET unavailable");
            return;
        }
        HPET_PERIOD_FS.store(period_fs, Ordering::Relaxed);
        HPET_BASE.store(mmio_base, Ordering::Relaxed);

        // Enable the main counter.
        let cfg = read64(mmio_base, HPET_CONFIG);
        write64(mmio_base, HPET_CONFIG, cfg | HPET_CFG_ENABLE);
    }
    crate::arch::serial::write_line(b"[hpet] initialised");
}

/// Returns the raw HPET main counter value (ticks since `init()`).
/// Returns 0 if the HPET is not initialised.
#[inline]
pub fn read_ticks() -> u64 {
    let base = HPET_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    unsafe { read64(base, HPET_MAIN_CTR) }
}

/// Returns the elapsed time in nanoseconds since the HPET was initialised.
/// Returns 0 if the HPET is not available.
pub fn read_ns() -> u64 {
    let ticks = read_ticks();
    let period_fs = HPET_PERIOD_FS.load(Ordering::Relaxed);
    if period_fs == 0 {
        return 0;
    }
    // ns = ticks * period_fs / 1_000_000
    // To avoid overflow: use 128-bit intermediate.
    let wide = (ticks as u128) * (period_fs as u128);
    (wide / 1_000_000) as u64
}

/// Returns `true` if the HPET is available and producing monotonic timestamps.
#[inline]
pub fn is_available() -> bool {
    HPET_BASE.load(Ordering::Relaxed) != 0
}
