// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Machine control helpers for reboot and poweroff under QEMU/OVMF.

use crate::arch::x86_64::serial;

const KBD_CMD_PORT: u16 = 0x64;
const RESET_CTL_PORT: u16 = 0xCF9;
const QEMU_DEBUG_EXIT_PORT: u16 = 0xF4;
const QEMU_ACPI_PM_PORT: u16 = 0x604;
const BOCHS_PM_PORT: u16 = 0xB004;
const LEGACY_QEMU_PM_PORT: u16 = 0x4004;

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
unsafe fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
}

#[inline]
fn halt_forever() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

/// Request a full machine reboot.
pub fn reboot() -> ! {
    serial::write_line(b"[power] reboot requested");
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    for _ in 0..100_000 {
        if unsafe { inb(KBD_CMD_PORT) } & 0x02 == 0 {
            break;
        }
    }
    unsafe { outb(KBD_CMD_PORT, 0xFE) };

    unsafe {
        outb(RESET_CTL_PORT, 0x02);
        outb(RESET_CTL_PORT, 0x06);
    }

    serial::write_line(b"[power] reboot request did not reset the VM; halting");
    halt_forever()
}

/// Request a clean machine poweroff.
pub fn shutdown() -> ! {
    serial::write_line(b"[power] clean shutdown requested");
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    unsafe {
        // Preferred QEMU path: this exits the VM immediately when
        // `isa-debug-exit` is present in the launch configuration.
        outw(QEMU_DEBUG_EXIT_PORT, 0x10);
        outw(QEMU_ACPI_PM_PORT, 0x2000);
        outw(BOCHS_PM_PORT, 0x2000);
        outw(LEGACY_QEMU_PM_PORT, 0x3400);
    }

    serial::write_line(b"[power] shutdown request did not power off the VM; halting");
    halt_forever()
}
