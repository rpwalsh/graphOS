// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Axis-Aligned Bounding Box.

use crate::math::{Mat4, Vec3};

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const EMPTY: Self = Self {
        min: Vec3 {
            x: f32::MAX,
            y: f32::MAX,
            z: f32::MAX,
        },
        max: Vec3 {
            x: f32::MIN,
            y: f32::MIN,
            z: f32::MIN,
        },
    };
    pub const UNIT: Self = Self {
        min: Vec3 {
            x: -0.5,
            y: -0.5,
            z: -0.5,
        },
        max: Vec3 {
            x: 0.5,
            y: 0.5,
            z: 0.5,
        },
    };

    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn is_empty(self) -> bool {
        self.min.x > self.max.x
    }

    pub fn centre(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn half_extent(self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    pub fn expand(&mut self, p: Vec3) {
        self.min = self.min.min_comp(p);
        self.max = self.max.max_comp(p);
    }

    pub fn union(self, o: Self) -> Self {
        Self {
            min: self.min.min_comp(o.min),
            max: self.max.max_comp(o.max),
        }
    }

    pub fn contains(self, p: Vec3) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    /// Transform this AABB by a matrix.  Result is the AABB of the 8 transformed corners.
    pub fn transform(self, m: Mat4) -> Self {
        let corners = [
            Vec3::new(self.min.x, self.min.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.max.z),
        ];
        let mut out = Self::EMPTY;
        for c in &corners {
            out.expand(m.transform_point(*c));
        }
        out
    }
}

impl Default for Aabb {
    fn default() -> Self {
        Self::UNIT
    }
}
