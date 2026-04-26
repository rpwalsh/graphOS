// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! KASLR (Kernel Address Space Layout Randomization) support.
//!
//! graphOS supports KASLR at two levels:
//!
//! ## Level 1 — Physical load slide (loader-assisted)
//! The UEFI loader can load the kernel image at a randomly chosen physical
//! base address (aligned to 2 MiB).  The kernel records this slide at boot
//! time by comparing the link-time base symbol (`__kernel_phys_link_base`
//! from the linker script) against the actual `BootInfo::kernel_phys_start`.
//!
//! Until the loader implements randomization, the slide is 0 and KASLR
//! degrades gracefully to a fixed-base binary — **the kernel still boots
//! correctly** because this module is read-only after `init`.
//!
//! ## Level 2 — Heap / stack entropy (always active)
//! Fine-grained ASLR for ring-3 process stacks and heaps is handled by
//! `mm::address_space` using RDRAND-seeded offsets.  That is independent of
//! this module.
//!
//! ## Security note
//! The slide value is a secret.  It must **never** be disclosed to ring-3
//! tasks via syscall return values, /proc, or IPC.  The only legitimate
//! kernel-internal users are the page-fault handler (to convert virtual
//! addresses to physical) and the symbol resolver (for kernel crash reports,
//! which are filtered before being passed to ring-3 debug clients).

use core::sync::atomic::{AtomicU64, Ordering};

// ── Link-time base ────────────────────────────────────────────────────────────

// The linker script must export `__kernel_phys_link_base` as the physical
// address the kernel was linked to run from.  On a non-KASLR build this is
// typically 0x0010_0000 (1 MiB) or whatever is configured in linker.ld.
//
// We read it via an `extern "C"` symbol to avoid any relocation fixing up
// a hard-coded constant.
unsafe extern "C" {
    static __kernel_phys_link_base: u8;
}

// ── Runtime slide ─────────────────────────────────────────────────────────────

/// Physical slide applied by the loader.
/// 0 = no KASLR (kernel loaded at its link-time physical base).
/// Non-zero = loader placed the kernel at (link_base + SLIDE).
static KASLR_SLIDE: AtomicU64 = AtomicU64::new(0);

/// `true` once `init()` has been called.
static INITIALIZED: AtomicU64 = AtomicU64::new(0); // 0=no, 1=yes

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise KASLR support.
///
/// Must be called once during early boot, before any address translation
/// that depends on the slide.
///
/// `kernel_phys_start` — the actual physical address where the kernel image
/// was placed by the loader (from `BootInfo::kernel_phys_start`).
pub fn init(kernel_phys_start: u64) {
    if INITIALIZED.swap(1, Ordering::AcqRel) != 0 {
        return; // called twice — ignore
    }

    let link_base = unsafe { &__kernel_phys_link_base as *const u8 as u64 };
    let slide = kernel_phys_start.wrapping_sub(link_base);

    KASLR_SLIDE.store(slide, Ordering::Release);

    if slide == 0 {
        crate::arch::serial::write_line(b"[kaslr] no slide - kernel at link-time base");
    } else {
        crate::arch::serial::write_bytes(b"[kaslr] slide=0x");
        crate::arch::serial::write_hex(slide);
        crate::arch::serial::write_line(b"");
    }
}

/// Return the current KASLR slide in bytes.
///
/// **Must not be exposed to ring-3 callers.**
#[inline]
pub fn slide() -> u64 {
    KASLR_SLIDE.load(Ordering::Acquire)
}

/// Translate a link-time virtual address to the runtime virtual address by
/// applying the KASLR slide.
///
/// Use this when converting addresses embedded in the kernel ELF (e.g.
/// symbol tables) to runtime addresses.
#[inline]
pub fn apply(link_vaddr: u64) -> u64 {
    link_vaddr.wrapping_add(slide())
}
