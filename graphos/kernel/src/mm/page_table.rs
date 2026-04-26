// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Kernel-owned page table construction — paging bootstrap.
//!
//! After UEFI exits boot services, the kernel inherits an identity-mapped
//! page table from OVMF. This module allocates fresh page-table frames from
//! the frame allocator, builds a kernel-owned identity map, validates the
//! tables, and switches CR3.
//!
//! ## Design
//! - Uses 2 MiB huge pages for the identity map to minimise frame usage.
//! - Maps the first `identity_limit` bytes 1:1 (physical == virtual).
//! - All page-table frames are registered in `mm::reserved` via
//!   `add_post_init()` so the reserved-range audit stays correct.
//! - `validate()` reads back the constructed tables and checks that every
//!   PML4→PDPT→PD link is present and the PD huge-page entries point at
//!   the expected physical addresses.
//! - `activate()` writes the new PML4 to CR3, taking over from the UEFI
//!   identity map.
//!
//! ## Invariants
//! - Every frame allocated here is 4 KiB-aligned (guaranteed by frame_alloc).
//! - Page tables are zeroed before use.
//! - No frame is ever double-mapped.
//! - `validate()` must succeed before `activate()` is called.
//!
//! ## Limitations (current pass)
//! - Identity map only; no higher-half kernel mapping yet.
//! - No NX/permission hardening on sections yet.
//! - Only maps the first N GiB with 2 MiB pages.

use crate::arch::serial;
use crate::arch::x86_64::paging::flags;
use crate::mm::frame_alloc;
use crate::mm::reserved;

/// Number of page-table frames we track for audit/reservation.
/// Increased from 16 to 128 to support dynamic MMIO mapping for multiple virtio devices
/// and other PCI MMIO regions that may be discovered at runtime.
const MAX_PT_FRAMES: usize = 128;

/// Page-table frames allocated during bootstrap.
static mut PT_FRAMES: [u64; MAX_PT_FRAMES] = [0; MAX_PT_FRAMES];
static mut PT_FRAME_COUNT: usize = 0;

/// Physical address of the active kernel PML4 (set after activate()).
static mut ACTIVE_PML4: u64 = 0;

/// Result of page-table bootstrap.
pub struct BootstrapResult {
    /// Physical address of the new PML4.
    pub pml4_phys: u64,
    /// Number of page-table frames allocated.
    pub frames_used: usize,
    /// Total bytes identity-mapped.
    pub mapped_bytes: u64,
}

/// IA32_EFER MSR address.
const IA32_EFER: u32 = 0xC000_0080;

/// Enable the NX/XD bit in IA32_EFER (bit 11). Must be called before
/// building page tables that use NO_EXECUTE, and before loading CR3.
pub unsafe fn enable_nxe() {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_EFER,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    let new_lo = lo | (1 << 11); // NXE bit
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_EFER,
            in("eax") new_lo,
            in("edx") hi,
            options(nomem, nostack),
        );
    }
    serial::write_line(b"[page_table] IA32_EFER.NXE enabled");
}

/// Allocate a zeroed 4 KiB frame and record it in our tracking array.
/// Returns `None` if the frame allocator is exhausted or tracking is full.
fn alloc_pt_frame() -> Option<u64> {
    let frame = frame_alloc::alloc_frame()?;

    // Zero the frame. SAFETY: The frame allocator guarantees this address
    // is valid, usable physical memory not overlapping any reserved range.
    // We are in identity-mapped mode (UEFI/OVMF tables), so phys == virt.
    let ptr = frame as *mut u8;
    unsafe {
        core::ptr::write_bytes(ptr, 0, 4096);
    }

    // Track the frame.
    unsafe {
        let i = PT_FRAME_COUNT;
        if i < MAX_PT_FRAMES {
            PT_FRAMES[i] = frame;
            PT_FRAME_COUNT = i + 1;
        }
    }

    Some(frame)
}

/// Build a kernel-owned identity-map page table using 2 MiB huge pages.
///
/// Maps the first `limit_bytes` of physical address space 1:1.
/// `limit_bytes` is rounded up to the next 2 MiB boundary.
///
/// Returns `None` if frame allocation fails.
///
/// # Safety
/// Must be called during single-threaded early init while the UEFI identity
/// map is still active (so physical addresses are directly accessible as
/// virtual addresses).
pub unsafe fn build_identity_map(limit_bytes: u64) -> Option<BootstrapResult> {
    let limit = align_up_2m(limit_bytes);
    let num_2m_pages = (limit / (2 * 1024 * 1024)) as usize;

    serial::write_line(b"[page_table] === Building kernel page tables ===");
    serial::write_bytes(b"[page_table] Identity map limit: ");
    serial::write_hex_inline(limit);
    serial::write_bytes(b" (");
    serial::write_u64_dec_inline(limit / (1024 * 1024));
    serial::write_line(b" MiB)");
    serial::write_bytes(b"[page_table] 2 MiB pages needed: ");
    serial::write_u64_dec(num_2m_pages as u64);

    // How many PDs do we need? Each PD holds 512 entries × 2 MiB = 1 GiB.
    let num_pds = num_2m_pages.div_ceil(512);

    // Allocate PML4.
    let pml4 = alloc_pt_frame()?;

    // Allocate PDPT (one is enough for up to 512 GiB).
    let pdpt = alloc_pt_frame()?;

    // Link PDPT into PML4 entry 0.
    let pml4_table = pml4 as *mut u64;
    unsafe {
        pml4_table.write(pdpt | flags::PRESENT | flags::WRITABLE);
    }

    // Allocate and populate PDs.
    let mut pages_mapped: usize = 0;
    for pd_idx in 0..num_pds {
        let pd = alloc_pt_frame()?;

        // Link PD into PDPT.
        let pdpt_table = pdpt as *mut u64;
        unsafe {
            pdpt_table
                .add(pd_idx)
                .write(pd | flags::PRESENT | flags::WRITABLE);
        }

        // Fill PD entries with 2 MiB huge pages.
        let pd_table = pd as *mut u64;
        let entries_this_pd = if pd_idx == num_pds - 1 {
            num_2m_pages - pages_mapped
        } else {
            512
        };

        for entry in 0..entries_this_pd {
            let phys_addr = (pages_mapped + entry) as u64 * 2 * 1024 * 1024;
            // M10 fix: mark non-code pages as No-Execute.
            // The kernel image fits in the first 4 MiB (2 huge pages).
            // Pages beyond that are data/stack/heap — mark NX.
            let nx = if phys_addr >= 4 * 1024 * 1024 {
                flags::NO_EXECUTE
            } else {
                0
            };
            unsafe {
                pd_table
                    .add(entry)
                    .write(phys_addr | flags::PRESENT | flags::WRITABLE | flags::HUGE_PAGE | nx);
            }
        }

        pages_mapped += entries_this_pd;
    }

    let frames_used = unsafe { PT_FRAME_COUNT };
    let mapped_bytes = pages_mapped as u64 * 2 * 1024 * 1024;

    serial::write_bytes(b"[page_table] Frames allocated for tables: ");
    serial::write_u64_dec(frames_used as u64);
    serial::write_bytes(b"[page_table] Total mapped: ");
    serial::write_hex_inline(mapped_bytes);
    serial::write_bytes(b" (");
    serial::write_u64_dec_inline(mapped_bytes / (1024 * 1024));
    serial::write_line(b" MiB)");
    serial::write_bytes(b"[page_table] New PML4 at: ");
    serial::write_hex(pml4);

    Some(BootstrapResult {
        pml4_phys: pml4,
        frames_used,
        mapped_bytes,
    })
}

/// Validate a kernel page table by reading back the structure.
///
/// Walks PML4 → PDPT → PD and checks:
/// 1. PML4 entry 0 is present and points to a valid PDPT.
/// 2. Each PDPT entry that should be populated is present.
/// 3. Each PD entry is a 2 MiB huge page pointing at the correct physical address.
///
/// Returns the number of errors found. Zero means the tables are safe to activate.
///
/// # Safety
/// Must be called while the UEFI identity map is still active so the
/// page-table frames are accessible at their physical addresses.
pub unsafe fn validate(result: &BootstrapResult) -> usize {
    serial::write_line(b"[page_table] === Validating kernel page tables ===");
    let mut errors: usize = 0;

    let pml4 = result.pml4_phys;
    let pml4_table = pml4 as *const u64;

    // Check PML4 entry 0.
    let pml4e0 = unsafe { pml4_table.read() };
    if pml4e0 & flags::PRESENT == 0 {
        serial::write_line(b"[page_table] ERROR: PML4[0] not present");
        errors += 1;
        return errors;
    }
    let pdpt = pml4e0 & 0x000F_FFFF_FFFF_F000;

    // Check that PML4 entries 1..511 are NOT present (we only map entry 0).
    for i in 1..512u64 {
        let entry = unsafe { pml4_table.add(i as usize).read() };
        if entry & flags::PRESENT != 0 {
            serial::write_bytes(b"[page_table] WARN: unexpected PML4[");
            serial::write_u64_dec_inline(i);
            serial::write_line(b"] present");
        }
    }

    // Walk PDPT entries.
    let pdpt_table = pdpt as *const u64;
    let expected_2m_pages = result.mapped_bytes / (2 * 1024 * 1024);
    let expected_pds = (expected_2m_pages as usize).div_ceil(512);

    let mut total_pages_checked: u64 = 0;

    for pd_idx in 0..expected_pds {
        let pdpte = unsafe { pdpt_table.add(pd_idx).read() };
        if pdpte & flags::PRESENT == 0 {
            serial::write_bytes(b"[page_table] ERROR: PDPT[");
            serial::write_u64_dec_inline(pd_idx as u64);
            serial::write_line(b"] not present");
            errors += 1;
            continue;
        }

        let pd = pdpte & 0x000F_FFFF_FFFF_F000;
        let pd_table = pd as *const u64;

        // How many entries in this PD?
        let remaining = expected_2m_pages - total_pages_checked;
        let entries_this_pd = if remaining > 512 { 512 } else { remaining };

        for entry_idx in 0..entries_this_pd as usize {
            let pde = unsafe { pd_table.add(entry_idx).read() };
            let expected_phys =
                total_pages_checked * 2 * 1024 * 1024 + entry_idx as u64 * 2 * 1024 * 1024;

            if pde & flags::PRESENT == 0 {
                serial::write_bytes(b"[page_table] ERROR: PD[");
                serial::write_u64_dec_inline(pd_idx as u64);
                serial::write_bytes(b"][");
                serial::write_u64_dec_inline(entry_idx as u64);
                serial::write_line(b"] not present");
                errors += 1;
                continue;
            }
            if pde & flags::HUGE_PAGE == 0 {
                serial::write_bytes(b"[page_table] ERROR: PD[");
                serial::write_u64_dec_inline(pd_idx as u64);
                serial::write_bytes(b"][");
                serial::write_u64_dec_inline(entry_idx as u64);
                serial::write_line(b"] not a huge page");
                errors += 1;
                continue;
            }

            let mapped_phys = pde & 0x000F_FFFF_FFE0_0000; // 2 MiB-aligned mask
            if mapped_phys != expected_phys {
                serial::write_bytes(b"[page_table] ERROR: PD[");
                serial::write_u64_dec_inline(pd_idx as u64);
                serial::write_bytes(b"][");
                serial::write_u64_dec_inline(entry_idx as u64);
                serial::write_bytes(b"] maps ");
                serial::write_hex_inline(mapped_phys);
                serial::write_bytes(b" expected ");
                serial::write_hex(expected_phys);
                errors += 1;
            }
        }

        total_pages_checked += entries_this_pd;
    }

    // Check PDPT entries beyond expected PDs are not present.
    for i in expected_pds..512 {
        let entry = unsafe { pdpt_table.add(i).read() };
        if entry & flags::PRESENT != 0 {
            serial::write_bytes(b"[page_table] WARN: unexpected PDPT[");
            serial::write_u64_dec_inline(i as u64);
            serial::write_line(b"] present");
        }
    }

    if errors == 0 {
        serial::write_bytes(b"[page_table] Validation passed: ");
        serial::write_u64_dec_inline(total_pages_checked);
        serial::write_line(b" 2 MiB pages verified");
    } else {
        serial::write_bytes(b"[page_table] Validation FAILED: ");
        serial::write_u64_dec_inline(errors as u64);
        serial::write_line(b" errors");
    }

    serial::write_line(b"[page_table] === End validation ===");
    errors
}

/// Switch CR3 to the kernel-owned page tables.
///
/// After this call, the UEFI-provided page tables are no longer in use.
/// The identity map built by `build_identity_map` is active.
///
/// # Safety
/// - `validate()` must have returned zero errors for the given result.
/// - Must be called during single-threaded early init.
/// - The identity map must cover all memory currently in use (kernel image,
///   stack, framebuffer, page-table frames, serial MMIO, etc.).
pub unsafe fn activate(result: &BootstrapResult) {
    serial::write_bytes(b"[page_table] Switching CR3 to ");
    serial::write_hex(result.pml4_phys);

    // SAFETY: result.pml4_phys points to a validated kernel-owned PML4.
    // The identity map covers all memory the kernel is currently using.
    // Writing CR3 implicitly flushes the TLB.
    unsafe {
        core::arch::asm!(
            "mov cr3, {}",
            in(reg) result.pml4_phys,
            options(nostack, preserves_flags),
        );
    }

    // Record the active PML4 for future reference.
    unsafe {
        ACTIVE_PML4 = result.pml4_phys;
    }

    serial::write_line(b"[page_table] CR3 switch complete - kernel page tables active");
}

/// Load an arbitrary validated address-space root into CR3.
///
/// Used by the scheduler when switching between kernel tasks and user tasks
/// that carry a private address space.
pub unsafe fn load_address_space(pml4_phys: u64) {
    unsafe {
        core::arch::asm!(
            "mov cr3, {}",
            in(reg) pml4_phys,
            options(nostack, preserves_flags),
        );
    }
}

/// Returns the physical address of the currently loaded CR3 root.
pub fn current_pml4() -> u64 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    cr3 & 0x000F_FFFF_FFFF_F000
}

/// Flush one virtual page from the local TLB.
///
/// Used after servicing a demand fault in the currently active address space.
pub unsafe fn flush_page(vaddr: u64) {
    unsafe {
        core::arch::asm!(
            "invlpg [{}]",
            in(reg) vaddr,
            options(nostack, preserves_flags),
        );
    }
}

/// Returns the physical address of the currently active kernel PML4,
/// or 0 if `activate()` has not been called.
pub fn active_pml4() -> u64 {
    unsafe { ACTIVE_PML4 }
}

/// Run `f` with the shared kernel page tables loaded, then restore the
/// previously active CR3.
///
/// This is used by IRQ handlers that must touch kernel-only mappings (for
/// example late-discovered MMIO windows) even when the interrupted task is
/// running with a private user CR3.
pub fn with_kernel_address_space<R>(f: impl FnOnce() -> R) -> R {
    let kernel_root = active_pml4();
    if kernel_root == 0 {
        return f();
    }

    let current_root = current_pml4();
    if current_root == kernel_root {
        return f();
    }

    unsafe { load_address_space(kernel_root) };
    let result = f();
    unsafe { load_address_space(current_root) };
    result
}

/// Ensure a physical 2 MiB region is identity-mapped in the active kernel
/// page tables.
///
/// This is used for late-discovered MMIO windows that sit above the initial
/// 4 GiB bootstrap map.
pub fn ensure_identity_mapped_2m(phys_addr: u64) -> bool {
    let base = phys_addr & !(super::super::arch::x86_64::paging::PAGE_SIZE_2M - 1);
    let pml4_phys = active_pml4();
    if pml4_phys == 0 {
        return false;
    }

    let pml4 = pml4_phys as *mut u64;
    let pml4_idx = crate::arch::x86_64::paging::pml4_index(base);
    let pdpt_phys = unsafe {
        let entry_ptr = pml4.add(pml4_idx);
        let entry = entry_ptr.read();
        if entry & flags::PRESENT != 0 {
            entry & 0x000F_FFFF_FFFF_F000
        } else {
            let Some(frame) = alloc_pt_frame() else {
                return false;
            };
            entry_ptr.write(frame | flags::PRESENT | flags::WRITABLE);
            reserved::add_post_init(frame, frame + 4096, b"page-table frame");
            frame
        }
    };

    let pdpt = pdpt_phys as *mut u64;
    let pdpt_idx = crate::arch::x86_64::paging::pdpt_index(base);
    let pd_phys = unsafe {
        let entry_ptr = pdpt.add(pdpt_idx);
        let entry = entry_ptr.read();
        if entry & flags::PRESENT != 0 {
            entry & 0x000F_FFFF_FFFF_F000
        } else {
            let Some(frame) = alloc_pt_frame() else {
                return false;
            };
            entry_ptr.write(frame | flags::PRESENT | flags::WRITABLE);
            reserved::add_post_init(frame, frame + 4096, b"page-table frame");
            frame
        }
    };

    let pd = pd_phys as *mut u64;
    let pd_idx = crate::arch::x86_64::paging::pd_index(base);
    unsafe {
        let entry_ptr = pd.add(pd_idx);
        let entry = entry_ptr.read();
        if entry & flags::PRESENT != 0 {
            return (entry & 0x000F_FFFF_FFE0_0000) == base;
        }

        entry_ptr.write(
            base | flags::PRESENT
                | flags::WRITABLE
                | flags::HUGE_PAGE
                | flags::NO_CACHE
                | flags::NO_EXECUTE,
        );
        flush_page(base);
    }

    true
}

/// Register all page-table frames as reserved (post-init) for audit.
///
/// # Safety
/// Must be called during single-threaded init.
pub unsafe fn register_pt_frames_reserved() {
    let count = unsafe { PT_FRAME_COUNT };
    for &frame in unsafe { &PT_FRAMES[..count] } {
        unsafe {
            reserved::add_post_init(frame, frame + 4096, b"page-table frame");
        }
    }
    if count > 0 {
        serial::write_bytes(b"[page_table] Registered ");
        serial::write_u64_dec_inline(count as u64);
        serial::write_line(b" page-table frames as reserved (post-init)");
    }
}

/// Number of page-table frames allocated so far.
pub fn pt_frame_count() -> usize {
    unsafe { PT_FRAME_COUNT }
}

#[inline]
fn align_up_2m(addr: u64) -> u64 {
    let two_m = 2 * 1024 * 1024;
    (addr + two_m - 1) & !(two_m - 1)
}

// ---------------------------------------------------------------------------
// W^X kernel section remapping
// ---------------------------------------------------------------------------

// Section boundaries exported by the linker script.
unsafe extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

/// Split any 2 MiB huge pages that overlap the kernel image into 4 KiB pages
/// and apply W^X permissions to each section:
///
/// - `.text`:   PRESENT | (no WRITABLE) | (no NO_EXECUTE)  — RX
/// - `.rodata`: PRESENT | NO_EXECUTE    | (no WRITABLE)     — R
/// - `.data`/`.bss` and remainder: PRESENT | WRITABLE | NO_EXECUTE — RW
///
/// Must be called after `activate()` so the kernel-owned CR3 is live.
///
/// # Safety
/// - Must be called during single-threaded early init.
/// - Requires the frame allocator to be initialised (Stage 7+).
/// - Paging must be active (Stage 8+).
pub unsafe fn remap_kernel_sections() {
    let kernel_start = unsafe { &__kernel_start as *const u8 as u64 };
    let text_end = unsafe { &__text_end as *const u8 as u64 };
    let rodata_start = unsafe { &__rodata_start as *const u8 as u64 };
    let rodata_end = unsafe { &__rodata_end as *const u8 as u64 };
    let kernel_end = unsafe { &__kernel_end as *const u8 as u64 };

    serial::write_bytes(b"[page_table] W^X remap: kernel ");
    serial::write_hex_inline(kernel_start);
    serial::write_bytes(b" - ");
    serial::write_hex(kernel_end);

    let pml4_phys = unsafe { ACTIVE_PML4 };
    if pml4_phys == 0 {
        serial::write_line(b"[page_table] W^X remap: skipped (no active PML4)");
        return;
    }

    // Walk every 2 MiB page that contains any part of the kernel image.
    let first_2m = kernel_start & !(2 * 1024 * 1024 - 1);
    let last_2m = (kernel_end + 2 * 1024 * 1024 - 1) & !(2 * 1024 * 1024 - 1);

    let mut phys = first_2m;
    while phys < last_2m {
        let split_ok =
            unsafe { split_huge_page_wx(pml4_phys, phys, text_end, rodata_start, rodata_end) };
        if !split_ok {
            serial::write_bytes(b"[page_table] W^X remap FAILED at ");
            serial::write_hex(phys);
        }
        phys += 2 * 1024 * 1024;
    }

    serial::write_line(b"[page_table] W^X remap complete");
}

/// Split the 2 MiB huge page at `huge_base` into 512 × 4 KiB pages, setting
/// permissions based on where each 4 KiB page falls relative to section boundaries.
///
/// Returns `true` on success, `false` if frame allocation fails.
///
/// # Safety
/// `pml4_phys` must point to the active, kernel-owned PML4.
/// Interrupts should be disabled by the caller (single-threaded early init).
unsafe fn split_huge_page_wx(
    pml4_phys: u64,
    huge_base: u64,
    text_end: u64,
    rodata_start: u64,
    rodata_end: u64,
) -> bool {
    use crate::arch::x86_64::paging::{pd_index, pdpt_index, pml4_index};

    let pml4 = pml4_phys as *mut u64;
    let pml4e = unsafe { pml4.add(pml4_index(huge_base)).read() };
    if pml4e & flags::PRESENT == 0 {
        return false;
    }
    let pdpt = (pml4e & 0x000F_FFFF_FFFF_F000) as *mut u64;

    let pdpte = unsafe { pdpt.add(pdpt_index(huge_base)).read() };
    if pdpte & flags::PRESENT == 0 {
        return false;
    }
    let pd = (pdpte & 0x000F_FFFF_FFFF_F000) as *mut u64;

    let pd_idx = pd_index(huge_base);
    let pd_entry_ptr = unsafe { pd.add(pd_idx) };
    let pde = unsafe { pd_entry_ptr.read() };

    // Already a 4 KiB page table — nothing to split.
    if pde & flags::PRESENT != 0 && pde & flags::HUGE_PAGE == 0 {
        return true;
    }

    // Allocate a fresh 4 KiB PT frame.
    let Some(pt_phys) = alloc_pt_frame() else {
        return false;
    };
    unsafe {
        reserved::add_post_init(pt_phys, pt_phys + 4096, b"page-table frame");
    }
    let pt = pt_phys as *mut u64;

    // Populate 512 × 4 KiB entries.
    for i in 0..512u64 {
        let page_phys = huge_base + i * 4096;
        let flags_val = if page_phys >= rodata_start && page_phys < rodata_end {
            // .rodata: read-only, no-execute
            flags::PRESENT | flags::NO_EXECUTE
        } else if page_phys < text_end {
            // .text (and pages before kernel_start within this huge page): execute, no-write
            flags::PRESENT
        } else {
            // .data, .bss, and anything above rodata: writable, no-execute
            flags::PRESENT | flags::WRITABLE | flags::NO_EXECUTE
        };
        unsafe { pt.add(i as usize).write(page_phys | flags_val) };
    }

    // Replace huge-page PD entry with a pointer to the new PT.
    unsafe { pd_entry_ptr.write(pt_phys | flags::PRESENT | flags::WRITABLE) };

    // Flush all 512 affected 4 KiB TLB entries.
    for i in 0..512u64 {
        unsafe { flush_page(huge_base + i * 4096) };
    }

    true
}
