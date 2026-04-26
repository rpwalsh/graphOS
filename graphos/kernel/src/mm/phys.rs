// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Physical memory map capture and reporting.
//!
//! Copies the memory region table from BootInfo into kernel-owned storage
//! so the rest of the kernel can reason about physical memory layout.

use crate::arch::serial;
use crate::bootinfo::{BootInfo, MemoryRegion, MemoryRegionKind};

const MAX_REGIONS: usize = 256;

static mut REGION_STORE: [MemoryRegion; MAX_REGIONS] = [MemoryRegion {
    start: 0,
    length: 0,
    kind: MemoryRegionKind::Unknown,
    _pad: 0,
}; MAX_REGIONS];

static mut REGION_COUNT: usize = 0;

/// Copy the memory map from BootInfo into kernel-owned storage and log it.
///
/// # Safety contract (caller)
/// Must be called exactly once, during single-threaded early init, with a
/// valid `boot_info` whose `memory_regions()` pointer is still accessible.
pub fn init(boot_info: &BootInfo) {
    // SAFETY: boot_info.memory_regions() requires that the pointer and count
    // are valid. The loader guarantees this for the static BootInfo.
    let regions = unsafe { boot_info.memory_regions() };
    let count = regions.len().min(MAX_REGIONS);

    // SAFETY: Single-threaded early init. REGION_STORE and REGION_COUNT are
    // written here exactly once and become read-only for the rest of execution.
    unsafe {
        REGION_STORE[..count].copy_from_slice(&regions[..count]);
        REGION_COUNT = count;
    }

    #[cfg(feature = "boot-demos")]
    log_memory_map();
    #[cfg(not(feature = "boot-demos"))]
    log_memory_map_summary();
}

/// Log the captured memory map to serial in a structured table.
///
/// Called internally by `init()`. Reads REGION_STORE/REGION_COUNT which
/// were just populated in the same single-threaded init path.
fn log_memory_map() {
    // SAFETY: Called from init() immediately after populating the statics.
    // Still single-threaded.
    let count = unsafe { REGION_COUNT };
    serial::write_line(b"[phys] === Physical Memory Map ===");
    serial::write_bytes(b"[phys] Region count: ");
    serial::write_u64_dec(count as u64);

    serial::write_line(b"[phys] --- start ------------ end ------------ kind -------- size");
    let mut total_usable: u64 = 0;

    for r in unsafe { &REGION_STORE[..count] } {
        let end = r.start + r.length;
        let kind_str = match r.kind {
            MemoryRegionKind::Usable => {
                total_usable += r.length;
                b"Usable      " as &[u8]
            }
            MemoryRegionKind::Reserved => b"Reserved    ",
            MemoryRegionKind::AcpiReclaim => b"AcpiReclaim ",
            MemoryRegionKind::AcpiNvs => b"AcpiNvs     ",
            MemoryRegionKind::Mmio => b"Mmio        ",
            MemoryRegionKind::Unknown => b"Unknown     ",
        };
        serial::write_bytes(b"  ");
        serial::write_hex_inline(r.start);
        serial::write_bytes(b" .. ");
        serial::write_hex_inline(end);
        serial::write_bytes(b"  ");
        serial::write_bytes(kind_str);
        serial::write_hex_inline(r.length);
        serial::write_line(b"");
    }

    serial::write_bytes(b"[phys] Total usable: ");
    serial::write_hex_inline(total_usable);
    serial::write_bytes(b" (~");
    // MiB as decimal.
    let mib = total_usable / (1024 * 1024);
    if mib == 0 {
        serial::write_bytes(b"<1");
    } else {
        serial::write_u64_dec_inline(mib);
    }
    serial::write_line(b" MiB)");
    serial::write_line(b"[phys] === End Memory Map ===");
}

fn log_memory_map_summary() {
    let count = unsafe { REGION_COUNT };
    let mut total_usable: u64 = 0;
    for r in unsafe { &REGION_STORE[..count] } {
        if matches!(r.kind, MemoryRegionKind::Usable) {
            total_usable += r.length;
        }
    }

    serial::write_bytes(b"[phys] Region count: ");
    serial::write_u64_dec(count as u64);
    serial::write_bytes(b"[phys] Total usable: ");
    serial::write_hex_inline(total_usable);
    serial::write_bytes(b" (~");
    let mib = total_usable / (1024 * 1024);
    if mib == 0 {
        serial::write_bytes(b"<1");
    } else {
        serial::write_u64_dec_inline(mib);
    }
    serial::write_line(b" MiB)");
}

/// Iterate over captured usable regions, calling `f` for each.
///
/// The underlying static region store is populated once during `init()` and
/// never mutated again. Callers must ensure `init()` has completed before
/// calling this function.
pub fn with_usable_regions(mut f: impl FnMut(u64, u64)) {
    // SAFETY: REGION_STORE and REGION_COUNT are written exactly once in init()
    // during single-threaded early boot, and are read-only thereafter.
    let count = unsafe { REGION_COUNT };
    for r in unsafe { &REGION_STORE[..count] } {
        if matches!(r.kind, MemoryRegionKind::Usable) {
            f(r.start, r.length);
        }
    }
}

/// Return the exclusive upper bound of the highest usable physical address.
///
/// This reflects what the loader/firmware reported as usable RAM, not a
/// hardcoded policy limit. Returns `0` when no usable regions are present.
pub fn max_usable_end() -> u64 {
    let mut max_end = 0u64;
    with_usable_regions(|start, length| {
        let end = start.saturating_add(length);
        if end > max_end {
            max_end = end;
        }
    });
    max_end
}
