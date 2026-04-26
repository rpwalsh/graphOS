// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Ray casting and hit testing.

use crate::aabb::Aabb;
use crate::math::Vec3;
use crate::node::NodeId;

// ── Ray ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct Ray {
    pub origin: Vec3,
    pub dir: Vec3, // must be normalised
}

impl Ray {
    pub fn new(origin: Vec3, dir: Vec3) -> Self {
        Self {
            origin,
            dir: dir.normalise(),
        }
    }
    pub fn at(self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }
}

// ── Hit record ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct HitRecord {
    pub node: NodeId,
    /// Distance along the ray.
    pub t: f32,
    /// World-space hit position.
    pub position: Vec3,
    /// World-space surface normal.
    pub normal: Vec3,
    /// UV coordinate at the hit point.
    pub uv: [f32; 2],
}

// ── AABB intersection ─────────────────────────────────────────────────────────

/// Slab method ray-AABB intersection.
///
/// Returns `(t_min, t_max)` if the ray hits the box, or `None` if it misses.
pub fn ray_aabb(ray: Ray, aabb: Aabb) -> Option<(f32, f32)> {
    let inv_dir = Vec3::new(
        1.0 / ray.dir.x.max(f32::EPSILON).min(f32::MAX),
        1.0 / ray.dir.y.max(f32::EPSILON).min(f32::MAX),
        1.0 / ray.dir.z.max(f32::EPSILON).min(f32::MAX),
    );

    let t1 = (aabb.min.x - ray.origin.x) * inv_dir.x;
    let t2 = (aabb.max.x - ray.origin.x) * inv_dir.x;
    let t3 = (aabb.min.y - ray.origin.y) * inv_dir.y;
    let t4 = (aabb.max.y - ray.origin.y) * inv_dir.y;
    let t5 = (aabb.min.z - ray.origin.z) * inv_dir.z;
    let t6 = (aabb.max.z - ray.origin.z) * inv_dir.z;

    let t_min = t1.min(t2).max(t3.min(t4)).max(t5.min(t6));
    let t_max = t1.max(t2).min(t3.max(t4)).min(t5.max(t6));

    if t_max < 0.0 || t_min > t_max {
        None
    } else {
        Some((t_min.max(0.0), t_max))
    }
}

/// Ray-quad (plane) intersection.
///
/// The quad is in the XY plane at Z=0 in world space, transformed by `world`.
/// Returns the hit `t` if within the unit quad bounds, or `None`.
pub fn ray_quad(ray: Ray, world: crate::math::Mat4) -> Option<f32> {
    // Transform ray to quad local space.
    let inv = approx_inv(world);
    let local_origin = inv.transform_point(ray.origin);
    let local_dir = inv.transform_dir(ray.dir);

    // Plane Z=0 intersection.
    if local_dir.z.abs() < 1e-6 {
        return None;
    }
    let t = -local_origin.z / local_dir.z;
    if t < 0.0 {
        return None;
    }

    let hit = local_origin + local_dir * t;
    if hit.x.abs() <= 0.5 && hit.y.abs() <= 0.5 {
        Some(t)
    } else {
        None
    }
}

/// Rough inverse for TRS-only matrices (inverse = Transpose(R) * Translate(-t) * Scale(1/s)).
fn approx_inv(m: crate::math::Mat4) -> crate::math::Mat4 {
    // For rigid-body transforms this is exact.
    // Use the transpose trick: inv = transpose(rot) * negate(trans).
    let a = m.0;
    // Extract scale-squared for each column.
    let s0 = (a[0][0] * a[0][0] + a[1][0] * a[1][0] + a[2][0] * a[2][0]).max(1e-12);
    let s1 = (a[0][1] * a[0][1] + a[1][1] * a[1][1] + a[2][1] * a[2][1]).max(1e-12);
    let s2 = (a[0][2] * a[0][2] + a[1][2] * a[1][2] + a[2][2] * a[2][2]).max(1e-12);
    let inv_s = [1.0 / s0, 1.0 / s1, 1.0 / s2];
    // Transpose upper-left 3x3, accounting for scale.
    let r = [
        [a[0][0] * inv_s[0], a[1][0] * inv_s[0], a[2][0] * inv_s[0]],
        [a[0][1] * inv_s[1], a[1][1] * inv_s[1], a[2][1] * inv_s[1]],
        [a[0][2] * inv_s[2], a[1][2] * inv_s[2], a[2][2] * inv_s[2]],
    ];
    let tx = -(r[0][0] * a[0][3] + r[0][1] * a[1][3] + r[0][2] * a[2][3]);
    let ty = -(r[1][0] * a[0][3] + r[1][1] * a[1][3] + r[1][2] * a[2][3]);
    let tz = -(r[2][0] * a[0][3] + r[2][1] * a[1][3] + r[2][2] * a[2][3]);
    crate::math::Mat4::from_rows([
        [r[0][0], r[0][1], r[0][2], tx],
        [r[1][0], r[1][1], r[1][2], ty],
        [r[2][0], r[2][1], r[2][2], tz],
        [0.0, 0.0, 0.0, 1.0],
    ])
}
