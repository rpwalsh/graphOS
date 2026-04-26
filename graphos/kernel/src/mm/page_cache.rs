// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal file-backed page cache for userspace mappings.
//!
//! This is intentionally fixed-size and boring, but it gives GraphOS a real
//! shared page source for file-backed mappings instead of treating every fault
//! as a private memcpy forever.

use spin::Mutex;

use crate::mm::frame_alloc;

const PAGE_SIZE: usize = 4096;
const MAX_CACHE_PAGES: usize = 64;
const MAX_PATH_LEN: usize = 63;

#[derive(Clone, Copy)]
struct CacheEntry {
    path: [u8; MAX_PATH_LEN + 1],
    path_len: usize,
    page_index: u64,
    frame: u64,
    valid_len: usize,
    refs: u16,
    active: bool,
}

impl CacheEntry {
    const EMPTY: Self = Self {
        path: [0; MAX_PATH_LEN + 1],
        path_len: 0,
        page_index: 0,
        frame: 0,
        valid_len: 0,
        refs: 0,
        active: false,
    };

    fn matches(&self, path: &[u8], page_index: u64) -> bool {
        self.active
            && self.page_index == page_index
            && self.path_len == path.len()
            && &self.path[..self.path_len] == path
    }
}

struct PageCacheState {
    entries: [CacheEntry; MAX_CACHE_PAGES],
}

impl PageCacheState {
    const fn new() -> Self {
        Self {
            entries: [CacheEntry::EMPTY; MAX_CACHE_PAGES],
        }
    }
}

static PAGE_CACHE: Mutex<PageCacheState> = Mutex::new(PageCacheState::new());

pub fn acquire(path: &[u8], page_index: u64) -> Option<(u64, usize)> {
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return None;
    }

    let mut state = PAGE_CACHE.lock();
    for entry in state.entries.iter_mut() {
        if entry.matches(path, page_index) {
            entry.refs = entry.refs.saturating_add(1);
            return Some((entry.frame, entry.valid_len));
        }
    }

    let slot_idx = state
        .entries
        .iter()
        .position(|entry| !entry.active || entry.refs == 0)?;

    if state.entries[slot_idx].active && state.entries[slot_idx].frame != 0 {
        frame_alloc::dealloc_frame(state.entries[slot_idx].frame);
    }

    let frame = frame_alloc::alloc_frame()?;
    unsafe {
        core::ptr::write_bytes(frame as *mut u8, 0, PAGE_SIZE);
    }

    let mut scratch = [0u8; PAGE_SIZE];
    let offset = page_index.checked_mul(PAGE_SIZE as u64)?;
    let valid_len = crate::vfs::read_at(path, offset, &mut scratch).ok()?;
    unsafe {
        core::ptr::copy_nonoverlapping(scratch.as_ptr(), frame as *mut u8, valid_len);
    }

    let entry = &mut state.entries[slot_idx];
    entry.path[..path.len()].copy_from_slice(path);
    entry.path_len = path.len();
    entry.page_index = page_index;
    entry.frame = frame;
    entry.valid_len = valid_len;
    entry.refs = 1;
    entry.active = true;
    Some((frame, valid_len))
}

pub fn release_frame(frame: u64) {
    let mut state = PAGE_CACHE.lock();
    for entry in state.entries.iter_mut() {
        if entry.active && entry.frame == frame {
            if entry.refs > 0 {
                entry.refs -= 1;
            }
            if entry.refs == 0 {
                frame_alloc::dealloc_frame(entry.frame);
                *entry = CacheEntry::EMPTY;
            }
            return;
        }
    }
}

pub fn contains_frame(frame: u64) -> bool {
    let state = PAGE_CACHE.lock();
    state
        .entries
        .iter()
        .any(|entry| entry.active && entry.frame == frame)
}
