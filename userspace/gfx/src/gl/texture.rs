// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Texture objects — 2D, Cube, 2DArray, 3D.

use crate::command::{PixelFormat, ResourceId};

// ── Texture target ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TextureTarget {
    Texture2D = 0,
    TextureCubeMap = 1,
    Texture2DArray = 2,
    Texture3D = 3,
}

// ── Sampler parameters ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FilterMode {
    Nearest = 0,
    Linear = 1,
    NearestMipmapNearest = 2,
    LinearMipmapNearest = 3,
    NearestMipmapLinear = 4,
    LinearMipmapLinear = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WrapMode {
    Repeat = 0,
    MirroredRepeat = 1,
    ClampToEdge = 2,
    ClampToBorder = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplerParams {
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub wrap_s: WrapMode,
    pub wrap_t: WrapMode,
    pub wrap_r: WrapMode,
    pub max_aniso: u8,
    pub base_level: u8,
    pub max_level: u8,
}

impl SamplerParams {
    pub const LINEAR_REPEAT: Self = Self {
        min_filter: FilterMode::LinearMipmapLinear,
        mag_filter: FilterMode::Linear,
        wrap_s: WrapMode::Repeat,
        wrap_t: WrapMode::Repeat,
        wrap_r: WrapMode::Repeat,
        max_aniso: 1,
        base_level: 0,
        max_level: 255,
    };
    pub const NEAREST_CLAMP: Self = Self {
        min_filter: FilterMode::Nearest,
        mag_filter: FilterMode::Nearest,
        wrap_s: WrapMode::ClampToEdge,
        wrap_t: WrapMode::ClampToEdge,
        wrap_r: WrapMode::ClampToEdge,
        max_aniso: 1,
        base_level: 0,
        max_level: 0,
    };
    pub const LINEAR_CLAMP: Self = Self {
        min_filter: FilterMode::LinearMipmapLinear,
        mag_filter: FilterMode::Linear,
        wrap_s: WrapMode::ClampToEdge,
        wrap_t: WrapMode::ClampToEdge,
        wrap_r: WrapMode::ClampToEdge,
        max_aniso: 16,
        base_level: 0,
        max_level: 255,
    };
}

impl Default for SamplerParams {
    fn default() -> Self {
        Self::LINEAR_CLAMP
    }
}

// ── GlTexture ─────────────────────────────────────────────────────────────────

pub struct GlTexture {
    pub(crate) name: u32,
    pub(crate) target: TextureTarget,
    pub(crate) format: PixelFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) depth: u32,
    pub(crate) levels: u32,
    pub(crate) layers: u32,
    pub(crate) resource: ResourceId,
    pub(crate) sampler: SamplerParams,
}

impl GlTexture {
    pub(crate) fn new(name: u32, target: TextureTarget) -> Self {
        Self {
            name,
            target,
            format: PixelFormat::Rgba8Unorm,
            width: 0,
            height: 0,
            depth: 1,
            levels: 1,
            layers: 1,
            resource: ResourceId::INVALID,
            sampler: SamplerParams::default(),
        }
    }

    pub fn name(&self) -> u32 {
        self.name
    }
    pub fn target(&self) -> TextureTarget {
        self.target
    }
    pub fn format(&self) -> PixelFormat {
        self.format
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn levels(&self) -> u32 {
        self.levels
    }
    pub fn resource(&self) -> ResourceId {
        self.resource
    }
    pub fn sampler(&self) -> &SamplerParams {
        &self.sampler
    }
    pub fn is_ready(&self) -> bool {
        self.resource.is_valid()
    }
}

// ── GlRenderbuffer ────────────────────────────────────────────────────────────

/// A renderbuffer attachment (non-sampleable render target / depth-stencil).
pub struct GlRenderbuffer {
    pub(crate) name: u32,
    pub(crate) format: PixelFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) samples: u8,
    pub(crate) resource: ResourceId,
}

impl GlRenderbuffer {
    pub(crate) fn new(name: u32) -> Self {
        Self {
            name,
            format: PixelFormat::Bgra8Unorm,
            width: 0,
            height: 0,
            samples: 1,
            resource: ResourceId::INVALID,
        }
    }
    pub fn name(&self) -> u32 {
        self.name
    }
    pub fn resource(&self) -> ResourceId {
        self.resource
    }
    pub fn is_ready(&self) -> bool {
        self.resource.is_valid()
    }
}
