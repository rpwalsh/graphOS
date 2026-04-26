// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Vertex Array Objects — bind vertex buffer + attribute layout descriptors.

use crate::command::ResourceId;
use crate::types::{IndexFormat, VertexLayout};

// ── Vertex attribute ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AttribType {
    F32 = 0,
    I8 = 1,
    I16 = 2,
    I32 = 3,
    U8 = 4,
    U16 = 5,
    U32 = 6,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexAttrib {
    /// Attribute location in the shader.
    pub location: u32,
    /// Number of components (1–4).
    pub components: u8,
    pub ty: AttribType,
    /// Normalise integer types to [0,1] / [-1,1].
    pub normalise: bool,
    /// Byte offset within the vertex struct.
    pub offset: u32,
    /// Which binding slot (vertex buffer index) supplies this attribute.
    pub binding: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexBinding {
    /// Byte stride between consecutive vertices.
    pub stride: u32,
    /// Step rate: 0 = per-vertex, 1 = per-instance.
    pub step_rate: u32,
}

// ── GlVao ─────────────────────────────────────────────────────────────────────

/// A Vertex Array Object.
///
/// Stores the vertex attribute layout and the vertex/index buffer bindings.
/// Corresponds to `glGenVertexArrays` / `glBindVertexArray`.
pub struct GlVao {
    pub(crate) name: u32,
    pub(crate) attribs: [Option<VertexAttrib>; 16],
    pub(crate) bindings: [Option<VertexBinding>; 8],
    /// Bound vertex buffer resource per binding slot.
    pub(crate) vbos: [ResourceId; 8],
    /// Bound index buffer resource (INVALID = no index buffer).
    pub(crate) ibo: ResourceId,
    pub(crate) index_fmt: IndexFormat,
    /// Pre-computed `VertexLayout` for the kernel executor (Phase 1).
    pub(crate) layout: Option<VertexLayout>,
}

impl GlVao {
    pub(crate) fn new(name: u32) -> Self {
        Self {
            name,
            attribs: [None; 16],
            bindings: [None; 8],
            vbos: [ResourceId::INVALID; 8],
            ibo: ResourceId::INVALID,
            index_fmt: IndexFormat::U16,
            layout: None,
        }
    }

    pub fn name(&self) -> u32 {
        self.name
    }

    pub fn vbo(&self, slot: usize) -> ResourceId {
        self.vbos[slot]
    }
    pub fn ibo(&self) -> ResourceId {
        self.ibo
    }
    pub fn index_fmt(&self) -> IndexFormat {
        self.index_fmt
    }

    /// Infer the `VertexLayout` tag from the declared attributes.
    ///
    /// Phase 1 executor uses this to select the correct blit/shade path.
    pub fn infer_layout(&mut self) -> VertexLayout {
        // Count position, UV, normal, colour slots.
        let has_pos3 = self
            .attribs
            .iter()
            .flatten()
            .any(|a| a.location == 0 && a.components == 3);
        let has_pos2 = self
            .attribs
            .iter()
            .flatten()
            .any(|a| a.location == 0 && a.components == 2);
        let has_uv = self
            .attribs
            .iter()
            .flatten()
            .any(|a| a.location == 1 && a.components == 2);
        let has_nor = self
            .attribs
            .iter()
            .flatten()
            .any(|a| a.location == 2 && a.components == 3);
        let has_col = self
            .attribs
            .iter()
            .flatten()
            .any(|a| a.location == 1 && a.components == 4);

        let layout = if has_pos3 && has_uv && has_nor {
            VertexLayout::Pos3Uv2Nor
        } else if has_pos3 && has_col {
            VertexLayout::Pos3Color
        } else {
            VertexLayout::Pos2Uv2
        };
        self.layout = Some(layout);
        layout
    }
}
