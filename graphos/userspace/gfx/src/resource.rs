// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GPU resource RAII wrappers — `GpuImage`, `RenderTarget`, `DepthBuffer`,
//! `GpuBuffer`, `Sampler`.
//!
//! Resources are created through `GpuDevice` factory methods and freed on drop.
//! The kernel owns the actual backing memory; these are reference-counted handles.

use crate::command::{PixelFormat, ResourceId, ResourceKind};
use crate::types::BufferKind;

// ── Sampler ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SamplerFilter {
    Nearest = 0,
    Linear = 1,
    LinearMipmap = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SamplerWrap {
    Clamp = 0,
    Repeat = 1,
    Mirror = 2,
}

/// Texture sampler parameters (embedded in `GpuImage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampler {
    pub filter: SamplerFilter,
    pub wrap_u: SamplerWrap,
    pub wrap_v: SamplerWrap,
}

impl Sampler {
    pub const LINEAR_CLAMP: Self = Self {
        filter: SamplerFilter::Linear,
        wrap_u: SamplerWrap::Clamp,
        wrap_v: SamplerWrap::Clamp,
    };
    pub const NEAREST_CLAMP: Self = Self {
        filter: SamplerFilter::Nearest,
        wrap_u: SamplerWrap::Clamp,
        wrap_v: SamplerWrap::Clamp,
    };
    pub const LINEAR_REPEAT: Self = Self {
        filter: SamplerFilter::Linear,
        wrap_u: SamplerWrap::Repeat,
        wrap_v: SamplerWrap::Repeat,
    };
}

// ── GpuImage ──────────────────────────────────────────────────────────────────

/// A GPU texture resource (read-only after upload, or as sampled render target).
///
/// Owned; the kernel resource is freed when this is dropped via
/// `GpuDevice::free_resource()` — call `GpuImage::into_raw()` to suppress that.
pub struct GpuImage {
    pub(crate) id: ResourceId,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub sampler: Sampler,
}

impl GpuImage {
    /// Raw resource handle.
    #[inline]
    pub fn id(&self) -> ResourceId {
        self.id
    }

    /// Consume without freeing the underlying kernel resource.
    pub fn into_raw(mut self) -> ResourceId {
        let id = self.id;
        self.id = ResourceId::INVALID;
        id
    }
}

// ── RenderTarget ──────────────────────────────────────────────────────────────

/// A GPU render target — writable as a color attachment and readable as a texture.
pub struct RenderTarget {
    pub(crate) id: ResourceId,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

impl RenderTarget {
    #[inline]
    pub fn id(&self) -> ResourceId {
        self.id
    }
}

// ── DepthBuffer ───────────────────────────────────────────────────────────────

/// A depth/stencil buffer, paired with a `RenderTarget` for 3D rendering.
pub struct DepthBuffer {
    pub(crate) id: ResourceId,
    pub width: u32,
    pub height: u32,
}

impl DepthBuffer {
    #[inline]
    pub fn id(&self) -> ResourceId {
        self.id
    }
}

// ── GpuBuffer ─────────────────────────────────────────────────────────────────

/// A typed GPU buffer (vertex, index, uniform, or storage).
pub struct GpuBuffer {
    pub(crate) id: ResourceId,
    pub kind: BufferKind,
    /// Size in bytes.
    pub size: u32,
}

impl GpuBuffer {
    #[inline]
    pub fn id(&self) -> ResourceId {
        self.id
    }
}
