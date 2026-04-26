// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal linear algebra — Vec2/3/4, Mat4, Quat.
//!
//! All in row-major order.  No external math crate dependency (no_std).

use core::ops::{Add, Mul, Neg, Sub};

// ── libm wrappers (no_std safe) ───────────────────────────────────────────────
#[inline(always)]
fn fsqrt(x: f32) -> f32 {
    libm::sqrtf(x)
}
#[inline(always)]
fn fsin(x: f32) -> f32 {
    libm::sinf(x)
}
#[inline(always)]
fn fcos(x: f32) -> f32 {
    libm::cosf(x)
}
#[inline(always)]
fn facos(x: f32) -> f32 {
    libm::acosf(x)
}
#[inline(always)]
fn ftan(x: f32) -> f32 {
    libm::tanf(x)
}

// ── Vec2 ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };
    pub const ONE: Self = Self { x: 1.0, y: 1.0 };
    #[inline]
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y
    }
    #[inline]
    pub fn len_sq(self) -> f32 {
        self.dot(self)
    }
    #[inline]
    pub fn len(self) -> f32 {
        fsqrt(self.len_sq())
    }
    #[inline]
    pub fn normalise(self) -> Self {
        let l = self.len();
        if l < 1e-10 {
            Self::ZERO
        } else {
            Self::new(self.x / l, self.y / l)
        }
    }
    #[inline]
    pub fn lerp(self, o: Self, t: f32) -> Self {
        Self::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
}
impl Add for Vec2 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}
impl Sub for Vec2 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}
impl Mul<f32> for Vec2 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s)
    }
}
impl Neg for Vec2 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y)
    }
}

// ── Vec3 ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };
    pub const ONE: Self = Self {
        x: 1.0,
        y: 1.0,
        z: 1.0,
    };
    pub const UP: Self = Self {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    };
    pub const FORWARD: Self = Self {
        x: 0.0,
        y: 0.0,
        z: -1.0,
    };
    pub const RIGHT: Self = Self {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    };

    #[inline]
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    #[inline]
    pub fn len_sq(self) -> f32 {
        self.dot(self)
    }
    #[inline]
    pub fn len(self) -> f32 {
        fsqrt(self.len_sq())
    }
    #[inline]
    pub fn normalise(self) -> Self {
        let l = self.len();
        if l < 1e-10 {
            Self::ZERO
        } else {
            self * (1.0 / l)
        }
    }
    #[inline]
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    #[inline]
    pub fn lerp(self, o: Self, t: f32) -> Self {
        self + (o - self) * t
    }
    #[inline]
    pub fn min_comp(self, o: Self) -> Self {
        Self::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }
    #[inline]
    pub fn max_comp(self, o: Self) -> Self {
        Self::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }
    #[inline]
    pub fn extend(self, w: f32) -> Vec4 {
        Vec4::new(self.x, self.y, self.z, w)
    }
}
impl Add for Vec3 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for Vec3 {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}
impl Neg for Vec3 {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.x, -self.y, -self.z)
    }
}

// ── Vec4 ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Vec4 {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 0.0,
    };
    pub const ONE: Self = Self {
        x: 1.0,
        y: 1.0,
        z: 1.0,
        w: 1.0,
    };
    #[inline]
    pub fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }
    #[inline]
    pub fn xyz(self) -> Vec3 {
        Vec3::new(self.x, self.y, self.z)
    }
    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }
}
impl Mul<f32> for Vec4 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s, self.w * s)
    }
}
impl Add for Vec4 {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y, self.z + o.z, self.w + o.w)
    }
}

// ── Quaternion ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quat {
    pub const IDENTITY: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };

    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle_rad: f32) -> Self {
        let half = angle_rad * 0.5;
        let s = fsin(half);
        let a = axis.normalise();
        Self {
            x: a.x * s,
            y: a.y * s,
            z: a.z * s,
            w: fcos(half),
        }
    }

    #[inline]
    pub fn from_euler_xyz(x: f32, y: f32, z: f32) -> Self {
        let (sx, cx) = (fsin(x * 0.5), fcos(x * 0.5));
        let (sy, cy) = (fsin(y * 0.5), fcos(y * 0.5));
        let (sz, cz) = (fsin(z * 0.5), fcos(z * 0.5));
        Self {
            x: sx * cy * cz + cx * sy * sz,
            y: cx * sy * cz - sx * cy * sz,
            z: cx * cy * sz + sx * sy * cz,
            w: cx * cy * cz - sx * sy * sz,
        }
    }

    #[inline]
    pub fn len_sq(self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w
    }

    #[inline]
    pub fn normalise(self) -> Self {
        let l = fsqrt(self.len_sq());
        if l < 1e-10 {
            Self::IDENTITY
        } else {
            Self {
                x: self.x / l,
                y: self.y / l,
                z: self.z / l,
                w: self.w / l,
            }
        }
    }

    #[inline]
    pub fn conjugate(self) -> Self {
        Self {
            x: -self.x,
            y: -self.y,
            z: -self.z,
            w: self.w,
        }
    }

    #[inline]
    pub fn mul_quat(self, o: Self) -> Self {
        Self {
            x: self.w * o.x + self.x * o.w + self.y * o.z - self.z * o.y,
            y: self.w * o.y - self.x * o.z + self.y * o.w + self.z * o.x,
            z: self.w * o.z + self.x * o.y - self.y * o.x + self.z * o.w,
            w: self.w * o.w - self.x * o.x - self.y * o.y - self.z * o.z,
        }
    }

    /// Rotate a vector.
    #[inline]
    pub fn rotate(self, v: Vec3) -> Vec3 {
        let qv = Vec3::new(self.x, self.y, self.z);
        let t = qv.cross(v) * 2.0;
        v + t * self.w + qv.cross(t)
    }

    /// Slerp.
    pub fn slerp(self, mut o: Self, t: f32) -> Self {
        let mut dot = self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w;
        if dot < 0.0 {
            o = Self {
                x: -o.x,
                y: -o.y,
                z: -o.z,
                w: -o.w,
            };
            dot = -dot;
        }
        if dot > 0.9995 {
            // Linear fallback.
            let r = Self {
                x: self.x + (o.x - self.x) * t,
                y: self.y + (o.y - self.y) * t,
                z: self.z + (o.z - self.z) * t,
                w: self.w + (o.w - self.w) * t,
            };
            return r.normalise();
        }
        let theta_0 = facos(dot);
        let theta = theta_0 * t;
        let s0 = fsin(theta_0 - theta) / fsin(theta_0);
        let s1 = fsin(theta) / fsin(theta_0);
        Self {
            x: self.x * s0 + o.x * s1,
            y: self.y * s0 + o.y * s1,
            z: self.z * s0 + o.z * s1,
            w: self.w * s0 + o.w * s1,
        }
    }

    /// Convert to 3x3 rotation sub-matrix (as column-major Mat4 upper-left).
    pub fn to_mat4(self) -> Mat4 {
        let q = self.normalise();
        let x2 = q.x * q.x;
        let y2 = q.y * q.y;
        let z2 = q.z * q.z;
        let xy = q.x * q.y;
        let xz = q.x * q.z;
        let yz = q.y * q.z;
        let wx = q.w * q.x;
        let wy = q.w * q.y;
        let wz = q.w * q.z;
        Mat4::from_rows([
            [1.0 - 2.0 * (y2 + z2), 2.0 * (xy - wz), 2.0 * (xz + wy), 0.0],
            [2.0 * (xy + wz), 1.0 - 2.0 * (x2 + z2), 2.0 * (yz - wx), 0.0],
            [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (x2 + y2), 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }
}

impl Default for Quat {
    fn default() -> Self {
        Self::IDENTITY
    }
}

// ── Mat4 ──────────────────────────────────────────────────────────────────────

/// Row-major 4×4 matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Mat4 {
    pub const IDENTITY: Self = Self([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    pub const ZERO: Self = Self([[0.0; 4]; 4]);

    #[inline]
    pub fn from_rows(rows: [[f32; 4]; 4]) -> Self {
        Self(rows)
    }

    #[inline]
    pub fn col(&self, c: usize) -> Vec4 {
        Vec4::new(self.0[0][c], self.0[1][c], self.0[2][c], self.0[3][c])
    }

    #[inline]
    pub fn row(&self, r: usize) -> Vec4 {
        Vec4::new(self.0[r][0], self.0[r][1], self.0[r][2], self.0[r][3])
    }

    pub fn transpose(self) -> Self {
        let m = self.0;
        Mat4::from_rows([
            [m[0][0], m[1][0], m[2][0], m[3][0]],
            [m[0][1], m[1][1], m[2][1], m[3][1]],
            [m[0][2], m[1][2], m[2][2], m[3][2]],
            [m[0][3], m[1][3], m[2][3], m[3][3]],
        ])
    }

    pub fn translation(t: Vec3) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][3] = t.x;
        m.0[1][3] = t.y;
        m.0[2][3] = t.z;
        m
    }

    pub fn scale(s: Vec3) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0][0] = s.x;
        m.0[1][1] = s.y;
        m.0[2][2] = s.z;
        m
    }

    pub fn look_at(eye: Vec3, target: Vec3, up: Vec3) -> Self {
        let f = (target - eye).normalise();
        let r = f.cross(up).normalise();
        let u = r.cross(f);
        Mat4::from_rows([
            [r.x, r.y, r.z, -r.dot(eye)],
            [u.x, u.y, u.z, -u.dot(eye)],
            [-f.x, -f.y, -f.z, f.dot(eye)],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    pub fn perspective(fov_y_rad: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / ftan(fov_y_rad * 0.5);
        let d = near - far;
        Mat4::from_rows([
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, (far + near) / d, 2.0 * far * near / d],
            [0.0, 0.0, -1.0, 0.0],
        ])
    }

    pub fn orthographic(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Self {
        let rw = 1.0 / (right - left);
        let rh = 1.0 / (top - bottom);
        let rd = 1.0 / (near - far);
        Mat4::from_rows([
            [2.0 * rw, 0.0, 0.0, -(right + left) * rw],
            [0.0, 2.0 * rh, 0.0, -(top + bottom) * rh],
            [0.0, 0.0, 2.0 * rd, (far + near) * rd],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    #[inline]
    pub fn mul_vec4(self, v: Vec4) -> Vec4 {
        let m = &self.0;
        Vec4::new(
            m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z + m[0][3] * v.w,
            m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z + m[1][3] * v.w,
            m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z + m[2][3] * v.w,
            m[3][0] * v.x + m[3][1] * v.y + m[3][2] * v.z + m[3][3] * v.w,
        )
    }

    #[inline]
    pub fn transform_point(self, p: Vec3) -> Vec3 {
        self.mul_vec4(p.extend(1.0)).xyz()
    }

    #[inline]
    pub fn transform_dir(self, d: Vec3) -> Vec3 {
        self.mul_vec4(d.extend(0.0)).xyz()
    }

    pub fn as_flat(&self) -> [f32; 16] {
        let m = &self.0;
        [
            m[0][0], m[0][1], m[0][2], m[0][3], m[1][0], m[1][1], m[1][2], m[1][3], m[2][0],
            m[2][1], m[2][2], m[2][3], m[3][0], m[3][1], m[3][2], m[3][3],
        ]
    }
}

impl Mul for Mat4 {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        let a = &self.0;
        let b = &rhs.0;
        let mut out = [[0.0f32; 4]; 4];
        for r in 0..4 {
            for c in 0..4 {
                out[r][c] =
                    a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c] + a[r][3] * b[3][c];
            }
        }
        Mat4(out)
    }
}

impl Default for Mat4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}
