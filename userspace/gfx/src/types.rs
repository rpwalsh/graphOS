// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Shared pipeline state types used in both the `GpuCmd` wire protocol and
//! the higher-level `RenderEncoder` API.
//!
//! These are `#[repr(u8)]` C-ABI-stable enums and structs so they can be
//! serialised directly into the kernel wire buffer without any conversion.

// ── Blend state ───────────────────────────────────────────────────────────────

/// Source / destination blend factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlendFactor {
    Zero = 0,
    One = 1,
    SrcAlpha = 2,
    OneMinusSrcAlpha = 3,
    DstAlpha = 4,
    OneMinusDstAlpha = 5,
    SrcColor = 6,
    OneMinusSrcColor = 7,
}

/// Blend equation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlendOp {
    Add = 0,
    Subtract = 1,
    ReverseSubtract = 2,
    Min = 3,
    Max = 4,
}

/// Per-attachment blend descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendState {
    pub enabled: bool,
    pub src_color: BlendFactor,
    pub dst_color: BlendFactor,
    pub color_op: BlendOp,
    pub src_alpha: BlendFactor,
    pub dst_alpha: BlendFactor,
    pub alpha_op: BlendOp,
    /// Channel write mask: bit 0=R, 1=G, 2=B, 3=A.
    pub write_mask: u8,
}

impl BlendState {
    /// Pre-multiplied alpha Porter-Duff src-over.
    pub const ALPHA: Self = Self {
        enabled: true,
        src_color: BlendFactor::One,
        dst_color: BlendFactor::OneMinusSrcAlpha,
        color_op: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::OneMinusSrcAlpha,
        alpha_op: BlendOp::Add,
        write_mask: 0x0F,
    };
    /// Straight (non-pre-multiplied) alpha.
    pub const ALPHA_STRAIGHT: Self = Self {
        enabled: true,
        src_color: BlendFactor::SrcAlpha,
        dst_color: BlendFactor::OneMinusSrcAlpha,
        color_op: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::OneMinusSrcAlpha,
        alpha_op: BlendOp::Add,
        write_mask: 0x0F,
    };
    /// Additive (glow, bloom).
    pub const ADDITIVE: Self = Self {
        enabled: true,
        src_color: BlendFactor::One,
        dst_color: BlendFactor::One,
        color_op: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::One,
        alpha_op: BlendOp::Add,
        write_mask: 0x0F,
    };
    /// Opaque overwrite — no blending.
    pub const OPAQUE: Self = Self {
        enabled: false,
        src_color: BlendFactor::One,
        dst_color: BlendFactor::Zero,
        color_op: BlendOp::Add,
        src_alpha: BlendFactor::One,
        dst_alpha: BlendFactor::Zero,
        alpha_op: BlendOp::Add,
        write_mask: 0x0F,
    };
}

// ── Depth / stencil state ─────────────────────────────────────────────────────

/// Depth comparison function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DepthOp {
    Never = 0,
    Less = 1,
    Equal = 2,
    LessOrEqual = 3,
    Greater = 4,
    NotEqual = 5,
    GreaterOrEqual = 6,
    Always = 7,
}

/// Depth test and write configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthState {
    pub test_enable: bool,
    pub write_enable: bool,
    pub compare_op: DepthOp,
}

impl DepthState {
    pub const DISABLED: Self = Self {
        test_enable: false,
        write_enable: false,
        compare_op: DepthOp::Always,
    };
    pub const READ_WRITE: Self = Self {
        test_enable: true,
        write_enable: true,
        compare_op: DepthOp::LessOrEqual,
    };
    pub const READ_ONLY: Self = Self {
        test_enable: true,
        write_enable: false,
        compare_op: DepthOp::LessOrEqual,
    };
}

// ── Rasterizer state ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CullMode {
    None = 0,
    Front = 1,
    Back = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FillMode {
    Solid = 0,
    Wireframe = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterState {
    pub cull_mode: CullMode,
    pub fill_mode: FillMode,
    /// Counter-clockwise winding = front face.
    pub front_ccw: bool,
    pub depth_clip: bool,
}

impl RasterState {
    pub const DEFAULT: Self = Self {
        cull_mode: CullMode::Back,
        fill_mode: FillMode::Solid,
        front_ccw: true,
        depth_clip: true,
    };
    pub const NO_CULL: Self = Self {
        cull_mode: CullMode::None,
        ..Self::DEFAULT
    };
    pub const WIREFRAME: Self = Self {
        fill_mode: FillMode::Wireframe,
        ..Self::DEFAULT
    };
}

// ── Primitive topology ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Topology {
    Triangles = 0,
    TriangleStrip = 1,
    Lines = 2,
    LineStrip = 3,
    Points = 4,
}

// ── Index format ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IndexFormat {
    U16 = 0,
    U32 = 1,
}

// ── Vertex layout ─────────────────────────────────────────────────────────────

/// Tells the kernel executor how to interpret the vertex buffer bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VertexLayout {
    /// `{ pos: [f32;2], uv: [f32;2] }` — 16 bytes/vert.
    Pos2Uv2 = 0,
    /// `{ pos: [f32;3], uv: [f32;2], nor: [f32;3] }` — 32 bytes/vert.
    Pos3Uv2Nor = 1,
    /// `{ pos: [f32;3], color: u32 }` — 16 bytes/vert.
    Pos3Color = 2,
}

// ── Buffer kind ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BufferKind {
    Vertex = 0,
    Index = 1,
    Uniform = 2,
    Storage = 3,
}

// ── Viewport / Scissor ────────────────────────────────────────────────────────

/// Viewport transform: maps NDC → screen pixels.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

impl Viewport {
    pub fn screen(w: u32, h: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: w as f32,
            height: h as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }
}

/// Scissor rectangle — pixels outside are discarded by the rasterizer.
#[derive(Debug, Clone, Copy)]
pub struct Scissor {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Scissor {
    pub fn screen(w: u32, h: u32) -> Self {
        Self { x: 0, y: 0, w, h }
    }
}
