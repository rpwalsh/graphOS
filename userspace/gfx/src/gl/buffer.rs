// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! VBO / IBO / UBO — GL buffer objects.

extern crate alloc;
use crate::command::ResourceId;
use crate::types::BufferKind;
use alloc::vec::Vec;

// ── Buffer targets ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BufferTarget {
    ArrayBuffer = 0,        // vertex data
    ElementArrayBuffer = 1, // index data
    UniformBuffer = 2,      // uniform block
    ShaderStorage = 3,      // SSBO
    TransformFeedback = 4,
    PixelPack = 5,
    PixelUnpack = 6,
}

impl BufferTarget {
    pub fn to_kind(self) -> BufferKind {
        match self {
            Self::ArrayBuffer => BufferKind::Vertex,
            Self::ElementArrayBuffer => BufferKind::Index,
            Self::UniformBuffer => BufferKind::Uniform,
            Self::ShaderStorage => BufferKind::Storage,
            _ => BufferKind::Vertex,
        }
    }
}

// ── Buffer usage hint ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BufferUsage {
    StaticDraw = 0,
    DynamicDraw = 1,
    StreamDraw = 2,
    StaticRead = 3,
    DynamicRead = 4,
    StreamRead = 5,
    StaticCopy = 6,
    DynamicCopy = 7,
    StreamCopy = 8,
}

// ── GlBuffer ──────────────────────────────────────────────────────────────────

/// A GL buffer object backed by a kernel `ResourceId`.
pub struct GlBuffer {
    pub(crate) name: u32,
    pub(crate) target: BufferTarget,
    pub(crate) usage: BufferUsage,
    pub(crate) size: u32,
    /// Kernel GPU resource handle.  `INVALID` until `buffer_data` is called.
    pub(crate) resource: ResourceId,
    /// CPU shadow copy (for dynamic/stream buffers).
    pub(crate) shadow: Option<Vec<u8>>,
}

impl GlBuffer {
    pub(crate) fn new(name: u32) -> Self {
        Self {
            name,
            target: BufferTarget::ArrayBuffer,
            usage: BufferUsage::StaticDraw,
            size: 0,
            resource: ResourceId::INVALID,
            shadow: None,
        }
    }

    pub fn name(&self) -> u32 {
        self.name
    }
    pub fn target(&self) -> BufferTarget {
        self.target
    }
    pub fn usage(&self) -> BufferUsage {
        self.usage
    }
    pub fn size(&self) -> u32 {
        self.size
    }
    pub fn resource(&self) -> ResourceId {
        self.resource
    }

    /// Whether this buffer has been allocated on the GPU.
    pub fn is_ready(&self) -> bool {
        self.resource.is_valid()
    }
}
