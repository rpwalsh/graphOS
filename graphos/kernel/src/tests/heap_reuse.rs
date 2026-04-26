// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Regression tests for the MVP heap allocator.

extern crate alloc;

use alloc::alloc::{alloc, dealloc};
use core::alloc::Layout;

use crate::diag;

pub fn run_tests() -> u32 {
    let mut failures = 0;

    if !test_small_block_reuse() {
        failures += 1;
    }
    if !test_large_block_reuse() {
        failures += 1;
    }

    failures
}

fn test_small_block_reuse() -> bool {
    let layout = unsafe { Layout::from_size_align_unchecked(64, 8) };

    let first = unsafe { alloc(layout) };
    if first.is_null() {
        diag::test_fail(b"heap: first small allocation failed");
        return false;
    }

    unsafe {
        first.write_bytes(0xA5, 64);
        dealloc(first, layout);
    }

    let second = unsafe { alloc(layout) };
    if second.is_null() {
        diag::test_fail(b"heap: second small allocation failed");
        return false;
    }

    let reused = first == second;
    unsafe { dealloc(second, layout) };

    if reused {
        diag::test_pass(b"heap: reusable 64-byte block");
        true
    } else {
        diag::test_fail(b"heap: freed small block was not reused");
        false
    }
}

fn test_large_block_reuse() -> bool {
    let layout = unsafe { Layout::from_size_align_unchecked(12 * 1024, 4096) };

    let first = unsafe { alloc(layout) };
    if first.is_null() {
        diag::test_fail(b"heap: first large allocation failed");
        return false;
    }

    unsafe {
        first.write_bytes(0x5A, layout.size());
        dealloc(first, layout);
    }

    let second = unsafe { alloc(layout) };
    if second.is_null() {
        diag::test_fail(b"heap: second large allocation failed");
        return false;
    }

    let reused = first == second;
    unsafe { dealloc(second, layout) };

    if reused {
        diag::test_pass(b"heap: reusable 12 KiB block");
        true
    } else {
        diag::test_fail(b"heap: freed large block was not reused");
        false
    }
}
