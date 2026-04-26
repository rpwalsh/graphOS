// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Camera — view/projection matrices, frustum, and culling.

use crate::aabb::Aabb;
use crate::math::{Mat4, Vec3, Vec4};

// ── Projection ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum Projection {
    Perspective {
        fov_y: f32, // radians
        aspect: f32,
        near: f32,
        far: f32,
    },
    Orthographic {
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    },
}

impl Projection {
    pub fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Self {
        Self::Perspective {
            fov_y: fov_y_deg * core::f32::consts::PI / 180.0,
            aspect,
            near,
            far,
        }
    }

    pub fn ortho_screen(w: f32, h: f32) -> Self {
        Self::Orthographic {
            left: 0.0,
            right: w,
            bottom: h,
            top: 0.0,
            near: -1.0,
            far: 1.0,
        }
    }

    pub fn matrix(&self) -> Mat4 {
        match *self {
            Self::Perspective {
                fov_y,
                aspect,
                near,
                far,
            } => Mat4::perspective(fov_y, aspect, near, far),
            Self::Orthographic {
                left,
                right,
                bottom,
                top,
                near,
                far,
            } => Mat4::orthographic(left, right, bottom, top, near, far),
        }
    }

    pub fn near(&self) -> f32 {
        match *self {
            Self::Perspective { near, .. } | Self::Orthographic { near, .. } => near,
        }
    }
    pub fn far(&self) -> f32 {
        match *self {
            Self::Perspective { far, .. } | Self::Orthographic { far, .. } => far,
        }
    }
}

// ── Frustum ───────────────────────────────────────────────────────────────────

/// 6-plane view frustum for visibility culling.
#[derive(Debug, Clone, Copy)]
pub struct Frustum {
    /// Planes in [left, right, bottom, top, near, far] order.
    /// Each plane is (normal.xyz, d) where the positive half-space is inside.
    planes: [Vec4; 6],
}

impl Frustum {
    /// Extract frustum planes from a combined view-projection matrix.
    pub fn from_view_proj(vp: Mat4) -> Self {
        let m = vp.0;
        // Rows
        let r0 = Vec4::new(m[0][0], m[0][1], m[0][2], m[0][3]);
        let r1 = Vec4::new(m[1][0], m[1][1], m[1][2], m[1][3]);
        let r2 = Vec4::new(m[2][0], m[2][1], m[2][2], m[2][3]);
        let r3 = Vec4::new(m[3][0], m[3][1], m[3][2], m[3][3]);

        fn norm(p: Vec4) -> Vec4 {
            let l = libm::sqrtf(p.x * p.x + p.y * p.y + p.z * p.z);
            if l < 1e-10 {
                p
            } else {
                p * (1.0 / l)
            }
        }

        fn add4(a: Vec4, b: Vec4) -> Vec4 {
            Vec4::new(a.x + b.x, a.y + b.y, a.z + b.z, a.w + b.w)
        }
        fn sub4(a: Vec4, b: Vec4) -> Vec4 {
            Vec4::new(a.x - b.x, a.y - b.y, a.z - b.z, a.w - b.w)
        }

        Self {
            planes: [
                norm(add4(r3, r0)), // left
                norm(sub4(r3, r0)), // right
                norm(add4(r3, r1)), // bottom
                norm(sub4(r3, r1)), // top
                norm(add4(r3, r2)), // near
                norm(sub4(r3, r2)), // far
            ],
        }
    }

    /// Returns `true` if the AABB is at least partially inside the frustum.
    pub fn test_aabb(&self, aabb: Aabb) -> bool {
        let c = aabb.centre();
        let h = aabb.half_extent();
        for p in &self.planes {
            // Signed distance of centre to plane, plus max projection of half-extents.
            let d = p.x * c.x + p.y * c.y + p.z * c.z + p.w;
            let r = h.x * p.x.abs() + h.y * p.y.abs() + h.z * p.z.abs();
            if d + r < 0.0 {
                return false;
            }
        }
        true
    }

    /// Returns `true` if the sphere is at least partially inside the frustum.
    pub fn test_sphere(&self, centre: Vec3, radius: f32) -> bool {
        for p in &self.planes {
            let d = p.x * centre.x + p.y * centre.y + p.z * centre.z + p.w;
            if d < -radius {
                return false;
            }
        }
        true
    }
}

// ── Camera ────────────────────────────────────────────────────────────────────

pub struct Camera {
    pub position: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    pub projection: Projection,
    view_dirty: bool,
    view_cache: Mat4,
    proj_cache: Mat4,
    vp_cache: Mat4,
    frustum: Frustum,
}

impl Camera {
    pub fn perspective(fov_deg: f32, aspect: f32, near: f32, far: f32) -> Self {
        let proj = Projection::perspective(fov_deg, aspect, near, far);
        let mut cam = Self {
            position: Vec3::new(0.0, 0.0, 5.0),
            target: Vec3::ZERO,
            up: Vec3::UP,
            projection: proj,
            view_dirty: true,
            view_cache: Mat4::IDENTITY,
            proj_cache: proj.matrix(),
            vp_cache: Mat4::IDENTITY,
            frustum: Frustum {
                planes: [Vec4::ZERO; 6],
            },
        };
        cam.recompute();
        cam
    }

    pub fn set_position(&mut self, p: Vec3) {
        self.position = p;
        self.view_dirty = true;
    }
    pub fn set_target(&mut self, t: Vec3) {
        self.target = t;
        self.view_dirty = true;
    }
    pub fn set_aspect(&mut self, a: f32) {
        if let Projection::Perspective { ref mut aspect, .. } = self.projection {
            *aspect = a;
        }
        self.proj_cache = self.projection.matrix();
        self.view_dirty = true;
    }

    pub fn view_matrix(&mut self) -> Mat4 {
        self.recompute();
        self.view_cache
    }
    pub fn proj_matrix(&self) -> Mat4 {
        self.proj_cache
    }
    pub fn view_proj(&mut self) -> Mat4 {
        self.recompute();
        self.vp_cache
    }
    pub fn frustum(&mut self) -> &Frustum {
        self.recompute();
        &self.frustum
    }

    fn recompute(&mut self) {
        if !self.view_dirty {
            return;
        }
        self.view_cache = Mat4::look_at(self.position, self.target, self.up);
        self.vp_cache = self.proj_cache * self.view_cache;
        self.frustum = Frustum::from_view_proj(self.vp_cache);
        self.view_dirty = false;
    }

    /// Unproject a screen-space point (NDC [-1,1]) to a world-space ray origin + dir.
    pub fn unproject_ray(&mut self, ndc_x: f32, ndc_y: f32) -> crate::hit::Ray {
        let inv_vp = mat4_inverse(self.view_proj());
        let near_h = inv_vp.mul_vec4(Vec4::new(ndc_x, ndc_y, -1.0, 1.0));
        let far_h = inv_vp.mul_vec4(Vec4::new(ndc_x, ndc_y, 1.0, 1.0));
        let near_pt = Vec3::new(
            near_h.x / near_h.w,
            near_h.y / near_h.w,
            near_h.z / near_h.w,
        );
        let far_pt = Vec3::new(far_h.x / far_h.w, far_h.y / far_h.w, far_h.z / far_h.w);
        let dir = (far_pt - near_pt).normalise();
        crate::hit::Ray {
            origin: near_pt,
            dir,
        }
    }
}

/// Naive 4×4 matrix inverse for camera unproject (column-expand cofactor method).
fn mat4_inverse(m: Mat4) -> Mat4 {
    let a = m.0;
    let mut inv = [[0.0f32; 4]; 4];

    let a00 = a[0][0];
    let a01 = a[0][1];
    let a02 = a[0][2];
    let a03 = a[0][3];
    let a10 = a[1][0];
    let a11 = a[1][1];
    let a12 = a[1][2];
    let a13 = a[1][3];
    let a20 = a[2][0];
    let a21 = a[2][1];
    let a22 = a[2][2];
    let a23 = a[2][3];
    let a30 = a[3][0];
    let a31 = a[3][1];
    let a32 = a[3][2];
    let a33 = a[3][3];

    inv[0][0] =
        a11 * a22 * a33 - a11 * a23 * a32 - a21 * a12 * a33 + a21 * a13 * a32 + a31 * a12 * a23
            - a31 * a13 * a22;
    inv[1][0] =
        -a10 * a22 * a33 + a10 * a23 * a32 + a20 * a12 * a33 - a20 * a13 * a32 - a30 * a12 * a23
            + a30 * a13 * a22;
    inv[2][0] =
        a10 * a21 * a33 - a10 * a23 * a31 - a20 * a11 * a33 + a20 * a13 * a31 + a30 * a11 * a23
            - a30 * a13 * a21;
    inv[3][0] =
        -a10 * a21 * a32 + a10 * a22 * a31 + a20 * a11 * a32 - a20 * a12 * a31 - a30 * a11 * a22
            + a30 * a12 * a21;
    inv[0][1] =
        -a01 * a22 * a33 + a01 * a23 * a32 + a21 * a02 * a33 - a21 * a03 * a32 - a31 * a02 * a23
            + a31 * a03 * a22;
    inv[1][1] =
        a00 * a22 * a33 - a00 * a23 * a32 - a20 * a02 * a33 + a20 * a03 * a32 + a30 * a02 * a23
            - a30 * a03 * a22;
    inv[2][1] =
        -a00 * a21 * a33 + a00 * a23 * a31 + a20 * a01 * a33 - a20 * a03 * a31 - a30 * a01 * a23
            + a30 * a03 * a21;
    inv[3][1] =
        a00 * a21 * a32 - a00 * a22 * a31 - a20 * a01 * a32 + a20 * a02 * a31 + a30 * a01 * a22
            - a30 * a02 * a21;
    inv[0][2] =
        a01 * a12 * a33 - a01 * a13 * a32 - a11 * a02 * a33 + a11 * a03 * a32 + a31 * a02 * a13
            - a31 * a03 * a12;
    inv[1][2] =
        -a00 * a12 * a33 + a00 * a13 * a32 + a10 * a02 * a33 - a10 * a03 * a32 - a30 * a02 * a13
            + a30 * a03 * a12;
    inv[2][2] =
        a00 * a11 * a33 - a00 * a13 * a31 - a10 * a01 * a33 + a10 * a03 * a31 + a30 * a01 * a13
            - a30 * a03 * a11;
    inv[3][2] =
        -a00 * a11 * a32 + a00 * a12 * a31 + a10 * a01 * a32 - a10 * a02 * a31 - a30 * a01 * a12
            + a30 * a02 * a11;
    inv[0][3] =
        -a01 * a12 * a23 + a01 * a13 * a22 + a11 * a02 * a23 - a11 * a03 * a22 - a21 * a02 * a13
            + a21 * a03 * a12;
    inv[1][3] =
        a00 * a12 * a23 - a00 * a13 * a22 - a10 * a02 * a23 + a10 * a03 * a22 + a20 * a02 * a13
            - a20 * a03 * a12;
    inv[2][3] =
        -a00 * a11 * a23 + a00 * a13 * a21 + a10 * a01 * a23 - a10 * a03 * a21 - a20 * a01 * a13
            + a20 * a03 * a11;
    inv[3][3] =
        a00 * a11 * a22 - a00 * a12 * a21 - a10 * a01 * a22 + a10 * a02 * a21 + a20 * a01 * a12
            - a20 * a02 * a11;

    let det = a00 * inv[0][0] + a01 * inv[1][0] + a02 * inv[2][0] + a03 * inv[3][0];
    if det.abs() < 1e-12 {
        return Mat4::IDENTITY;
    }
    let inv_det = 1.0 / det;
    for r in &mut inv {
        for v in r.iter_mut() {
            *v *= inv_det;
        }
    }
    Mat4(inv)
}
