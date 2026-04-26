// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! CPU context for task switching.
//!
//! Defines the register state that must be saved/restored on every context
//! switch. This is the x86_64-specific layout pushed onto the kernel stack.
//!
//! ## Layout contract
//! The `CpuContext` struct is `#[repr(C)]` and is read/written by assembly
//! stubs. Changing field order requires updating the asm.

/// Saved register state for a suspended task.
///
/// The order matches the push/pop sequence in the (future) context-switch
/// assembly stub: callee-saved registers first, then the instruction pointer
/// and stack pointer at the end so the switch code can `ret` into the task.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpuContext {
    // Callee-saved registers (System V AMD64 ABI).
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    // Instruction and stack pointers — set by switch stub.
    pub rip: u64,
    pub rsp: u64,

    // RFLAGS — restored via `popfq` in the switch path.
    pub rflags: u64,
}

impl CpuContext {
    /// A zeroed context. Used as the initial value for tasks that have not
    /// yet been scheduled.
    pub const fn zero() -> Self {
        Self {
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: 0,
            rsp: 0,
            rflags: 0,
        }
    }

    /// Create an initial context for a kernel-mode task.
    ///
    /// `entry` is the function pointer the task will begin executing at.
    /// `stack_top` is the top (highest address) of the task's kernel stack.
    ///
    /// RFLAGS is set to 0x200 (IF=1, interrupts enabled) so the task can
    /// be preempted once the timer is live.
    pub const fn new_kernel(entry: u64, stack_top: u64) -> Self {
        Self {
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: entry,
            rsp: stack_top,
            rflags: 0x200, // IF=1
        }
    }
}
