// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Physical frame allocator.
//!
//! Tracks free physical RAM as merged 4 KiB-aligned segments rather than a
//! fixed per-frame table. This keeps all usable RAM visible to the kernel even
//! when the machine has more than 128 MiB, while still supporting contiguous
//! allocation and specific-frame claims during bootstrap.

use crate::arch::serial;
use crate::mm::phys;
use crate::mm::reserved;
use spin::Mutex;

pub const FRAME_SIZE: u64 = 4096;
const MAX_FREE_SEGMENTS: usize = 1024;

#[derive(Clone, Copy)]
struct FreeSegment {
    start: u64,
    end: u64,
}

impl FreeSegment {
    const EMPTY: Self = Self { start: 0, end: 0 };

    fn bytes(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    fn frames(self) -> usize {
        (self.bytes() / FRAME_SIZE) as usize
    }

    fn contains_frame(self, frame: u64) -> bool {
        frame >= self.start && frame.saturating_add(FRAME_SIZE) <= self.end
    }
}

struct FramePool {
    segments: [FreeSegment; MAX_FREE_SEGMENTS],
    segment_count: usize,
    total_frames: usize,
    free_frames: usize,
    skipped: usize,
    high_water: usize,
}

impl FramePool {
    const fn new() -> Self {
        Self {
            segments: [FreeSegment::EMPTY; MAX_FREE_SEGMENTS],
            segment_count: 0,
            total_frames: 0,
            free_frames: 0,
            skipped: 0,
            high_water: 0,
        }
    }

    fn allocated_frames(&self) -> usize {
        self.total_frames.saturating_sub(self.free_frames)
    }

    fn note_allocation(&mut self) -> usize {
        let allocated = self.allocated_frames();
        if allocated > self.high_water {
            self.high_water = allocated;
        }
        allocated
    }
}

static POOL: Mutex<FramePool> = Mutex::new(FramePool::new());

/// Populate the free-segment table from usable physical memory regions,
/// skipping any frame that overlaps a reserved range.
pub fn init() {
    {
        let mut pool = POOL.lock();
        *pool = FramePool::new();

        phys::with_usable_regions(|start, length| {
            let mut addr = align_up(start, FRAME_SIZE);
            let end = start.saturating_add(length);
            let mut run_start = 0u64;
            let mut run_end = 0u64;

            while addr.saturating_add(FRAME_SIZE) <= end {
                let is_reserved = unsafe { reserved::is_reserved(addr) };
                if is_reserved {
                    pool.skipped += 1;
                    if run_start != run_end {
                        if !insert_free_segment(&mut pool, run_start, run_end) {
                            serial::write_line(
                                b"[frame_alloc] WARNING: free segment table full during init",
                            );
                        }
                        run_start = 0;
                        run_end = 0;
                    }
                } else {
                    if run_start == run_end {
                        run_start = addr;
                    }
                    run_end = addr.saturating_add(FRAME_SIZE);
                }
                addr = addr.saturating_add(FRAME_SIZE);
            }

            if run_start != run_end && !insert_free_segment(&mut pool, run_start, run_end) {
                serial::write_line(b"[frame_alloc] WARNING: free segment table full during init");
            }
        });

        pool.total_frames = total_segment_frames(&pool);
        pool.free_frames = pool.total_frames;
    }

    // SAFETY: Single-threaded early init.
    unsafe { reserved::mark_init_complete() };
}

/// Allocate a single 4 KiB physical frame. Returns `None` if exhausted.
pub fn alloc_frame() -> Option<u64> {
    let (frame, allocated, total) = {
        let mut pool = POOL.lock();
        if pool.segment_count == 0 {
            return None;
        }

        let frame = pool.segments[0].start;
        consume_segment_front(&mut pool, 0, 1);
        let allocated = pool.note_allocation();
        (frame, allocated, pool.total_frames)
    };

    publish_frame_telemetry(allocated, total);
    Some(frame)
}

/// Allocate a physically contiguous run of `pages` 4 KiB frames.
pub fn alloc_contiguous_run(pages: usize) -> Option<u64> {
    if pages == 0 {
        return None;
    }

    let bytes = (pages as u64).checked_mul(FRAME_SIZE)?;
    let result = {
        let mut pool = POOL.lock();
        let mut run = None;
        for idx in 0..pool.segment_count {
            if pool.segments[idx].bytes() < bytes {
                continue;
            }

            let base = pool.segments[idx].start;
            consume_segment_front(&mut pool, idx, pages);
            let allocated = pool.note_allocation();
            run = Some((base, allocated, pool.total_frames));
            break;
        }
        run
    };

    let (base, allocated, total) = result?;
    publish_frame_telemetry(allocated, total);
    Some(base)
}

/// Claim a specific frame from the allocator if it is still free.
pub fn claim_frame(frame: u64) -> bool {
    if frame & (FRAME_SIZE - 1) != 0 {
        return false;
    }

    let result = {
        let mut pool = POOL.lock();
        let mut removed = false;

        for idx in 0..pool.segment_count {
            let segment = pool.segments[idx];
            if !segment.contains_frame(frame) {
                continue;
            }

            if segment.start == frame && segment.end == frame.saturating_add(FRAME_SIZE) {
                remove_segment(&mut pool, idx);
            } else if segment.start == frame {
                pool.segments[idx].start = pool.segments[idx].start.saturating_add(FRAME_SIZE);
            } else if segment.end == frame.saturating_add(FRAME_SIZE) {
                pool.segments[idx].end = pool.segments[idx].end.saturating_sub(FRAME_SIZE);
            } else {
                if pool.segment_count >= MAX_FREE_SEGMENTS {
                    serial::write_line(
                        b"[frame_alloc] WARNING: free segment table full during claim_frame",
                    );
                    return false;
                }
                let tail = FreeSegment {
                    start: frame.saturating_add(FRAME_SIZE),
                    end: segment.end,
                };
                pool.segments[idx].end = frame;
                insert_segment_at(&mut pool, idx + 1, tail);
            }

            pool.free_frames = pool.free_frames.saturating_sub(1);
            removed = true;
            break;
        }

        if removed {
            let allocated = pool.note_allocation();
            Some((allocated, pool.total_frames))
        } else {
            None
        }
    };

    if let Some((allocated, total)) = result {
        publish_frame_telemetry(allocated, total);
        true
    } else {
        false
    }
}

/// Return a frame to the allocator, coalescing with adjacent free segments.
pub fn dealloc_frame(frame: u64) -> bool {
    if frame & (FRAME_SIZE - 1) != 0 {
        return false;
    }

    let range_end = frame.saturating_add(FRAME_SIZE);
    let result = {
        let mut pool = POOL.lock();
        if pool.free_frames >= pool.total_frames {
            serial::write_line(b"[frame_alloc] WARNING: dealloc on full pool ignored");
            return false;
        }

        for idx in 0..pool.segment_count {
            let segment = pool.segments[idx];
            if frame < segment.end && segment.start < range_end {
                serial::write_line(b"[frame_alloc] WARNING: duplicate free frame ignored");
                return false;
            }
        }

        let is_reserved = unsafe { reserved::is_reserved(frame) };
        if is_reserved {
            serial::write_line(b"[frame_alloc] WARNING: reserved frame returned to allocator");
            return false;
        }

        if !insert_free_segment(&mut pool, frame, range_end) {
            serial::write_line(b"[frame_alloc] WARNING: free segment table full, frame leaked");
            return false;
        }

        pool.free_frames = pool.free_frames.saturating_add(1);
        Some((pool.allocated_frames(), pool.total_frames))
    };

    if let Some((allocated, total)) = result {
        publish_frame_telemetry(allocated, total);
        true
    } else {
        false
    }
}

/// Log a summary of the frame allocator state to serial.
pub fn log_summary() {
    let pool = POOL.lock();
    serial::write_bytes(b"[frame_alloc] tracked segments:    ");
    serial::write_u64_dec(pool.segment_count as u64);
    serial::write_bytes(b"[frame_alloc] total managed frames:");
    serial::write_u64_dec(pool.total_frames as u64);
    serial::write_bytes(b"[frame_alloc] allocated now:       ");
    serial::write_u64_dec(pool.allocated_frames() as u64);
    serial::write_bytes(b"[frame_alloc] remaining:           ");
    serial::write_u64_dec(pool.free_frames as u64);
    serial::write_bytes(b"[frame_alloc] high-water mark:     ");
    serial::write_u64_dec(pool.high_water as u64);
    serial::write_bytes(b"[frame_alloc] skipped (reserved):  ");
    serial::write_u64_dec(pool.skipped as u64);
    serial::write_bytes(b"[frame_alloc] free pool:           ~");
    serial::write_u64_dec_inline((pool.free_frames as u64 * FRAME_SIZE) / (1024 * 1024));
    serial::write_line(b" MiB");
}

/// Return the number of frames currently available.
pub fn available_frames() -> usize {
    POOL.lock().free_frames
}

/// Return the number of frames currently allocated.
pub fn allocated_count() -> usize {
    POOL.lock().allocated_frames()
}

fn publish_frame_telemetry(allocated: usize, total: usize) {
    crate::graph::twin::ingest_frame_event(crate::arch::timer::ticks(), allocated, total);
}

fn consume_segment_front(pool: &mut FramePool, idx: usize, pages: usize) {
    let bytes = (pages as u64).saturating_mul(FRAME_SIZE);
    pool.segments[idx].start = pool.segments[idx].start.saturating_add(bytes);
    pool.free_frames = pool.free_frames.saturating_sub(pages);
    if pool.segments[idx].start >= pool.segments[idx].end {
        remove_segment(pool, idx);
    }
}

fn insert_free_segment(pool: &mut FramePool, start: u64, end: u64) -> bool {
    if start >= end {
        return true;
    }

    let mut insert_at = 0usize;
    while insert_at < pool.segment_count && pool.segments[insert_at].start < start {
        insert_at += 1;
    }

    let mut merge_idx = insert_at;
    if insert_at > 0 && pool.segments[insert_at - 1].end >= start {
        merge_idx = insert_at - 1;
        if end > pool.segments[merge_idx].end {
            pool.segments[merge_idx].end = end;
        }
    } else {
        if pool.segment_count >= MAX_FREE_SEGMENTS {
            return false;
        }
        insert_segment_at(pool, insert_at, FreeSegment { start, end });
    }

    while merge_idx + 1 < pool.segment_count
        && pool.segments[merge_idx + 1].start <= pool.segments[merge_idx].end
    {
        let next_end = pool.segments[merge_idx + 1].end;
        if next_end > pool.segments[merge_idx].end {
            pool.segments[merge_idx].end = next_end;
        }
        remove_segment(pool, merge_idx + 1);
    }
    true
}

fn insert_segment_at(pool: &mut FramePool, idx: usize, segment: FreeSegment) {
    for pos in (idx..pool.segment_count).rev() {
        pool.segments[pos + 1] = pool.segments[pos];
    }
    pool.segments[idx] = segment;
    pool.segment_count += 1;
}

fn remove_segment(pool: &mut FramePool, idx: usize) {
    if idx >= pool.segment_count {
        return;
    }
    for pos in idx + 1..pool.segment_count {
        pool.segments[pos - 1] = pool.segments[pos];
    }
    pool.segment_count -= 1;
    pool.segments[pool.segment_count] = FreeSegment::EMPTY;
}

fn total_segment_frames(pool: &FramePool) -> usize {
    let mut total = 0usize;
    for idx in 0..pool.segment_count {
        total = total.saturating_add(pool.segments[idx].frames());
    }
    total
}

#[inline]
fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}
