// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ACPI power management — S5 (soft power-off) and S3 (suspend-to-RAM).

use core::sync::atomic::{AtomicBool, Ordering};

/// SLP_EN bit in PM1x control register.
const SLP_EN: u16 = 1 << 13;

static SUSPEND_REQUESTED: AtomicBool = AtomicBool::new(false);

// ── I/O port helpers ──────────────────────────────────────────────────────────

fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Perform an ACPI soft power-off (S5). Does not return on success.
pub fn poweroff() -> ! {
    crate::arch::serial::write_line(b"[acpi] poweroff (S5)");
    let pm1a = super::pm1a_cnt_port();
    let pm1b = super::pm1b_cnt_port();
    let slp_a = super::slp_typ_s5_a() | SLP_EN;
    let slp_b = super::slp_typ_s5_b() | SLP_EN;
    if pm1a != 0 {
        outw(pm1a, slp_a);
    }
    if pm1b != 0 {
        outw(pm1b, slp_b);
    }
    // Fallback: QEMU write 0x2000 to port 0x604 (ACPI PM1a on QEMU q35/i440fx).
    outw(0x604, 0x2000);
    // If still running, spin-halt.
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
}

/// Perform a system reboot by pulsing the PS/2 controller reset line.
pub fn reboot() -> ! {
    crate::arch::serial::write_line(b"[acpi] reboot");
    // Try ACPI reset register (FADT offset 116+, if available — fall back to PS/2).
    // PS/2 reset: port 0x64, command 0xFE.
    unsafe {
        core::arch::asm!("mov al, 0xFE", "out 0x64, al", options(nomem, nostack));
    }
    // Fallback: triple fault.
    unsafe {
        core::arch::asm!("ud2", options(nomem, nostack));
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Mark that an S3 suspend was requested; handled on the next idle pass.
pub fn request_suspend() {
    SUSPEND_REQUESTED.store(true, Ordering::Relaxed);
    crate::arch::serial::write_line(b"[acpi] S3 suspend requested");
}

/// Returns `true` if a suspend was requested (consumed on read).
pub fn take_suspend_request() -> bool {
    SUSPEND_REQUESTED.swap(false, Ordering::Relaxed)
}
