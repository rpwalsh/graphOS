// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AArch64 architecture module.

pub mod boot;
pub mod exceptions;
pub mod gic;
pub mod mm;
pub mod serial;
pub mod timer;

/// Initialise AArch64 hardware.
pub fn init() {
    serial::init();
    gic::init();
}

/// Halt the current CPU core.
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack));
        }
    }
}

/// Spin for `us` microseconds.
pub fn udelay(us: u64) {
    timer::udelay(us);
}
