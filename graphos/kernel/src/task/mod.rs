// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Task / process management.
//!
//! Provides the Task Control Block (TCB), CPU register context, a static
//! task table, and the context-switch assembly stub. The scheduler (sched
//! module) consumes these to dispatch tasks.
//!
//! ## Current scope
//! - Kernel-only tasks (ring 0, shared address space).
//! - Fixed-size static task table (no heap).
//! - Stack allocated from pre-reserved pool (guarantees contiguity).
//! - Cooperative context switch via inline assembly.
//! - Garbage collection of dead tasks via reaper task.
//! - User-address-space construction with ELF-backed task launch.
//! - Protected ring-3 entry with a live `syscall/sysret` fast path.
//!
//! ## Stack allocation strategy
//! Under identity mapping (phys == virt), task stacks require physically
//! contiguous frames. The stack_pool module pre-allocates contiguous regions
//! during early boot, before the heap consumes frames from the pool.
//!
//! ## Reaper task
//! When a task returns, the trampoline marks it Dead but cannot immediately
//! free its stack (the trampoline is still running on it). Instead, the
//! reaper task periodically collects dead tasks and reclaims their resources.
//!

pub mod context;
pub mod reaper;
pub mod stack_pool;
pub mod switch;
pub mod table;
pub mod tcb;
