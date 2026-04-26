// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GPU fences and CPU/GPU synchronization.
//!
//! Fences are allocated by the kernel (`SYS_GPU_FENCE_ALLOC`) and signalled
//! when the executor reaches a `GpuCmd::Signal` in the command stream.
//! The compositor uses fences to know when a submitted frame has been consumed
//! so it can safely reuse render target resources.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

extern crate alloc;

// ── FenceId ───────────────────────────────────────────────────────────────────

/// Opaque kernel fence handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FenceId(pub u32);

impl FenceId {
    pub const INVALID: Self = Self(0);
    pub fn is_valid(self) -> bool {
        self.0 != 0
    }
}

// ── Fence ─────────────────────────────────────────────────────────────────────

/// CPU-side handle to a kernel GPU fence.
///
/// Created via `GpuDevice::alloc_fence()`.  Signalled when the kernel executor
/// reaches a `GpuCmd::Signal { fence }` command in the submitted batch.
pub struct Fence {
    pub(crate) id: FenceId,
    pub(crate) signalled: bool,
}

impl Fence {
    /// The kernel handle for this fence.  Embed in `GpuCmd::Signal` to signal it.
    pub fn id(&self) -> FenceId {
        self.id
    }

    /// Check (non-blocking) whether this fence has been signalled.
    pub fn is_signalled(&self) -> bool {
        self.signalled || self.poll_kernel()
    }

    /// Block until this fence is signalled by the kernel executor.
    pub fn wait(&mut self) {
        if self.signalled {
            return;
        }
        // SYS_GPU_FENCE_WAIT — blocks in kernel until fence signalled.
        const SYS_GPU_FENCE_WAIT: u64 = 0x506;
        unsafe {
            core::arch::asm!(
                "int 0x80",
                in("rax") SYS_GPU_FENCE_WAIT,
                in("rdi") self.id.0 as u64,
                options(nostack),
            );
        }
        self.signalled = true;
    }

    /// Non-blocking kernel poll.
    fn poll_kernel(&self) -> bool {
        // SYS_GPU_FENCE_POLL — returns 1 if signalled, 0 if pending.
        const SYS_GPU_FENCE_POLL: u64 = 0x507;
        let ret: u64;
        unsafe {
            core::arch::asm!(
                "int 0x80",
                in("rax") SYS_GPU_FENCE_POLL,
                in("rdi") self.id.0 as u64,
                lateout("rax") ret,
                options(nostack),
            );
        }
        ret == 1
    }
}

// ── Timeline ──────────────────────────────────────────────────────────────────

/// Monotonically increasing GPU timeline.
///
/// Each `advance()` call inserts a `GpuCmd::Signal` into the command buffer
/// and returns the new timeline value.  `wait_for(n)` blocks until the
/// timeline reaches `n`.
///
/// Modelled after Vulkan timeline semaphores / Metal shared events.
pub struct Timeline {
    pub(crate) fence_id: FenceId,
    /// The last submitted (CPU-side) timeline value.
    pub(crate) submitted: u64,
    /// The last confirmed (GPU-side) completed value.
    pub(crate) completed: u64,
}

impl Timeline {
    pub fn new(fence_id: FenceId) -> Self {
        Self {
            fence_id,
            submitted: 0,
            completed: 0,
        }
    }

    /// Returns the current CPU-side timeline value.
    pub fn current(&self) -> u64 {
        self.submitted
    }

    /// Returns the last confirmed GPU-completed value.
    pub fn completed(&self) -> u64 {
        self.completed
    }

    /// Increment and return the next timeline value.
    ///
    /// Caller must insert `GpuCmd::Signal { fence: self.fence_id }` into the
    /// command buffer at the point where the timeline should advance.
    pub fn advance(&mut self) -> u64 {
        self.submitted += 1;
        self.submitted
    }

    /// Block until the GPU timeline reaches `value`.
    pub fn wait_for(&mut self, value: u64) {
        if self.completed >= value {
            return;
        }
        const SYS_GPU_FENCE_WAIT: u64 = 0x506;
        unsafe {
            core::arch::asm!(
                "int 0x80",
                in("rax") SYS_GPU_FENCE_WAIT,
                in("rdi") self.fence_id.0 as u64,
                options(nostack),
            );
        }
        self.completed = value;
    }
}
