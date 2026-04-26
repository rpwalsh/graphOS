// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! `Transform3D` — TRS (Translate, Rotate, Scale) transform with cached world matrix.

use crate::math::{Mat4, Quat, Vec3};

/// A 3D transform: translation, rotation (quaternion), scale.
///
/// `local_matrix()` = T * R * S.
/// `world_matrix()` = parent_world * local_matrix (cached, invalidated on change).
#[derive(Debug, Clone)]
pub struct Transform3D {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    /// Cached local-to-world matrix.  Dirty when `None`.
    world_cache: Option<Mat4>,
}

impl Default for Transform3D {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            world_cache: None,
        }
    }
}

impl Transform3D {
    pub fn new(position: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self {
            position,
            rotation,
            scale,
            world_cache: None,
        }
    }

    pub fn from_position(p: Vec3) -> Self {
        Self {
            position: p,
            ..Self::default()
        }
    }

    pub fn from_position_scale(p: Vec3, s: Vec3) -> Self {
        Self {
            position: p,
            scale: s,
            ..Self::default()
        }
    }

    // ── Setters (invalidate cache) ────────────────────────────────────────────

    pub fn set_position(&mut self, p: Vec3) {
        self.position = p;
        self.invalidate();
    }
    pub fn set_rotation(&mut self, r: Quat) {
        self.rotation = r;
        self.invalidate();
    }
    pub fn set_scale(&mut self, s: Vec3) {
        self.scale = s;
        self.invalidate();
    }

    pub fn translate(&mut self, delta: Vec3) {
        self.position = self.position + delta;
        self.invalidate();
    }

    pub fn rotate_local(&mut self, delta: Quat) {
        self.rotation = self.rotation.mul_quat(delta).normalise();
        self.invalidate();
    }

    fn invalidate(&mut self) {
        self.world_cache = None;
    }

    // ── Matrix computation ────────────────────────────────────────────────────

    /// Local TRS matrix.
    pub fn local_matrix(&self) -> Mat4 {
        Mat4::translation(self.position) * self.rotation.to_mat4() * Mat4::scale(self.scale)
    }

    /// World matrix given the parent's world matrix.
    ///
    /// Pass `Mat4::IDENTITY` for root nodes.
    pub fn world_matrix(&mut self, parent_world: Mat4) -> Mat4 {
        if let Some(m) = self.world_cache {
            return m;
        }
        let m = parent_world * self.local_matrix();
        self.world_cache = Some(m);
        m
    }

    /// Invalidate this node's cache (call when parent changes).
    pub fn mark_dirty(&mut self) {
        self.world_cache = None;
    }

    /// Forward direction in world space.
    pub fn forward(&self) -> Vec3 {
        self.rotation.rotate(Vec3::FORWARD)
    }
    /// Right direction in world space.
    pub fn right(&self) -> Vec3 {
        self.rotation.rotate(Vec3::RIGHT)
    }
    /// Up direction in world space.
    pub fn up(&self) -> Vec3 {
        self.rotation.rotate(Vec3::UP)
    }
}
