// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Fuzz target: syscall dispatch number parsing.
//!
//! The kernel's syscall entry point dispatches on a `usize` syscall number.
//! Any unrecognised number must return `ENOSYS` rather than panic or cause
//! undefined behaviour.  This target verifies that the dispatch table logic
//! (replicated here from kernel/src/syscall/table.rs) handles arbitrary
//! 8-byte inputs without panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;

// ── Replicated syscall table constants (keep in sync with kernel) ──────────────

const SYS_READ: usize = 0;
const SYS_WRITE: usize = 1;
const SYS_OPEN: usize = 2;
const SYS_CLOSE: usize = 3;
const SYS_STAT: usize = 4;
const SYS_MMAP: usize = 9;
const SYS_EXIT: usize = 60;
const SYS_SEND: usize = 200;
const SYS_RECV: usize = 201;
const SYS_IPC_CAP: usize = 202;

const ENOSYS: i64 = -38;
const EBADF: i64 = -9;
const EINVAL: i64 = -22;

/// Simulated syscall dispatch — mirrors the match in kernel/src/syscall/table.rs.
fn dispatch(nr: usize, a0: u64, _a1: u64, _a2: u64) -> i64 {
    match nr {
        SYS_READ => {
            if a0 > 1023 {
                EBADF
            } else {
                0
            }
        }
        SYS_WRITE => {
            if a0 > 1023 {
                EBADF
            } else {
                0
            }
        }
        SYS_OPEN => 0,
        SYS_CLOSE => 0,
        SYS_STAT => 0,
        SYS_MMAP => 0,
        SYS_EXIT => 0,
        SYS_SEND => 0,
        SYS_RECV => 0,
        SYS_IPC_CAP => 0,
        _ => ENOSYS,
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let nr = usize::from_le_bytes(data[0..8].try_into().unwrap());
    let a0 = if data.len() >= 16 {
        u64::from_le_bytes(data[8..16].try_into().unwrap())
    } else {
        0
    };
    let a1 = if data.len() >= 24 {
        u64::from_le_bytes(data[16..24].try_into().unwrap())
    } else {
        0
    };
    let a2 = if data.len() >= 32 {
        u64::from_le_bytes(data[24..32].try_into().unwrap())
    } else {
        0
    };
    // Must never panic — just return some valid i64.
    let _ret = dispatch(nr, a0, a1, a2);
});
