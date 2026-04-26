// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Uniform values — mirrors the glUniform* family.

/// A typed uniform value uploadable to a `GlProgram`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UniformValue {
    Int(i32),
    UInt(u32),
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    IVec2([i32; 2]),
    IVec3([i32; 3]),
    IVec4([i32; 4]),
    UVec2([u32; 2]),
    UVec3([u32; 3]),
    UVec4([u32; 4]),
    Mat2([[f32; 2]; 2]),
    Mat3([[f32; 3]; 3]),
    Mat4([[f32; 4]; 4]),
    /// Texture unit binding (glUniform1i for sampler2D).
    Sampler(u32),
    /// Boolean (stored as i32: 0 = false, 1 = true).
    Bool(bool),
}

impl UniformValue {
    /// Pack this value as raw u32 words for the `GpuCmd::SetUniform` slot.
    ///
    /// Returns `(slot_words, extra_words)` — the first 4 words fit in the
    /// command word slot; extra_words require a separate `UploadBuffer` call
    /// for mat3/mat4.
    pub fn as_u32x4(self) -> [u32; 4] {
        match self {
            Self::Int(v) => [v as u32, 0, 0, 0],
            Self::UInt(v) => [v, 0, 0, 0],
            Self::Float(v) => [v.to_bits(), 0, 0, 0],
            Self::Bool(v) => [v as u32, 0, 0, 0],
            Self::Sampler(v) => [v, 0, 0, 0],
            Self::Vec2([a, b]) => [a.to_bits(), b.to_bits(), 0, 0],
            Self::IVec2([a, b]) => [a as u32, b as u32, 0, 0],
            Self::UVec2([a, b]) => [a, b, 0, 0],
            Self::Vec3([a, b, c]) => [a.to_bits(), b.to_bits(), c.to_bits(), 0],
            Self::IVec3([a, b, c]) => [a as u32, b as u32, c as u32, 0],
            Self::UVec3([a, b, c]) => [a, b, c, 0],
            Self::Vec4([a, b, c, d]) => [a.to_bits(), b.to_bits(), c.to_bits(), d.to_bits()],
            Self::IVec4([a, b, c, d]) => [a as u32, b as u32, c as u32, d as u32],
            Self::UVec4([a, b, c, d]) => [a, b, c, d],
            // Matrices are larger — caller must use a UBO instead.
            Self::Mat2(m) => [
                m[0][0].to_bits(),
                m[0][1].to_bits(),
                m[1][0].to_bits(),
                m[1][1].to_bits(),
            ],
            Self::Mat3(_) => [0; 4], // Use UBO
            Self::Mat4(_) => [0; 4], // Use UBO
        }
    }
}
