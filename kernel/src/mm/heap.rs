// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Contiguous heap allocator used during early kernel bring-up.
//!
//! The allocator keeps the current design constraint that heap pages must be
//! physically contiguous under the identity map, but it now supports real
//! reclamation for small allocations instead of leaking everything forever.
//!
//! Design:
//! - Small allocations (up to 4 KiB, power-of-two aligned) are served from
//!   fixed-size page-backed free lists.
//! - Larger allocations are page-granular and reclaimed through a fixed-size
//!   free-range table.
//! - This is intentionally an MVP allocator, not a fully general-purpose VM
//!   heap. It gives the kernel reusable boxes, strings, and metadata tables
//!   without pretending we already have slabs or paging-backed arenas.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use spin::Mutex;

use crate::arch::serial;
use crate::mm::frame_alloc;

const HEAP_MAX_PAGES: usize = 16384;
const PAGE_SIZE: usize = 4096;
const INITIAL_HEAP_PAGES: usize = 8192;
const MIN_BOOTSTRAP_HEAP_PAGES: usize = 128;
const SMALL_BLOCK_SIZES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const MAX_LARGE_FREE_RANGES: usize = 128;

#[repr(C)]
struct FreeNode {
    next: *mut FreeNode,
}

#[derive(Clone, Copy)]
struct FreeRange {
    offset: usize,
    len: usize,
}

impl FreeRange {
    const EMPTY: Self = Self { offset: 0, len: 0 };

    const fn end(self) -> usize {
        self.offset + self.len
    }
}

#[derive(Clone, Copy)]
pub struct HeapStats {
    pub capacity: usize,
    pub cursor: usize,
    pub pages_allocated: usize,
    pub small_pool_pages: usize,
    pub small_reuse_hits: usize,
    pub large_reuse_hits: usize,
    pub free_small_blocks: usize,
    pub free_large_bytes: usize,
}

struct HeapState {
    /// Base of the contiguous heap region (identity-mapped physical addr).
    base: usize,
    /// Current bump cursor measured from `base`.
    next: usize,
    /// Total contiguous bytes currently backed.
    capacity: usize,
    /// Number of pages in the contiguous heap.
    pages_allocated: usize,
    /// Per-size-class intrusive free lists.
    free_lists: [usize; SMALL_BLOCK_SIZES.len()],
    /// Pages dedicated to the small-block pools.
    small_pool_pages: usize,
    /// Number of allocations satisfied from an existing free-list node.
    small_reuse_hits: usize,
    /// Reclaimable page-granular free ranges for larger allocations.
    large_free_ranges: [FreeRange; MAX_LARGE_FREE_RANGES],
    large_free_count: usize,
    /// Number of larger allocations satisfied from a reclaimed range.
    large_reuse_hits: usize,
    /// Bytes currently reclaimable from large-allocation frees.
    free_large_bytes: usize,
}

static HEAP: Mutex<HeapState> = Mutex::new(HeapState {
    base: 0,
    next: 0,
    capacity: 0,
    pages_allocated: 0,
    free_lists: [0; SMALL_BLOCK_SIZES.len()],
    small_pool_pages: 0,
    small_reuse_hits: 0,
    large_free_ranges: [FreeRange::EMPTY; MAX_LARGE_FREE_RANGES],
    large_free_count: 0,
    large_reuse_hits: 0,
    free_large_bytes: 0,
});

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

fn grow_one_page(state: &mut HeapState) -> bool {
    if state.pages_allocated >= HEAP_MAX_PAGES {
        return false;
    }

    if state.pages_allocated == 0 {
        let Some(base) = frame_alloc::alloc_contiguous_run(1) else {
            return false;
        };
        state.base = base as usize;
    } else if !frame_alloc::claim_frame((state.base + state.pages_allocated * PAGE_SIZE) as u64) {
        return false;
    }

    // SAFETY: The heap uses identity-mapped frames; we just obtained this page
    // from the frame allocator and own it exclusively.
    unsafe {
        ptr::write_bytes(
            (state.base + (state.pages_allocated * PAGE_SIZE)) as *mut u8,
            0,
            PAGE_SIZE,
        )
    };

    state.pages_allocated += 1;
    state.capacity = state.pages_allocated * PAGE_SIZE;
    true
}

fn ensure_capacity(state: &mut HeapState, end: usize) -> bool {
    while end > state.capacity {
        if !grow_one_page(state) {
            return false;
        }
    }
    true
}

fn small_class_index(layout: Layout) -> Option<usize> {
    let need = layout
        .size()
        .max(layout.align())
        .max(core::mem::size_of::<FreeNode>());
    SMALL_BLOCK_SIZES.iter().position(|&size| size >= need)
}

unsafe fn push_free_block(state: &mut HeapState, class_idx: usize, block: *mut u8) {
    let node = block as *mut FreeNode;
    unsafe {
        (*node).next = state.free_lists[class_idx] as *mut FreeNode;
    }
    state.free_lists[class_idx] = node as usize;
}

fn refill_small_class(state: &mut HeapState, class_idx: usize) -> bool {
    let page_start = align_up(state.next, PAGE_SIZE);
    let page_end = page_start.saturating_add(PAGE_SIZE);
    if !ensure_capacity(state, page_end) {
        return false;
    }

    state.next = page_end;
    state.small_pool_pages += 1;

    let block_size = SMALL_BLOCK_SIZES[class_idx];
    let blocks = PAGE_SIZE / block_size;
    let base = state.base + page_start;

    for i in 0..blocks {
        let block = (base + i * block_size) as *mut u8;
        // SAFETY: This freshly-carved page belongs exclusively to the heap and
        // is subdivided into block-size-aligned slots.
        unsafe { push_free_block(state, class_idx, block) };
    }

    true
}

unsafe fn alloc_small(state: &mut HeapState, class_idx: usize) -> *mut u8 {
    let reused = state.free_lists[class_idx] != 0;
    if state.free_lists[class_idx] == 0 && !refill_small_class(state, class_idx) {
        return ptr::null_mut();
    }

    let node = state.free_lists[class_idx] as *mut FreeNode;
    if node.is_null() {
        return ptr::null_mut();
    }

    state.free_lists[class_idx] = unsafe { (*node).next as usize };
    if reused {
        state.small_reuse_hits += 1;
    }
    node as *mut u8
}

fn alloc_bump(state: &mut HeapState, layout: Layout) -> *mut u8 {
    let aligned = align_up(state.next, layout.align());
    let end = aligned.saturating_add(layout.size());
    if !ensure_capacity(state, end) {
        return ptr::null_mut();
    }

    let out = (state.base + aligned) as *mut u8;
    state.next = end;
    out
}

fn large_layout_size(layout: Layout) -> usize {
    align_up(layout.size().max(1), PAGE_SIZE)
}

fn large_layout_align(layout: Layout) -> usize {
    layout.align().max(PAGE_SIZE)
}

fn remove_large_range(state: &mut HeapState, idx: usize) {
    if idx >= state.large_free_count {
        return;
    }

    let removed_len = state.large_free_ranges[idx].len;
    for i in idx..state.large_free_count.saturating_sub(1) {
        state.large_free_ranges[i] = state.large_free_ranges[i + 1];
    }
    if state.large_free_count > 0 {
        state.large_free_count -= 1;
        state.large_free_ranges[state.large_free_count] = FreeRange::EMPTY;
    }
    state.free_large_bytes = state.free_large_bytes.saturating_sub(removed_len);
}

fn coalesce_large_ranges(state: &mut HeapState, idx: usize) {
    let mut cursor = idx;
    while cursor > 0 {
        let left = state.large_free_ranges[cursor - 1];
        let current = state.large_free_ranges[cursor];
        if left.end() < current.offset {
            break;
        }
        state.large_free_ranges[cursor - 1].len = left.end().max(current.end()) - left.offset;
        remove_large_range(state, cursor);
        cursor -= 1;
        state.free_large_bytes += state.large_free_ranges[cursor].len.saturating_sub(left.len);
    }

    while cursor + 1 < state.large_free_count {
        let current = state.large_free_ranges[cursor];
        let right = state.large_free_ranges[cursor + 1];
        if current.end() < right.offset {
            break;
        }
        let merged_len = current.end().max(right.end()) - current.offset;
        state.large_free_ranges[cursor].len = merged_len;
        remove_large_range(state, cursor + 1);
        state.free_large_bytes += merged_len.saturating_sub(current.len);
    }
}

fn insert_large_range(state: &mut HeapState, range: FreeRange) -> bool {
    if range.len == 0 {
        return true;
    }
    if state.large_free_count >= MAX_LARGE_FREE_RANGES {
        return false;
    }

    let mut insert_at = 0usize;
    while insert_at < state.large_free_count
        && state.large_free_ranges[insert_at].offset < range.offset
    {
        insert_at += 1;
    }

    for i in (insert_at..state.large_free_count).rev() {
        state.large_free_ranges[i + 1] = state.large_free_ranges[i];
    }
    state.large_free_ranges[insert_at] = range;
    state.large_free_count += 1;
    state.free_large_bytes += range.len;
    coalesce_large_ranges(state, insert_at);
    true
}

fn alloc_large_from_free_ranges(state: &mut HeapState, layout: Layout) -> *mut u8 {
    let need = large_layout_size(layout);
    let align = large_layout_align(layout);

    for idx in 0..state.large_free_count {
        let range = state.large_free_ranges[idx];
        let aligned_offset = align_up(range.offset, align);
        let padding = aligned_offset.saturating_sub(range.offset);
        if padding > range.len {
            continue;
        }
        let usable = range.len - padding;
        if usable < need {
            continue;
        }

        remove_large_range(state, idx);

        let head = FreeRange {
            offset: range.offset,
            len: padding,
        };
        let tail_end = aligned_offset + need;
        let tail = FreeRange {
            offset: tail_end,
            len: range.end().saturating_sub(tail_end),
        };

        let _ = insert_large_range(state, head);
        let _ = insert_large_range(state, tail);
        state.large_reuse_hits += 1;

        return (state.base + aligned_offset) as *mut u8;
    }

    ptr::null_mut()
}

fn alloc_large(state: &mut HeapState, layout: Layout) -> *mut u8 {
    let reused = alloc_large_from_free_ranges(state, layout);
    if !reused.is_null() {
        return reused;
    }

    let aligned = align_up(state.next, large_layout_align(layout));
    let end = aligned.saturating_add(large_layout_size(layout));
    if !ensure_capacity(state, end) {
        return ptr::null_mut();
    }

    let out = (state.base + aligned) as *mut u8;
    state.next = end;
    out
}

pub fn init() {
    let mut state = HEAP.lock();
    let mut requested_pages = HEAP_MAX_PAGES;
    while requested_pages >= MIN_BOOTSTRAP_HEAP_PAGES {
        if let Some(base) = frame_alloc::alloc_contiguous_run(requested_pages) {
            state.base = base as usize;
            state.pages_allocated = requested_pages;
            state.capacity = requested_pages * PAGE_SIZE;
            // SAFETY: The frame allocator handed us an exclusive contiguous run.
            unsafe { ptr::write_bytes(state.base as *mut u8, 0, state.capacity) };
            break;
        }
        requested_pages = requested_pages.saturating_sub(128);
    }

    if state.base == 0 {
        serial::write_line(b"[heap] ERROR: no contiguous bootstrap heap run available");
        return;
    }

    if state.pages_allocated < INITIAL_HEAP_PAGES {
        serial::write_bytes(b"[heap] WARNING: bootstrap heap below target pages=");
        serial::write_u64_dec(state.pages_allocated as u64);
    }

    serial::write_bytes(b"[heap] mvp allocator: ");
    serial::write_u64_dec_inline(state.capacity as u64);
    serial::write_bytes(b" bytes base=");
    serial::write_hex_inline(state.base as u64);
    serial::write_bytes(b" small-classes=");
    serial::write_u64_dec(SMALL_BLOCK_SIZES.len() as u64);
}

pub fn stats() -> HeapStats {
    let state = HEAP.lock();
    let mut free_small_blocks = 0usize;
    for &head in state.free_lists.iter() {
        let mut cursor = head as *mut FreeNode;
        while !cursor.is_null() {
            free_small_blocks += 1;
            // SAFETY: Every node on the list was inserted by push_free_block.
            cursor = unsafe { (*cursor).next };
        }
    }

    HeapStats {
        capacity: state.capacity,
        cursor: state.next,
        pages_allocated: state.pages_allocated,
        small_pool_pages: state.small_pool_pages,
        small_reuse_hits: state.small_reuse_hits,
        large_reuse_hits: state.large_reuse_hits,
        free_small_blocks,
        free_large_bytes: state.free_large_bytes,
    }
}

struct MvpAllocator;

unsafe impl GlobalAlloc for MvpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut state = HEAP.lock();
        if state.base == 0 {
            return ptr::null_mut();
        }

        match small_class_index(layout) {
            Some(class_idx) => unsafe { alloc_small(&mut state, class_idx) },
            None => alloc_large(&mut state, layout),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let mut state = HEAP.lock();
        if state.base == 0 {
            return;
        }

        if let Some(class_idx) = small_class_index(layout) {
            unsafe { push_free_block(&mut state, class_idx, ptr) };
            return;
        }

        let base = state.base;
        let ptr_addr = ptr as usize;
        if ptr_addr < base {
            return;
        }

        let range = FreeRange {
            offset: ptr_addr - base,
            len: large_layout_size(layout),
        };
        if !insert_large_range(&mut state, range) {
            serial::write_line(b"[heap] WARNING: large free-range table full, block leaked");
        }
    }
}

#[global_allocator]
static ALLOCATOR: MvpAllocator = MvpAllocator;
