// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Linear algebra: Vec2/3/4 + Mat4 (column-major, like GL).

use core::ops::{Add, Mul, Neg, Sub};
use libm::{cosf, sinf, sqrtf, tanf};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    pub const ONE: Vec3 = Vec3 {
        x: 1.0,
        y: 1.0,
        z: 1.0,
    };
    pub const X: Vec3 = Vec3 {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    };
    pub const Y: Vec3 = Vec3 {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    };
    pub const Z: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    };

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    pub const fn splat(v: f32) -> Self {
        Self { x: v, y: v, z: v }
    }

    pub fn dot(self, b: Vec3) -> f32 {
        self.x * b.x + self.y * b.y + self.z * b.z
    }
    pub fn cross(self, b: Vec3) -> Vec3 {
        Vec3 {
            x: self.y * b.z - self.z * b.y,
            y: self.z * b.x - self.x * b.z,
            z: self.x * b.y - self.y * b.x,
        }
    }
    pub fn length(self) -> f32 {
        sqrtf(self.dot(self))
    }
    pub fn normalize(self) -> Vec3 {
        let l = self.length();
        if l > 1e-6 {
            self * (1.0 / l)
        } else {
            Vec3::ZERO
        }
    }
    pub fn lerp(self, b: Vec3, t: f32) -> Vec3 {
        self * (1.0 - t) + b * t
    }
    pub const fn extend(self, w: f32) -> Vec4 {
        Vec4 {
            x: self.x,
            y: self.y,
            z: self.z,
            w,
        }
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, r: Vec3) -> Vec3 {
        Vec3::new(self.x + r.x, self.y + r.y, self.z + r.z)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, r: Vec3) -> Vec3 {
        Vec3::new(self.x - r.x, self.y - r.y, self.z - r.z)
    }
}
impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

impl Vec4 {
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }
    pub const fn xyz(self) -> Vec3 {
        Vec3 {
            x: self.x,
            y: self.y,
            z: self.z,
        }
    }
}

impl Add for Vec4 {
    type Output = Vec4;
    fn add(self, r: Vec4) -> Vec4 {
        Vec4::new(self.x + r.x, self.y + r.y, self.z + r.z, self.w + r.w)
    }
}
impl Sub for Vec4 {
    type Output = Vec4;
    fn sub(self, r: Vec4) -> Vec4 {
        Vec4::new(self.x - r.x, self.y - r.y, self.z - r.z, self.w - r.w)
    }
}
impl Mul<f32> for Vec4 {
    type Output = Vec4;
    fn mul(self, s: f32) -> Vec4 {
        Vec4::new(self.x * s, self.y * s, self.z * s, self.w * s)
    }
}

/// Column-major 4x4 matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4 {
    pub cols: [Vec4; 4],
}

impl Default for Mat4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4 {
        cols: [
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        ],
    };

    pub fn translation(t: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.cols[3] = Vec4::new(t.x, t.y, t.z, 1.0);
        m
    }

    pub fn scale(s: Vec3) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.cols[0].x = s.x;
        m.cols[1].y = s.y;
        m.cols[2].z = s.z;
        m
    }

    pub fn rotation_x(a: f32) -> Mat4 {
        let (c, s) = (cosf(a), sinf(a));
        Mat4 {
            cols: [
                Vec4::new(1.0, 0.0, 0.0, 0.0),
                Vec4::new(0.0, c, s, 0.0),
                Vec4::new(0.0, -s, c, 0.0),
                Vec4::new(0.0, 0.0, 0.0, 1.0),
            ],
        }
    }
    pub fn rotation_y(a: f32) -> Mat4 {
        let (c, s) = (cosf(a), sinf(a));
        Mat4 {
            cols: [
                Vec4::new(c, 0.0, -s, 0.0),
                Vec4::new(0.0, 1.0, 0.0, 0.0),
                Vec4::new(s, 0.0, c, 0.0),
                Vec4::new(0.0, 0.0, 0.0, 1.0),
            ],
        }
    }
    pub fn rotation_z(a: f32) -> Mat4 {
        let (c, s) = (cosf(a), sinf(a));
        Mat4 {
            cols: [
                Vec4::new(c, s, 0.0, 0.0),
                Vec4::new(-s, c, 0.0, 0.0),
                Vec4::new(0.0, 0.0, 1.0, 0.0),
                Vec4::new(0.0, 0.0, 0.0, 1.0),
            ],
        }
    }

    /// Right-handed perspective projection matching `glFrustum`/`gluPerspective`
    /// with depth mapped to NDC [-1, 1] (we then remap to [0, 1] in viewport).
    pub fn perspective(fov_y_rad: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / tanf(fov_y_rad * 0.5);
        let nf = 1.0 / (near - far);
        Mat4 {
            cols: [
                Vec4::new(f / aspect, 0.0, 0.0, 0.0),
                Vec4::new(0.0, f, 0.0, 0.0),
                Vec4::new(0.0, 0.0, (far + near) * nf, -1.0),
                Vec4::new(0.0, 0.0, 2.0 * far * near * nf, 0.0),
            ],
        }
    }

    /// Right-handed look-at, like `gluLookAt`.
    pub fn look_at(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
        let f = (center - eye).normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);
        Mat4 {
            cols: [
                Vec4::new(s.x, u.x, -f.x, 0.0),
                Vec4::new(s.y, u.y, -f.y, 0.0),
                Vec4::new(s.z, u.z, -f.z, 0.0),
                Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
            ],
        }
    }

    pub fn mul_vec4(&self, v: Vec4) -> Vec4 {
        self.cols[0] * v.x + self.cols[1] * v.y + self.cols[2] * v.z + self.cols[3] * v.w
    }

    pub fn mul_mat(&self, b: &Mat4) -> Mat4 {
        Mat4 {
            cols: [
                self.mul_vec4(b.cols[0]),
                self.mul_vec4(b.cols[1]),
                self.mul_vec4(b.cols[2]),
                self.mul_vec4(b.cols[3]),
            ],
        }
    }

    /// Returns transposed upper-left 3x3 columns.
    ///
    /// This is commonly used as a normal transform approximation. It is exact
    /// for rigid transforms and uniform scale; for arbitrary non-uniform scale
    /// or shear, callers should use a true inverse-transpose path.
    pub fn upper3_transposed(&self) -> [Vec3; 3] {
        [
            Vec3::new(self.cols[0].x, self.cols[1].x, self.cols[2].x),
            Vec3::new(self.cols[0].y, self.cols[1].y, self.cols[2].y),
            Vec3::new(self.cols[0].z, self.cols[1].z, self.cols[2].z),
        ]
    }

    /// Alias for [`Mat4::upper3_transposed`] with a more explicit name.
    pub fn normal_matrix_approx_transposed(&self) -> [Vec3; 3] {
        self.upper3_transposed()
    }
}

impl Mul<Mat4> for Mat4 {
    type Output = Mat4;
    fn mul(self, rhs: Mat4) -> Mat4 {
        self.mul_mat(&rhs)
    }
}
