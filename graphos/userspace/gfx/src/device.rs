// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GPU device and context — the compositor's handle to the kernel GPU substrate.
//!
//! ## Opening the device
//!
//! ```
//! let mut device = GpuDevice::open().expect("GPU substrate unavailable");
//! let sc = device.create_swapchain(1920, 1080, PixelFormat::Bgra8Unorm);
//! ```
//!
//! ## Building and submitting a frame
//!
//! ```
//! let mut ctx = GpuContext::new(&device);
//! ctx.cmd().begin_frame(sc.back_buffer(), Some(Color::BLACK));
//! // ... draw calls ...
//! ctx.cmd().present(sc.back_buffer());
//! ctx.submit();  // → SYS_GPU_SUBMIT syscall
//! ```
//!
//! ## Design
//!
//! `GpuDevice` is a thin wrapper around the kernel GPU channel.  It holds no
//! pixel data and performs no rendering.  `GpuContext` owns a `CommandBuffer`
//! and a reusable scratch allocation for the wire-format batch.

extern crate alloc;
use alloc::vec::Vec;

use crate::command::{CommandBuffer, GpuCmd, PixelFormat, ResourceId, ResourceKind};
use crate::swapchain::SwapChain;
use crate::sync::{Fence, FenceId};
use crate::types::BufferKind;
use crate::wire;

const GPU_KIND_BUFFER_BASE: u8 = 0x80;

// ── Syscall numbers (mirrors kernel/src/syscall/numbers.rs) ──────────────────

/// Submit a batch of `GpuCmd`s to the kernel GPU executor.
///
/// arg0 = pointer to wire-format batch buffer (user VA)
/// arg1 = byte length of batch
///
/// Returns 0 on success, or a negative error code.
// Syscall numbers — must match kernel/src/syscall/numbers.rs
/// Query GPU executor capabilities (arg0 = out-ptr to GpuCaps struct).
const SYS_GPU_CAPS: u64 = 0x500; // SYS_GPU_QUERY_CAPS
/// Allocate a GPU resource (arg0 = ptr to GpuAllocRequest).
const SYS_GPU_ALLOC: u64 = 0x501; // SYS_GPU_RESOURCE_CREATE
/// Free a GPU resource (arg0 = ResourceId).
const SYS_GPU_FREE: u64 = 0x502; // SYS_GPU_RESOURCE_DESTROY
/// Allocate a fence (returns FenceId).
const SYS_GPU_FENCE_ALLOC: u64 = 0x505; // SYS_GPU_FENCE_ALLOC
/// Block until fence is signalled (arg0 = FenceId, arg1 = timeout_ticks).
const SYS_GPU_FENCE_WAIT: u64 = 0x506; // SYS_GPU_FENCE_WAIT
/// Submit a GraphOS-native wire command buffer (arg0 = ptr, arg1 = len).
const SYS_GPU_SUBMIT: u64 = 0x508; // SYS_GPU_SUBMIT

// ── Raw syscall helper ────────────────────────────────────────────────────────

/// Issue a GraphOS ring-3 syscall.
///
/// # Safety
/// The caller must ensure all pointer arguments point to valid, readable/writable
/// memory for the duration of the syscall.
#[inline(always)]
unsafe fn syscall2(nr: u64, a0: u64, a1: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "int 0x80",
        in("rax") nr,
        in("rdi") a0,
        in("rsi") a1,
        lateout("rax") ret,
        options(nostack)
    );
    ret
}

#[inline(always)]
unsafe fn syscall1(nr: u64, a0: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "int 0x80",
        in("rax") nr,
        in("rdi") a0,
        lateout("rax") ret,
        options(nostack)
    );
    ret
}

#[inline(always)]
unsafe fn syscall0(nr: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "int 0x80",
        in("rax") nr,
        lateout("rax") ret,
        options(nostack)
    );
    ret
}

// ── GPU capabilities ──────────────────────────────────────────────────────────

/// Capability flags returned by the kernel GPU executor.
///
/// In the current scanout-only build, only `present_2d` is expected to be
/// true. Once the native GPU backend lands, the remaining fields will reflect
/// hardware reality.
#[derive(Debug, Clone, Copy, Default)]
pub struct GpuCaps {
    /// Basic 2D present (framebuffer write + flush) available.
    pub present_2d: bool,
    /// Hardware-accelerated fill/blit (GPU-native).
    pub hw_fill: bool,
    /// Hardware-accelerated Gaussian blur.
    pub hw_blur: bool,
    /// Depth buffer and 3D mesh support.
    pub depth_3d: bool,
    /// Programmable shader support.
    pub shaders: bool,
    /// Screen width reported by the display.
    pub screen_w: u32,
    /// Screen height reported by the display.
    pub screen_h: u32,
}

#[repr(C)]
struct RawCaps {
    flags: u32,
    screen_w: u32,
    screen_h: u32,
    _pad: u32,
}

const CAPS_F_PRESENT_2D: u32 = 1 << 0;
const CAPS_F_HW_FILL: u32 = 1 << 1;
const CAPS_F_HW_BLUR: u32 = 1 << 2;
const CAPS_F_DEPTH_3D: u32 = 1 << 3;
const CAPS_F_SHADERS: u32 = 1 << 4;

// ── GpuDevice ─────────────────────────────────────────────────────────────────

/// Handle to the kernel GPU substrate.
///
/// Returned by `GpuDevice::open()`.  Carries the negotiated capabilities and
/// is used as the factory for resources, fences, and swap chains.
///
/// There should be exactly one `GpuDevice` per process (the compositor).
pub struct GpuDevice {
    caps: GpuCaps,
}

impl GpuDevice {
    /// Open the kernel GPU channel and query capabilities.
    ///
    /// Returns `None` if the kernel GPU substrate is unavailable
    /// (no virtio-gpu device detected).
    pub fn open() -> Option<Self> {
        let mut raw = RawCaps {
            flags: 0,
            screen_w: 0,
            screen_h: 0,
            _pad: 0,
        };
        let ret = unsafe { syscall1(SYS_GPU_CAPS, &raw as *const RawCaps as u64) };
        if ret != 0 {
            return None;
        }

        let caps = GpuCaps {
            present_2d: raw.flags & CAPS_F_PRESENT_2D != 0,
            hw_fill: raw.flags & CAPS_F_HW_FILL != 0,
            hw_blur: raw.flags & CAPS_F_HW_BLUR != 0,
            depth_3d: raw.flags & CAPS_F_DEPTH_3D != 0,
            shaders: raw.flags & CAPS_F_SHADERS != 0,
            screen_w: raw.screen_w,
            screen_h: raw.screen_h,
        };
        Some(Self { caps })
    }

    #[inline]
    pub fn caps(&self) -> &GpuCaps {
        &self.caps
    }
    #[inline]
    pub fn screen_w(&self) -> u32 {
        self.caps.screen_w
    }
    #[inline]
    pub fn screen_h(&self) -> u32 {
        self.caps.screen_h
    }

    /// Allocate a GPU resource (texture, render target, or depth buffer).
    ///
    /// Returns `None` if the kernel rejects the request (OOM, invalid format).
    pub fn alloc_resource(
        &self,
        w: u32,
        h: u32,
        fmt: PixelFormat,
        kind: ResourceKind,
    ) -> Option<ResourceId> {
        #[repr(C)]
        struct Req {
            w: u32,
            h: u32,
            fmt: u8,
            kind: u8,
            _pad: [u8; 2],
        }
        let req = Req {
            w,
            h,
            fmt: fmt as u8,
            kind: kind as u8,
            _pad: [0; 2],
        };
        let id = unsafe { syscall1(SYS_GPU_ALLOC, &req as *const Req as u64) };
        if id == 0 {
            None
        } else {
            Some(ResourceId(id as u32))
        }
    }

    pub fn alloc_buffer(&self, kind: BufferKind, size: u32) -> Option<ResourceId> {
        #[repr(C)]
        struct Req {
            w: u32,
            h: u32,
            fmt: u8,
            kind: u8,
            _pad: [u8; 2],
        }
        let req = Req {
            w: size,
            h: 1,
            fmt: 0,
            kind: GPU_KIND_BUFFER_BASE | kind as u8,
            _pad: [0; 2],
        };
        let id = unsafe { syscall1(SYS_GPU_ALLOC, &req as *const Req as u64) };
        if id == 0 {
            None
        } else {
            Some(ResourceId(id as u32))
        }
    }

    /// Free a previously allocated GPU resource.
    pub fn free_resource(&self, id: ResourceId) {
        unsafe {
            syscall1(SYS_GPU_FREE, id.0 as u64);
        }
    }

    /// Allocate a fence.
    pub fn alloc_fence(&self) -> Option<Fence> {
        let id = unsafe { syscall0(SYS_GPU_FENCE_ALLOC) };
        if id == 0 {
            None
        } else {
            Some(Fence {
                id: FenceId(id as u32),
                signalled: false,
            })
        }
    }

    /// Create a swap chain for the display surface.
    pub fn create_swapchain(&self, fmt: PixelFormat) -> Option<SwapChain> {
        let back = self.alloc_resource(
            self.caps.screen_w,
            self.caps.screen_h,
            fmt,
            ResourceKind::RenderTarget,
        )?;
        Some(SwapChain {
            back_buffer: back,
            width: self.caps.screen_w,
            height: self.caps.screen_h,
        })
    }

    pub fn submit(&self, cmds: &CommandBuffer) -> bool {
        if cmds.is_empty() {
            return true;
        }
        let mut wire_buf = Vec::with_capacity(4096);
        wire::encode(cmds, &mut wire_buf);
        let ret = unsafe {
            syscall2(
                SYS_GPU_SUBMIT,
                wire_buf.as_ptr() as u64,
                wire_buf.len() as u64,
            )
        };
        ret == 0
    }
}

// ── GpuContext ─────────────────────────────────────────────────────────────────

/// Per-frame command builder and submission handle.
///
/// Create one per frame; submit with `GpuContext::submit()`.  The internal
/// wire buffer is reused across frames to avoid heap churn.
pub struct GpuContext<'dev> {
    device: &'dev GpuDevice,
    cmds: CommandBuffer,
    wire: Vec<u8>,
}

impl<'dev> GpuContext<'dev> {
    pub fn new(device: &'dev GpuDevice) -> Self {
        Self {
            device,
            cmds: CommandBuffer::with_capacity(128),
            wire: Vec::with_capacity(4096),
        }
    }

    /// Mutable access to the command buffer for the current frame.
    #[inline]
    pub fn cmd(&mut self) -> &mut CommandBuffer {
        &mut self.cmds
    }

    /// Submit all queued commands to the kernel GPU executor.
    ///
    /// Clears the command buffer on success.  Returns `false` if the kernel
    /// rejected the batch (e.g., invalid resource handles).
    pub fn submit(&mut self) -> bool {
        if self.cmds.is_empty() {
            return true;
        }

        self.wire.clear();
        wire::encode(&self.cmds, &mut self.wire);

        let ret = unsafe {
            syscall2(
                SYS_GPU_SUBMIT,
                self.wire.as_ptr() as u64,
                self.wire.len() as u64,
            )
        };

        self.cmds.clear();
        ret == 0
    }

    #[inline]
    pub fn device(&self) -> &GpuDevice {
        self.device
    }
}
