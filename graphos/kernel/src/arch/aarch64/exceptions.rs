// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AArch64 exception table and syscall entry via `SVC #0`.

use core::arch::global_asm;

/// Exception types from ESR_EL1.EC field.
pub const EC_SVC64: u32 = 0x15;
pub const EC_DABT_EL0: u32 = 0x24;
pub const EC_DABT_EL1: u32 = 0x25;

/// Called from the assembly exception vector for SVC #0 (syscall).
#[no_mangle]
pub extern "C" fn aarch64_syscall_handler(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    crate::syscall::dispatch(nr, &[a0, a1, a2, a3, 0, 0])
}

/// Called for any unhandled synchronous exception.
#[no_mangle]
pub extern "C" fn aarch64_sync_exception(esr: u64, far: u64, elr: u64) {
    super::serial::write_bytes(b"[aarch64] sync exception ESR=");
    super::serial::write_hex(esr);
    super::serial::write_bytes(b" FAR=");
    super::serial::write_hex(far);
    super::serial::write_bytes(b" ELR=");
    super::serial::write_hex(elr);
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack));
        }
    }
}

global_asm!(
    ".section .text.exceptions",
    ".balign 0x800",
    ".global exception_vector_table",
    "exception_vector_table:",
    // Current EL with SP0 — synchronous.
    ".balign 0x80",
    "b .",
    // Current EL with SP0 — IRQ.
    ".balign 0x80",
    "b .",
    // Current EL with SP0 — FIQ.
    ".balign 0x80",
    "b .",
    // Current EL with SP0 — SError.
    ".balign 0x80",
    "b .",
    // Current EL with SPx — synchronous.
    ".balign 0x80",
    "b el1_sync",
    // Current EL with SPx — IRQ.
    ".balign 0x80",
    "b el1_irq",
    // Current EL with SPx — FIQ.
    ".balign 0x80",
    "b .",
    // Current EL with SPx — SError.
    ".balign 0x80",
    "b .",
    // Lower EL (AArch64) — synchronous (SVC / data abort from EL0).
    ".balign 0x80",
    "b el0_sync",
    // Lower EL (AArch64) — IRQ.
    ".balign 0x80",
    "b el0_irq",
    // Lower EL (AArch64) — FIQ.
    ".balign 0x80",
    "b .",
    // Lower EL (AArch64) — SError.
    ".balign 0x80",
    "b .",
    // ── el0_sync: SVC or data abort from EL0 ────────────────────────────────
    "el0_sync:",
    // Save integer registers we will clobber.  We save x0-x5 (args) and
    // x8 (syscall number) and x30 (LR / frame link; not needed for eret
    // but saves ABI compliance when we bl into Rust).
    "sub  sp,  sp, #64",
    "stp  x0,  x1,  [sp, #0]",
    "stp  x2,  x3,  [sp, #16]",
    "stp  x4,  x5,  [sp, #32]",
    "stp  x8,  x30, [sp, #48]",
    // Build the Rust call: fn(nr: u64, a0..a3: u64) -> u64
    // nr = x8, args = x0..x3 already in position.
    "mov  x4, x3",
    "mov  x3, x2",
    "mov  x2, x1",
    "mov  x1, x0",
    "mov  x0, x8",
    "bl   aarch64_syscall_handler",
    // x0 now holds the syscall return value.
    // Restore saved registers (skip restoring x0 — it IS the return value).
    "ldp  x8,  x30, [sp, #48]",
    "ldp  x4,  x5,  [sp, #32]",
    "ldp  x2,  x3,  [sp, #16]",
    // Restore x1 from slot; leave x0 as return value.
    "ldr  x1,  [sp, #8]",
    "add  sp,  sp, #64",
    "eret",
    // ── el0_irq ──────────────────────────────────────────────────────────────
    "el0_irq:",
    "b .",
    // ── el1_sync ─────────────────────────────────────────────────────────────
    "el1_sync:",
    "mrs x0, esr_el1",
    "mrs x1, far_el1",
    "mrs x2, elr_el1",
    "bl  aarch64_sync_exception",
    "b .",
    // ── el1_irq ──────────────────────────────────────────────────────────────
    "el1_irq:",
    "b .",
);
