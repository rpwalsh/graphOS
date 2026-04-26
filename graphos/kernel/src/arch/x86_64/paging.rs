// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Paging — early virtual memory bootstrap.
//!
//! After UEFI exit_boot_services(), the loader leaves us with an
//! identity-mapped page table set up by OVMF. This module:
//!
//! 1. Reads and reports the current CR3 (PML4 base).
//! 2. Defines the type boundaries for kernel page tables.
//!
//! The actual construction of kernel-owned page tables (identity map
//! with 2 MiB huge pages, validation, CR3 switch) is in `mm::page_table`.
//!
//! ## Future work
//! - Higher-half kernel mapping
//! - NX, WP, and per-section permission bits

use crate::arch::x86_64::serial;

/// Read the current CR3 register (physical address of the PML4).
fn read_cr3() -> u64 {
    let val: u64;
    // SAFETY: Reading CR3 is a non-destructive privileged operation.
    // We are in ring 0 during early kernel init.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) val, options(nomem, nostack));
    }
    val
}

/// Page table entry index helpers, used by `mm::page_table`.
pub const PAGE_SIZE_4K: u64 = 4096;
pub const PAGE_SIZE_2M: u64 = 2 * 1024 * 1024;

/// Extract the PML4 index (bits 47:39) from a virtual address.
#[inline]
pub const fn pml4_index(vaddr: u64) -> usize {
    ((vaddr >> 39) & 0x1FF) as usize
}

/// Extract the PDPT index (bits 38:30) from a virtual address.
#[inline]
pub const fn pdpt_index(vaddr: u64) -> usize {
    ((vaddr >> 30) & 0x1FF) as usize
}

/// Extract the PD index (bits 29:21) from a virtual address.
#[inline]
pub const fn pd_index(vaddr: u64) -> usize {
    ((vaddr >> 21) & 0x1FF) as usize
}

/// Extract the PT index (bits 20:12) from a virtual address.
#[inline]
pub const fn pt_index(vaddr: u64) -> usize {
    ((vaddr >> 12) & 0x1FF) as usize
}

/// Common page-table entry flags (subset — extend as needed).
pub mod flags {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const WRITE_THROUGH: u64 = 1 << 3;
    pub const NO_CACHE: u64 = 1 << 4;
    pub const HUGE_PAGE: u64 = 1 << 7;
    pub const NO_EXECUTE: u64 = 1 << 63;
}

/// Early paging init: read and report the loader-provided page tables.
pub fn init() {
    let cr3 = read_cr3();
    // CR3 bits 51:12 hold the PML4 physical base address (4 KiB aligned).
    // Bits 3 (PWT) and 4 (PCD) are cache-control flags; the rest of the
    // lower 12 bits are reserved/zero (assuming PCID is disabled).
    let pml4_phys = cr3 & 0x000F_FFFF_FFFF_F000;

    serial::write_line(b"[paging] === Page Table Diagnostics ===");
    serial::write_bytes(b"[paging] CR3 (raw):       ");
    serial::write_hex(cr3);
    serial::write_bytes(b"[paging] PML4 phys base:  ");
    serial::write_hex(pml4_phys);
    serial::write_line(b"[paging] Status: using UEFI/loader identity map");
    serial::write_line(b"[paging] Next: build kernel-owned tables (future pass)");
    serial::write_line(b"[paging] === End Paging Diagnostics ===");
}

/// Enable SMEP, SMAP and UMIP by setting the corresponding CR4 bits.
///
/// * **SMEP** (bit 20): Supervisor-Mode Execution Prevention — the kernel
///   cannot execute code in user-mapped pages.
/// * **SMAP** (bit 21): Supervisor-Mode Access Prevention — the kernel
///   cannot *read or write* user-mapped pages unless RFLAGS.AC is set.
/// * **UMIP** (bit 11): User-Mode Instruction Prevention — rings > 0 cannot
///   execute SGDT/SIDT/SLDT/SMSW/STR, which leak kernel layout.
///
/// Called once, immediately after the kernel's own CR3 is active.
/// SAFETY: Must be called in ring 0 after a valid kernel address space is
/// active. SMAP requires that kernel code never touches user memory without
/// explicit `stac`/`clac` bracketing — enforced by the page-fault handler.
pub fn harden_cr4() {
    const UMIP: u64 = 1 << 11;
    const PCE: u64 = 1 << 8; // allow RDPMC from ring-3 (PMU telemetry)
    const SMEP: u64 = 1 << 20;
    const SMAP: u64 = 1 << 21;
    unsafe {
        // Read current CR4 — cpu_init may have already set SMEP/SMAP/UMIP;
        // only set bits that CPUID says the CPU supports.
        let ecx1: u32;
        let ebx7: u32;
        let ecx7: u32;
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "xor ecx, ecx",
            "cpuid",
            "mov {ecx1:e}, ecx",
            "pop rbx",
            ecx1 = out(reg) ecx1,
            lateout("eax") _,
            lateout("ecx") _,
            lateout("edx") _,
            options(nomem),
        );
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {ebx7_out:e}, ebx",
            "pop rbx",
            ebx7_out = out(reg) ebx7,
            lateout("eax") _,
            out("ecx") ecx7,
            lateout("edx") _,
            options(nomem),
        );
        let _ = ecx1; // available for future checks (e.g. RDRAND bit 30)
        let mut mask: u64 = PCE; // always safe to set PCE
        if ebx7 & (1 << 20) != 0 {
            mask |= SMAP;
        }
        if ebx7 & (1 << 7) != 0 {
            mask |= SMEP;
        }
        if ecx7 & (1 << 2) != 0 {
            mask |= UMIP;
        }

        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        core::arch::asm!("mov cr4, {}", in(reg) cr4 | mask, options(nomem, nostack));
    }
    serial::write_line(b"[paging] CR4: PCE enabled (SMEP/SMAP/UMIP if CPU supports)");
}

/// Read a 64-bit hardware random number via the RDRAND instruction.
///
/// Returns `Some(val)` if the CPU reports a valid sample within a small
/// retry budget (hardware is only allowed to fail transiently, but QEMU
/// always succeeds on the first attempt).  Returns `None` if the CPU does
/// not support RDRAND or consistently returns a carry-clear result.
#[inline]
pub fn rdrand64() -> Option<u64> {
    // Check CPUID.01H:ECX[30] before executing RDRAND.
    let ecx1: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "xor ecx, ecx",
            "cpuid",
            "mov {ecx1_out:e}, ecx",
            "pop rbx",
            ecx1_out = out(reg) ecx1,
            lateout("eax") _,
            lateout("ecx") _,
            lateout("edx") _,
            options(nomem),
        );
    }
    if ecx1 & (1 << 30) == 0 {
        return None;
    }
    let mut val: u64;
    let mut ok: u8;
    for _ in 0..10u32 {
        unsafe {
            core::arch::asm!(
                "rdrand {val}",
                "setc   {ok}",
                val = out(reg) val,
                ok  = out(reg_byte) ok,
                options(nomem, nostack),
            );
        }
        if ok != 0 {
            return Some(val);
        }
    }
    None
}
