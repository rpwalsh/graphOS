// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Render state machine — mirrors the GL 3.3 Core Profile per-context state.

// ── Capabilities (glEnable / glDisable) ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Capability {
    Blend = 0x0BE2,
    CullFace = 0x0B44,
    DepthTest = 0x0B71,
    Dither = 0x0BD0,
    LineSmooth = 0x0B20,
    PolygonOffsetFill = 0x8037,
    SampleAlphaToCoverage = 0x809E,
    SampleCoverage = 0x80A0,
    ScissorTest = 0x0C11,
    StencilTest = 0x0B90,
    PrimitiveRestart = 0x8F9D,
    DepthClamp = 0x864F,
    Multisample = 0x809D,
}

// ── Blend ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BlendFactor {
    Zero = 0,
    One = 1,
    SrcColor = 0x0300,
    OneMinusSrcColor = 0x0301,
    SrcAlpha = 0x0302,
    OneMinusSrcAlpha = 0x0303,
    DstAlpha = 0x0304,
    OneMinusDstAlpha = 0x0305,
    DstColor = 0x0306,
    OneMinusDstColor = 0x0307,
    SrcAlphaSaturate = 0x0308,
    ConstantColor = 0x8001,
    OneMinusConstantColor = 0x8002,
    ConstantAlpha = 0x8003,
    OneMinusConstantAlpha = 0x8004,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BlendEquation {
    FuncAdd = 0x8006,
    FuncSubtract = 0x800A,
    FuncReverseSubtract = 0x800B,
    Min = 0x8007,
    Max = 0x8008,
}

// ── Depth ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DepthFunc {
    Never = 0x0200,
    Less = 0x0201,
    Equal = 0x0202,
    LessOrEqual = 0x0203,
    Greater = 0x0204,
    NotEqual = 0x0205,
    GreaterOrEqual = 0x0206,
    Always = 0x0207,
}

// ── Stencil ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum StencilFunc {
    Never = 0x0200,
    Less = 0x0201,
    Equal = 0x0202,
    LessOrEqual = 0x0203,
    Greater = 0x0204,
    NotEqual = 0x0205,
    GreaterOrEqual = 0x0206,
    Always = 0x0207,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum StencilAction {
    Keep = 0x1E00,
    Zero = 0,
    Replace = 0x1E01,
    IncrWrap = 0x8507,
    DecrWrap = 0x8508,
    Invert = 0x150A,
    Incr = 0x1E02,
    Decr = 0x1E03,
}

// ── Cull / winding ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CullFace {
    Front = 0x0404,
    Back = 0x0405,
    FrontAndBack = 0x0408,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum FrontFace {
    CW = 0x0900,
    CCW = 0x0901,
}

// ── Polygon offset ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct PolygonOffset {
    pub factor: f32,
    pub units: f32,
}

// ── Per-context render state snapshot ────────────────────────────────────────

/// Complete GL render state for one context.
///
/// All fields mirror GL 3.3 Core Profile defaults.
#[derive(Clone)]
pub struct RenderState {
    // Enable flags
    pub blend: bool,
    pub cull_face: bool,
    pub depth_test: bool,
    pub dither: bool,
    pub scissor_test: bool,
    pub stencil_test: bool,
    pub polygon_offset_fill: bool,
    pub multisample: bool,
    pub depth_clamp: bool,
    pub primitive_restart: bool,

    // Viewport / scissor
    pub viewport: Viewport,
    pub scissor: ScissorBox,

    // Clear values
    pub clear_color: [f32; 4],
    pub clear_depth: f64,
    pub clear_stencil: i32,

    // Color mask
    pub color_mask: [bool; 4],

    // Blend
    pub blend_src_rgb: BlendFactor,
    pub blend_dst_rgb: BlendFactor,
    pub blend_src_alpha: BlendFactor,
    pub blend_dst_alpha: BlendFactor,
    pub blend_eq_rgb: BlendEquation,
    pub blend_eq_alpha: BlendEquation,
    pub blend_color: [f32; 4],

    // Depth
    pub depth_func: DepthFunc,
    pub depth_mask: bool,
    pub depth_range: [f64; 2],

    // Stencil
    pub stencil_func: StencilFunc,
    pub stencil_ref: i32,
    pub stencil_mask_read: u32,
    pub stencil_mask_write: u32,
    pub stencil_fail: StencilAction,
    pub stencil_zfail: StencilAction,
    pub stencil_zpass: StencilAction,

    // Cull / winding
    pub cull_face_mode: CullFace,
    pub front_face: FrontFace,

    // Line / point
    pub line_width: f32,
    pub point_size: f32,

    // Polygon offset
    pub poly_offset: PolygonOffset,

    // Primitive restart index
    pub primitive_restart_index: u32,

    // Sample coverage
    pub sample_coverage_value: f32,
    pub sample_coverage_invert: bool,
}

impl Default for RenderState {
    fn default() -> Self {
        Self {
            blend: false,
            cull_face: false,
            depth_test: false,
            dither: true,
            scissor_test: false,
            stencil_test: false,
            polygon_offset_fill: false,
            multisample: true,
            depth_clamp: false,
            primitive_restart: false,
            viewport: Viewport::default(),
            scissor: ScissorBox::default(),
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            color_mask: [true; 4],
            blend_src_rgb: BlendFactor::One,
            blend_dst_rgb: BlendFactor::Zero,
            blend_src_alpha: BlendFactor::One,
            blend_dst_alpha: BlendFactor::Zero,
            blend_eq_rgb: BlendEquation::FuncAdd,
            blend_eq_alpha: BlendEquation::FuncAdd,
            blend_color: [0.0; 4],
            depth_func: DepthFunc::Less,
            depth_mask: true,
            depth_range: [0.0, 1.0],
            stencil_func: StencilFunc::Always,
            stencil_ref: 0,
            stencil_mask_read: 0xFFFF_FFFF,
            stencil_mask_write: 0xFFFF_FFFF,
            stencil_fail: StencilAction::Keep,
            stencil_zfail: StencilAction::Keep,
            stencil_zpass: StencilAction::Keep,
            cull_face_mode: CullFace::Back,
            front_face: FrontFace::CCW,
            line_width: 1.0,
            point_size: 1.0,
            poly_offset: PolygonOffset {
                factor: 0.0,
                units: 0.0,
            },
            primitive_restart_index: 0,
            sample_coverage_value: 1.0,
            sample_coverage_invert: false,
        }
    }
}

// ── Viewport / scissor ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct Viewport {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScissorBox {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
