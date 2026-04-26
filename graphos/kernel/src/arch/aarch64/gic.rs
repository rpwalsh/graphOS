// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AArch64 GICv2/GICv3 interrupt controller stubs.

/// GICv2 distributor base on QEMU `virt`.
const GICD_BASE: u64 = 0x0800_0000;
/// GICv2 CPU interface base.
const GICC_BASE: u64 = 0x0801_0000;

pub fn init() {
    // Enable distributor.
    unsafe {
        core::ptr::write_volatile(GICD_BASE as *mut u32, 1);
        // Enable CPU interface + EOI mode.
        core::ptr::write_volatile((GICC_BASE + 0x00) as *mut u32, 1);
        core::ptr::write_volatile((GICC_BASE + 0x04) as *mut u32, 0xFF);
    }
}

pub fn enable_irq(irq: u32) {
    let reg = (GICD_BASE + 0x100 + (irq / 32) as u64 * 4) as *mut u32;
    unsafe {
        let cur = core::ptr::read_volatile(reg);
        core::ptr::write_volatile(reg, cur | (1 << (irq % 32)));
    }
}

pub fn ack_irq() -> u32 {
    unsafe { core::ptr::read_volatile((GICC_BASE + 0x0C) as *const u32) & 0x3FF }
}

pub fn eoi_irq(irq: u32) {
    unsafe {
        core::ptr::write_volatile((GICC_BASE + 0x10) as *mut u32, irq);
    }
}
