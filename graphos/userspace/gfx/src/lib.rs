// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphos-gfx — GraphOS GPU platform library.
//!
//! Provides the userspace half of the kernel GPU substrate:
//!
//! - [`command`]: `GpuCmd` typed wire protocol, `CommandBuffer` builder.
//! - [`resource`]: `GpuImage`, `RenderTarget`, `GpuBuffer` RAII wrappers.
//! - [`device`]: `GpuDevice` (kernel channel) and `GpuContext` (frame builder).
//! - [`sync`]: `Fence`, `Timeline`.
//! - [`swapchain`]: `SwapChain` for present-surface management.
//! - [`wire`]: `#[repr(C)]` serialization to/from the syscall batch buffer.
//!
//! This crate is `no_std` + `alloc`.  It never writes pixels — it builds and
//! submits typed command buffers to the kernel GPU executor.
#![no_std]
extern crate alloc;

pub mod command;
pub mod device;
pub mod gl;
pub mod resource;
pub mod swapchain;
pub mod sync;
pub mod types;
pub mod wire;

pub use command::{
    BlendMode, Color, CommandBuffer, GpuCmd, GradientDir, PixelFormat, Rect, ResourceId,
    ResourceKind,
};
pub use device::{GpuContext, GpuDevice};
pub use resource::{GpuImage, RenderTarget};
pub use swapchain::SwapChain;
pub use sync::{Fence, FenceId};
