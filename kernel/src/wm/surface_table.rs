// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Kernel-global surface table.
//!
//! A surface is a pixel buffer whose physical frames are shared between:
//! 1. The **owner** task (a ring-3 app) that draws into it.
//! 2. The **compositor** task (or kernel compositor) that reads from it.
//!
//! ## Lifetime
//! - `alloc_surface()`: allocates physical frames, registers the record.
//! - `free_surface()`:  returns frames to the frame allocator; the VMA entries
//!   in the owner's and compositor's address spaces must be unmapped before
//!   calling this.
//!
//! ## Invariants
//! - Frames in the surface table are NOT tracked by any `AddressSpace` as
//!   owned. They survive address-space teardown. The surface must be
//!   explicitly freed.
//! - `SYS_SURFACE_PRESENT` validates that the calling task's index matches
//!   `owner_task` before accepting a present request.
//! - All mutations are serialised behind `SURFACE_TABLE: Mutex<SurfaceTable>`.

use crate::arch::interrupts;
use spin::Mutex;

use crate::arch::serial;
use crate::mm::frame_alloc;

/// Maximum number of simultaneously live surfaces.
pub const MAX_SURFACES_GLOBAL: usize = 32;

/// Maximum physical frames per surface.
///
/// 4096 frames × 4 KiB = 16 MiB per surface, enough for large desktop windows
/// and multi-buffered frontends without tripping an arbitrary cap.
pub const MAX_SURFACE_FRAMES: usize = 4096;

/// BGRA / xRGB 32-bit pixel format — the only format currently supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra32,
}

/// A globally registered pixel surface.
#[derive(Clone, Copy)]
pub struct SurfaceRecord {
    /// Unique surface identifier (1-based; 0 = empty slot).
    pub id: u32,
    /// Pixel dimensions.
    pub width: u16,
    pub height: u16,
    /// Bytes per row (stride ≥ width × 4 for BGRA32).
    pub stride: u32,
    /// Physical frames backing the pixel buffer.
    frames: [u64; MAX_SURFACE_FRAMES],
    frame_count: usize,
    /// Task table index of the owning ring-3 process.
    pub owner_task: usize,
    /// Pixel format (currently always BGRA32).
    pub format: PixelFormat,
    /// Whether this slot is occupied.
    active: bool,
}

impl SurfaceRecord {
    const EMPTY: Self = Self {
        id: 0,
        width: 0,
        height: 0,
        stride: 0,
        frames: [0u64; MAX_SURFACE_FRAMES],
        frame_count: 0,
        owner_task: 0,
        format: PixelFormat::Bgra32,
        active: false,
    };

    /// Returns the slice of physical frames for this surface.
    pub fn frames(&self) -> &[u64] {
        &self.frames[..self.frame_count]
    }
}

// SAFETY: SurfaceRecord is accessed only through the TABLE Mutex.
unsafe impl Send for SurfaceRecord {}

/// The global surface registry.
struct SurfaceTable {
    records: [SurfaceRecord; MAX_SURFACES_GLOBAL],
    next_id: u32,
}

impl SurfaceTable {
    const fn new() -> Self {
        Self {
            records: [SurfaceRecord::EMPTY; MAX_SURFACES_GLOBAL],
            next_id: 1,
        }
    }
}

static TABLE: Mutex<SurfaceTable> = Mutex::new(SurfaceTable::new());

// ---------------------------------------------------------------------------
// Present queue
// ---------------------------------------------------------------------------

/// Capacity of the present queue ring.
const PRESENT_QUEUE_CAP: usize = 128;

/// A simple lock-free-read ring queue for surface_id pending presentation.
struct PresentQueue {
    ring: [u32; PRESENT_QUEUE_CAP],
    head: usize,
    tail: usize,
    count: usize,
}

impl PresentQueue {
    const fn new() -> Self {
        Self {
            ring: [0u32; PRESENT_QUEUE_CAP],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    fn push(&mut self, id: u32) -> bool {
        // Coalesce repeat presents for the same surface while it is already
        // pending. This keeps the queue bounded by dirty surfaces rather than
        // raw frame rate from a single producer.
        if self.contains(id) {
            return true;
        }
        if self.count >= PRESENT_QUEUE_CAP {
            return false;
        }
        self.ring[self.tail] = id;
        self.tail = (self.tail + 1) % PRESENT_QUEUE_CAP;
        self.count += 1;
        true
    }

    fn contains(&self, id: u32) -> bool {
        let mut idx = self.head;
        for _ in 0..self.count {
            if self.ring[idx] == id {
                return true;
            }
            idx = (idx + 1) % PRESENT_QUEUE_CAP;
        }
        false
    }

    fn pop(&mut self) -> Option<u32> {
        if self.count == 0 {
            return None;
        }
        let id = self.ring[self.head];
        self.head = (self.head + 1) % PRESENT_QUEUE_CAP;
        self.count -= 1;
        Some(id)
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }
}

static PRESENT_QUEUE: Mutex<PresentQueue> = Mutex::new(PresentQueue::new());

/// Error returned when a present-queue operation fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentError {
    /// The present queue is full; the frame was not queued.
    QueueFull,
}

/// Push a surface ID onto the present queue.
///
/// Returns `Ok(())` on success, `Err(PresentError::QueueFull)` if the queue
/// has no capacity.  The caller (syscall handler) must propagate this error
/// to ring-3 so the app can back-pressure or drop client-side.
pub fn present_queue_push(surface_id: u32) -> Result<(), PresentError> {
    interrupts::without_interrupts(|| {
        let mut q = PRESENT_QUEUE.lock();
        if !q.push(surface_id) {
            serial::write_line(b"[surface] present queue full");
            Err(PresentError::QueueFull)
        } else {
            Ok(())
        }
    })
}

/// Returns the maximum number of surfaces the present queue can hold.
pub const fn present_queue_capacity() -> usize {
    PRESENT_QUEUE_CAP
}

/// Pop the next surface ID from the present queue.
///
/// Called from the kernel compositor render path.
pub fn present_queue_pop() -> Option<u32> {
    interrupts::without_interrupts(|| PRESENT_QUEUE.lock().pop())
}

/// Returns true if the present queue is non-empty.
pub fn present_queue_pending() -> bool {
    interrupts::without_interrupts(|| !PRESENT_QUEUE.lock().is_empty())
}

// ---------------------------------------------------------------------------
// Surface allocation
// ---------------------------------------------------------------------------

/// Allocation error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceError {
    /// No free surface slot.
    TableFull,
    /// Requested dimensions are zero or exceed the frame limit.
    DimensionsTooLarge,
    /// Physical frame allocator exhausted.
    OutOfMemory,
}

/// Allocate a new pixel surface.
///
/// Allocates `ceil(width × height × 4 / 4096)` physical frames from the
/// frame allocator and registers a `SurfaceRecord`. Returns `(surface_id,
/// frame_slice)` on success. The caller must immediately map the frames
/// into the owner task's address space before returning to userspace.
pub fn alloc_surface(width: u16, height: u16, owner_task: usize) -> Result<u32, SurfaceError> {
    if width == 0 || height == 0 {
        return Err(SurfaceError::DimensionsTooLarge);
    }
    // Bytes needed: width × height × 4 (BGRA32), rounded up to page boundary.
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or(SurfaceError::DimensionsTooLarge)?;
    let frame_count = bytes.div_ceil(4096);
    if frame_count > MAX_SURFACE_FRAMES {
        return Err(SurfaceError::DimensionsTooLarge);
    }

    interrupts::without_interrupts(|| {
        let mut tbl = TABLE.lock();

        // Find a free slot.
        let slot_idx = tbl
            .records
            .iter()
            .position(|r| !r.active)
            .ok_or(SurfaceError::TableFull)?;

        // Allocate frames.
        let mut frames = [0u64; MAX_SURFACE_FRAMES];
        let mut allocated = 0usize;
        for i in 0..frame_count {
            match frame_alloc::alloc_frame() {
                Some(f) => {
                    // Zero-fill the frame so the surface starts blank.
                    unsafe {
                        core::ptr::write_bytes(f as *mut u8, 0, 4096);
                    }
                    frames[i] = f;
                    allocated += 1;
                }
                None => {
                    // Rollback already-allocated frames.
                    for frame in frames.iter().take(allocated) {
                        frame_alloc::dealloc_frame(*frame);
                    }
                    return Err(SurfaceError::OutOfMemory);
                }
            }
        }

        let id = tbl.next_id;
        tbl.next_id = tbl.next_id.wrapping_add(1).max(1); // never 0

        let stride = (width as u32) * 4;
        let rec = &mut tbl.records[slot_idx];
        rec.id = id;
        rec.width = width;
        rec.height = height;
        rec.stride = stride;
        rec.frames = frames;
        rec.frame_count = frame_count;
        rec.owner_task = owner_task;
        rec.format = PixelFormat::Bgra32;
        rec.active = true;

        serial::write_bytes(b"[surface] allocated id=");
        serial::write_u64_dec_inline(id as u64);
        serial::write_bytes(b" dim=");
        serial::write_u64_dec_inline(width as u64);
        serial::write_bytes(b"x");
        serial::write_u64_dec_inline(height as u64);
        serial::write_bytes(b" frames=");
        serial::write_u64_dec(frame_count as u64);

        Ok(id)
    })
}

/// Free a surface and return its physical frames to the allocator.
///
/// # Precondition
/// The caller must have already unmapped the surface from all address spaces
/// that imported it. Failure to do so results in dangling mappings.
pub fn free_surface(id: u32) -> bool {
    interrupts::without_interrupts(|| {
        let mut tbl = TABLE.lock();
        for rec in tbl.records.iter_mut() {
            if rec.active && rec.id == id {
                for i in 0..rec.frame_count {
                    frame_alloc::dealloc_frame(rec.frames[i]);
                    rec.frames[i] = 0;
                }
                *rec = SurfaceRecord::EMPTY;
                serial::write_bytes(b"[surface] freed id=");
                serial::write_u64_dec(id as u64);
                return true;
            }
        }
        false
    })
}

/// Look up the physical frames for a surface.
///
/// Copies the frame list into `out_frames` (must be at least
/// `MAX_SURFACE_FRAMES` entries). Returns the frame count, or 0 if not found.
pub fn surface_frames(id: u32, out_frames: &mut [u64; MAX_SURFACE_FRAMES]) -> usize {
    interrupts::without_interrupts(|| {
        let tbl = TABLE.lock();
        for rec in tbl.records.iter() {
            if rec.active && rec.id == id {
                out_frames[..rec.frame_count].copy_from_slice(&rec.frames[..rec.frame_count]);
                return rec.frame_count;
            }
        }
        0
    })
}

/// Return the physical address of the first backing page for a surface, or
/// `None` if the surface is not found or has no backing pages.
///
/// Used by `SYS_GPU_SURFACE_IMPORT` to attach a surface backing page to a GPU
/// resource for zero-copy texture access.
pub fn surface_phys_addr(id: u32) -> Option<u64> {
    interrupts::without_interrupts(|| {
        let tbl = TABLE.lock();
        for rec in tbl.records.iter() {
            if rec.active && rec.id == id && rec.frame_count > 0 && rec.frames[0] != 0 {
                return Some(rec.frames[0]);
            }
        }
        None
    })
}

/// Return the owner task index for a surface, or `None` if not found.
pub fn surface_owner(id: u32) -> Option<usize> {
    interrupts::without_interrupts(|| {
        let tbl = TABLE.lock();
        for rec in tbl.records.iter() {
            if rec.active && rec.id == id {
                return Some(rec.owner_task);
            }
        }
        None
    })
}

/// Return the dimensions of an active surface.
pub fn surface_dimensions(id: u32) -> Option<(u16, u16)> {
    interrupts::without_interrupts(|| {
        let tbl = TABLE.lock();
        for rec in tbl.records.iter() {
            if rec.active && rec.id == id {
                return Some((rec.width, rec.height));
            }
        }
        None
    })
}

/// Return whether a surface ID is currently active.
pub fn surface_exists(id: u32) -> bool {
    interrupts::without_interrupts(|| TABLE.lock().records.iter().any(|r| r.active && r.id == id))
}
