// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Mesh — vertex and index buffer descriptors.

use graphos_gfx::command::ResourceId;
use graphos_gfx::types::{IndexFormat, VertexLayout};

/// Opaque mesh handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeshId(pub u32);

/// A sub-mesh: a range of index data within the shared mesh buffers.
#[derive(Debug, Clone, Copy)]
pub struct Submesh {
    pub first_index: u32,
    pub index_count: u32,
    pub first_vertex: u32,
    pub vertex_count: u32,
}

/// A GPU mesh — vertex + index buffers uploaded to the kernel.
pub struct Mesh {
    pub id: MeshId,
    pub vertex_buf: ResourceId,
    pub index_buf: ResourceId,
    pub layout: VertexLayout,
    pub index_fmt: IndexFormat,
    pub submeshes: [Submesh; 8],
    pub submesh_count: u8,
    pub vertex_count: u32,
    pub index_count: u32,
    pub aabb: crate::aabb::Aabb,
}

impl Mesh {
    pub fn new(id: MeshId) -> Self {
        Self {
            id,
            vertex_buf: ResourceId::INVALID,
            index_buf: ResourceId::INVALID,
            layout: VertexLayout::Pos3Uv2Nor,
            index_fmt: IndexFormat::U32,
            submeshes: [Submesh {
                first_index: 0,
                index_count: 0,
                first_vertex: 0,
                vertex_count: 0,
            }; 8],
            submesh_count: 0,
            vertex_count: 0,
            index_count: 0,
            aabb: crate::aabb::Aabb::UNIT,
        }
    }

    pub fn submesh(&self, i: usize) -> Option<&Submesh> {
        if i < self.submesh_count as usize {
            Some(&self.submeshes[i])
        } else {
            None
        }
    }

    pub fn is_ready(&self) -> bool {
        self.vertex_buf.is_valid()
    }
}

// ── Built-in mesh primitives ──────────────────────────────────────────────────

/// Vertices for a unit quad in the XY plane (Z=0), CCW winding.
/// Layout: Pos3Uv2Nor (32 bytes/vert).
/// 4 vertices, 6 indices (two triangles).
pub fn quad_vertices() -> [f32; 4 * 8] {
    // pos.xyz, uv.xy, nor.xyz
    [
        // pos            uv       normal
        -0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.5, -0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.5, 0.5,
        0.0, 1.0, 1.0, 0.0, 0.0, 1.0, -0.5, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0,
    ]
}

pub fn quad_indices() -> [u32; 6] {
    [0, 1, 2, 0, 2, 3]
}

/// 24-vertex, 36-index unit cube.
pub fn cube_vertices() -> [f32; 24 * 8] {
    let faces: &[([f32; 3], [f32; 3])] = &[
        // normal, up-dir (used to compute tangent space)
        ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),  // +Z
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]), // -Z
        ([0.0, 1.0, 0.0], [0.0, 0.0, -1.0]), // +Y
        ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]), // -Y
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),  // +X
        ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), // -X
    ];
    let uvs = [[0.0f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let mut out = [0.0f32; 24 * 8];
    for (fi, (nor, up)) in faces.iter().enumerate() {
        let n = [nor[0], nor[1], nor[2]];
        let r = [
            up[1] * n[2] - up[2] * n[1],
            up[2] * n[0] - up[0] * n[2],
            up[0] * n[1] - up[1] * n[0],
        ]; // right = up × normal
        let u = [
            n[1] * r[2] - n[2] * r[1],
            n[2] * r[0] - n[0] * r[2],
            n[0] * r[1] - n[1] * r[0],
        ]; // up_local = normal × right
        let corners = [
            [
                -0.5f32 * r[0] - 0.5 * u[0] + 0.5 * n[0],
                -0.5 * r[1] - 0.5 * u[1] + 0.5 * n[1],
                -0.5 * r[2] - 0.5 * u[2] + 0.5 * n[2],
            ],
            [
                0.5 * r[0] - 0.5 * u[0] + 0.5 * n[0],
                0.5 * r[1] - 0.5 * u[1] + 0.5 * n[1],
                0.5 * r[2] - 0.5 * u[2] + 0.5 * n[2],
            ],
            [
                0.5 * r[0] + 0.5 * u[0] + 0.5 * n[0],
                0.5 * r[1] + 0.5 * u[1] + 0.5 * n[1],
                0.5 * r[2] + 0.5 * u[2] + 0.5 * n[2],
            ],
            [
                -0.5 * r[0] + 0.5 * u[0] + 0.5 * n[0],
                -0.5 * r[1] + 0.5 * u[1] + 0.5 * n[1],
                -0.5 * r[2] + 0.5 * u[2] + 0.5 * n[2],
            ],
        ];
        for (vi, (corner, uv)) in corners.iter().zip(uvs.iter()).enumerate() {
            let base = (fi * 4 + vi) * 8;
            out[base..base + 3].copy_from_slice(corner);
            out[base + 3..base + 5].copy_from_slice(uv);
            out[base + 5..base + 8].copy_from_slice(&n);
        }
    }
    out
}

pub fn cube_indices() -> [u32; 36] {
    let mut idx = [0u32; 36];
    for f in 0..6u32 {
        let b = f * 4;
        let o = f * 6;
        idx[o as usize..o as usize + 6].copy_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    idx
}
