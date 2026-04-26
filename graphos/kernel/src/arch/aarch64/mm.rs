// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AArch64 MMU + page-table management.
//!
//! Uses 4KiB pages, 4-level page tables (48-bit VA), TCR_EL1 configured for
//! 256 TiB virtual address space.

pub const PAGE_SIZE: usize = 4096;
pub const PAGE_BITS: usize = 12;

// ── Translation granule descriptors ──────────────────────────────────────────

/// Page descriptor attributes (EL1 normal memory, inner/outer write-back).
const PAGE_DESC_VALID: u64 = 1 << 0;
const PAGE_DESC_TABLE: u64 = 1 << 1;
const PAGE_DESC_AF: u64 = 1 << 10; // Access flag.
const PAGE_DESC_ISH: u64 = 3 << 8; // Inner-shareable.
const PAGE_DESC_NORMAL: u64 = 0 << 2; // MAIR index 0 (normal WB).
const PAGE_DESC_AP_RW: u64 = 0 << 6; // AP[2:1] = 0b00 (EL1 RW, EL0 no access).

pub fn make_page_desc(phys: u64) -> u64 {
    (phys & !0xFFF)
        | PAGE_DESC_VALID
        | PAGE_DESC_TABLE
        | PAGE_DESC_AF
        | PAGE_DESC_ISH
        | PAGE_DESC_NORMAL
        | PAGE_DESC_AP_RW
}

/// Install `TTBR0_EL1` (user) and `TTBR1_EL1` (kernel) page tables.
/// # Safety
/// Caller must ensure `ttbr0` and `ttbr1` point to valid 4 KiB-aligned page tables.
pub unsafe fn install_page_tables(ttbr0: u64, ttbr1: u64) {
    unsafe {
        core::arch::asm!(
            "msr ttbr0_el1, {0}",
            "msr ttbr1_el1, {1}",
            "isb",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            in(reg) ttbr0,
            in(reg) ttbr1,
            options(nomem, nostack)
        );
    }
}
