// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Reserved physical memory range registry.
//!
//! Before the frame allocator populates its free pool, this module records
//! every physical range that must **never** be handed out:
//!
//! - Kernel image (.text through .bss / __kernel_end)
//! - Display scanout linear region
//! - BootInfo structure itself
//! - Memory region array pointed to by BootInfo
//! - ACPI RSDP region
//! - Page-table frames allocated during paging bootstrap
//! - Early kernel stack (if statically placed)
//! - Any future initramfs or module pages
//!
//! The frame allocator queries `is_reserved()` for every candidate frame
//! before admitting it to the free pool.
//!
//! ## Hardening invariants
//! - Ranges with `start >= end` are rejected (zero-length or inverted).
//! - Overlapping ranges are permitted (the same byte may be covered by
//!   multiple labels — e.g. kernel image vs loader view). The overlap
//!   is detected and logged as a diagnostic, but is not an error.
//! - `add_post_init()` allows registering ranges *after* the frame
//!   allocator has run (e.g. page-table frames). These ranges are
//!   recorded for auditing and future free-list patching.
//! - `total_reserved_bytes()` reports the gross coverage (may double-count
//!   overlaps — that is conservative and correct for diagnostics).

use crate::arch::serial;

/// Maximum number of reserved ranges we track.
const MAX_RESERVED: usize = 64;

/// A half-open physical address range `[start, end)` with a label tag.
#[derive(Clone, Copy)]
struct Range {
    start: u64,
    end: u64,
    /// Was this range added after frame_alloc::init()?
    post_init: bool,
}

static mut RANGES: [Range; MAX_RESERVED] = [Range {
    start: 0,
    end: 0,
    post_init: false,
}; MAX_RESERVED];
static mut COUNT: usize = 0;
/// Set to `true` once frame_alloc::init() has been called.
static mut INIT_PHASE_COMPLETE: bool = false;

/// Register a physical range as reserved (pre-init only).
///
/// # Safety
/// Must be called only during single-threaded early init, before
/// `frame_alloc::init()`.
pub unsafe fn add(start: u64, end: u64, label: &[u8]) {
    // Reject degenerate ranges.
    if start >= end {
        serial::write_bytes(b"[reserved] WARNING: degenerate range rejected: ");
        serial::write_line(label);
        return;
    }

    // SAFETY: Single-threaded during early init; writing our own static.
    unsafe {
        let i = COUNT;
        if i >= MAX_RESERVED {
            serial::write_line(b"[reserved] WARNING: range table full, cannot add:");
            serial::write_line(label);
            return;
        }

        // Check for overlaps with existing ranges (diagnostic, not error).
        for (j, r) in RANGES[..i].iter().copied().enumerate() {
            if start < r.end && r.start < end {
                serial::write_bytes(b"[reserved] NOTE: overlap with range ");
                serial::write_u64_dec_inline(j as u64);
                serial::write_bytes(b" (");
                serial::write_hex_inline(r.start);
                serial::write_bytes(b"...");
                serial::write_hex_inline(r.end);
                serial::write_line(b")");
            }
        }

        RANGES[i] = Range {
            start,
            end,
            post_init: false,
        };
        COUNT = i + 1;
    }

    log_range_added(label, start, end, false);
}

/// Register a physical range as reserved **after** the frame allocator has
/// already been initialized. Used for page-table frames and other
/// allocations that happen during paging bootstrap.
///
/// These frames were handed out by `frame_alloc` so they are already not
/// in the free pool. Recording them here is for audit completeness and
/// so future compaction / free-list rebuild can account for them.
///
/// # Safety
/// Must be called during single-threaded init (no concurrent access to
/// the RANGES array).
pub unsafe fn add_post_init(start: u64, end: u64, label: &[u8]) {
    if start >= end {
        serial::write_bytes(b"[reserved] WARNING: degenerate post-init range rejected: ");
        serial::write_line(label);
        return;
    }

    unsafe {
        let i = COUNT;
        if i >= MAX_RESERVED {
            serial::write_line(b"[reserved] WARNING: range table full (post-init), cannot add:");
            serial::write_line(label);
            return;
        }
        RANGES[i] = Range {
            start,
            end,
            post_init: true,
        };
        COUNT = i + 1;
    }

    log_range_added(label, start, end, true);
}

/// Mark the init phase as complete. Called by `frame_alloc::init()` after
/// populating the free pool.
///
/// # Safety
/// Must be called exactly once, during single-threaded init.
pub unsafe fn mark_init_complete() {
    unsafe {
        INIT_PHASE_COMPLETE = true;
    }
}

/// Returns `true` if the 4 KiB frame at `frame_addr` overlaps any reserved range.
///
/// # Safety
/// Must be called after all pre-init `add()` calls are complete and before
/// concurrent access. In practice, `frame_alloc::init()` calls this during
/// single-threaded early init.
pub unsafe fn is_reserved(frame_addr: u64) -> bool {
    let frame_end = frame_addr + 4096;
    // SAFETY: Reading static populated during single-threaded init.
    let count = unsafe { COUNT };
    for r in unsafe { &RANGES[..count] } {
        // Overlap check: ranges overlap iff start < other_end && other_start < end.
        if frame_addr < r.end && r.start < frame_end {
            return true;
        }
    }
    false
}

/// Log all registered reserved ranges to serial.
///
/// Must be called after all `add()` calls are complete.
pub fn log_all() {
    // SAFETY: COUNT is only mutated during single-threaded early init via add().
    // This function is called after all add() calls are complete, so the read
    // is race-free.
    let count = unsafe { COUNT };
    serial::write_bytes(b"[reserved] Total reserved ranges: ");
    serial::write_u64_dec(count as u64);

    let total = total_reserved_bytes();
    serial::write_bytes(b"[reserved] Gross reserved bytes:  ");
    serial::write_hex_inline(total);
    serial::write_bytes(b" (~");
    let kib = total / 1024;
    if kib < 1024 {
        serial::write_u64_dec_inline(kib);
        serial::write_line(b" KiB)");
    } else {
        serial::write_u64_dec_inline(kib / 1024);
        serial::write_line(b" MiB)");
    }
}

/// Returns the number of registered reserved ranges.
///
/// Safe to call after all `add()` calls are complete.
pub fn count() -> usize {
    // SAFETY: COUNT is written during single-threaded early init and read-only
    // thereafter. This function is called after init completes.
    unsafe { COUNT }
}

/// Returns the `[start, end)` addresses of the `index`-th reserved range.
///
/// Returns `None` if `index >= count()`.
pub fn get(index: usize) -> Option<(u64, u64)> {
    // SAFETY: Same invariant as count() — init is complete, values are frozen.
    let c = unsafe { COUNT };
    if index >= c {
        return None;
    }
    let r = unsafe { RANGES[index] };
    Some((r.start, r.end))
}

/// Total bytes covered by all reserved ranges (gross — overlaps are
/// double-counted). This is intentionally conservative.
pub fn total_reserved_bytes() -> u64 {
    let count = unsafe { COUNT };
    let mut total: u64 = 0;
    for r in unsafe { &RANGES[..count] } {
        total += r.end.saturating_sub(r.start);
    }
    total
}

/// Returns the number of reserved ranges added after frame_alloc init.
pub fn post_init_count() -> usize {
    let count = unsafe { COUNT };
    let mut n = 0usize;
    for range in unsafe { &RANGES[..count] } {
        if range.post_init {
            n += 1;
        }
    }
    n
}

// ---- Internal helpers ----

fn log_range_added(label: &[u8], start: u64, end: u64, post: bool) {
    if post {
        serial::write_bytes(b"[reserved+] ");
    } else {
        serial::write_bytes(b"[reserved]  ");
    }
    serial::write_bytes(label);
    serial::write_bytes(b"  ");
    serial::write_hex_inline(start);
    serial::write_bytes(b" .. ");
    serial::write_hex_inline(end);
    serial::write_bytes(b"  (");
    let kb = (end.saturating_sub(start)) / 1024;
    if kb == 0 {
        serial::write_bytes(b"<1");
    } else {
        serial::write_u64_dec_inline(kb);
    }
    serial::write_line(b" KiB)");
}
