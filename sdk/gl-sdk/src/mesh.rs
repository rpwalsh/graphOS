// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Mesh helpers and the canonical [`Vertex`] layout.

use crate::math::{Vec2, Vec3};
use crate::shader::Varying;

/// Standard vertex: position + normal + UV + per-vertex tint.
#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub pos: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    pub color: Vec3,
}

impl Vertex {
    pub const fn new(pos: Vec3, normal: Vec3, uv: Vec2, color: Vec3) -> Self {
        Self {
            pos,
            normal,
            uv,
            color,
        }
    }
}

/// Static mesh built from caller-provided slices.
pub struct Mesh<'a> {
    pub vertices: &'a [Vertex],
    pub indices: &'a [u32],
}

impl<'a> Mesh<'a> {
    pub const fn new(vertices: &'a [Vertex], indices: &'a [u32]) -> Self {
        Self { vertices, indices }
    }
}

/// The classic Phong / forward-shading varying payload.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdVarying {
    pub world_pos: Vec3,
    pub normal: Vec3,
    pub uv: Vec2,
    pub color: Vec3,
}

impl Varying for StdVarying {
    fn weighted_sum(a: Self, wa: f32, b: Self, wb: f32, c: Self, wc: f32) -> Self {
        Self {
            world_pos: Vec3::new(
                a.world_pos.x * wa + b.world_pos.x * wb + c.world_pos.x * wc,
                a.world_pos.y * wa + b.world_pos.y * wb + c.world_pos.y * wc,
                a.world_pos.z * wa + b.world_pos.z * wb + c.world_pos.z * wc,
            ),
            normal: Vec3::new(
                a.normal.x * wa + b.normal.x * wb + c.normal.x * wc,
                a.normal.y * wa + b.normal.y * wb + c.normal.y * wc,
                a.normal.z * wa + b.normal.z * wb + c.normal.z * wc,
            ),
            uv: Vec2::new(
                a.uv.x * wa + b.uv.x * wb + c.uv.x * wc,
                a.uv.y * wa + b.uv.y * wb + c.uv.y * wc,
            ),
            color: Vec3::new(
                a.color.x * wa + b.color.x * wb + c.color.x * wc,
                a.color.y * wa + b.color.y * wb + c.color.y * wc,
                a.color.z * wa + b.color.z * wb + c.color.z * wc,
            ),
        }
    }
    fn scale(self, s: f32) -> Self {
        Self {
            world_pos: self.world_pos * s,
            normal: self.normal * s,
            uv: Vec2::new(self.uv.x * s, self.uv.y * s),
            color: self.color * s,
        }
    }
}

// ───────── built-in mesh generators (write into caller-provided storage) ─────

/// Generate a unit cube centered at the origin.
/// Requires `verts.len() >= 24` and `indices.len() >= 36`.
pub fn build_cube(verts: &mut [Vertex], indices: &mut [u32], color: Vec3) -> (usize, usize) {
    debug_assert!(
        verts.len() >= 24,
        "build_cube requires at least 24 vertices"
    );
    debug_assert!(
        indices.len() >= 36,
        "build_cube requires at least 36 indices"
    );
    if verts.len() < 24 || indices.len() < 36 {
        return (0, 0);
    }
    let faces: [[Vec3; 4]; 6] = [
        // +X
        [
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(0.5, -0.5, 0.5),
        ],
        // -X
        [
            Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(-0.5, 0.5, 0.5),
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(-0.5, -0.5, -0.5),
        ],
        // +Y
        [
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(-0.5, 0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(0.5, 0.5, -0.5),
        ],
        // -Y
        [
            Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(0.5, -0.5, 0.5),
        ],
        // +Z
        [
            Vec3::new(-0.5, -0.5, 0.5),
            Vec3::new(0.5, -0.5, 0.5),
            Vec3::new(0.5, 0.5, 0.5),
            Vec3::new(-0.5, 0.5, 0.5),
        ],
        // -Z
        [
            Vec3::new(0.5, -0.5, -0.5),
            Vec3::new(-0.5, -0.5, -0.5),
            Vec3::new(-0.5, 0.5, -0.5),
            Vec3::new(0.5, 0.5, -0.5),
        ],
    ];
    let normals = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z];
    let uvs = [
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(0.0, 1.0),
    ];

    let mut vi = 0usize;
    let mut ii = 0usize;
    for (face, n) in faces.iter().zip(normals.iter()) {
        let base = vi as u32;
        for k in 0..4 {
            verts[vi] = Vertex::new(face[k], *n, uvs[k], color);
            vi += 1;
        }
        indices[ii] = base;
        indices[ii + 1] = base + 1;
        indices[ii + 2] = base + 2;
        indices[ii + 3] = base;
        indices[ii + 4] = base + 2;
        indices[ii + 5] = base + 3;
        ii += 6;
    }
    (vi, ii)
}

pub fn build_cube_checked(
    verts: &mut [Vertex],
    indices: &mut [u32],
    color: Vec3,
) -> Option<(usize, usize)> {
    if verts.len() < 24 || indices.len() < 36 {
        return None;
    }
    Some(build_cube(verts, indices, color))
}

/// Generate a UV-sphere (longitude × latitude tessellation) with radius 1.
/// `lon` and `lat` are the segment counts. Required storage:
///   verts.len() >= (lon+1) * (lat+1)
///   indices.len() >= lon * lat * 6
pub fn build_sphere(
    verts: &mut [Vertex],
    indices: &mut [u32],
    lon: u32,
    lat: u32,
    color: Vec3,
) -> (usize, usize) {
    use libm::{cosf, sinf};
    if lon == 0 || lat == 0 {
        return (0, 0);
    }
    let need_v = (lon as usize + 1) * (lat as usize + 1);
    let need_i = lon as usize * lat as usize * 6;
    debug_assert!(
        verts.len() >= need_v,
        "build_sphere vertex capacity too small"
    );
    debug_assert!(
        indices.len() >= need_i,
        "build_sphere index capacity too small"
    );
    if verts.len() < need_v || indices.len() < need_i {
        return (0, 0);
    }
    let pi = core::f32::consts::PI;
    let mut vi = 0usize;
    for j in 0..=lat {
        let theta = (j as f32) * pi / (lat as f32);
        let st = sinf(theta);
        let ct = cosf(theta);
        for i in 0..=lon {
            let phi = (i as f32) * 2.0 * pi / (lon as f32);
            let sp = sinf(phi);
            let cp = cosf(phi);
            let p = Vec3::new(cp * st, ct, sp * st);
            verts[vi] = Vertex::new(
                p,
                p.normalize(),
                Vec2::new(i as f32 / lon as f32, j as f32 / lat as f32),
                color,
            );
            vi += 1;
        }
    }
    let mut ii = 0usize;
    let stride = lon + 1;
    for j in 0..lat {
        for i in 0..lon {
            let a = j * stride + i;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices[ii] = a;
            indices[ii + 1] = c;
            indices[ii + 2] = b;
            indices[ii + 3] = b;
            indices[ii + 4] = c;
            indices[ii + 5] = d;
            ii += 6;
        }
    }
    (vi, ii)
}

pub fn build_sphere_checked(
    verts: &mut [Vertex],
    indices: &mut [u32],
    lon: u32,
    lat: u32,
    color: Vec3,
) -> Option<(usize, usize)> {
    if lon == 0 || lat == 0 {
        return None;
    }
    let need_v = (lon as usize + 1) * (lat as usize + 1);
    let need_i = lon as usize * lat as usize * 6;
    if verts.len() < need_v || indices.len() < need_i {
        return None;
    }
    Some(build_sphere(verts, indices, lon, lat, color))
}
