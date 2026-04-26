// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Draw call encoding — mirrors `glDraw*` family.

use crate::command::{CommandBuffer, ResourceId};
use crate::types::{IndexFormat, Topology, VertexLayout};

// ── Draw parameters ───────────────────────────────────────────────────────────

/// Parameters for an indexed or non-indexed draw.
///
/// All draw calls end up calling `CommandBuffer::draw_primitives`.
#[derive(Debug, Clone, Copy)]
pub struct DrawParams {
    pub vertex_buf: ResourceId,
    pub index_buf: ResourceId,
    pub layout: VertexLayout,
    pub index_fmt: IndexFormat,
    pub topology: Topology,
    pub first: u32,
    pub count: u32,
    pub instances: u32,
}

impl DrawParams {
    /// Non-indexed draw, triangle list.
    pub fn arrays(vbo: ResourceId, layout: VertexLayout, first: u32, count: u32) -> Self {
        Self {
            vertex_buf: vbo,
            index_buf: ResourceId::INVALID,
            layout,
            index_fmt: IndexFormat::U16,
            topology: Topology::Triangles,
            first,
            count,
            instances: 1,
        }
    }

    /// Indexed draw, triangle list.
    pub fn elements(
        vbo: ResourceId,
        ibo: ResourceId,
        layout: VertexLayout,
        index_fmt: IndexFormat,
        first: u32,
        count: u32,
    ) -> Self {
        Self {
            vertex_buf: vbo,
            index_buf: ibo,
            layout,
            index_fmt,
            topology: Topology::Triangles,
            first,
            count,
            instances: 1,
        }
    }

    /// Instanced variant.
    pub fn with_instances(mut self, n: u32) -> Self {
        self.instances = n;
        self
    }
    /// Override topology.
    pub fn with_topology(mut self, t: Topology) -> Self {
        self.topology = t;
        self
    }
}

/// Encode a draw call into a `CommandBuffer`.
#[inline]
pub fn draw(cmds: &mut CommandBuffer, params: DrawParams) {
    cmds.draw_primitives(
        params.vertex_buf,
        params.index_buf,
        params.layout,
        params.index_fmt,
        params.topology,
        params.first,
        params.count,
        params.instances,
    );
}
