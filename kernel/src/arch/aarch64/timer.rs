// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AArch64 generic timer.

const CNTFRQ_EL0: u64 = 0;

/// Read the current counter value (CNTPCT_EL0).
pub fn read_counter() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) val, options(nomem, nostack));
    }
    val
}

/// Read the timer frequency (CNTFRQ_EL0).
pub fn frequency() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) val, options(nomem, nostack));
    }
    val
}

/// Spin-wait for approximately `us` microseconds.
pub fn udelay(us: u64) {
    let freq = frequency();
    let ticks = freq / 1_000_000 * us;
    let start = read_counter();
    while read_counter().wrapping_sub(start) < ticks {}
}
