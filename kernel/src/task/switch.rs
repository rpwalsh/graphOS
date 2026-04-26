// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Context switch assembly — x86_64.
//!
//! Provides the low-level `switch_context` function that saves the current
//! task's CPU registers and restores the next task's registers.
//!
//! ## Register convention
//! The System V AMD64 ABI designates rbx, rbp, r12–r15 as callee-saved.
//! The switch function saves/restores these plus rip, rsp, and rflags.
//! This matches the `CpuContext` layout in `task::context`.
//!
//! ## How it works
//! 1. Save the current task's callee-saved registers to `*old_ctx`.
//! 2. Save rflags to `*old_ctx`.
//! 3. Save rsp to `*old_ctx`.
//! 4. Capture return address (rip) via the `lea` of label `1:`.
//! 5. Load the new task's registers from `*new_ctx`.
//! 6. Push the new task's rip and `ret` into it.
//!
//! When the old task is eventually switched back to, it resumes at label `1:`
//! and returns normally to its caller.
//!
//! ## Safety contract
//! - Both pointers must point to valid, non-overlapping `CpuContext` structs.
//! - The new context must have a valid rsp (pointing to a mapped, sufficiently
//!   large stack) and a valid rip (pointing to executable code).
//! - Interrupts should be disabled across the switch (or the caller must
//!   ensure the IDT/stack is safe for both tasks).

use super::context::CpuContext;

/// Perform a context switch: save current registers to `old`, restore from `new`.
///
/// # Safety
/// Both `old` and `new` must point to valid `CpuContext` structs. The `new`
/// context must have valid rip and rsp values. The caller must ensure that
/// the stack pointed to by `new.rsp` is mapped and large enough.
#[inline(never)]
pub unsafe fn switch_context(old: *mut CpuContext, new: *const CpuContext) {
    // SAFETY: We perform a raw register save/restore sequence that matches
    // the CpuContext layout (rbx, rbp, r12, r13, r14, r15, rip, rsp, rflags).
    //
    // Field offsets (each u64 = 8 bytes):
    //   rbx    = [old + 0]
    //   rbp    = [old + 8]
    //   r12    = [old + 16]
    //   r13    = [old + 24]
    //   r14    = [old + 32]
    //   r15    = [old + 40]
    //   rip    = [old + 48]
    //   rsp    = [old + 56]
    //   rflags = [old + 64]
    unsafe {
        core::arch::asm!(
            // ---- Save current task's state into *old (rdi) ----
            "mov [rdi + 0],  rbx",
            "mov [rdi + 8],  rbp",
            "mov [rdi + 16], r12",
            "mov [rdi + 24], r13",
            "mov [rdi + 32], r14",
            "mov [rdi + 40], r15",
            // Capture the return address — when this task is resumed, it
            // will continue at label 22 and return to the caller.
            "lea rax, [rip + 22f]",
            "mov [rdi + 48], rax",      // save rip
            "mov [rdi + 56], rsp",      // save rsp
            "pushfq",
            "pop rax",
            "mov [rdi + 64], rax",      // save rflags

            // ---- Restore next task's state from *new (rsi) ----
            "mov rbx, [rsi + 0]",
            "mov rbp, [rsi + 8]",
            "mov r12, [rsi + 16]",
            "mov r13, [rsi + 24]",
            "mov r14, [rsi + 32]",
            "mov r15, [rsi + 40]",
            "mov rsp, [rsi + 56]",      // switch stack
            // Restore rflags.
            "mov rax, [rsi + 64]",
            "push rax",
            "popfq",
            // Jump to the new task's rip. For a task that was previously
            // saved, this goes to label 22. For a brand-new task, this goes
            // to its entry function.
            "mov rax, [rsi + 48]",
            "push rax",
            "ret",

            // ---- Resume point for the old task ----
            "22:",
            // The old task resumes here when it is switched back to.
            // Nothing to do — just fall through to the function epilogue.

            // Clobbered registers. rax is used as scratch.
            out("rax") _,
            in("rdi") old,
            in("rsi") new,
            // We clobber memory because we're switching stacks and the
            // compiler must not assume memory contents are preserved.
            clobber_abi("C"),
        );
    }
}
