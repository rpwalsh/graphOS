// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Fixed-size swap staging area for evicted anonymous pages.
//!
//! This is RAM-backed for now because GraphOS still lacks a real block driver,
//! but it gives the VM layer an honest eviction/reload path instead of simply
//! failing the next allocation under pressure.

use spin::Mutex;

const PAGE_SIZE: usize = 4096;
const MAX_SWAP_SLOTS: usize = 128;

struct SwapState {
    active: [bool; MAX_SWAP_SLOTS],
    bytes: [[u8; PAGE_SIZE]; MAX_SWAP_SLOTS],
}

impl SwapState {
    const fn new() -> Self {
        Self {
            active: [false; MAX_SWAP_SLOTS],
            bytes: [[0; PAGE_SIZE]; MAX_SWAP_SLOTS],
        }
    }
}

static SWAP: Mutex<SwapState> = Mutex::new(SwapState::new());

pub fn swap_out(frame: u64) -> Option<u16> {
    let mut state = SWAP.lock();
    let slot = state.active.iter().position(|used| !*used)?;
    let src = unsafe { core::slice::from_raw_parts(frame as *const u8, PAGE_SIZE) };
    state.bytes[slot].copy_from_slice(src);
    state.active[slot] = true;
    Some(slot as u16)
}

pub fn swap_in(slot: u16, frame: u64) -> bool {
    let mut state = SWAP.lock();
    let idx = slot as usize;
    if idx >= MAX_SWAP_SLOTS || !state.active[idx] {
        return false;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(state.bytes[idx].as_ptr(), frame as *mut u8, PAGE_SIZE);
    }
    state.active[idx] = false;
    true
}

pub fn discard(slot: u16) {
    let mut state = SWAP.lock();
    let idx = slot as usize;
    if idx < MAX_SWAP_SLOTS {
        state.active[idx] = false;
    }
}

pub fn active_slots() -> usize {
    let state = SWAP.lock();
    state.active.iter().filter(|used| **used).count()
}
