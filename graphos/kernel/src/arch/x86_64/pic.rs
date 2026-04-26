// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! 8259 PIC — Programmable Interrupt Controller driver.
//!
//! The legacy 8259 PIC is a pair of cascaded chips (master + slave) that
//! multiplex 15 hardware IRQ lines onto CPU interrupt vectors. UEFI
//! firmware leaves the PIC in an undefined state, so we must:
//!
//! 1. Remap IRQs 0–7 from vectors 8–15 (which collide with CPU exceptions)
//!    to vectors 32–39.
//! 2. Remap IRQs 8–15 to vectors 40–47.
//! 3. Mask all lines except those we explicitly enable.
//!
//! ## Why not APIC?
//! The PIC is simpler and universally present. Once SMP support is added,
//! this module will be replaced by an APIC/IO-APIC driver. For single-core
//! boot the PIC is correct and sufficient.
//!
//! ## Safety
//! All I/O port access is `unsafe`. The PIC is accessed via ports 0x20–0x21
//! (master) and 0xA0–0xA1 (slave), which are standardized on every x86 PC.

use crate::arch::x86_64::serial;

// ════════════════════════════════════════════════════════════════════
// I/O port helpers
// ════════════════════════════════════════════════════════════════════

/// Write a byte to an x86 I/O port.
///
/// # Safety
/// Caller must ensure the port address is valid and the write is expected
/// by the hardware at that address.
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

/// Read a byte from an x86 I/O port.
///
/// # Safety
/// Caller must ensure the port address is valid.
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

/// Small I/O delay — write to port 0x80 (POST code port, safe to write).
/// Required between successive PIC command bytes on fast CPUs.
#[inline]
unsafe fn io_wait() {
    unsafe { outb(0x80, 0) };
}

// ════════════════════════════════════════════════════════════════════
// PIC constants
// ════════════════════════════════════════════════════════════════════

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// First interrupt vector for master PIC (IRQ 0–7).
pub const PIC1_OFFSET: u8 = 32;
/// First interrupt vector for slave PIC (IRQ 8–15).
pub const PIC2_OFFSET: u8 = 40;

/// End-of-interrupt command byte.
const EOI: u8 = 0x20;

// ICW1 flags
const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;

// ICW4 flags
const ICW4_8086: u8 = 0x01;

// ════════════════════════════════════════════════════════════════════
// Public interface
// ════════════════════════════════════════════════════════════════════

/// Remap and initialise both PICs.
///
/// After this call:
/// - Master PIC: IRQ 0–7 → vectors 32–39
/// - Slave PIC:  IRQ 8–15 → vectors 40–47
/// - All IRQ lines are masked (disabled). Call `unmask()` to enable specific lines.
///
/// # Safety
/// Must be called exactly once during early boot, before enabling interrupts.
pub unsafe fn init() {
    // Save current masks so we can restore application-level state if
    // we ever need to (we won't — we mask everything fresh).
    let _mask1 = unsafe { inb(PIC1_DATA) };
    let _mask2 = unsafe { inb(PIC2_DATA) };

    // ICW1: begin initialization sequence (cascade mode, ICW4 needed).
    unsafe {
        outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();
        outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();
    }

    // ICW2: set vector offsets.
    unsafe {
        outb(PIC1_DATA, PIC1_OFFSET);
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();
    }

    // ICW3: tell master there is a slave on IRQ2, tell slave its cascade ID.
    unsafe {
        outb(PIC1_DATA, 0x04); // slave on IRQ2 (bit 2)
        io_wait();
        outb(PIC2_DATA, 0x02); // cascade identity = 2
        io_wait();
    }

    // ICW4: 8086 mode (not 8080 mode).
    unsafe {
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();
    }

    // Mask all IRQs. Individual drivers will unmask their own lines.
    unsafe {
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }

    serial::write_line(b"[pic] 8259 PIC remapped: IRQ 0-7 -> vec 32-39, IRQ 8-15 -> vec 40-47");
}

/// Unmask (enable) a specific IRQ line.
///
/// `irq` is the hardware IRQ number (0–15).
/// IRQ 0 = PIT timer, IRQ 1 = keyboard, IRQ 2 = cascade (do not mask).
///
/// # Safety
/// The corresponding IDT vector must have a valid handler installed
/// before unmasking, or the CPU will triple-fault on the first interrupt.
pub unsafe fn unmask(irq: u8) {
    if irq < 8 {
        let mask = unsafe { inb(PIC1_DATA) };
        unsafe { outb(PIC1_DATA, mask & !(1 << irq)) };
    } else if irq < 16 {
        // Slave PIC — must also unmask cascade line (IRQ2) on master.
        let mask2 = unsafe { inb(PIC2_DATA) };
        unsafe { outb(PIC2_DATA, mask2 & !(1 << (irq - 8))) };
        // Ensure cascade (IRQ2) is unmasked on master.
        let mask1 = unsafe { inb(PIC1_DATA) };
        unsafe { outb(PIC1_DATA, mask1 & !(1 << 2)) };
    }
    serial::write_bytes(b"[pic] unmasked IRQ ");
    serial::write_u64_dec(irq as u64);
}

/// Mask (disable) a specific IRQ line.
pub unsafe fn mask(irq: u8) {
    if irq < 8 {
        let m = unsafe { inb(PIC1_DATA) };
        unsafe { outb(PIC1_DATA, m | (1 << irq)) };
    } else if irq < 16 {
        let m = unsafe { inb(PIC2_DATA) };
        unsafe { outb(PIC2_DATA, m | (1 << (irq - 8))) };
    }
}

/// Send End-Of-Interrupt to the PIC(s).
///
/// Must be called at the end of every IRQ handler, or the PIC will not
/// deliver further interrupts on that line.
///
/// For IRQs 8–15, EOI must be sent to both slave and master.
pub unsafe fn end_of_interrupt(irq: u8) {
    if irq >= 8 {
        unsafe { outb(PIC2_CMD, EOI) };
    }
    unsafe { outb(PIC1_CMD, EOI) };
}

/// Disable both PICs by masking all lines.
///
/// Call this before switching to APIC mode.
pub unsafe fn disable() {
    unsafe {
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
    serial::write_line(b"[pic] all IRQs masked (PIC disabled)");
}
