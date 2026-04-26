// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Pre-allocated kernel stack pool.
//!
//! Under identity mapping (phys == virt), task stacks require physically
//! contiguous frames. The bump heap allocator consumes frames from the same
//! pool, potentially fragmenting it. To guarantee contiguous stacks, we
//! pre-allocate a pool of stack regions during early boot, before the heap.
//!
//! ## Design
//! - `init()` is called early in boot (before heap init).
//! - Pre-allocates `MAX_STACKS` contiguous regions, each `KERNEL_STACK_FRAMES` frames.
//! - `alloc_stack()` returns a pre-allocated region or `None` if exhausted.
//! - `free_stack()` returns a region to the pool for reuse.
//!
//! ## Production considerations
//! - Pool size is static; cannot grow at runtime.
//! - If more tasks are needed than pool slots, task creation fails.
//! - Future: virtual memory removes the contiguity requirement entirely.

use spin::Mutex;

use crate::arch::serial;
use crate::mm::frame_alloc;

use super::tcb::{KERNEL_STACK_FRAMES, KERNEL_STACK_SIZE};

/// Maximum number of pre-allocated stacks.
/// Must be >= MAX_TASKS in table.rs.
pub const MAX_STACKS: usize = 64;

/// A pre-allocated stack region.
#[derive(Clone, Copy)]
struct StackSlot {
    /// Physical base address (lowest address) of the stack.
    base: u64,
    /// Whether this slot is currently in use.
    in_use: bool,
}

impl StackSlot {
    const fn empty() -> Self {
        Self {
            base: 0,
            in_use: false,
        }
    }
}

/// The global stack pool.
struct StackPool {
    slots: [StackSlot; MAX_STACKS],
    /// Number of slots that have been allocated from the frame allocator.
    allocated: usize,
    /// Whether init() has been called.
    initialized: bool,
}

impl StackPool {
    const fn new() -> Self {
        Self {
            slots: [StackSlot::empty(); MAX_STACKS],
            allocated: 0,
            initialized: false,
        }
    }
}

static POOL: Mutex<StackPool> = Mutex::new(StackPool::new());

#[inline(always)]
fn current_rsp() -> u64 {
    let rsp: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, rsp",
            out(reg) rsp,
            options(nomem, nostack, preserves_flags),
        );
    }
    rsp
}

/// Initialize the stack pool by pre-allocating contiguous stack regions.
///
/// Must be called early in boot, BEFORE heap init, to ensure contiguous
/// frames are available.
///
/// # Arguments
/// * `count` - Number of stacks to pre-allocate. Clamped to MAX_STACKS.
///
/// # Returns
/// Number of stacks successfully allocated.
pub fn init(count: usize) -> usize {
    let mut pool = POOL.lock();
    if pool.initialized {
        serial::write_line(b"[stack_pool] WARNING: already initialized");
        return pool.allocated;
    }

    let target = count.min(MAX_STACKS);
    let mut allocated = 0;

    serial::write_bytes(b"[stack_pool] Pre-allocating ");
    serial::write_u64_dec_inline(target as u64);
    serial::write_bytes(b" stacks (");
    serial::write_u64_dec_inline((target * KERNEL_STACK_SIZE / 1024) as u64);
    serial::write_line(b" KiB total)...");

    for i in 0..target {
        match alloc_contiguous_stack() {
            Some(base) => {
                pool.slots[i] = StackSlot {
                    base,
                    in_use: false,
                };
                allocated += 1;
            }
            None => {
                serial::write_bytes(b"[stack_pool] WARNING: only allocated ");
                serial::write_u64_dec_inline(allocated as u64);
                serial::write_bytes(b" of ");
                serial::write_u64_dec_inline(target as u64);
                serial::write_line(b" stacks");
                break;
            }
        }
    }

    pool.allocated = allocated;
    pool.initialized = true;

    serial::write_bytes(b"[stack_pool] Allocated ");
    serial::write_u64_dec_inline(allocated as u64);
    serial::write_bytes(b" stacks, ");
    serial::write_u64_dec_inline((allocated * KERNEL_STACK_SIZE / 1024) as u64);
    serial::write_line(b" KiB reserved");

    allocated
}

/// Allocate a contiguous stack region from the frame allocator.
fn alloc_contiguous_stack() -> Option<u64> {
    // Use the allocator's built-in contiguous-run path rather than probing
    // frame-by-frame (which fails when reserved ranges fragment the address
    // space between individual allocations).
    let base = frame_alloc::alloc_contiguous_run(KERNEL_STACK_FRAMES)?;

    // Zero the stack memory.
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, KERNEL_STACK_SIZE);
    }

    Some(base)
}

/// Acquire a stack from the pool.
///
/// # Returns
/// - `Some((base, top))` - Stack base (lowest address) and top (highest address).
/// - `None` - No free stacks available.
pub fn alloc_stack() -> Option<(u64, u64)> {
    let live_rsp = current_rsp();
    let result = {
        let mut pool = POOL.lock();

        if !pool.initialized {
            serial::write_line(b"[stack_pool] ERROR: not initialized");
            return None;
        }

        let allocated = pool.allocated;
        let mut found = None;
        for slot in pool.slots[..allocated].iter_mut() {
            let slot_top = slot.base.saturating_add(KERNEL_STACK_SIZE as u64);
            let is_current_stack = live_rsp >= slot.base && live_rsp < slot_top;
            if !slot.in_use && slot.base != 0 {
                if is_current_stack {
                    continue;
                }
                slot.in_use = true;
                found = Some((slot.base, slot_top));
                break;
            }
        }

        if found.is_none() {
            serial::write_line(b"[stack_pool] WARNING: no free stack (current stack filtered)");
        }
        found
    };

    // Zero the stack *after* releasing POOL.lock().  Zeroing while holding
    // the lock would cause a single-CPU deadlock if a timer-driven context
    // switch gave another task a chance to call alloc_stack() or free_stack()
    // while we held the lock.  The slot is already marked in_use above, so
    // no other allocator can hand it out before we finish zeroing.
    if let Some((base, top)) = result {
        unsafe {
            core::ptr::write_bytes(base as *mut u8, 0, KERNEL_STACK_SIZE);
        }
        Some((base, top))
    } else {
        None
    }
}

/// Return a stack to the pool.
///
/// # Safety
/// The caller must ensure:
/// - `base` was previously returned by `alloc_stack()`.
/// - No references to the stack memory remain.
pub fn free_stack(base: u64) {
    let found = {
        let mut pool = POOL.lock();
        let allocated = pool.allocated;
        let mut found = false;
        for slot in pool.slots[..allocated].iter_mut() {
            if slot.base == base && slot.in_use {
                slot.in_use = false;
                found = true;
                break;
            }
        }
        found
    };

    if !found {
        serial::write_bytes(b"[stack_pool] WARNING: free_stack called with unknown base 0x");
        serial::write_hex(base);
    }
}

/// Returns the number of free stacks available.
pub fn free_count() -> usize {
    let pool = POOL.lock();
    let allocated = pool.allocated;
    pool.slots[..allocated]
        .iter()
        .filter(|s| !s.in_use && s.base != 0)
        .count()
}

/// Returns the total number of allocated stack slots.
pub fn total_count() -> usize {
    POOL.lock().allocated
}
