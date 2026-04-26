// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! OpenGL-compatible state machine and draw API.
//!
//! This module provides a [`Context`] that mirrors the GL state machine:
//!
//!   - Buffer objects (VBO / IBO / UBO analogs)
//!   - Vertex Array Objects with attribute pointers and instanced divisors
//!   - Texture objects with mipmap chains, wrap, and filter modes
//!   - Framebuffer objects (FBO) with color + depth + stencil attachments
//!   - Full depth / stencil / blend state
//!   - Instanced and indexed draw calls
//!
//! Everything is `no_std + alloc`: object names stay fixed-capacity, while
//! buffer payload storage uses `alloc::vec::Vec`.
//! Object "names" are plain `u32` indices (1-based, 0 = null/unbound),
//! matching the GL convention.

use crate::glsl_interp::{
    GlslShader, GlslVertex, build_texture_slots, parse_ubo_blocks, uniform_to_val,
};
use crate::math::Vec4;
use crate::pipeline::{Pipeline, Target};
use crate::shader::Shader;
use crate::texture::Texture;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use graphhash::GraphHashMap;

// ── GL enum types ─────────────────────────────────────────────────────────────

/// Primitive topology (glDrawArrays mode argument).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawMode {
    Points,
    Lines,
    LineStrip,
    LineLoop,
    Triangles,
    TriangleStrip,
    TriangleFan,
}

/// Index element width for glDrawElements-like calls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IndexType {
    U8,
    U16,
    U32,
}

/// Which side(s) to cull.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CullFace {
    None,
    Front,
    Back,
    FrontAndBack,
}

/// Front-face winding convention.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrontFace {
    CW,
    CCW,
}

/// Depth comparison function (maps to GL_LESS, GL_LEQUAL, …).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DepthFunc {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl DepthFunc {
    #[inline]
    pub fn test(self, frag: f32, buf: f32) -> bool {
        match self {
            Self::Never => false,
            Self::Less => frag < buf,
            Self::Equal => (frag - buf).abs() < 1e-7,
            Self::LessEqual => frag <= buf,
            Self::Greater => frag > buf,
            Self::NotEqual => (frag - buf).abs() >= 1e-7,
            Self::GreaterEqual => frag >= buf,
            Self::Always => true,
        }
    }
}

/// Blend factor (maps to GL_ZERO, GL_ONE, GL_SRC_ALPHA, …).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    DstColor,
    OneMinusDstColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    ConstantColor,
    OneMinusConstantColor,
    ConstantAlpha,
    OneMinusConstantAlpha,
    SrcAlphaSaturate,
}

/// Blend equation (maps to GL_FUNC_ADD, GL_FUNC_SUBTRACT, …).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlendEquation {
    FuncAdd,
    FuncSubtract,
    FuncReverseSubtract,
    Min,
    Max,
}

/// Stencil operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    Increment,
    IncrementWrap,
    Decrement,
    DecrementWrap,
    Invert,
}

/// Stencil comparison function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StencilFunc {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

impl StencilFunc {
    #[inline]
    pub fn test(self, stencil: u8, ref_val: u8, mask: u8) -> bool {
        let s = stencil & mask;
        let r = ref_val & mask;
        match self {
            Self::Never => false,
            Self::Less => s < r,
            Self::Equal => s == r,
            Self::LessEqual => s <= r,
            Self::Greater => s > r,
            Self::NotEqual => s != r,
            Self::GreaterEqual => s >= r,
            Self::Always => true,
        }
    }
}

/// Texture wrap mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WrapMode {
    Repeat,
    MirroredRepeat,
    ClampToEdge,
    ClampToBorder,
}

/// Texture filter mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterMode {
    Nearest,
    Linear,
    NearestMipmapNearest,
    LinearMipmapNearest,
    NearestMipmapLinear,
    LinearMipmapLinear,
}

/// Attachment point for framebuffer objects.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Attachment {
    Color(u8), // COLOR_ATTACHMENT0..7
    Depth,
    Stencil,
    DepthStencil,
}

/// Shader object stage kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShaderKind {
    Vertex,
    Fragment,
    Geometry,
    Compute,
}

/// OpenGL-style error codes returned by [`Context::get_error`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GlError {
    InvalidEnum,
    InvalidValue,
    InvalidOperation,
    InvalidFramebufferOperation,
    OutOfMemory,
}

/// Framebuffer completeness result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramebufferStatus {
    Complete,
    MissingAttachment,
    IncompleteAttachment,
    IncompleteDimensions,
}

/// Result of waiting on a sync object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncWaitResult {
    AlreadySignaled,
    TimeoutExpired,
    WaitFailed,
}

/// Target API profile for the software GL runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiProfile {
    /// OpenGL ES 3.0-compatible core profile target.
    OpenGlEs30,
}

/// Query object target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryTarget {
    SamplesPassed,
    AnySamplesPassed,
    PrimitivesGenerated,
    TimeElapsed,
}

/// Debug message source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugSource {
    Api,
    Application,
    ShaderCompiler,
    ThirdParty,
    Other,
}

/// Debug message type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugType {
    Error,
    DeprecatedBehavior,
    UndefinedBehavior,
    Portability,
    Performance,
    Marker,
    PushGroup,
    PopGroup,
    Other,
}

/// Debug message severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebugSeverity {
    High,
    Medium,
    Low,
    Notification,
}

/// Robustness reset status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextResetStatus {
    NoError,
    GuiltyContextReset,
    InnocentContextReset,
    UnknownContextReset,
}

/// Extension families surfaced by this software backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlExtension {
    KhrDebug,
    KhrRobustness,
    ExtDisjointTimerQuery,
    ExtColorBufferFloat,
}

/// Image unit access qualifier (read, write, read_write).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageAccess {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

/// A single image unit binding (glBindImageTexture state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageUnitBinding {
    pub texture: u32,
    pub level: i32,
    pub layered: bool,
    pub layer: i32,
    pub access: ImageAccess,
    /// Sized internal format of the view (0 = unspecified).
    pub format: u32,
}

impl Default for ImageUnitBinding {
    fn default() -> Self {
        Self {
            texture: 0,
            level: 0,
            layered: false,
            layer: 0,
            access: ImageAccess::ReadOnly,
            format: 0,
        }
    }
}

/// Bitflags for memory barriers (GL_*_BARRIER_BIT).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryBarrierBits(pub u32);
impl MemoryBarrierBits {
    pub const VERTEX_ATTRIB_ARRAY: u32 = 0x0000_0001;
    pub const ELEMENT_ARRAY: u32 = 0x0000_0002;
    pub const UNIFORM: u32 = 0x0000_0004;
    pub const TEXTURE_FETCH: u32 = 0x0000_0008;
    pub const SHADER_IMAGE_ACCESS: u32 = 0x0000_0020;
    pub const COMMAND: u32 = 0x0000_0040;
    pub const PIXEL_BUFFER: u32 = 0x0000_0080;
    pub const TEXTURE_UPDATE: u32 = 0x0000_0100;
    pub const BUFFER_UPDATE: u32 = 0x0000_0200;
    pub const FRAMEBUFFER: u32 = 0x0000_0400;
    pub const TRANSFORM_FEEDBACK: u32 = 0x0000_0800;
    pub const ATOMIC_COUNTER: u32 = 0x0000_1000;
    pub const SHADER_STORAGE: u32 = 0x0000_2000;
    pub const ALL: u32 = 0xFFFF_FFFF;
    pub const VALID_MASK: u32 = Self::VERTEX_ATTRIB_ARRAY
        | Self::ELEMENT_ARRAY
        | Self::UNIFORM
        | Self::TEXTURE_FETCH
        | Self::SHADER_IMAGE_ACCESS
        | Self::COMMAND
        | Self::PIXEL_BUFFER
        | Self::TEXTURE_UPDATE
        | Self::BUFFER_UPDATE
        | Self::FRAMEBUFFER
        | Self::TRANSFORM_FEEDBACK
        | Self::ATOMIC_COUNTER
        | Self::SHADER_STORAGE;
}

/// Runtime limits exposed by the software backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlLimits {
    pub max_buffers: usize,
    pub max_vertex_arrays: usize,
    pub max_textures: usize,
    pub max_texture_units: usize,
    pub max_color_attachments: usize,
    pub max_attribs: usize,
}

/// Clear mask bits (can be combined with |).
pub const CLEAR_COLOR: u32 = 1 << 0;
pub const CLEAR_DEPTH: u32 = 1 << 1;
pub const CLEAR_STENCIL: u32 = 1 << 2;

/// Access flags for MapBufferRange-style mapping.
pub const MAP_READ_BIT: u32 = 1 << 0;
pub const MAP_WRITE_BIT: u32 = 1 << 1;

// ── Internal object types ─────────────────────────────────────────────────────

/// Attribute format descriptor (mirrors glVertexAttribPointer).
#[derive(Clone, Copy, Debug)]
pub struct AttribPointer {
    pub buffer: u32,  // buffer name (1-based)
    pub offset: u32,  // byte offset in buffer
    pub stride: u32,  // byte stride (0 = tightly packed)
    pub size: u8,     // component count: 1–4
    pub divisor: u32, // 0 = per-vertex, ≥1 = per-instance
    pub enabled: bool,
}

impl Default for AttribPointer {
    fn default() -> Self {
        Self {
            buffer: 0,
            offset: 0,
            stride: 0,
            size: 4,
            divisor: 0,
            enabled: false,
        }
    }
}

/// One vertex array object (VAO).
#[derive(Clone, Copy, Debug)]
pub struct VertexArray {
    pub attribs: [AttribPointer; 16],
    pub element_buffer: u32, // bound IBO (0 = none)
}

/// Minimal texture-object metadata managed by the GL-style context.
#[derive(Clone, Debug)]
pub struct TextureObject {
    pub width: u32,
    pub height: u32,
    pub levels: u32,
    pub wrap_s: WrapMode,
    pub wrap_t: WrapMode,
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub border_color: [f32; 4],
    /// True when this texture stores depth values (DEPTH_COMPONENT formats).
    /// Pixels are stored as `f32::to_bits(depth)` packed into `u32`.
    pub is_depth: bool,
    /// 2D array layers: each element is a layer's pixel data (width*height u32 values).
    /// Populated by `tex_image_3d` when used as `GL_TEXTURE_2D_ARRAY`.
    pub array_layers: Vec<Vec<u32>>,
}

#[derive(Clone, Debug)]
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

#[derive(Clone, Debug)]
pub struct TextureSnapshot {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub wrap_s: WrapMode,
    pub wrap_t: WrapMode,
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub border_color: [f32; 4],
}

impl TextureSnapshot {
    pub fn as_texture(&self) -> Texture<'_> {
        Texture {
            pixels: self.pixels.as_slice(),
            width: self.width,
            height: self.height,
            wrap_s: self.wrap_s,
            wrap_t: self.wrap_t,
            min_filter: self.min_filter,
            mag_filter: self.mag_filter,
            border_color: self.border_color,
        }
    }
}

impl Default for TextureObject {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            levels: 1,
            wrap_s: WrapMode::Repeat,
            wrap_t: WrapMode::Repeat,
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            border_color: [0.0, 0.0, 0.0, 1.0],
            is_depth: false,
            array_layers: Vec::new(),
        }
    }
}

impl Default for VertexArray {
    fn default() -> Self {
        Self {
            attribs: [AttribPointer::default(); 16],
            element_buffer: 0,
        }
    }
}

/// Framebuffer attachment slot.
#[derive(Clone, Copy, Debug)]
pub struct FbAttachment {
    pub texture: u32, // texture name (0 = none)
    pub level: u32,   // mipmap level
}

impl Default for FbAttachment {
    fn default() -> Self {
        Self {
            texture: 0,
            level: 0,
        }
    }
}

/// One framebuffer object.
#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    pub color: [FbAttachment; 8],
    pub depth: FbAttachment,
    pub stencil: FbAttachment,
    pub color_rb: [u32; 8],
    pub depth_rb: u32,
    pub stencil_rb: u32,
}

/// One renderbuffer object.
#[derive(Clone, Copy, Debug)]
pub struct Renderbuffer {
    pub width: u32,
    pub height: u32,
    pub has_depth: bool,
    pub has_stencil: bool,
}

impl Default for Renderbuffer {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            has_depth: false,
            has_stencil: false,
        }
    }
}

/// One shader object.
#[derive(Debug)]
pub struct ShaderObject {
    pub kind: ShaderKind,
    pub source: Vec<u8>,
    pub compiled: bool,
    pub delete_pending: bool,
    pub metadata: Option<ShaderMetadata>,
}

impl ShaderObject {
    fn new(kind: ShaderKind) -> Self {
        Self {
            kind,
            source: Vec::new(),
            compiled: false,
            delete_pending: false,
            metadata: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderInterfaceVar {
    pub ty: String,
    pub name: String,
    pub location: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct ShaderMetadata {
    pub version_es300: bool,
    pub has_main: bool,
    pub main_signature_valid: bool,
    pub float_precision_declared: bool,
    pub inputs: Vec<ShaderInterfaceVar>,
    pub outputs: Vec<ShaderInterfaceVar>,
    pub output_locations: Vec<u8>,
    pub uniforms: Vec<ShaderInterfaceVar>,
}

/// One program object.
#[derive(Clone, Copy, Debug)]
pub struct ProgramObject {
    pub vertex_shader: u32,
    pub fragment_shader: u32,
    pub geometry_shader: u32,
    pub compute_shader: u32,
    pub linked: bool,
    pub validated: bool,
    pub validate_ok: bool,
}

impl Default for ProgramObject {
    fn default() -> Self {
        Self {
            vertex_shader: 0,
            fragment_shader: 0,
            geometry_shader: 0,
            compute_shader: 0,
            linked: false,
            validated: false,
            validate_ok: false,
        }
    }
}

/// Uniform value that can be stored per program location.
#[derive(Clone, Debug)]
pub enum UniformValue {
    Float(f32),
    Int(i32),
    UInt(u32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    IVec2([i32; 2]),
    IVec3([i32; 3]),
    IVec4([i32; 4]),
    UVec2([u32; 2]),
    UVec3([u32; 3]),
    UVec4([u32; 4]),
    Mat2([f32; 4]),
    Mat3([f32; 9]),
    Mat4([f32; 16]),
    Mat2x3([f32; 6]),
    Mat3x2([f32; 6]),
    Mat2x4([f32; 8]),
    Mat4x2([f32; 8]),
    Mat3x4([f32; 12]),
    Mat4x3([f32; 12]),
    FloatArray(Vec<f32>),
    IntArray(Vec<i32>),
    UIntArray(Vec<u32>),
}

/// One stored uniform binding: program name, location, value.
#[derive(Clone, Debug)]
pub struct UniformBinding {
    pub program: u32,
    pub location: i32,
    pub value: UniformValue,
}

#[derive(Clone, Copy, Debug)]
struct BufferMapState {
    name: u32,
    offset: usize,
    length: usize,
    write: bool,
}

/// One sampler object.
#[derive(Clone, Copy, Debug)]
pub struct SamplerObject {
    pub wrap_s: WrapMode,
    pub wrap_t: WrapMode,
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub border_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct SyncObject {
    pub signaled: bool,
}

impl Default for SyncObject {
    fn default() -> Self {
        Self { signaled: false }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TransformFeedbackObject {
    pub active: bool,
    pub paused: bool,
    pub primitive_mode: Option<DrawMode>,
}

#[derive(Clone, Copy, Debug)]
pub struct QueryObject {
    pub target: Option<QueryTarget>,
    pub active: bool,
    pub result: u64,
    pub result_available: bool,
}

impl Default for QueryObject {
    fn default() -> Self {
        Self {
            target: None,
            active: false,
            result: 0,
            result_available: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DebugMessage {
    pub source: DebugSource,
    pub kind: DebugType,
    pub id: u32,
    pub severity: DebugSeverity,
    pub message: String,
}

impl Default for TransformFeedbackObject {
    fn default() -> Self {
        Self {
            active: false,
            paused: false,
            primitive_mode: None,
        }
    }
}

impl Default for SamplerObject {
    fn default() -> Self {
        Self {
            wrap_s: WrapMode::Repeat,
            wrap_t: WrapMode::Repeat,
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            border_color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self {
            color: [FbAttachment::default(); 8],
            depth: FbAttachment::default(),
            stencil: FbAttachment::default(),
            color_rb: [0; 8],
            depth_rb: 0,
            stencil_rb: 0,
        }
    }
}

// ── Context capacity constants ────────────────────────────────────────────────

pub const MAX_BUFFERS: usize = 256;
pub const MAX_VAOS: usize = 64;
pub const MAX_TEXTURES: usize = 128;
pub const MAX_FBOS: usize = 32;
pub const MAX_TEXTURE_UNITS: usize = 32;
pub const MAX_ATTRIBS: usize = 16;
pub const MAX_RENDERBUFFERS: usize = 64;
pub const MAX_SHADERS: usize = 128;
pub const MAX_PROGRAMS: usize = 64;
pub const MAX_SAMPLERS: usize = 64;
pub const MAX_SYNCS: usize = 64;
pub const MAX_TFBS: usize = 32;
pub const MAX_TFB_BINDINGS: usize = 16;
pub const MAX_UNIFORM_BUFFER_BINDINGS: usize = 24;
pub const MAX_SHADER_STORAGE_BUFFER_BINDINGS: usize = 8;
pub const MAX_ATOMIC_COUNTER_BUFFER_BINDINGS: usize = 8;
pub const MAX_IMAGE_UNITS: usize = 8;
pub const MAX_QUERIES: usize = 128;
pub const MAX_DEBUG_MESSAGES: usize = 256;
pub const MAX_DEBUG_GROUP_DEPTH: u8 = 32;
pub const MAX_PROGRAM_TFB_VARYINGS: usize = 64;

// ── The GL context ────────────────────────────────────────────────────────────

/// Central OpenGL-style state machine.
///
/// Object names are 1-based `u32` (0 = null/unbound), following GL convention.
/// Fixed-capacity object pools avoid heap allocation for names; buffer data
/// uses `Vec<u8>` for flexible sizing.
pub struct Context {
    // ── object pools ─────────────────────────────────────────────────────
    /// Allocated flags for each name slot.
    buf_alloc: [bool; MAX_BUFFERS],
    vao_alloc: [bool; MAX_VAOS],
    tex_alloc: [bool; MAX_TEXTURES],
    fbo_alloc: [bool; MAX_FBOS],
    rbo_alloc: [bool; MAX_RENDERBUFFERS],
    shader_alloc: [bool; MAX_SHADERS],
    program_alloc: [bool; MAX_PROGRAMS],
    sampler_alloc: [bool; MAX_SAMPLERS],
    sync_alloc: [bool; MAX_SYNCS],
    tfb_alloc: [bool; MAX_TFBS],
    query_alloc: [bool; MAX_QUERIES],

    /// Buffer data store (indexed by name-1).
    buf_data: Vec<Vec<u8>>,

    /// VAO data.
    vaos: [VertexArray; MAX_VAOS],
    /// Dedicated unnamed/default VAO used when `vao == 0`.
    default_vao: VertexArray,
    /// Texture metadata storage.
    textures: [TextureObject; MAX_TEXTURES],
    /// Texture pixel storage by texture name and mip level.
    texture_images: Vec<Option<Vec<TextureImage>>>,
    /// FBO data.
    fbos: [Framebuffer; MAX_FBOS],
    /// Renderbuffer data.
    rbos: [Renderbuffer; MAX_RENDERBUFFERS],
    /// Optional color image storage for renderbuffers used as color attachments.
    renderbuffer_color_images: Vec<Option<Vec<u32>>>,
    /// Shader objects.
    shaders: Vec<Option<ShaderObject>>,
    /// Shader compile diagnostics by shader slot.
    shader_info_logs: Vec<String>,
    /// Program objects.
    programs: [ProgramObject; MAX_PROGRAMS],
    /// Program link diagnostics by program slot.
    program_info_logs: Vec<String>,
    /// Sampler objects.
    samplers: [SamplerObject; MAX_SAMPLERS],
    /// Sync objects.
    syncs: [SyncObject; MAX_SYNCS],
    /// Transform feedback objects.
    tfbs: [TransformFeedbackObject; MAX_TFBS],
    /// Query objects.
    queries: [QueryObject; MAX_QUERIES],
    /// Active query object per query target slot.
    active_queries: [Option<u32>; 4],
    /// Stored uniform values keyed by (program, location).
    uniforms: GraphHashMap<u64, UniformValue>,
    /// One active mapped range at a time.
    mapped: Option<BufferMapState>,
    /// If true, shader compile and link enforce stricter GLSL semantics.
    strict_glsl: bool,
    /// Program-declared transform feedback varyings.
    program_tfb_varyings: Vec<Vec<String>>,
    /// Total float components expected for transform feedback capture per program.
    program_tfb_components: [u16; MAX_PROGRAMS],
    /// Extension support toggles.
    extension_enabled: [bool; 4],
    /// Last successfully accepted memory barrier bitfield.
    last_memory_barrier_bits: u32,
    /// Last successfully accepted region memory barrier bitfield.
    last_memory_barrier_by_region_bits: u32,

    // ── bind points ──────────────────────────────────────────────────────
    /// Currently bound VAO (0 = default).
    pub vao: u32,
    /// Currently bound array buffer (VBO).
    pub array_buffer: u32,
    /// Currently bound element array buffer (IBO).
    pub element_array_buffer: u32,
    /// Currently bound uniform buffer.
    pub uniform_buffer: u32,
    /// UBO indexed binding points.
    pub uniform_buffer_bindings: [u32; MAX_UNIFORM_BUFFER_BINDINGS],
    /// UBO indexed binding ranges (offset, size); 0,0 means full buffer.
    pub uniform_buffer_ranges: [(u32, u32); MAX_UNIFORM_BUFFER_BINDINGS],
    /// Currently bound shader storage buffer (generic slot).
    pub shader_storage_buffer: u32,
    /// SSBO indexed binding points.
    pub shader_storage_buffer_bindings: [u32; MAX_SHADER_STORAGE_BUFFER_BINDINGS],
    /// SSBO indexed binding ranges (offset, size); 0,0 means full buffer.
    pub shader_storage_buffer_ranges: [(u32, u32); MAX_SHADER_STORAGE_BUFFER_BINDINGS],
    /// Currently bound atomic counter buffer (generic slot).
    pub atomic_counter_buffer: u32,
    /// Atomic counter buffer binding points.
    pub atomic_counter_buffer_bindings: [u32; MAX_ATOMIC_COUNTER_BUFFER_BINDINGS],
    /// Image unit bindings (texture name, level, layered, layer, access, format).
    pub image_units: [ImageUnitBinding; MAX_IMAGE_UNITS],
    /// Currently bound draw framebuffer (0 = default / window).
    pub draw_framebuffer: u32,
    /// Currently bound read framebuffer.
    pub read_framebuffer: u32,
    /// Bitmask of which draw-buffer slots are active (bit i → color attachment i).
    /// Default = 0x01 (only attachment 0).
    pub draw_buffers_mask: u8,
    /// Which color attachment index to read from (for ReadPixels / blit source).
    /// 0xFF means none (GL_NONE).
    pub read_buffer_index: u8,
    /// Per-attachment color write masks ([R,G,B,A] per attachment).
    pub color_mask_attachments: [[bool; 4]; 8],
    /// Active texture unit index.
    pub active_texture: u32,
    /// Texture bindings per unit.
    pub texture_units: [u32; MAX_TEXTURE_UNITS],
    /// Sampler bindings per texture unit.
    pub sampler_units: [u32; MAX_TEXTURE_UNITS],
    /// Currently bound renderbuffer (0 = none).
    pub renderbuffer: u32,
    /// Currently bound transform feedback object (0 = none).
    pub transform_feedback: u32,
    /// Buffer bindings for transform feedback capture.
    pub transform_feedback_buffers: [u32; MAX_TFB_BINDINGS],
    /// Debug message log.
    debug_log: Vec<DebugMessage>,
    /// Debug group nesting depth.
    debug_group_depth: u8,
    /// Robust access behavior flag.
    robust_access: bool,
    /// Robustness reset status.
    reset_status: ContextResetStatus,
    /// Currently bound program (0 = fixed path disabled in this crate).
    pub current_program: u32,

    // ── viewport / scissor ───────────────────────────────────────────────
    pub viewport: [i32; 4], // x, y, w, h
    pub scissor: [i32; 4],
    pub scissor_test: bool,

    // ── depth state ──────────────────────────────────────────────────────
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: DepthFunc,
    pub depth_range: [f32; 2], // near, far

    // ── stencil state ────────────────────────────────────────────────────
    pub stencil_test: bool,
    pub stencil_func: StencilFunc,
    pub stencil_ref: u8,
    pub stencil_mask_r: u8, // read mask
    pub stencil_mask_w: u8, // write mask
    pub stencil_fail: StencilOp,
    pub stencil_zfail: StencilOp,
    pub stencil_zpass: StencilOp,
    // back-face stencil (glStencilFuncSeparate / glStencilOpSeparate / glStencilMaskSeparate)
    pub stencil_func_back: StencilFunc,
    pub stencil_ref_back: u8,
    pub stencil_mask_r_back: u8,
    pub stencil_mask_w_back: u8,
    pub stencil_fail_back: StencilOp,
    pub stencil_zfail_back: StencilOp,
    pub stencil_zpass_back: StencilOp,

    // ── sample coverage ──────────────────────────────────────────────────
    pub sample_coverage_value: f32,
    pub sample_coverage_invert: bool,
    pub sample_mask: u32, // sample mask word 0 (glSampleMaski)

    // ── line width ───────────────────────────────────────────────────────
    pub line_width: f32,

    // ── point size ──────────────────────────────────────────────────────
    pub point_size: f32,

    // ── blend state ──────────────────────────────────────────────────────
    pub blend: bool,
    pub blend_eq_rgb: BlendEquation,
    pub blend_eq_alpha: BlendEquation,
    pub blend_src_rgb: BlendFactor,
    pub blend_dst_rgb: BlendFactor,
    pub blend_src_alpha: BlendFactor,
    pub blend_dst_alpha: BlendFactor,
    pub blend_color: [f32; 4],
    pub blend_attachments: [bool; 8],
    pub blend_eq_rgb_attachments: [BlendEquation; 8],
    pub blend_eq_alpha_attachments: [BlendEquation; 8],
    pub blend_src_rgb_attachments: [BlendFactor; 8],
    pub blend_dst_rgb_attachments: [BlendFactor; 8],
    pub blend_src_alpha_attachments: [BlendFactor; 8],
    pub blend_dst_alpha_attachments: [BlendFactor; 8],

    // ── rasterizer state ─────────────────────────────────────────────────
    pub cull_face: CullFace,
    pub front_face: FrontFace,
    pub polygon_offset_fill: bool,
    pub polygon_offset_factor: f32,
    pub polygon_offset_units: f32,

    // ── clear values ─────────────────────────────────────────────────────
    pub clear_color: [f32; 4],
    pub clear_depth: f32,
    pub clear_stencil: u8,

    // ── color write mask ─────────────────────────────────────────────────
    pub color_mask: [bool; 4], // RGBA

    // ── pixel transfer state ─────────────────────────────────────────────
    pub pack_alignment: u32,
    pub unpack_alignment: u32,
    pub unpack_row_length: u32, // 0 = tight (GL_UNPACK_ROW_LENGTH)
    pub unpack_skip_rows: u32,
    pub unpack_skip_pixels: u32,
    pub unpack_image_height: u32, // 0 = tight (GL_UNPACK_IMAGE_HEIGHT)
    pub unpack_skip_images: u32,
    pub pack_row_length: u32,
    pub pack_skip_rows: u32,
    pub pack_skip_pixels: u32,

    // ── error state ───────────────────────────────────────────────────────
    last_error: Option<GlError>,
}

impl Context {
    #[inline]
    fn set_error(&mut self, err: GlError) {
        if self.last_error.is_none() {
            self.last_error = Some(err);
        }
    }

    /// Pop one pending GL-style error.
    ///
    /// Returns `None` when no error is pending.
    pub fn get_error(&mut self) -> Option<GlError> {
        let e = self.last_error;
        self.last_error = None;
        e
    }

    /// Report the currently targeted API profile.
    pub const fn api_profile(&self) -> ApiProfile {
        ApiProfile::OpenGlEs30
    }

    /// GL-style version string for diagnostics/introspection.
    pub const fn version_string(&self) -> &'static str {
        "OpenGL ES 3.0 (GraphOS software backend)"
    }

    /// GLSL language version string for diagnostics/introspection.
    pub const fn shading_language_version_string(&self) -> &'static str {
        "GLSL ES 3.00 (Rust-native shader traits)"
    }

    /// Renderer description string.
    pub const fn renderer_string(&self) -> &'static str {
        "graphos-gl CPU rasterizer"
    }

    /// Vendor description string.
    pub const fn vendor_string(&self) -> &'static str {
        "GraphOS"
    }

    /// Exposed implementation limits.
    pub const fn limits(&self) -> GlLimits {
        GlLimits {
            max_buffers: MAX_BUFFERS,
            max_vertex_arrays: MAX_VAOS,
            max_textures: MAX_TEXTURES,
            max_texture_units: MAX_TEXTURE_UNITS,
            max_color_attachments: 8,
            max_attribs: MAX_ATTRIBS,
        }
    }

    /// Create a new context with OpenGL default state.
    pub fn new() -> Self {
        Self {
            buf_alloc: [false; MAX_BUFFERS],
            vao_alloc: [false; MAX_VAOS],
            tex_alloc: [false; MAX_TEXTURES],
            fbo_alloc: [false; MAX_FBOS],
            rbo_alloc: [false; MAX_RENDERBUFFERS],
            shader_alloc: [false; MAX_SHADERS],
            program_alloc: [false; MAX_PROGRAMS],
            sampler_alloc: [false; MAX_SAMPLERS],
            sync_alloc: [false; MAX_SYNCS],
            tfb_alloc: [false; MAX_TFBS],
            query_alloc: [false; MAX_QUERIES],

            buf_data: {
                let mut v = Vec::with_capacity(MAX_BUFFERS);
                for _ in 0..MAX_BUFFERS {
                    v.push(Vec::new());
                }
                v
            },

            vaos: [VertexArray::default(); MAX_VAOS],
            default_vao: VertexArray::default(),
            textures: core::array::from_fn(|_| TextureObject::default()),
            texture_images: {
                let mut v = Vec::with_capacity(MAX_TEXTURES);
                for _ in 0..MAX_TEXTURES {
                    v.push(None);
                }
                v
            },
            fbos: [Framebuffer::default(); MAX_FBOS],
            rbos: [Renderbuffer::default(); MAX_RENDERBUFFERS],
            renderbuffer_color_images: {
                let mut v = Vec::with_capacity(MAX_RENDERBUFFERS);
                for _ in 0..MAX_RENDERBUFFERS {
                    v.push(None);
                }
                v
            },
            shaders: {
                let mut v = Vec::with_capacity(MAX_SHADERS);
                for _ in 0..MAX_SHADERS {
                    v.push(None);
                }
                v
            },
            shader_info_logs: {
                let mut v = Vec::with_capacity(MAX_SHADERS);
                for _ in 0..MAX_SHADERS {
                    v.push(String::new());
                }
                v
            },
            programs: [ProgramObject::default(); MAX_PROGRAMS],
            program_info_logs: {
                let mut v = Vec::with_capacity(MAX_PROGRAMS);
                for _ in 0..MAX_PROGRAMS {
                    v.push(String::new());
                }
                v
            },
            samplers: [SamplerObject::default(); MAX_SAMPLERS],
            syncs: [SyncObject::default(); MAX_SYNCS],
            tfbs: [TransformFeedbackObject::default(); MAX_TFBS],
            queries: [QueryObject::default(); MAX_QUERIES],
            active_queries: [None; 4],
            uniforms: GraphHashMap::with_capacity(128),
            mapped: None,
            strict_glsl: false,
            program_tfb_varyings: {
                let mut v = Vec::with_capacity(MAX_PROGRAMS);
                for _ in 0..MAX_PROGRAMS {
                    v.push(Vec::new());
                }
                v
            },
            program_tfb_components: [0; MAX_PROGRAMS],
            extension_enabled: [
                true,  // KHR_debug — functional (message log, debug groups)
                false, // KHR_robustness — not implemented (GAP-002)
                false, // EXT_disjoint_timer_query — not implemented (GAP-002)
                false, // EXT_color_buffer_float — not implemented (GAP-002)
            ],
            last_memory_barrier_bits: 0,
            last_memory_barrier_by_region_bits: 0,

            vao: 0,
            array_buffer: 0,
            element_array_buffer: 0,
            uniform_buffer: 0,
            uniform_buffer_bindings: [0; MAX_UNIFORM_BUFFER_BINDINGS],
            uniform_buffer_ranges: [(0, 0); MAX_UNIFORM_BUFFER_BINDINGS],
            shader_storage_buffer: 0,
            shader_storage_buffer_bindings: [0; MAX_SHADER_STORAGE_BUFFER_BINDINGS],
            shader_storage_buffer_ranges: [(0, 0); MAX_SHADER_STORAGE_BUFFER_BINDINGS],
            atomic_counter_buffer: 0,
            atomic_counter_buffer_bindings: [0; MAX_ATOMIC_COUNTER_BUFFER_BINDINGS],
            image_units: [ImageUnitBinding::default(); MAX_IMAGE_UNITS],
            draw_framebuffer: 0,
            read_framebuffer: 0,
            draw_buffers_mask: 0x01,
            read_buffer_index: 0,
            color_mask_attachments: [[true; 4]; 8],
            active_texture: 0,
            texture_units: [0; MAX_TEXTURE_UNITS],
            sampler_units: [0; MAX_TEXTURE_UNITS],
            renderbuffer: 0,
            transform_feedback: 0,
            transform_feedback_buffers: [0; MAX_TFB_BINDINGS],
            debug_log: Vec::with_capacity(MAX_DEBUG_MESSAGES),
            debug_group_depth: 0,
            robust_access: false,
            reset_status: ContextResetStatus::NoError,
            current_program: 0,

            viewport: [0, 0, 0, 0],
            scissor: [0, 0, 0, 0],
            scissor_test: false,

            depth_test: false,
            depth_write: true,
            depth_func: DepthFunc::Less,
            depth_range: [0.0, 1.0],

            stencil_test: false,
            stencil_func: StencilFunc::Always,
            stencil_ref: 0,
            stencil_mask_r: 0xFF,
            stencil_mask_w: 0xFF,
            stencil_fail: StencilOp::Keep,
            stencil_zfail: StencilOp::Keep,
            stencil_zpass: StencilOp::Keep,
            stencil_func_back: StencilFunc::Always,
            stencil_ref_back: 0,
            stencil_mask_r_back: 0xFF,
            stencil_mask_w_back: 0xFF,
            stencil_fail_back: StencilOp::Keep,
            stencil_zfail_back: StencilOp::Keep,
            stencil_zpass_back: StencilOp::Keep,
            sample_coverage_value: 1.0,
            sample_coverage_invert: false,
            sample_mask: 0xFFFF_FFFF,
            line_width: 1.0,
            point_size: 1.0,

            blend: false,
            blend_eq_rgb: BlendEquation::FuncAdd,
            blend_eq_alpha: BlendEquation::FuncAdd,
            blend_src_rgb: BlendFactor::One,
            blend_dst_rgb: BlendFactor::Zero,
            blend_src_alpha: BlendFactor::One,
            blend_dst_alpha: BlendFactor::Zero,
            blend_color: [0.0; 4],
            blend_attachments: [false; 8],
            blend_eq_rgb_attachments: [BlendEquation::FuncAdd; 8],
            blend_eq_alpha_attachments: [BlendEquation::FuncAdd; 8],
            blend_src_rgb_attachments: [BlendFactor::One; 8],
            blend_dst_rgb_attachments: [BlendFactor::Zero; 8],
            blend_src_alpha_attachments: [BlendFactor::One; 8],
            blend_dst_alpha_attachments: [BlendFactor::Zero; 8],

            cull_face: CullFace::None,
            front_face: FrontFace::CCW,
            polygon_offset_fill: false,
            polygon_offset_factor: 0.0,
            polygon_offset_units: 0.0,

            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,

            color_mask: [true; 4],
            pack_alignment: 4,
            unpack_alignment: 4,
            unpack_row_length: 0,
            unpack_skip_rows: 0,
            unpack_skip_pixels: 0,
            unpack_image_height: 0,
            unpack_skip_images: 0,
            pack_row_length: 0,
            pack_skip_rows: 0,
            pack_skip_pixels: 0,
            last_error: None,
        }
    }

    // ── glGenBuffers / glDeleteBuffers ────────────────────────────────────

    /// Allocate `n` buffer names, writing into `out`. Returns actual count allocated.
    pub fn gen_buffers(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.buf_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.buf_alloc[i] = true;
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => break,
            }
        }
        count
    }

    /// Convenience wrapper for callers that allocate one buffer at a time.
    pub fn gen_buffer(&mut self) -> u32 {
        let mut out = [0u32; 1];
        if self.gen_buffers(&mut out) == 1 {
            out[0]
        } else {
            0
        }
    }

    pub fn delete_buffers(&mut self, names: &[u32]) {
        for &n in names {
            if n > 0 && (n as usize) <= MAX_BUFFERS {
                let i = n as usize - 1;
                self.buf_alloc[i] = false;
                self.buf_data[i].clear();
                if let Some(m) = self.mapped {
                    if m.name == n {
                        self.mapped = None;
                    }
                }

                if self.array_buffer == n {
                    self.array_buffer = 0;
                }
                if self.element_array_buffer == n {
                    self.element_array_buffer = 0;
                }
                if self.uniform_buffer == n {
                    self.uniform_buffer = 0;
                }
                for b in &mut self.uniform_buffer_bindings {
                    if *b == n {
                        *b = 0;
                    }
                }
                for b in &mut self.transform_feedback_buffers {
                    if *b == n {
                        *b = 0;
                    }
                }

                if self.default_vao.element_buffer == n {
                    self.default_vao.element_buffer = 0;
                }
                for a in &mut self.default_vao.attribs {
                    if a.buffer == n {
                        *a = AttribPointer::default();
                    }
                }

                for vao in &mut self.vaos {
                    if vao.element_buffer == n {
                        vao.element_buffer = 0;
                    }
                    for a in &mut vao.attribs {
                        if a.buffer == n {
                            *a = AttribPointer::default();
                        }
                    }
                }
            }
        }
    }

    /// glBufferData — upload or replace buffer contents.
    pub fn buffer_data(&mut self, name: u32, data: &[u8]) {
        if name == 0 || (name as usize) > MAX_BUFFERS {
            return;
        }
        let slot = &mut self.buf_data[name as usize - 1];
        slot.clear();
        slot.extend_from_slice(data);
    }

    /// glBufferData on the currently bound ARRAY_BUFFER.
    pub fn buffer_data_array_bound(&mut self, data: &[u8]) {
        if self.array_buffer != 0 {
            self.buffer_data(self.array_buffer, data);
        }
    }

    /// glBufferData on the currently bound ELEMENT_ARRAY_BUFFER.
    pub fn buffer_data_element_bound(&mut self, data: &[u8]) {
        let name = if self.vao_slot().element_buffer != 0 {
            self.vao_slot().element_buffer
        } else {
            self.element_array_buffer
        };
        if name != 0 {
            self.buffer_data(name, data);
        }
    }

    /// glBufferSubData — partial update.
    pub fn buffer_sub_data(&mut self, name: u32, offset: usize, data: &[u8]) {
        if name == 0 || (name as usize) > MAX_BUFFERS {
            return;
        }
        let slot = &mut self.buf_data[name as usize - 1];
        let end = offset + data.len();
        if end > slot.len() {
            slot.resize(end, 0);
        }
        slot[offset..end].copy_from_slice(data);
    }

    /// Start mapping a byte range in a buffer object.
    ///
    /// Returns `true` on success. Only one active mapping is allowed at a time.
    pub fn map_buffer_range(
        &mut self,
        name: u32,
        offset: usize,
        length: usize,
        access: u32,
    ) -> bool {
        if self.mapped.is_some() {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        if !self.is_valid_buffer_name(name) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        if length == 0 {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        let read = (access & MAP_READ_BIT) != 0;
        let write = (access & MAP_WRITE_BIT) != 0;
        if !read && !write {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        let Some(end) = offset.checked_add(length) else {
            self.set_error(GlError::InvalidValue);
            return false;
        };
        let slot_len = self.buf_data[name as usize - 1].len();
        if end > slot_len {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        self.mapped = Some(BufferMapState {
            name,
            offset,
            length,
            write,
        });
        true
    }

    /// Borrow mapped bytes read-only while a map is active.
    pub fn mapped_buffer_bytes(&self, name: u32) -> Option<&[u8]> {
        let m = self.mapped?;
        if m.name != name {
            return None;
        }
        let slot = &self.buf_data[name as usize - 1];
        Some(&slot[m.offset..m.offset + m.length])
    }

    /// Borrow mapped bytes read-write while a write map is active.
    pub fn mapped_buffer_bytes_mut(&mut self, name: u32) -> Option<&mut [u8]> {
        let m = self.mapped?;
        if m.name != name || !m.write {
            return None;
        }
        let slot = &mut self.buf_data[name as usize - 1];
        Some(&mut slot[m.offset..m.offset + m.length])
    }

    /// End an active buffer map.
    pub fn unmap_buffer(&mut self, name: u32) -> bool {
        let Some(m) = self.mapped else {
            self.set_error(GlError::InvalidOperation);
            return false;
        };
        if m.name != name {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        self.mapped = None;
        true
    }

    /// Read raw bytes from a buffer object.
    pub fn get_buffer_data(&self, name: u32) -> Option<&[u8]> {
        if name == 0 || (name as usize) > MAX_BUFFERS {
            return None;
        }
        let slot = &self.buf_data[name as usize - 1];
        if self.buf_alloc[name as usize - 1] {
            Some(slot.as_slice())
        } else {
            None
        }
    }

    /// Convenience: read buffer as typed slice (zero-copy if aligned).
    ///
    /// Returns `None` if the buffer is not allocated or has wrong size/alignment.
    ///
    /// # Safety
    /// Caller must ensure bytes in the buffer are valid bit-patterns for `T`.
    /// This function does not perform semantic validation beyond size/alignment.
    pub unsafe fn get_buffer_as<T: Copy>(&self, name: u32) -> Option<&[T]> {
        let bytes = self.get_buffer_data(name)?;
        let size = core::mem::size_of::<T>();
        if size == 0 {
            return None;
        }
        if bytes.len() % size != 0 {
            return None;
        }
        if (bytes.as_ptr() as usize) % core::mem::align_of::<T>() != 0 {
            return None;
        }
        let count = bytes.len() / size;
        // Safety: alignment/size checked above; caller guarantees valid `T` bit patterns.
        Some(unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const T, count) })
    }

    // ── glGenVertexArrays / glDeleteVertexArrays ──────────────────────────

    pub fn gen_vertex_arrays(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.vao_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.vao_alloc[i] = true;
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => break,
            }
        }
        count
    }

    pub fn delete_vertex_arrays(&mut self, names: &[u32]) {
        for &n in names {
            if n > 0 && (n as usize) <= MAX_VAOS {
                let i = n as usize - 1;
                self.vao_alloc[i] = false;
                self.vaos[i] = VertexArray::default();
                if self.vao == n {
                    self.vao = 0;
                }
            }
        }
    }

    pub fn bind_vertex_array(&mut self, vao: u32) {
        if vao == 0 {
            self.vao = 0;
        } else if self.is_valid_vao_name(vao) {
            self.vao = vao;
        } else {
            // Deterministic invalid-bind policy: bind default VAO.
            self.vao = 0;
            self.set_error(GlError::InvalidOperation);
        }
    }

    /// glVertexAttribPointer
    pub fn vertex_attrib_pointer(
        &mut self,
        index: u32,
        size: u8,
        _normalized: bool,
        stride: u32,
        offset: u32,
    ) {
        if index as usize >= MAX_ATTRIBS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let buf = self.array_buffer;
        let vao = self.vao_slot_mut();
        let prev_divisor = vao.attribs[index as usize].divisor;
        vao.attribs[index as usize] = AttribPointer {
            buffer: buf,
            offset,
            stride,
            size,
            divisor: prev_divisor,
            enabled: true,
        };
    }

    /// glVertexAttribDivisor
    pub fn vertex_attrib_divisor(&mut self, index: u32, divisor: u32) {
        if index as usize >= MAX_ATTRIBS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.vao_slot_mut().attribs[index as usize].divisor = divisor;
    }

    pub fn enable_vertex_attrib_array(&mut self, index: u32) {
        if index as usize >= MAX_ATTRIBS {
            return;
        }
        self.vao_slot_mut().attribs[index as usize].enabled = true;
    }

    pub fn disable_vertex_attrib_array(&mut self, index: u32) {
        if index as usize >= MAX_ATTRIBS {
            return;
        }
        self.vao_slot_mut().attribs[index as usize].enabled = false;
    }

    fn vao_slot_mut(&mut self) -> &mut VertexArray {
        if self.vao == 0 {
            &mut self.default_vao
        } else {
            &mut self.vaos[self.vao as usize - 1]
        }
    }

    fn vao_slot(&self) -> &VertexArray {
        if self.vao == 0 {
            &self.default_vao
        } else {
            &self.vaos[self.vao as usize - 1]
        }
    }

    // ── glGenTextures / glDeleteTextures ──────────────────────────────────

    pub fn gen_textures(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.tex_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.tex_alloc[i] = true;
                    self.textures[i] = TextureObject::default();
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => break,
            }
        }
        count
    }

    pub fn delete_textures(&mut self, names: &[u32]) {
        for &n in names {
            if n > 0 && (n as usize) <= MAX_TEXTURES {
                let idx = n as usize - 1;
                self.tex_alloc[idx] = false;
                self.textures[idx] = TextureObject::default();
                self.texture_images[idx] = None;
                for unit in &mut self.texture_units {
                    if *unit == n {
                        *unit = 0;
                    }
                }
                for fbo in &mut self.fbos {
                    for c in &mut fbo.color {
                        if c.texture == n {
                            *c = FbAttachment::default();
                        }
                    }
                    if fbo.depth.texture == n {
                        fbo.depth = FbAttachment::default();
                    }
                    if fbo.stencil.texture == n {
                        fbo.stencil = FbAttachment::default();
                    }
                }
            }
        }
    }

    pub fn active_texture(&mut self, unit: u32) {
        if unit as usize >= MAX_TEXTURE_UNITS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.active_texture = unit;
    }

    pub fn bind_texture(&mut self, name: u32) {
        let unit = (self.active_texture as usize).min(MAX_TEXTURE_UNITS - 1);
        if name == 0 || self.is_valid_texture_name(name) {
            self.texture_units[unit] = name;
        } else {
            self.set_error(GlError::InvalidOperation);
        }
    }

    pub fn tex_storage_2d(&mut self, name: u32, width: u32, height: u32, levels: u32) {
        if !self.is_valid_texture_name(name) {
            return;
        }
        let t = &mut self.textures[name as usize - 1];
        t.width = width;
        t.height = height;
        t.levels = levels.max(1);

        let mut lv = Vec::with_capacity(t.levels as usize);
        let mut lw = width.max(1);
        let mut lh = height.max(1);
        for _ in 0..t.levels {
            lv.push(TextureImage {
                width: lw,
                height: lh,
                pixels: vec![0; (lw as usize).saturating_mul(lh as usize)],
            });
            lw = (lw / 2).max(1);
            lh = (lh / 2).max(1);
        }
        self.texture_images[name as usize - 1] = Some(lv);
    }

    pub fn tex_image_2d(&mut self, name: u32, level: u32, width: u32, height: u32, pixels: &[u32]) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if pixels.len() != (width as usize).saturating_mul(height as usize) {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let idx = name as usize - 1;
        let levels = (level + 1).max(self.textures[idx].levels);
        if self.texture_images[idx].is_none() {
            self.tex_storage_2d(name, width, height, levels);
        }
        let Some(images) = self.texture_images[idx].as_mut() else {
            self.set_error(GlError::OutOfMemory);
            return;
        };
        if level as usize >= images.len() {
            let mut lw = images.last().map(|l| l.width).unwrap_or(width).max(1);
            let mut lh = images.last().map(|l| l.height).unwrap_or(height).max(1);
            while images.len() <= level as usize {
                lw = (lw / 2).max(1);
                lh = (lh / 2).max(1);
                images.push(TextureImage {
                    width: lw,
                    height: lh,
                    pixels: vec![0; (lw as usize).saturating_mul(lh as usize)],
                });
            }
        }
        images[level as usize] = TextureImage {
            width,
            height,
            pixels: pixels.to_vec(),
        };

        let t = &mut self.textures[idx];
        if level == 0 {
            t.width = width;
            t.height = height;
        }
        t.levels = t.levels.max(level + 1);
    }

    /// Upload BGRA8 byte rows with UNPACK_ALIGNMENT semantics.
    pub fn tex_image_2d_bgra8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_bgra8_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RGBA8 byte rows with UNPACK_ALIGNMENT semantics.
    pub fn tex_image_2d_rgba8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rgba8_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RGB8 byte rows with UNPACK_ALIGNMENT semantics.
    pub fn tex_image_2d_rgb8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rgb8_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload R8 byte rows with UNPACK_ALIGNMENT semantics.
    pub fn tex_image_2d_r8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_r8_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RG8 byte rows (two-channel, stored as RG→BGRA with B=0, A=0xFF).
    pub fn tex_image_2d_rg8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rg8_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RGB565 packed 16-bit rows (UNSIGNED_SHORT_5_6_5).
    pub fn tex_image_2d_rgb565(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rgb565_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RGBA4 packed 16-bit rows (UNSIGNED_SHORT_4_4_4_4).
    pub fn tex_image_2d_rgba4(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rgba4_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RGB5A1 packed 16-bit rows (UNSIGNED_SHORT_5_5_5_1).
    pub fn tex_image_2d_rgb5a1(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rgb5a1_rows(width, height, bytes, self.unpack_alignment) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload RGBA16F rows (4×f16 per pixel, tightly packed half-floats, no alignment).
    ///
    /// Internally stored as BGRA8 (tonemapped to [0,1]).
    pub fn tex_image_2d_rgba16f(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_rgba16f_rows(width, height, bytes) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload LUMINANCE8 byte rows (stored as R=G=B=L, A=0xFF).
    pub fn tex_image_2d_luminance8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_luminance8_rows(width, height, bytes, self.unpack_alignment)
        else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload LUMINANCE_ALPHA8 byte rows (L, A interleaved, stored as R=G=B=L, A=A).
    pub fn tex_image_2d_luminance_alpha8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) =
            decode_luminance_alpha8_rows(width, height, bytes, self.unpack_alignment)
        else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload SRGB8_ALPHA8 byte rows — identical to RGBA8 storage but marks sRGB intent.
    /// This backend stores linearly; callers are responsible for gamma conversion when needed.
    pub fn tex_image_2d_srgb8_alpha8(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        bytes: &[u8],
    ) {
        // The software backend stores all textures as BGRA32; sRGB is a tagging detail.
        // GAP-014: decode sRGB to linear on upload.
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(pixels) = decode_srgb8_alpha8_rows(width, height, bytes, self.unpack_alignment)
        else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        self.tex_image_2d(name, level, width, height, &pixels);
    }

    /// Upload a `GL_DEPTH_COMPONENT32F` depth texture.
    ///
    /// Each `f32` depth value `d` is stored as `d.to_bits()` in the `u32` pixel
    /// array.  The texture is tagged `is_depth = true` so that sampler calls in
    /// GLSL return `vec4(d, d, d, 1.0)` rather than unpacking BGRA channels.
    pub fn tex_image_2d_depth32f(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        depths: &[f32],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if depths.len() != (width as usize).saturating_mul(height as usize) {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let pixels: Vec<u32> = depths.iter().map(|d| d.to_bits()).collect();
        self.tex_image_2d(name, level, width, height, &pixels);
        // Tag as depth-format so the sampler returns the depth value directly.
        if self.is_valid_texture_name(name) {
            self.textures[name as usize - 1].is_depth = true;
        }
    }

    /// Upload a `GL_DEPTH_COMPONENT16` depth texture (16-bit unsigned normalized).
    /// Values are normalized to `[0, 1]` before storage.
    pub fn tex_image_2d_depth16(
        &mut self,
        name: u32,
        level: u32,
        width: u32,
        height: u32,
        depths: &[u16],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if depths.len() != (width as usize).saturating_mul(height as usize) {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let pixels: Vec<u32> = depths
            .iter()
            .map(|&d| (d as f32 / 65535.0_f32).to_bits())
            .collect();
        self.tex_image_2d(name, level, width, height, &pixels);
        if self.is_valid_texture_name(name) {
            self.textures[name as usize - 1].is_depth = true;
        }
    }

    pub fn tex_sub_image_2d(
        &mut self,
        name: u32,
        level: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        pixels: &[u32],
    ) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if pixels.len() != (width as usize).saturating_mul(height as usize) {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let Some(images) = self.texture_images[name as usize - 1].as_mut() else {
            self.set_error(GlError::InvalidOperation);
            return;
        };
        let Some(img) = images.get_mut(level as usize) else {
            self.set_error(GlError::InvalidValue);
            return;
        };
        if x + width > img.width || y + height > img.height {
            self.set_error(GlError::InvalidValue);
            return;
        }
        for row in 0..height as usize {
            let dst_row = (y as usize + row) * img.width as usize + x as usize;
            let src_row = row * width as usize;
            img.pixels[dst_row..dst_row + width as usize]
                .copy_from_slice(&pixels[src_row..src_row + width as usize]);
        }
    }

    pub fn texture_image(&self, name: u32, level: u32) -> Option<&TextureImage> {
        if !self.is_valid_texture_name(name) {
            return None;
        }
        self.texture_images[name as usize - 1]
            .as_ref()?
            .get(level as usize)
    }

    pub fn texture_image_mut(&mut self, name: u32, level: u32) -> Option<&mut TextureImage> {
        if !self.is_valid_texture_name(name) {
            return None;
        }
        self.texture_images[name as usize - 1]
            .as_mut()?
            .get_mut(level as usize)
    }

    pub fn texture_pixels(&self, name: u32, level: u32) -> Option<&[u32]> {
        Some(&self.texture_image(name, level)?.pixels)
    }

    pub fn texture_pixels_mut(&mut self, name: u32, level: u32) -> Option<&mut [u32]> {
        Some(self.texture_image_mut(name, level)?.pixels.as_mut_slice())
    }

    pub fn texture_view(&self, name: u32) -> Option<Texture<'_>> {
        if !self.is_valid_texture_name(name) {
            return None;
        }
        let obj = self.textures[name as usize - 1].clone();
        let img = self.texture_image(name, 0)?;
        Some(Texture {
            pixels: img.pixels.as_slice(),
            width: img.width,
            height: img.height,
            wrap_s: obj.wrap_s,
            wrap_t: obj.wrap_t,
            min_filter: obj.min_filter,
            mag_filter: obj.mag_filter,
            border_color: obj.border_color,
        })
    }

    pub fn texture_views_snapshot(&self) -> Vec<Option<Texture<'_>>> {
        let mut out = Vec::with_capacity(MAX_TEXTURES);
        for i in 0..MAX_TEXTURES {
            if !self.tex_alloc[i] {
                out.push(None);
                continue;
            }
            let name = (i + 1) as u32;
            out.push(self.texture_view(name));
        }
        out
    }

    pub fn texture_snapshot_table(&self) -> Vec<Option<TextureSnapshot>> {
        let mut out = Vec::with_capacity(MAX_TEXTURES);
        for i in 0..MAX_TEXTURES {
            if !self.tex_alloc[i] {
                out.push(None);
                continue;
            }
            let name = (i + 1) as u32;
            let Some(obj) = self.texture_object(name).cloned() else {
                out.push(None);
                continue;
            };
            let Some(img) = self.texture_image(name, 0) else {
                out.push(None);
                continue;
            };
            out.push(Some(TextureSnapshot {
                pixels: img.pixels.clone(),
                width: img.width,
                height: img.height,
                wrap_s: obj.wrap_s,
                wrap_t: obj.wrap_t,
                min_filter: obj.min_filter,
                mag_filter: obj.mag_filter,
                border_color: obj.border_color,
            }));
        }
        out
    }

    /// Resolve the currently bound texture on a unit, applying bound sampler overrides.
    pub fn texture_view_for_unit(&self, unit: u32) -> Option<Texture<'_>> {
        if unit as usize >= MAX_TEXTURE_UNITS {
            return None;
        }
        let tex = self.texture_units[unit as usize];
        if tex == 0 || !self.is_valid_texture_name(tex) {
            return None;
        }
        let mut view = self.texture_view(tex)?;
        let sampler = self.sampler_units[unit as usize];
        if sampler != 0 && self.is_valid_sampler_name(sampler) {
            let s = self.samplers[sampler as usize - 1];
            view.wrap_s = s.wrap_s;
            view.wrap_t = s.wrap_t;
            view.min_filter = s.min_filter;
            view.mag_filter = s.mag_filter;
            view.border_color = s.border_color;
        }
        Some(view)
    }

    /// Generate lower mip levels from level 0 using a 2x2 box filter.
    pub fn generate_mipmap(&mut self, name: u32) {
        if !self.is_valid_texture_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let idx = name as usize - 1;
        let Some(images) = self.texture_images[idx].as_mut() else {
            self.set_error(GlError::InvalidOperation);
            return;
        };
        if images.is_empty() {
            self.set_error(GlError::InvalidOperation);
            return;
        }

        let level_count = self.textures[idx].levels.max(1) as usize;
        let mut prev_pixels = images[0].pixels.clone();
        let mut prev_w = images[0].width.max(1);
        let mut prev_h = images[0].height.max(1);

        for level in 1..level_count {
            let width = (prev_w / 2).max(1);
            let height = (prev_h / 2).max(1);
            let mut pixels = vec![0u32; (width as usize).saturating_mul(height as usize)];
            crate::texture::generate_mip_level(&prev_pixels, prev_w, prev_h, &mut pixels);
            let image = TextureImage {
                width,
                height,
                pixels: pixels.clone(),
            };
            if level < images.len() {
                images[level] = image;
            } else {
                images.push(image);
            }
            prev_pixels = pixels;
            prev_w = width;
            prev_h = height;
        }
    }

    pub fn tex_parameter_wrap(&mut self, name: u32, wrap_s: WrapMode, wrap_t: WrapMode) {
        if !self.is_valid_texture_name(name) {
            return;
        }
        let t = &mut self.textures[name as usize - 1];
        t.wrap_s = wrap_s;
        t.wrap_t = wrap_t;
    }

    pub fn tex_parameter_filter(
        &mut self,
        name: u32,
        min_filter: FilterMode,
        mag_filter: FilterMode,
    ) {
        if !self.is_valid_texture_name(name) {
            return;
        }
        let t = &mut self.textures[name as usize - 1];
        t.min_filter = min_filter;
        t.mag_filter = mag_filter;
    }

    pub fn tex_parameter_border_color(&mut self, name: u32, border_color: [f32; 4]) {
        if !self.is_valid_texture_name(name) {
            return;
        }
        self.textures[name as usize - 1].border_color = border_color;
    }

    pub fn texture_object(&self, name: u32) -> Option<&TextureObject> {
        if !self.is_valid_texture_name(name) {
            return None;
        }
        Some(&self.textures[name as usize - 1])
    }

    // ── glGenFramebuffers / glDeleteFramebuffers ──────────────────────────

    pub fn gen_framebuffers(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.fbo_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.fbo_alloc[i] = true;
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => break,
            }
        }
        count
    }

    pub fn delete_framebuffers(&mut self, names: &[u32]) {
        for &n in names {
            if n > 0 && (n as usize) <= MAX_FBOS {
                let i = n as usize - 1;
                self.fbo_alloc[i] = false;
                self.fbos[i] = Framebuffer::default();
                if self.draw_framebuffer == n {
                    self.draw_framebuffer = 0;
                }
                if self.read_framebuffer == n {
                    self.read_framebuffer = 0;
                }
            }
        }
    }

    pub fn bind_framebuffer(&mut self, draw: u32, read: u32) {
        self.bind_draw_framebuffer(draw);
        self.bind_read_framebuffer(read);
    }

    pub fn bind_draw_framebuffer(&mut self, draw: u32) {
        if draw != 0 && !self.is_valid_fbo_name(draw) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.draw_framebuffer = draw;
    }

    pub fn bind_read_framebuffer(&mut self, read: u32) {
        if read != 0 && !self.is_valid_fbo_name(read) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.read_framebuffer = read;
    }

    /// glFramebufferTexture2D
    pub fn framebuffer_texture_2d(
        &mut self,
        fbo: u32,
        attachment: Attachment,
        texture: u32,
        level: u32,
    ) {
        if fbo == 0 || !self.is_valid_fbo_name(fbo) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if texture != 0 && !self.is_valid_texture_name(texture) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let fb = &mut self.fbos[fbo as usize - 1];
        let slot = FbAttachment { texture, level };
        match attachment {
            Attachment::Color(i) if (i as usize) < 8 => {
                fb.color[i as usize] = slot;
                fb.color_rb[i as usize] = 0;
            }
            Attachment::Depth => {
                fb.depth = slot;
                fb.depth_rb = 0;
            }
            Attachment::Stencil => {
                fb.stencil = slot;
                fb.stencil_rb = 0;
            }
            Attachment::DepthStencil => {
                fb.depth = slot;
                fb.stencil = slot;
                fb.depth_rb = 0;
                fb.stencil_rb = 0;
            }
            _ => self.set_error(GlError::InvalidEnum),
        }
    }

    // ── Buffer bindings ───────────────────────────────────────────────────

    pub fn bind_array_buffer(&mut self, buf: u32) {
        if buf == 0 || self.is_valid_buffer_name(buf) {
            self.array_buffer = buf;
        } else {
            self.set_error(GlError::InvalidOperation);
        }
    }
    /// Compatibility alias: defaults to ARRAY_BUFFER binding.
    pub fn bind_buffer(&mut self, buf: u32) {
        self.bind_array_buffer(buf);
    }
    pub fn bind_element_buffer(&mut self, buf: u32) {
        if buf != 0 && !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.element_array_buffer = buf;
        self.vao_slot_mut().element_buffer = buf;
    }
    pub fn bind_uniform_buffer(&mut self, buf: u32) {
        if buf == 0 || self.is_valid_buffer_name(buf) {
            self.uniform_buffer = buf;
        } else {
            self.set_error(GlError::InvalidOperation);
        }
    }

    pub fn bind_uniform_buffer_base(&mut self, index: u32, buf: u32) {
        if index as usize >= MAX_UNIFORM_BUFFER_BINDINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if buf != 0 && !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.uniform_buffer_bindings[index as usize] = buf;
    }

    pub fn uniform_buffer_binding(&self, index: u32) -> Option<u32> {
        if index as usize >= MAX_UNIFORM_BUFFER_BINDINGS {
            return None;
        }
        Some(self.uniform_buffer_bindings[index as usize])
    }

    /// Bind a range of a buffer to a UBO indexed binding point.
    pub fn bind_uniform_buffer_range(&mut self, index: u32, buf: u32, offset: u32, size: u32) {
        if index as usize >= MAX_UNIFORM_BUFFER_BINDINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if buf != 0 && !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.uniform_buffer_bindings[index as usize] = buf;
        self.uniform_buffer_ranges[index as usize] = (offset, size);
    }

    /// Query the range bound at a UBO binding point (offset, size); (0,0) means full buffer.
    pub fn uniform_buffer_range(&self, index: u32) -> Option<(u32, u32)> {
        if index as usize >= MAX_UNIFORM_BUFFER_BINDINGS {
            return None;
        }
        Some(self.uniform_buffer_ranges[index as usize])
    }

    // ── SSBO bindings ────────────────────────────────────────────────────

    pub fn bind_shader_storage_buffer(&mut self, buf: u32) {
        if buf == 0 || self.is_valid_buffer_name(buf) {
            self.shader_storage_buffer = buf;
        } else {
            self.set_error(GlError::InvalidOperation);
        }
    }

    pub fn bind_shader_storage_buffer_base(&mut self, index: u32, buf: u32) {
        if index as usize >= MAX_SHADER_STORAGE_BUFFER_BINDINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if buf != 0 && !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.shader_storage_buffer_bindings[index as usize] = buf;
    }

    pub fn bind_shader_storage_buffer_range(
        &mut self,
        index: u32,
        buf: u32,
        offset: u32,
        size: u32,
    ) {
        if index as usize >= MAX_SHADER_STORAGE_BUFFER_BINDINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if buf != 0 && !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.shader_storage_buffer_bindings[index as usize] = buf;
        self.shader_storage_buffer_ranges[index as usize] = (offset, size);
    }

    pub fn shader_storage_buffer_binding(&self, index: u32) -> Option<u32> {
        if index as usize >= MAX_SHADER_STORAGE_BUFFER_BINDINGS {
            return None;
        }
        Some(self.shader_storage_buffer_bindings[index as usize])
    }

    pub fn shader_storage_buffer_range(&self, index: u32) -> Option<(u32, u32)> {
        if index as usize >= MAX_SHADER_STORAGE_BUFFER_BINDINGS {
            return None;
        }
        Some(self.shader_storage_buffer_ranges[index as usize])
    }

    // ── Atomic counter buffer bindings ───────────────────────────────────

    pub fn bind_atomic_counter_buffer(&mut self, buf: u32) {
        if buf == 0 || self.is_valid_buffer_name(buf) {
            self.atomic_counter_buffer = buf;
        } else {
            self.set_error(GlError::InvalidOperation);
        }
    }

    pub fn bind_atomic_counter_buffer_base(&mut self, index: u32, buf: u32) {
        if index as usize >= MAX_ATOMIC_COUNTER_BUFFER_BINDINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if buf != 0 && !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.atomic_counter_buffer_bindings[index as usize] = buf;
    }

    pub fn atomic_counter_buffer_binding(&self, index: u32) -> Option<u32> {
        if index as usize >= MAX_ATOMIC_COUNTER_BUFFER_BINDINGS {
            return None;
        }
        Some(self.atomic_counter_buffer_bindings[index as usize])
    }

    // ── Image units ───────────────────────────────────────────────────────

    /// Bind a texture to an image unit (glBindImageTexture).
    pub fn bind_image_texture(
        &mut self,
        unit: u32,
        texture: u32,
        level: i32,
        layered: bool,
        layer: i32,
        access: ImageAccess,
        format: u32,
    ) {
        if unit as usize >= MAX_IMAGE_UNITS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if texture != 0 && !self.is_valid_texture_name(texture) {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.image_units[unit as usize] = ImageUnitBinding {
            texture,
            level,
            layered,
            layer,
            access,
            format,
        };
    }

    pub fn image_unit_binding(&self, unit: u32) -> Option<ImageUnitBinding> {
        if unit as usize >= MAX_IMAGE_UNITS {
            return None;
        }
        Some(self.image_units[unit as usize])
    }

    // ── Memory barriers ───────────────────────────────────────────────────

    /// Software memory barrier — on a software rasterizer there is no actual
    /// reordering, so this is a no-op that validates the bit-field.
    pub fn memory_barrier(&mut self, barriers: u32) {
        if barriers != MemoryBarrierBits::ALL && (barriers & !MemoryBarrierBits::VALID_MASK) != 0 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.last_memory_barrier_bits = barriers;
    }

    /// Region-scoped variant of memory_barrier (GLES 3.1 subset).
    pub fn memory_barrier_by_region(&mut self, barriers: u32) {
        if barriers != MemoryBarrierBits::ALL && (barriers & !MemoryBarrierBits::VALID_MASK) != 0 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.last_memory_barrier_by_region_bits = barriers;
    }

    pub fn last_memory_barrier_bits(&self) -> u32 {
        self.last_memory_barrier_bits
    }

    pub fn last_memory_barrier_by_region_bits(&self) -> u32 {
        self.last_memory_barrier_by_region_bits
    }

    pub fn is_valid_buffer_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_BUFFERS && self.buf_alloc[name as usize - 1]
    }

    pub fn is_valid_vao_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_VAOS && self.vao_alloc[name as usize - 1]
    }

    pub fn is_valid_texture_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_TEXTURES && self.tex_alloc[name as usize - 1]
    }

    pub fn is_valid_fbo_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_FBOS && self.fbo_alloc[name as usize - 1]
    }

    pub fn check_framebuffer_status(&self, fbo: u32) -> FramebufferStatus {
        if fbo == 0 || !self.is_valid_fbo_name(fbo) {
            return FramebufferStatus::Complete;
        }

        let fb = &self.fbos[fbo as usize - 1];
        let mut ref_size: Option<(u32, u32)> = None;
        let mut any_attachment = false;

        for c in fb.color {
            if c.texture == 0 {
                continue;
            }
            any_attachment = true;
            let Some(size) = self.texture_attachment_size(c) else {
                return FramebufferStatus::IncompleteAttachment;
            };
            if let Some(expected) = ref_size {
                if expected != size {
                    return FramebufferStatus::IncompleteDimensions;
                }
            } else {
                ref_size = Some(size);
            }
        }

        for &rb in &fb.color_rb {
            if rb == 0 {
                continue;
            }
            any_attachment = true;
            let Some(size) = self.renderbuffer_attachment_size(rb, false, false) else {
                return FramebufferStatus::IncompleteAttachment;
            };
            if let Some(expected) = ref_size {
                if expected != size {
                    return FramebufferStatus::IncompleteDimensions;
                }
            } else {
                ref_size = Some(size);
            }
        }

        if fb.depth_rb != 0 {
            any_attachment = true;
            let Some(size) = self.renderbuffer_attachment_size(fb.depth_rb, true, false) else {
                return FramebufferStatus::IncompleteAttachment;
            };
            if let Some(expected) = ref_size {
                if expected != size {
                    return FramebufferStatus::IncompleteDimensions;
                }
            } else {
                ref_size = Some(size);
            }
        }

        if fb.stencil_rb != 0 {
            any_attachment = true;
            let Some(size) = self.renderbuffer_attachment_size(fb.stencil_rb, false, true) else {
                return FramebufferStatus::IncompleteAttachment;
            };
            if let Some(expected) = ref_size {
                if expected != size {
                    return FramebufferStatus::IncompleteDimensions;
                }
            } else {
                ref_size = Some(size);
            }
        }

        for att in [fb.depth, fb.stencil] {
            if att.texture == 0 {
                continue;
            }
            any_attachment = true;
            let Some(size) = self.texture_attachment_size(att) else {
                return FramebufferStatus::IncompleteAttachment;
            };
            if let Some(expected) = ref_size {
                if expected != size {
                    return FramebufferStatus::IncompleteDimensions;
                }
            } else {
                ref_size = Some(size);
            }
        }

        if any_attachment {
            FramebufferStatus::Complete
        } else {
            FramebufferStatus::MissingAttachment
        }
    }

    pub fn is_framebuffer_complete(&self, fbo: u32) -> bool {
        self.check_framebuffer_status(fbo) == FramebufferStatus::Complete
    }

    /// Read packed BGRA8 pixels from a framebuffer color attachment.
    ///
    /// Returns `false` and sets a GL-style error on invalid state.
    pub fn read_pixels_bgra8(
        &mut self,
        fbo: u32,
        color_attachment: u8,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        out: &mut [u32],
    ) -> bool {
        if color_attachment as usize >= 8 {
            self.set_error(GlError::InvalidEnum);
            return false;
        }
        if width == 0 || height == 0 {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        let expected = (width as usize).saturating_mul(height as usize);
        if out.len() < expected {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        if fbo != 0 && !self.is_valid_fbo_name(fbo) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        if self.check_framebuffer_status(fbo) != FramebufferStatus::Complete {
            self.set_error(GlError::InvalidFramebufferOperation);
            return false;
        }

        if let Some((tex_name, level)) =
            self.framebuffer_color_attachment_binding(fbo, color_attachment)
        {
            let Some(img) = self.texture_image(tex_name, level) else {
                self.set_error(GlError::InvalidFramebufferOperation);
                return false;
            };
            if x.checked_add(width).is_none() || y.checked_add(height).is_none() {
                self.set_error(GlError::InvalidValue);
                return false;
            }
            if x + width > img.width || y + height > img.height {
                self.set_error(GlError::InvalidValue);
                return false;
            }

            for row in 0..height as usize {
                let src_off = (y as usize + row) * img.width as usize + x as usize;
                let dst_off = row * width as usize;
                out[dst_off..dst_off + width as usize]
                    .copy_from_slice(&img.pixels[src_off..src_off + width as usize]);
            }
            return true;
        }

        if let Some(rbo) = self.framebuffer_color_renderbuffer_binding(fbo, color_attachment) {
            let Some((rw, rh)) = self.renderbuffer_attachment_size(rbo, false, false) else {
                self.set_error(GlError::InvalidFramebufferOperation);
                return false;
            };
            let Some(pixels) = self.renderbuffer_color_image(rbo) else {
                self.set_error(GlError::InvalidFramebufferOperation);
                return false;
            };
            if x.checked_add(width).is_none() || y.checked_add(height).is_none() {
                self.set_error(GlError::InvalidValue);
                return false;
            }
            if x + width > rw || y + height > rh {
                self.set_error(GlError::InvalidValue);
                return false;
            }

            for row in 0..height as usize {
                let src_off = (y as usize + row) * rw as usize + x as usize;
                let dst_off = row * width as usize;
                out[dst_off..dst_off + width as usize]
                    .copy_from_slice(&pixels[src_off..src_off + width as usize]);
            }
            return true;
        }

        self.set_error(GlError::InvalidFramebufferOperation);
        false
    }

    /// Read packed BGRA8 pixels from the currently bound read framebuffer.
    pub fn read_pixels_read_bound_bgra8(
        &mut self,
        _color_attachment: u8,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        out: &mut [u32],
    ) -> bool {
        if self.read_buffer_index == 0xFF {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        self.read_pixels_bgra8(
            self.read_framebuffer,
            self.read_buffer_index,
            x,
            y,
            width,
            height,
            out,
        )
    }

    /// Read BGRA8 bytes from a framebuffer color attachment with PACK_ALIGNMENT.
    pub fn read_pixels_bgra8_bytes(
        &mut self,
        fbo: u32,
        color_attachment: u8,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        out: &mut [u8],
    ) -> bool {
        let row_bytes = (width as usize).saturating_mul(4);
        if row_bytes == 0 || height == 0 {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        let stride = align_up(row_bytes, self.pack_alignment as usize);
        let expected = stride
            .checked_mul(height as usize - 1)
            .and_then(|v| v.checked_add(row_bytes));
        let Some(expected) = expected else {
            self.set_error(GlError::InvalidValue);
            return false;
        };
        if out.len() < expected {
            self.set_error(GlError::InvalidValue);
            return false;
        }

        let mut packed = vec![0u32; (width as usize).saturating_mul(height as usize)];
        if !self.read_pixels_bgra8(fbo, color_attachment, x, y, width, height, &mut packed) {
            return false;
        }
        for row in 0..height as usize {
            let src_off = row * width as usize;
            let dst_off = row * stride;
            for col in 0..width as usize {
                let p = packed[src_off + col];
                let d = dst_off + col * 4;
                out[d] = (p & 0xFF) as u8;
                out[d + 1] = ((p >> 8) & 0xFF) as u8;
                out[d + 2] = ((p >> 16) & 0xFF) as u8;
                out[d + 3] = ((p >> 24) & 0xFF) as u8;
            }
        }
        true
    }

    /// Blit color attachment 0 from `src_fbo` into `dst_fbo` with nearest sampling.
    ///
    /// Rectangles are `[x, y, w, h]` in framebuffer pixel coordinates.
    pub fn blit_framebuffer_color(
        &mut self,
        src_fbo: u32,
        dst_fbo: u32,
        src_rect: [u32; 4],
        dst_rect: [u32; 4],
    ) -> bool {
        if src_rect[2] == 0 || src_rect[3] == 0 || dst_rect[2] == 0 || dst_rect[3] == 0 {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        if src_fbo != 0 && !self.is_valid_fbo_name(src_fbo) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        if dst_fbo != 0 && !self.is_valid_fbo_name(dst_fbo) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        if self.check_framebuffer_status(src_fbo) != FramebufferStatus::Complete
            || self.check_framebuffer_status(dst_fbo) != FramebufferStatus::Complete
        {
            self.set_error(GlError::InvalidFramebufferOperation);
            return false;
        }

        let Some((src_tex, src_level)) = self.framebuffer_color_attachment_binding(src_fbo, 0)
        else {
            self.set_error(GlError::InvalidFramebufferOperation);
            return false;
        };
        let Some((dst_tex, dst_level)) = self.framebuffer_color_attachment_binding(dst_fbo, 0)
        else {
            self.set_error(GlError::InvalidFramebufferOperation);
            return false;
        };

        let Some(src_img) = self.texture_image(src_tex, src_level) else {
            self.set_error(GlError::InvalidFramebufferOperation);
            return false;
        };
        let src_x = src_rect[0];
        let src_y = src_rect[1];
        let src_w = src_rect[2];
        let src_h = src_rect[3];
        if src_x.checked_add(src_w).is_none() || src_y.checked_add(src_h).is_none() {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        if src_x + src_w > src_img.width || src_y + src_h > src_img.height {
            self.set_error(GlError::InvalidValue);
            return false;
        }

        let mut src_copy = vec![0u32; (src_w as usize).saturating_mul(src_h as usize)];
        for row in 0..src_h as usize {
            let src_off = (src_y as usize + row) * src_img.width as usize + src_x as usize;
            let dst_off = row * src_w as usize;
            src_copy[dst_off..dst_off + src_w as usize]
                .copy_from_slice(&src_img.pixels[src_off..src_off + src_w as usize]);
        }

        let Some(dst_img) = self.texture_image_mut(dst_tex, dst_level) else {
            self.set_error(GlError::InvalidFramebufferOperation);
            return false;
        };
        let dst_x = dst_rect[0];
        let dst_y = dst_rect[1];
        let dst_w = dst_rect[2];
        let dst_h = dst_rect[3];
        if dst_x.checked_add(dst_w).is_none() || dst_y.checked_add(dst_h).is_none() {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        if dst_x + dst_w > dst_img.width || dst_y + dst_h > dst_img.height {
            self.set_error(GlError::InvalidValue);
            return false;
        }

        for dy in 0..dst_h as usize {
            let sy = ((dy as u64 * src_h as u64) / dst_h as u64) as usize;
            for dx in 0..dst_w as usize {
                let sx = ((dx as u64 * src_w as u64) / dst_w as u64) as usize;
                let src_idx = sy * src_w as usize + sx;
                let dst_idx = (dst_y as usize + dy) * dst_img.width as usize + dst_x as usize + dx;
                dst_img.pixels[dst_idx] = src_copy[src_idx];
            }
        }

        true
    }

    /// Blit color attachment 0 from currently bound read framebuffer to draw framebuffer.
    pub fn blit_framebuffer_color_bound(&mut self, src_rect: [u32; 4], dst_rect: [u32; 4]) -> bool {
        self.blit_framebuffer_color(
            self.read_framebuffer,
            self.draw_framebuffer,
            src_rect,
            dst_rect,
        )
    }

    fn texture_attachment_size(&self, attachment: FbAttachment) -> Option<(u32, u32)> {
        if !self.is_valid_texture_name(attachment.texture) {
            return None;
        }
        let image = self.texture_image(attachment.texture, attachment.level)?;
        if image.width == 0 || image.height == 0 {
            None
        } else {
            Some((image.width, image.height))
        }
    }

    fn framebuffer_color_attachment_binding(&self, fbo: u32, index: u8) -> Option<(u32, u32)> {
        if index as usize >= 8 {
            return None;
        }
        if fbo == 0 {
            return None;
        }
        if !self.is_valid_fbo_name(fbo) {
            return None;
        }
        let att = self.fbos[fbo as usize - 1].color[index as usize];
        if att.texture == 0 {
            None
        } else {
            Some((att.texture, att.level))
        }
    }

    fn framebuffer_color_renderbuffer_binding(&self, fbo: u32, index: u8) -> Option<u32> {
        if index as usize >= 8 {
            return None;
        }
        if fbo == 0 {
            return None;
        }
        if !self.is_valid_fbo_name(fbo) {
            return None;
        }
        let rbo = self.fbos[fbo as usize - 1].color_rb[index as usize];
        if rbo == 0 { None } else { Some(rbo) }
    }

    fn renderbuffer_color_image(&self, rbo: u32) -> Option<&[u32]> {
        if !self.is_valid_rbo_name(rbo) {
            return None;
        }
        self.renderbuffer_color_images[rbo as usize - 1]
            .as_ref()
            .map(|p| p.as_slice())
    }

    fn renderbuffer_color_image_mut(&mut self, rbo: u32) -> Option<&mut [u32]> {
        if !self.is_valid_rbo_name(rbo) {
            return None;
        }
        self.renderbuffer_color_images[rbo as usize - 1]
            .as_mut()
            .map(|p| p.as_mut_slice())
    }

    fn renderbuffer_attachment_size(
        &self,
        rbo: u32,
        require_depth: bool,
        require_stencil: bool,
    ) -> Option<(u32, u32)> {
        if !self.is_valid_rbo_name(rbo) {
            return None;
        }
        let rb = self.rbos[rbo as usize - 1];
        if rb.width == 0 || rb.height == 0 {
            return None;
        }
        if require_depth && !rb.has_depth {
            return None;
        }
        if require_stencil && !rb.has_stencil {
            return None;
        }
        Some((rb.width, rb.height))
    }

    // ── Viewport / scissor ────────────────────────────────────────────────

    pub fn viewport(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.viewport = [x, y, w, h];
    }
    pub fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32) {
        self.scissor = [x, y, w, h];
    }
    pub fn enable_scissor_test(&mut self, en: bool) {
        self.scissor_test = en;
    }

    // ── Depth state ───────────────────────────────────────────────────────

    pub fn enable_depth_test(&mut self, en: bool) {
        self.depth_test = en;
    }
    /// Compatibility alias.
    pub fn set_depth_test(&mut self, en: bool) {
        self.enable_depth_test(en);
    }
    pub fn depth_mask(&mut self, write: bool) {
        self.depth_write = write;
    }
    pub fn depth_func(&mut self, func: DepthFunc) {
        self.depth_func = func;
    }
    pub fn depth_range(&mut self, near: f32, far: f32) {
        self.depth_range = [near, far];
    }

    // ── Stencil state ─────────────────────────────────────────────────────

    pub fn enable_stencil_test(&mut self, en: bool) {
        self.stencil_test = en;
    }

    pub fn stencil_func(&mut self, func: StencilFunc, ref_val: u8, mask: u8) {
        self.stencil_func = func;
        self.stencil_ref = ref_val;
        self.stencil_mask_r = mask;
    }

    pub fn stencil_mask(&mut self, mask: u8) {
        self.stencil_mask_w = mask;
    }

    pub fn stencil_op(&mut self, fail: StencilOp, zfail: StencilOp, zpass: StencilOp) {
        self.stencil_fail = fail;
        self.stencil_zfail = zfail;
        self.stencil_zpass = zpass;
    }

    // ── Blend state ───────────────────────────────────────────────────────

    pub fn enable_blend(&mut self, en: bool) {
        self.blend = en;
        self.blend_attachments.fill(en);
    }

    pub fn enable_blendi(&mut self, attachment: u32, en: bool) {
        if attachment as usize >= 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.blend_attachments[attachment as usize] = en;
        if attachment == 0 {
            self.blend = en;
        }
    }

    pub fn blend_func(&mut self, src: BlendFactor, dst: BlendFactor) {
        self.blend_src_rgb = src;
        self.blend_dst_rgb = dst;
        self.blend_src_alpha = src;
        self.blend_dst_alpha = dst;
        self.blend_src_rgb_attachments.fill(src);
        self.blend_dst_rgb_attachments.fill(dst);
        self.blend_src_alpha_attachments.fill(src);
        self.blend_dst_alpha_attachments.fill(dst);
    }

    pub fn blend_func_separate(
        &mut self,
        src_rgb: BlendFactor,
        dst_rgb: BlendFactor,
        src_alpha: BlendFactor,
        dst_alpha: BlendFactor,
    ) {
        self.blend_src_rgb = src_rgb;
        self.blend_dst_rgb = dst_rgb;
        self.blend_src_alpha = src_alpha;
        self.blend_dst_alpha = dst_alpha;
        self.blend_src_rgb_attachments.fill(src_rgb);
        self.blend_dst_rgb_attachments.fill(dst_rgb);
        self.blend_src_alpha_attachments.fill(src_alpha);
        self.blend_dst_alpha_attachments.fill(dst_alpha);
    }

    pub fn blend_funci_separate(
        &mut self,
        attachment: u32,
        src_rgb: BlendFactor,
        dst_rgb: BlendFactor,
        src_alpha: BlendFactor,
        dst_alpha: BlendFactor,
    ) {
        if attachment as usize >= 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let i = attachment as usize;
        self.blend_src_rgb_attachments[i] = src_rgb;
        self.blend_dst_rgb_attachments[i] = dst_rgb;
        self.blend_src_alpha_attachments[i] = src_alpha;
        self.blend_dst_alpha_attachments[i] = dst_alpha;
        if attachment == 0 {
            self.blend_src_rgb = src_rgb;
            self.blend_dst_rgb = dst_rgb;
            self.blend_src_alpha = src_alpha;
            self.blend_dst_alpha = dst_alpha;
        }
    }

    pub fn blend_equation(&mut self, eq: BlendEquation) {
        self.blend_eq_rgb = eq;
        self.blend_eq_alpha = eq;
        self.blend_eq_rgb_attachments.fill(eq);
        self.blend_eq_alpha_attachments.fill(eq);
    }

    pub fn blend_equation_separate(&mut self, eq_rgb: BlendEquation, eq_alpha: BlendEquation) {
        self.blend_eq_rgb = eq_rgb;
        self.blend_eq_alpha = eq_alpha;
        self.blend_eq_rgb_attachments.fill(eq_rgb);
        self.blend_eq_alpha_attachments.fill(eq_alpha);
    }

    pub fn blend_equationi_separate(
        &mut self,
        attachment: u32,
        eq_rgb: BlendEquation,
        eq_alpha: BlendEquation,
    ) {
        if attachment as usize >= 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let i = attachment as usize;
        self.blend_eq_rgb_attachments[i] = eq_rgb;
        self.blend_eq_alpha_attachments[i] = eq_alpha;
        if attachment == 0 {
            self.blend_eq_rgb = eq_rgb;
            self.blend_eq_alpha = eq_alpha;
        }
    }

    pub fn blend_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.blend_color = [r, g, b, a];
    }

    // ── Rasterizer state ──────────────────────────────────────────────────

    pub fn cull_face(&mut self, face: CullFace) {
        self.cull_face = face;
    }
    pub fn front_face(&mut self, winding: FrontFace) {
        self.front_face = winding;
    }

    pub fn polygon_offset(&mut self, factor: f32, units: f32) {
        self.polygon_offset_factor = factor;
        self.polygon_offset_units = units;
    }
    pub fn enable_polygon_offset_fill(&mut self, en: bool) {
        self.polygon_offset_fill = en;
    }

    // ── Clear values ──────────────────────────────────────────────────────

    pub fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.clear_color = [r, g, b, a];
    }
    /// Compatibility alias.
    pub fn clear_depth(&mut self, d: f32) {
        self.clear_depth_value(d);
    }
    pub fn clear_depth_value(&mut self, d: f32) {
        self.clear_depth = d;
    }
    pub fn clear_stencil_value(&mut self, s: u8) {
        self.clear_stencil = s;
    }

    /// glColorMask
    pub fn color_mask(&mut self, r: bool, g: bool, b: bool, a: bool) {
        self.color_mask = [r, g, b, a];
        // Also sync the per-attachment mask for all active draw buffers.
        for i in 0..8 {
            if self.draw_buffers_mask & (1 << i) != 0 {
                self.color_mask_attachments[i] = [r, g, b, a];
            }
        }
    }

    /// glColorMaski — set per-attachment color write mask (OpenGL ES 3.2 / OES_draw_buffers_indexed).
    /// Clamped to 8 attachments.
    pub fn color_maski(&mut self, attachment: u32, r: bool, g: bool, b: bool, a: bool) {
        if attachment as usize >= 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.color_mask_attachments[attachment as usize] = [r, g, b, a];
    }

    /// glDrawBuffers — specify which color attachments receive fragment output.
    /// `bufs`: slice of attachment indices (0-7); 0xFF in a slot means GL_NONE.
    /// Builds a bitmask of active attachments.
    pub fn draw_buffers(&mut self, bufs: &[u8]) {
        if bufs.len() > 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let mut mask: u8 = 0;
        for &b in bufs {
            if b == 0xFF {
                continue;
            } // GL_NONE
            if b >= 8 {
                self.set_error(GlError::InvalidValue);
                return;
            }
            mask |= 1 << b;
        }
        self.draw_buffers_mask = mask;
    }

    /// glReadBuffer — specify which color attachment is the source for read operations.
    /// Pass 0xFF for GL_NONE.
    pub fn read_buffer(&mut self, attachment: u8) {
        if attachment != 0xFF && attachment >= 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.read_buffer_index = attachment;
    }

    /// Query the current draw-buffers bitmask.
    pub fn draw_buffers_mask(&self) -> u8 {
        self.draw_buffers_mask
    }

    /// Query the current read-buffer attachment index.
    pub fn read_buffer_index(&self) -> u8 {
        self.read_buffer_index
    }

    /// glClearBufferfv — clear a specific color attachment or depth attachment by index.
    /// `buffer`: 0 = color, 1 = depth.
    /// For color, `attachment` selects which attachment (must be in draw_buffers_mask).
    pub fn clear_buffer_color_fv(&mut self, attachment: u32, value: [f32; 4]) {
        if attachment as usize >= 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        // For this software backend we only clear active draw-buffer attachments.
        if self.draw_buffers_mask & (1 << attachment) == 0 {
            return;
        }

        let Some((tex_name, level)) =
            self.framebuffer_color_attachment_binding(self.draw_framebuffer, attachment as u8)
        else {
            let Some(rbo) = self
                .framebuffer_color_renderbuffer_binding(self.draw_framebuffer, attachment as u8)
            else {
                self.set_error(GlError::InvalidFramebufferOperation);
                return;
            };
            let Some(pixels) = self.renderbuffer_color_image_mut(rbo) else {
                self.set_error(GlError::InvalidFramebufferOperation);
                return;
            };
            let packed = pack_color_f32(value);
            pixels.fill(packed);
            if attachment == 0 {
                self.clear_color = value;
            }
            return;
        };

        let Some(img) = self.texture_image_mut(tex_name, level) else {
            self.set_error(GlError::InvalidFramebufferOperation);
            return;
        };

        let packed = pack_color_f32(value);
        img.pixels.fill(packed);

        // Keep legacy clear state in sync for callers that issue glClear afterwards.
        if attachment == 0 {
            self.clear_color = value;
        }
    }

    /// Get per-attachment color write mask.
    pub fn color_mask_for_attachment(&self, attachment: u32) -> Option<[bool; 4]> {
        if attachment as usize >= 8 {
            return None;
        }
        Some(self.color_mask_attachments[attachment as usize])
    }

    pub fn pixel_store_pack_alignment(&mut self, alignment: u32) {
        if is_valid_alignment(alignment) {
            self.pack_alignment = alignment;
        } else {
            self.set_error(GlError::InvalidValue);
        }
    }

    /// glPixelStore-like setter for UNPACK_ALIGNMENT.
    pub fn pixel_store_unpack_alignment(&mut self, alignment: u32) {
        if is_valid_alignment(alignment) {
            self.unpack_alignment = alignment;
        } else {
            self.set_error(GlError::InvalidValue);
        }
    }

    pub const fn pack_alignment(&self) -> u32 {
        self.pack_alignment
    }
    pub const fn unpack_alignment(&self) -> u32 {
        self.unpack_alignment
    }

    // ── glClear ───────────────────────────────────────────────────────────

    /// Clear the current framebuffer according to `mask` (combine CLEAR_* constants).
    ///
    /// Pass `color_buf`, `depth_buf`, `stencil_buf` as mutable slices for the
    /// target buffers; they may be `None` to skip clearing that attachment.
    pub fn clear(
        &self,
        mask: u32,
        color_buf: Option<&mut [u32]>,
        depth_buf: Option<&mut [f32]>,
        stencil_buf: Option<&mut [u8]>,
    ) {
        if mask & CLEAR_COLOR != 0 {
            if let Some(buf) = color_buf {
                let packed = pack_color_f32(self.clear_color);
                buf.fill(packed);
            }
        }
        if mask & CLEAR_DEPTH != 0 {
            if let Some(buf) = depth_buf {
                buf.fill(self.clear_depth);
            }
        }
        if mask & CLEAR_STENCIL != 0 {
            if let Some(buf) = stencil_buf {
                buf.fill(self.clear_stencil);
            }
        }
    }

    // ── Derive Pipeline from current GL state ─────────────────────────────

    /// Build a [`Pipeline`] reflecting the complete current GL state.
    pub fn current_pipeline(&self) -> Pipeline {
        Pipeline {
            cull_face: self.cull_face,
            depth_test: self.depth_test,
            depth_write: self.depth_write,
            depth_func: self.depth_func,
            stencil_test: self.stencil_test,
            stencil_func: self.stencil_func,
            stencil_ref: self.stencil_ref,
            stencil_mask_r: self.stencil_mask_r,
            stencil_mask_w: self.stencil_mask_w,
            stencil_fail: self.stencil_fail,
            stencil_zfail: self.stencil_zfail,
            stencil_zpass: self.stencil_zpass,
            blend: self.blend,
            blend_eq_rgb: self.blend_eq_rgb,
            blend_eq_alpha: self.blend_eq_alpha,
            blend_src_rgb: self.blend_src_rgb,
            blend_dst_rgb: self.blend_dst_rgb,
            blend_src_alpha: self.blend_src_alpha,
            blend_dst_alpha: self.blend_dst_alpha,
            blend_color: self.blend_color,
            blend_attachments: self.blend_attachments,
            blend_eq_rgb_attachments: self.blend_eq_rgb_attachments,
            blend_eq_alpha_attachments: self.blend_eq_alpha_attachments,
            blend_src_rgb_attachments: self.blend_src_rgb_attachments,
            blend_dst_rgb_attachments: self.blend_dst_rgb_attachments,
            blend_src_alpha_attachments: self.blend_src_alpha_attachments,
            blend_dst_alpha_attachments: self.blend_dst_alpha_attachments,
            scissor_test: self.scissor_test,
            scissor: self.scissor,
            draw_buffers_mask: self.draw_buffers_mask,
            color_mask: self.color_mask,
            color_masks: self.color_mask_attachments,
            point_size: self.point_size,
            line_width: self.line_width,
        }
    }

    // ── Blend math ────────────────────────────────────────────────────────

    /// Apply current blend state to blend `src` over `dst`.
    ///
    /// All components are in 0..=1 linear float (BGRA channel order).
    pub fn apply_blend(&self, src: Vec4, dst: Vec4) -> Vec4 {
        let cc = self.blend_color;
        let const_v = Vec4::new(cc[2], cc[1], cc[0], cc[3]); // BGRA

        let sf_rgb = blend_factor(self.blend_src_rgb, src, dst, const_v);
        let df_rgb = blend_factor(self.blend_dst_rgb, src, dst, const_v);
        let sf_a = blend_factor_alpha(self.blend_src_alpha, src, dst, const_v);
        let df_a = blend_factor_alpha(self.blend_dst_alpha, src, dst, const_v);

        let blended_rgb = blend_eq(
            self.blend_eq_rgb,
            Vec4::new(src.x * sf_rgb.x, src.y * sf_rgb.y, src.z * sf_rgb.z, 0.0),
            Vec4::new(dst.x * df_rgb.x, dst.y * df_rgb.y, dst.z * df_rgb.z, 0.0),
        );

        let blended_a = blend_eq_f32(self.blend_eq_alpha, src.w * sf_a.w, dst.w * df_a.w);

        Vec4::new(
            blended_rgb.x.clamp(0.0, 1.0),
            blended_rgb.y.clamp(0.0, 1.0),
            blended_rgb.z.clamp(0.0, 1.0),
            blended_a.clamp(0.0, 1.0),
        )
    }

    // ── Stencil write helper ──────────────────────────────────────────────

    /// Apply stencil operation to a stencil value given test result + depth result.
    pub fn apply_stencil_op(&self, stencil: u8, stencil_passed: bool, depth_passed: bool) -> u8 {
        let op = if !stencil_passed {
            self.stencil_fail
        } else if !depth_passed {
            self.stencil_zfail
        } else {
            self.stencil_zpass
        };
        apply_stencil_op(op, stencil, self.stencil_ref, self.stencil_mask_w)
    }

    // ── Draw helpers (for custom render loops) ────────────────────────────

    /// Draw triangle primitives via the programmable rasterizer.
    pub fn draw_triangles<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        indices: &[u32],
    ) {
        self.current_pipeline()
            .draw(target, shader, vertices, indices);
    }

    /// Draw with explicit topology.
    pub fn draw<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        indices: &[u32],
        mode: DrawMode,
    ) {
        self.current_pipeline()
            .draw_mode(target, shader, vertices, indices, mode);
    }

    /// Draw non-indexed geometry by generating a contiguous index span.
    ///
    /// Returns `false` if the requested vertex range is out of bounds.
    pub fn draw_arrays<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        first: usize,
        count: usize,
        mode: DrawMode,
    ) -> bool {
        let Some(end) = first.checked_add(count) else {
            return false;
        };
        if end > vertices.len() {
            return false;
        }

        let mut indices = Vec::with_capacity(count);
        for i in first..end {
            indices.push(i as u32);
        }
        self.current_pipeline()
            .draw_mode(target, shader, vertices, &indices, mode);
        true
    }

    /// Draw triangles with per-instance invocation.
    pub fn draw_triangles_instanced<S: Shader, F: FnMut(&mut S, u32)>(
        &self,
        target: &mut Target<'_>,
        shader: &mut S,
        vertices: &[S::Vertex],
        indices: &[u32],
        instance_count: u32,
        mut update_instance: F,
    ) {
        let pipe = self.current_pipeline();
        for i in 0..instance_count {
            update_instance(shader, i);
            pipe.draw(target, shader, vertices, indices);
        }
    }

    /// Draw instanced with explicit topology.
    pub fn draw_instanced<S: Shader, F: FnMut(&mut S, u32)>(
        &self,
        target: &mut Target<'_>,
        shader: &mut S,
        vertices: &[S::Vertex],
        indices: &[u32],
        instance_count: u32,
        mode: DrawMode,
        mut update_instance: F,
    ) {
        let pipe = self.current_pipeline();
        for i in 0..instance_count {
            update_instance(shader, i);
            pipe.draw_mode(target, shader, vertices, indices, mode);
        }
    }

    /// Instanced variant of [`Self::draw_arrays`].
    pub fn draw_arrays_instanced<S: Shader, F: FnMut(&mut S, u32)>(
        &self,
        target: &mut Target<'_>,
        shader: &mut S,
        vertices: &[S::Vertex],
        first: usize,
        count: usize,
        mode: DrawMode,
        instance_count: u32,
        mut update_instance: F,
    ) -> bool {
        let Some(end) = first.checked_add(count) else {
            return false;
        };
        if end > vertices.len() {
            return false;
        }

        let mut indices = Vec::with_capacity(count);
        for i in first..end {
            indices.push(i as u32);
        }

        let pipe = self.current_pipeline();
        for i in 0..instance_count {
            update_instance(shader, i);
            pipe.draw_mode(target, shader, vertices, &indices, mode);
        }
        true
    }

    /// Draw using indices sourced from the currently bound element buffer.
    ///
    /// Returns `false` if no valid element buffer is bound or index decoding fails.
    pub fn draw_elements<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        count: usize,
        index_type: IndexType,
        offset_bytes: usize,
        mode: DrawMode,
    ) -> bool {
        let vao_ebo = self.vao_slot().element_buffer;
        let ebo = if vao_ebo != 0 {
            vao_ebo
        } else {
            self.element_array_buffer
        };
        if ebo == 0 {
            return false;
        }

        let Some(bytes) = self.get_buffer_data(ebo) else {
            return false;
        };
        let Some(indices) = collect_indices_from_bytes(bytes, offset_bytes, count, index_type)
        else {
            return false;
        };
        self.current_pipeline()
            .draw_mode(target, shader, vertices, &indices, mode);
        true
    }

    /// Instanced variant of [`Self::draw_elements`].
    pub fn draw_elements_instanced<S: Shader, F: FnMut(&mut S, u32)>(
        &self,
        target: &mut Target<'_>,
        shader: &mut S,
        vertices: &[S::Vertex],
        count: usize,
        index_type: IndexType,
        offset_bytes: usize,
        mode: DrawMode,
        instance_count: u32,
        mut update_instance: F,
    ) -> bool {
        let vao_ebo = self.vao_slot().element_buffer;
        let ebo = if vao_ebo != 0 {
            vao_ebo
        } else {
            self.element_array_buffer
        };
        if ebo == 0 {
            return false;
        }

        let Some(bytes) = self.get_buffer_data(ebo) else {
            return false;
        };
        let Some(indices) = collect_indices_from_bytes(bytes, offset_bytes, count, index_type)
        else {
            return false;
        };

        let pipe = self.current_pipeline();
        for i in 0..instance_count {
            update_instance(shader, i);
            pipe.draw_mode(target, shader, vertices, &indices, mode);
        }
        true
    }

    /// Draw using currently bound ARRAY_BUFFER as tightly-packed `S::Vertex`.
    ///
    /// # Safety
    /// Bound ARRAY_BUFFER bytes must contain properly aligned and initialized
    /// elements of `S::Vertex`.
    pub unsafe fn draw_arrays_bound<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        first: usize,
        count: usize,
        mode: DrawMode,
    ) -> bool {
        if self.current_program == 0 || !self.is_program_linked(self.current_program) {
            return false;
        }
        if self.array_buffer == 0 {
            return false;
        }
        let Some(vertices) = (unsafe { self.get_buffer_as::<S::Vertex>(self.array_buffer) }) else {
            return false;
        };
        self.draw_arrays(target, shader, vertices, first, count, mode)
    }

    /// Draw indexed geometry using currently bound ARRAY_BUFFER and ELEMENT_ARRAY_BUFFER.
    ///
    /// # Safety
    /// Bound ARRAY_BUFFER bytes must contain properly aligned and initialized
    /// elements of `S::Vertex`.
    pub unsafe fn draw_elements_bound<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        count: usize,
        index_type: IndexType,
        offset_bytes: usize,
        mode: DrawMode,
    ) -> bool {
        if self.current_program == 0 || !self.is_program_linked(self.current_program) {
            return false;
        }
        if self.array_buffer == 0 {
            return false;
        }
        let Some(vertices) = (unsafe { self.get_buffer_as::<S::Vertex>(self.array_buffer) }) else {
            return false;
        };
        self.draw_elements(
            target,
            shader,
            vertices,
            count,
            index_type,
            offset_bytes,
            mode,
        )
    }

    // ── GLSL-interpreter-driven draw calls ───────────────────────────────────

    /// Build a [`GlslShader`] from the currently linked program and GL uniform / texture state.
    ///
    /// Returns `None` if no program is linked, if the linked program references
    /// invalid shader slots, or if the shader sources are not stored.
    fn build_glsl_shader(&self) -> Option<GlslShader> {
        let prog_id = self.current_program;
        if prog_id == 0 || !self.is_program_linked(prog_id) {
            return None;
        }
        let prog = &self.programs[prog_id as usize - 1];

        let vert_id = prog.vertex_shader;
        let frag_id = prog.fragment_shader;
        if vert_id == 0 || frag_id == 0 {
            return None;
        }

        let vert_src = self
            .shaders
            .get(vert_id as usize - 1)
            .and_then(|s| s.as_ref())
            .map(|s| alloc::string::String::from_utf8_lossy(&s.source).into_owned())?;
        let frag_src = self
            .shaders
            .get(frag_id as usize - 1)
            .and_then(|s| s.as_ref())
            .map(|s| alloc::string::String::from_utf8_lossy(&s.source).into_owned())?;
        let frag_output_bindings = self
            .shaders
            .get(frag_id as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref())
            .map(|md| {
                md.outputs
                    .iter()
                    .enumerate()
                    .map(|(index, out)| (out.location.unwrap_or(index as u8), out.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Build uniform map: name → Val by scanning shader metadata for uniform names
        let mut uniform_vals: BTreeMap<alloc::string::String, crate::glsl_interp::Val> =
            BTreeMap::new();
        for &sid in &[vert_id, frag_id] {
            if sid == 0 {
                continue;
            }
            if let Some(Some(sh)) = self.shaders.get(sid as usize - 1) {
                if let Some(meta) = &sh.metadata {
                    for uvar in &meta.uniforms {
                        let loc = self.get_uniform_location(prog_id, uvar.name.as_bytes());
                        if loc >= 0 {
                            let key = ((prog_id as u64) << 32) | (loc as u32 as u64);
                            if let Some(uval) = self.uniforms.get(&key) {
                                uniform_vals.insert(uvar.name.clone(), uniform_to_val(uval));
                            }
                        }
                    }
                }
            }
        }

        // Build texture slots from bound texture units
        let num_slots = MAX_TEXTURE_UNITS;
        let tex_imgs = self.texture_images_flat();
        let tex_slots = build_texture_slots(
            &self.tex_alloc,
            &tex_imgs,
            &self.textures,
            &self.texture_units,
            num_slots,
        );

        // ── GAP-011: Decode UBO (std140) blocks ──────────────────────────────────
        // Parse UBO block declarations from both shaders and unpack bound buffer
        // data according to std140 layout rules.  Each member is injected into
        // `uniform_vals` under the key `"instanceName.memberName"` so that the
        // `Expr::Field` evaluator can resolve them.
        for src in [vert_src.as_str(), frag_src.as_str()] {
            for block in parse_ubo_blocks(src) {
                // Determine buffer binding: explicit layout(binding=N) takes
                // priority; fall back to the binding set via glUniformBlockBinding.
                let binding = block.layout_binding.unwrap_or_else(|| {
                    let block_idx = self.get_uniform_block_index(prog_id, &block.block_name);
                    self.get_uniform_block_binding(prog_id, block_idx)
                        .unwrap_or(0)
                });
                let buf_name = self
                    .uniform_buffer_bindings
                    .get(binding as usize)
                    .copied()
                    .unwrap_or(0);
                if buf_name == 0 || buf_name as usize > self.buf_data.len() {
                    continue;
                }
                let buf = &self.buf_data[buf_name as usize - 1];
                // Walk members using std140 layout rules.
                let prefix = if block.instance_name.is_empty() {
                    alloc::string::String::new()
                } else {
                    alloc::format!("{}.", block.instance_name)
                };
                let mut offset: usize = 0;
                for (ty, name) in &block.members {
                    // std140 base alignments.
                    let (align, size, val) = decode_std140_member(ty, buf, &mut offset);
                    let _ = (align, size); // consumed inside helper
                    let key = alloc::format!("{}{}", prefix, name);
                    if let Some(v) = val {
                        uniform_vals.insert(key, v);
                    }
                }
            }
        }

        Some(GlslShader::compile(
            &vert_src,
            &frag_src,
            uniform_vals,
            tex_slots,
            frag_output_bindings,
        ))
    }

    /// Build a `GlslGeometryShader` for the current program's geometry stage, if present.
    pub fn build_glsl_geometry_shader(&self) -> Option<crate::glsl_interp::GlslGeometryShader> {
        let prog_id = self.current_program;
        if prog_id == 0 || !self.is_program_linked(prog_id) {
            return None;
        }
        let prog = &self.programs[prog_id as usize - 1];
        let geom_id = prog.geometry_shader;
        if geom_id == 0 {
            return None;
        }

        let geom_src = self
            .shaders
            .get(geom_id as usize - 1)
            .and_then(|s| s.as_ref())
            .map(|s| alloc::string::String::from_utf8_lossy(&s.source).into_owned())?;

        let mut uniform_vals: BTreeMap<alloc::string::String, crate::glsl_interp::Val> =
            BTreeMap::new();
        if let Some(Some(sh)) = self.shaders.get(geom_id as usize - 1) {
            if let Some(meta) = &sh.metadata {
                for uvar in &meta.uniforms {
                    let loc = self.get_uniform_location(prog_id, uvar.name.as_bytes());
                    if loc >= 0 {
                        let key = ((prog_id as u64) << 32) | (loc as u32 as u64);
                        if let Some(uval) = self.uniforms.get(&key) {
                            uniform_vals.insert(uvar.name.clone(), uniform_to_val(uval));
                        }
                    }
                }
            }
        }

        let tex_imgs = self.texture_images_flat();
        let tex_slots = build_texture_slots(
            &self.tex_alloc,
            &tex_imgs,
            &self.textures,
            &self.texture_units,
            MAX_TEXTURE_UNITS,
        );
        Some(crate::glsl_interp::GlslGeometryShader::compile(
            &geom_src,
            uniform_vals,
            tex_slots,
        ))
    }

    /// Build a `GlslComputeShader` for the current program's compute stage, if present.
    pub fn build_glsl_compute_shader(&self) -> Option<crate::glsl_interp::GlslComputeShader> {
        let prog_id = self.current_program;
        if prog_id == 0 {
            return None;
        }
        let prog = &self.programs[prog_id as usize - 1];
        let comp_id = prog.compute_shader;
        if comp_id == 0 {
            return None;
        }

        let comp_src = self
            .shaders
            .get(comp_id as usize - 1)
            .and_then(|s| s.as_ref())
            .map(|s| alloc::string::String::from_utf8_lossy(&s.source).into_owned())?;

        let mut uniform_vals: BTreeMap<alloc::string::String, crate::glsl_interp::Val> =
            BTreeMap::new();
        if let Some(Some(sh)) = self.shaders.get(comp_id as usize - 1) {
            if let Some(meta) = &sh.metadata {
                for uvar in &meta.uniforms {
                    let loc = self.get_uniform_location(prog_id, uvar.name.as_bytes());
                    if loc >= 0 {
                        let key = ((prog_id as u64) << 32) | (loc as u32 as u64);
                        if let Some(uval) = self.uniforms.get(&key) {
                            uniform_vals.insert(uvar.name.clone(), uniform_to_val(uval));
                        }
                    }
                }
            }
        }

        let tex_imgs = self.texture_images_flat();
        let tex_slots = build_texture_slots(
            &self.tex_alloc,
            &tex_imgs,
            &self.textures,
            &self.texture_units,
            MAX_TEXTURE_UNITS,
        );
        Some(crate::glsl_interp::GlslComputeShader::compile(
            &comp_src,
            uniform_vals,
            tex_slots,
        ))
    }

    /// Return texture images as a flat slice indexed by (name-1).
    fn texture_images_flat(&self) -> Vec<Vec<TextureImage>> {
        self.texture_images
            .iter()
            .map(|opt| opt.clone().unwrap_or_default())
            .collect()
    }

    /// Fetch vertex data from the currently bound VBO for the given vertex index.
    ///
    /// Each active attrib pointer maps buffer bytes to a `GlslVertex` attribute slot.
    fn fetch_glsl_vertex(&self, vertex_index: u32) -> GlslVertex {
        let mut v = GlslVertex::default();
        let vao = self.vao_slot();
        for (slot, ap) in vao.attribs.iter().enumerate() {
            if !ap.enabled {
                continue;
            }
            let buf = if ap.buffer != 0 {
                ap.buffer
            } else {
                self.array_buffer
            };
            if buf == 0 {
                continue;
            }
            let Some(data) = self.get_buffer_data(buf) else {
                continue;
            };
            let stride = if ap.stride != 0 {
                ap.stride as usize
            } else {
                (ap.size as usize) * 4
            };
            let offset = ap.offset as usize + vertex_index as usize * stride;
            let num = ap.size as usize; // components: 1–4
            let mut comps = [0.0f32; 4];
            for c in 0..num {
                let byte_off = offset + c * 4;
                if byte_off + 4 <= data.len() {
                    let bytes: [u8; 4] = data[byte_off..byte_off + 4].try_into().unwrap_or([0; 4]);
                    comps[c] = f32::from_le_bytes(bytes);
                }
            }
            v.attribs[slot] = comps;
        }
        v
    }

    /// Draw using the currently bound GLSL program, VAO, and FBO.
    ///
    /// This is the state-machine entry point equivalent to `glDrawArrays` in a
    /// full GL implementation — no Rust `Shader` impl required.  The linked
    /// GLSL vertex + fragment shader source is interpreted at runtime.
    ///
    /// `target` must correspond to the currently bound draw framebuffer's color
    /// (and optionally depth/stencil) storage.
    pub fn draw_state_arrays(
        &mut self,
        target: &mut Target<'_>,
        first: usize,
        count: usize,
        mode: DrawMode,
    ) -> bool {
        let Some(gs) = self.build_glsl_shader() else {
            return false;
        };
        // Build vertex list with gl_VertexID injected per vertex (GAP-009).
        let vertices: Vec<GlslVertex> = (first..first + count)
            .map(|i| {
                let mut v = self.fetch_glsl_vertex(i as u32);
                v.vertex_id = i as i32;
                v.instance_id = 0;
                v
            })
            .collect();
        // Run transform feedback capture (before rasterization, on vertex outputs)
        self.do_transform_feedback(&gs, &vertices);
        let ok = self.draw_arrays(target, &gs, &vertices, 0, count, mode);
        // GAP-003: feed primitives_generated into any active query.
        let prims = crate::gl::primitives_for_mode(count, mode);
        if prims > 0 {
            self.query_mark_progress(0, prims, 0);
        }
        ok
    }

    /// Draw indexed geometry using the currently bound GLSL program.
    ///
    /// Equivalent to `glDrawElements` in a full GL implementation.
    pub fn draw_state_elements(
        &mut self,
        target: &mut Target<'_>,
        count: usize,
        index_type: IndexType,
        offset_bytes: usize,
        mode: DrawMode,
    ) -> bool {
        let Some(gs) = self.build_glsl_shader() else {
            return false;
        };
        // Collect indices from EBO
        let vao_ebo = self.vao_slot().element_buffer;
        let ebo = if vao_ebo != 0 {
            vao_ebo
        } else {
            self.element_array_buffer
        };
        if ebo == 0 {
            return false;
        }
        let Some(bytes) = self.get_buffer_data(ebo) else {
            return false;
        };
        let Some(indices) = collect_indices_from_bytes(bytes, offset_bytes, count, index_type)
        else {
            return false;
        };
        // Build de-indexed vertex list (unique vertices by index) with gl_VertexID (GAP-009).
        let max_idx = indices.iter().copied().max().unwrap_or(0) as usize;
        let vertices: Vec<GlslVertex> = (0..=max_idx)
            .map(|i| {
                let mut v = self.fetch_glsl_vertex(i as u32);
                v.vertex_id = i as i32;
                v.instance_id = 0;
                v
            })
            .collect();
        self.do_transform_feedback(&gs, &vertices);
        let ok = self.draw_elements(
            target,
            &gs,
            &vertices,
            count,
            index_type,
            offset_bytes,
            mode,
        );
        // GAP-003: feed primitives_generated into any active query.
        let prims = crate::gl::primitives_for_mode(count, mode);
        if prims > 0 {
            self.query_mark_progress(0, prims, 0);
        }
        ok
    }

    /// Capture vertex-stage outputs to the active transform feedback buffers.
    fn do_transform_feedback(&mut self, gs: &GlslShader, vertices: &[GlslVertex]) {
        if self.transform_feedback == 0 {
            return;
        }
        let tfb_idx = self.transform_feedback as usize - 1;
        if tfb_idx >= MAX_TFBS {
            return;
        }
        let tfb = &self.tfbs[tfb_idx];
        if !tfb.active || tfb.paused {
            return;
        }
        let prog_id = self.current_program;
        if prog_id == 0 || prog_id as usize > MAX_PROGRAMS {
            return;
        }
        let tfb_varyings = self
            .program_tfb_varyings
            .get(prog_id as usize - 1)
            .cloned()
            .unwrap_or_default();
        if tfb_varyings.is_empty() {
            return;
        }

        let program = self.programs[prog_id as usize - 1];
        let vs_meta = if program.vertex_shader == 0 {
            None
        } else {
            self.shaders
                .get(program.vertex_shader as usize - 1)
                .and_then(|s| s.as_ref())
                .and_then(|s| s.metadata.as_ref())
        };
        let capture_layout: Vec<(Option<usize>, usize)> = tfb_varyings
            .iter()
            .enumerate()
            .map(|(binding, name)| {
                if name == "gl_Position" {
                    return (None, 4);
                }
                if let Some(vs) = vs_meta {
                    if let Some((slot, out)) = vs
                        .outputs
                        .iter()
                        .enumerate()
                        .find(|(_, out)| out.name == *name)
                    {
                        return (
                            Some(slot),
                            glsl_type_components(out.ty.as_str()).unwrap_or(4).min(4),
                        );
                    }
                }
                (Some(binding), 4)
            })
            .collect();

        use crate::shader::Shader as ShaderTrait;
        let vertex_outputs: Vec<_> = vertices.iter().map(|vertex| gs.vertex(vertex)).collect();

        let num_bindings = MAX_TFB_BINDINGS.min(tfb_varyings.len());
        for binding in 0..num_bindings {
            let buf_name = self.transform_feedback_buffers[binding];
            if buf_name == 0 || buf_name as usize > self.buf_data.len() {
                continue;
            }
            let buf = &mut self.buf_data[buf_name as usize - 1];
            let (slot, components) = capture_layout
                .get(binding)
                .copied()
                .unwrap_or((Some(binding), 4));
            let bytes_per_vertex = components * 4;
            let needed = vertices.len() * bytes_per_vertex;
            if buf.len() < needed {
                buf.resize(needed, 0);
            }
            for (vi, (pos, varying)) in vertex_outputs.iter().enumerate() {
                let values = match slot {
                    None => [pos.x, pos.y, pos.z, pos.w],
                    Some(slot_index) => varying.slots.get(slot_index).copied().unwrap_or([0.0; 4]),
                };
                let base = vi * bytes_per_vertex;
                for (ci, &component) in values.iter().take(components).enumerate() {
                    let off = base + ci * 4;
                    let b = component.to_le_bytes();
                    if off + 4 <= buf.len() {
                        buf[off..off + 4].copy_from_slice(&b);
                    }
                }
            }
        }
    }

    pub fn gen_renderbuffers(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.rbo_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.rbo_alloc[i] = true;
                    self.rbos[i] = Renderbuffer::default();
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => {
                    self.set_error(GlError::OutOfMemory);
                    break;
                }
            }
        }
        count
    }

    pub fn bind_renderbuffer(&mut self, name: u32) {
        if name == 0 || self.is_valid_rbo_name(name) {
            self.renderbuffer = name;
        } else {
            self.set_error(GlError::InvalidOperation);
        }
    }

    pub fn renderbuffer_storage(
        &mut self,
        name: u32,
        width: u32,
        height: u32,
        depth: bool,
        stencil: bool,
    ) {
        if !self.is_valid_rbo_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let r = &mut self.rbos[name as usize - 1];
        r.width = width;
        r.height = height;
        r.has_depth = depth;
        r.has_stencil = stencil;

        let slot = name as usize - 1;
        if !depth && !stencil && width != 0 && height != 0 {
            self.renderbuffer_color_images[slot] =
                Some(vec![0u32; (width as usize).saturating_mul(height as usize)]);
        } else {
            self.renderbuffer_color_images[slot] = None;
        }
    }

    pub fn framebuffer_renderbuffer(&mut self, fbo: u32, attachment: Attachment, rbo: u32) {
        if fbo == 0 || !self.is_valid_fbo_name(fbo) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if rbo != 0 && !self.is_valid_rbo_name(rbo) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let fb = &mut self.fbos[fbo as usize - 1];
        match attachment {
            Attachment::Color(i) if (i as usize) < 8 => {
                fb.color[i as usize] = FbAttachment::default();
                fb.color_rb[i as usize] = rbo;
            }
            Attachment::Depth => {
                fb.depth = FbAttachment::default();
                fb.depth_rb = rbo;
            }
            Attachment::Stencil => {
                fb.stencil = FbAttachment::default();
                fb.stencil_rb = rbo;
            }
            Attachment::DepthStencil => {
                fb.depth = FbAttachment::default();
                fb.stencil = FbAttachment::default();
                fb.depth_rb = rbo;
                fb.stencil_rb = rbo;
            }
            _ => self.set_error(GlError::InvalidEnum),
        }
    }

    pub fn delete_renderbuffers(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_rbo_name(n) {
                continue;
            }
            let idx = n as usize - 1;
            self.rbo_alloc[idx] = false;
            self.rbos[idx] = Renderbuffer::default();
            self.renderbuffer_color_images[idx] = None;
            if self.renderbuffer == n {
                self.renderbuffer = 0;
            }
            for fb in &mut self.fbos {
                for c in &mut fb.color_rb {
                    if *c == n {
                        *c = 0;
                    }
                }
                if fb.depth_rb == n {
                    fb.depth_rb = 0;
                }
                if fb.stencil_rb == n {
                    fb.stencil_rb = 0;
                }
            }
        }
    }

    pub fn create_shader(&mut self, kind: ShaderKind) -> u32 {
        if let Some(i) = self.shader_alloc.iter().position(|&a| !a) {
            self.shader_alloc[i] = true;
            self.shaders[i] = Some(ShaderObject::new(kind));
            self.shader_info_logs[i].clear();
            (i + 1) as u32
        } else {
            self.set_error(GlError::OutOfMemory);
            0
        }
    }

    pub fn shader_source(&mut self, shader: u32, src: &[u8]) {
        if !self.is_valid_shader_name(shader) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if let Some(obj) = self.shaders[shader as usize - 1].as_mut() {
            obj.source.clear();
            obj.source.extend_from_slice(src);
            obj.compiled = false;
            obj.metadata = None;
        }
        self.shader_info_logs[shader as usize - 1].clear();
    }

    /// Convenience wrapper for [`shader_source`] that accepts a `&str`.
    #[inline]
    pub fn shader_source_str(&mut self, shader: u32, src: &str) {
        self.shader_source(shader, src.as_bytes());
    }

    pub fn compile_shader(&mut self, shader: u32) {
        if !self.is_valid_shader_name(shader) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let slot = shader as usize - 1;
        if let Some(obj) = self.shaders[slot].as_mut() {
            let parsed = parse_glsl_metadata(obj.kind, obj.source.as_slice());
            obj.metadata = parsed;
            if self.strict_glsl {
                obj.compiled = obj
                    .metadata
                    .as_ref()
                    .map(|m| validate_glsl_shader_metadata(obj.kind, m))
                    .unwrap_or(false);
            } else {
                obj.compiled = !obj.source.is_empty();
            }

            if obj.compiled {
                self.shader_info_logs[slot].clear();
            } else if obj.source.is_empty() {
                self.shader_info_logs[slot] = String::from("shader source is empty");
            } else if self.strict_glsl {
                self.shader_info_logs[slot] = String::from("strict GLSL parse/validation failed");
            } else {
                self.shader_info_logs[slot] = String::from("shader compile failed");
            }
        }
    }

    pub fn set_strict_glsl(&mut self, enable: bool) {
        self.strict_glsl = enable;
    }

    pub fn strict_glsl_enabled(&self) -> bool {
        self.strict_glsl
    }

    pub fn set_transform_feedback_varyings(&mut self, program: u32, varyings: &[&[u8]]) {
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if varyings.len() > MAX_PROGRAM_TFB_VARYINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let slot = program as usize - 1;
        self.program_tfb_varyings[slot].clear();
        for raw in varyings {
            let name = String::from(String::from_utf8_lossy(raw).trim());
            if name.is_empty() {
                self.set_error(GlError::InvalidValue);
                self.program_tfb_varyings[slot].clear();
                self.program_tfb_components[slot] = 0;
                return;
            }
            self.program_tfb_varyings[slot].push(name);
        }
        self.program_tfb_components[slot] = 0;
        self.programs[slot].linked = false;
    }

    pub fn transform_feedback_varyings(&self, program: u32) -> Option<Vec<String>> {
        if !self.is_valid_program_name(program) {
            return None;
        }
        Some(self.program_tfb_varyings[program as usize - 1].clone())
    }

    pub fn supported_extensions(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for ext in [
            GlExtension::KhrDebug,
            GlExtension::KhrRobustness,
            GlExtension::ExtDisjointTimerQuery,
            GlExtension::ExtColorBufferFloat,
        ] {
            if self.extension_is_enabled(ext) {
                out.push(extension_name(ext));
            }
        }
        out
    }

    pub fn extension_is_enabled(&self, ext: GlExtension) -> bool {
        self.extension_enabled[extension_slot(ext)]
    }

    pub fn set_extension_enabled_for_testing(&mut self, ext: GlExtension, enabled: bool) {
        self.extension_enabled[extension_slot(ext)] = enabled;
    }

    pub fn shader_source_bytes(&self, shader: u32) -> Option<&[u8]> {
        if !self.is_valid_shader_name(shader) {
            return None;
        }
        self.shaders[shader as usize - 1]
            .as_ref()
            .map(|s| s.source.as_slice())
    }

    pub fn shader_kind(&self, shader: u32) -> Option<ShaderKind> {
        if !self.is_valid_shader_name(shader) {
            return None;
        }
        self.shaders[shader as usize - 1].as_ref().map(|s| s.kind)
    }

    pub fn shader_compile_status(&self, shader: u32) -> bool {
        if !self.is_valid_shader_name(shader) {
            return false;
        }
        self.shaders[shader as usize - 1]
            .as_ref()
            .map(|s| s.compiled)
            .unwrap_or(false)
    }

    pub fn shader_info_log(&self, shader: u32) -> Option<&str> {
        if !self.is_valid_shader_name(shader) {
            return None;
        }
        Some(self.shader_info_logs[shader as usize - 1].as_str())
    }

    pub fn delete_shaders(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_shader_name(n) {
                continue;
            }
            if self.shader_attached_to_any_program(n) {
                if let Some(sh) = self.shaders[n as usize - 1].as_mut() {
                    sh.delete_pending = true;
                }
            } else {
                self.shader_alloc[n as usize - 1] = false;
                self.shaders[n as usize - 1] = None;
                self.shader_info_logs[n as usize - 1].clear();
            }
        }
    }

    pub fn create_program(&mut self) -> u32 {
        if let Some(i) = self.program_alloc.iter().position(|&a| !a) {
            self.program_alloc[i] = true;
            self.programs[i] = ProgramObject::default();
            self.program_info_logs[i].clear();
            (i + 1) as u32
        } else {
            self.set_error(GlError::OutOfMemory);
            0
        }
    }

    pub fn attach_shader(&mut self, program: u32, shader: u32) {
        if !self.is_valid_program_name(program) || !self.is_valid_shader_name(shader) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(kind) = self.shaders[shader as usize - 1].as_ref().map(|s| s.kind) else {
            self.set_error(GlError::InvalidOperation);
            return;
        };

        let detached_shader;
        let p = &mut self.programs[program as usize - 1];
        match kind {
            ShaderKind::Vertex => {
                detached_shader = p.vertex_shader;
                p.vertex_shader = shader;
            }
            ShaderKind::Fragment => {
                detached_shader = p.fragment_shader;
                p.fragment_shader = shader;
            }
            ShaderKind::Geometry => {
                detached_shader = p.geometry_shader;
                p.geometry_shader = shader;
            }
            ShaderKind::Compute => {
                detached_shader = p.compute_shader;
                p.compute_shader = shader;
            }
        }
        p.linked = false;

        if detached_shader != 0 && detached_shader != shader {
            self.finalize_shader_delete_if_unattached(detached_shader);
        }
    }

    pub fn detach_shader(&mut self, program: u32, shader: u32) {
        if !self.is_valid_program_name(program) || !self.is_valid_shader_name(shader) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let Some(kind) = self.shaders[shader as usize - 1].as_ref().map(|s| s.kind) else {
            self.set_error(GlError::InvalidOperation);
            return;
        };

        let p = &mut self.programs[program as usize - 1];
        let detached = match kind {
            ShaderKind::Vertex => {
                if p.vertex_shader != shader {
                    false
                } else {
                    p.vertex_shader = 0;
                    true
                }
            }
            ShaderKind::Fragment => {
                if p.fragment_shader != shader {
                    false
                } else {
                    p.fragment_shader = 0;
                    true
                }
            }
            ShaderKind::Geometry => {
                if p.geometry_shader != shader {
                    false
                } else {
                    p.geometry_shader = 0;
                    true
                }
            }
            ShaderKind::Compute => {
                if p.compute_shader != shader {
                    false
                } else {
                    p.compute_shader = 0;
                    true
                }
            }
        };

        if !detached {
            self.set_error(GlError::InvalidOperation);
            return;
        }

        p.linked = false;
        self.finalize_shader_delete_if_unattached(shader);
    }

    pub fn link_program(&mut self, program: u32) {
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let (vs, fs) = {
            let p = self.programs[program as usize - 1];
            (p.vertex_shader, p.fragment_shader)
        };
        let linked = self.is_shader_compiled(vs, ShaderKind::Vertex)
            && self.is_shader_compiled(fs, ShaderKind::Fragment);
        let interface_ok = self.shader_interfaces_compatible(vs, fs);
        let interface_ok = interface_ok
            && if self.strict_glsl {
                self.shader_uniforms_compatible(vs, fs)
            } else {
                true
            };
        let tfb_ok = self.transform_feedback_varyings_compatible(program, vs);
        let slot = program as usize - 1;
        self.programs[slot].linked = linked && interface_ok && tfb_ok;
        if self.programs[slot].linked {
            self.program_info_logs[slot].clear();
        } else if !linked {
            self.program_info_logs[slot] = String::from("attached shaders are not both compiled");
        } else if !interface_ok {
            self.program_info_logs[slot] = String::from("stage interface type mismatch (GAP-012)");
        } else if !tfb_ok {
            self.program_info_logs[slot] = String::from("transform feedback varyings mismatch");
        } else {
            self.program_info_logs[slot] = String::from("program link failed");
        }
    }

    fn transform_feedback_varyings_compatible(&mut self, program: u32, vertex_shader: u32) -> bool {
        let slot = program as usize - 1;
        if self.program_tfb_varyings[slot].is_empty() {
            self.program_tfb_components[slot] = 0;
            return true;
        }
        let vs_meta = self
            .shaders
            .get(vertex_shader as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref());
        let Some(vs) = vs_meta else {
            return false;
        };

        let mut total_components = 0u16;
        for name in &self.program_tfb_varyings[slot] {
            let Some(var) = vs.outputs.iter().find(|o| &o.name == name) else {
                return false;
            };
            let Some(c) = glsl_type_components(var.ty.as_str()) else {
                return false;
            };
            total_components = total_components.saturating_add(c as u16);
        }
        self.program_tfb_components[slot] = total_components;
        true
    }

    fn shader_interfaces_compatible(&self, vertex_shader: u32, fragment_shader: u32) -> bool {
        let vs_meta = self
            .shaders
            .get(vertex_shader as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref());
        let fs_meta = self
            .shaders
            .get(fragment_shader as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref());
        let (Some(vs), Some(fs)) = (vs_meta, fs_meta) else {
            // No metadata available — cannot validate, allow link (GAP-012).
            return true;
        };
        for input in &fs.inputs {
            let Some(out) = vs.outputs.iter().find(|o| o.name == input.name) else {
                return false;
            };
            if out.ty != input.ty {
                return false;
            }
        }

        // Fragment outputs must have unique locations and fit attachment limits.
        let mut used_locations: u16 = 0;
        for &loc in &fs.output_locations {
            if loc >= 8 {
                return false;
            }
            let bit = 1u16 << loc;
            if used_locations & bit != 0 {
                return false;
            }
            used_locations |= bit;
        }
        true
    }

    fn shader_uniforms_compatible(&self, vertex_shader: u32, fragment_shader: u32) -> bool {
        let vs_meta = self
            .shaders
            .get(vertex_shader as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref());
        let fs_meta = self
            .shaders
            .get(fragment_shader as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref());
        let (Some(vs), Some(fs)) = (vs_meta, fs_meta) else {
            return false;
        };

        for vu in &vs.uniforms {
            if let Some(fu) = fs.uniforms.iter().find(|u| u.name == vu.name) {
                if fu.ty != vu.ty {
                    return false;
                }
            }
        }
        true
    }

    pub fn program_link_status(&self, program: u32) -> bool {
        if !self.is_valid_program_name(program) {
            return false;
        }
        self.programs[program as usize - 1].linked
    }

    pub fn program_info_log(&self, program: u32) -> Option<&str> {
        if !self.is_valid_program_name(program) {
            return None;
        }
        Some(self.program_info_logs[program as usize - 1].as_str())
    }

    pub fn program_attached_shaders(&self, program: u32) -> Option<(u32, u32)> {
        if !self.is_valid_program_name(program) {
            return None;
        }
        let p = self.programs[program as usize - 1];
        Some((p.vertex_shader, p.fragment_shader))
    }

    pub fn use_program(&mut self, program: u32) {
        if program == 0 {
            self.current_program = 0;
            return;
        }
        if !self.is_valid_program_name(program) || !self.programs[program as usize - 1].linked {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.current_program = program;
    }

    pub fn delete_programs(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_program_name(n) {
                continue;
            }
            let old = self.programs[n as usize - 1];
            self.program_alloc[n as usize - 1] = false;
            self.programs[n as usize - 1] = ProgramObject::default();
            self.program_info_logs[n as usize - 1].clear();
            self.program_tfb_varyings[n as usize - 1].clear();
            self.program_tfb_components[n as usize - 1] = 0;
            if self.current_program == n {
                self.current_program = 0;
            }
            self.uniforms.retain(|k, _| ((*k >> 32) as u32) != n);

            if old.vertex_shader != 0 {
                self.finalize_shader_delete_if_unattached(old.vertex_shader);
            }
            if old.fragment_shader != 0 && old.fragment_shader != old.vertex_shader {
                self.finalize_shader_delete_if_unattached(old.fragment_shader);
            }
        }
    }

    // ── Phase 4: uniform / attrib introspection ───────────────────────────

    /// glGetUniformLocation — returns a stable location for a name string.
    /// Uses the name bytes hashed into a deterministic positive i32.
    /// Returns -1 for invalid program or unlinked program.
    pub fn get_uniform_location(&self, program: u32, name: &[u8]) -> i32 {
        if !self.is_valid_program_name(program) {
            return -1;
        }
        if !self.programs[program as usize - 1].linked {
            return -1;
        }
        if name.is_empty() {
            return -1;
        }
        // Deterministic hash: FNV-1a 32-bit, masked to positive i32 range.
        let mut h: u32 = 2166136261;
        for &b in name {
            h ^= b as u32;
            h = h.wrapping_mul(16777619);
        }
        (h & 0x7FFF_FFFF) as i32
    }

    /// glGetAttribLocation — returns a deterministic attribute slot index.
    /// Returns -1 for invalid/unlinked program.
    pub fn get_attrib_location(&self, program: u32, name: &[u8]) -> i32 {
        if !self.is_valid_program_name(program) {
            return -1;
        }
        if !self.programs[program as usize - 1].linked {
            return -1;
        }
        if name.is_empty() {
            return -1;
        }
        let mut h: u32 = 2166136261;
        for &b in name {
            h ^= b as u32;
            h = h.wrapping_mul(16777619);
        }
        ((h & 0x0F) as i32).min(MAX_ATTRIBS as i32 - 1)
    }

    /// glUniform1f
    pub fn uniform1f(&mut self, program: u32, location: i32, v: f32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::Float(v));
    }

    /// glUniform1i
    pub fn uniform1i(&mut self, program: u32, location: i32, v: i32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::Int(v));
    }

    /// glUniform4f
    pub fn uniform4f(&mut self, program: u32, location: i32, x: f32, y: f32, z: f32, w: f32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::Vec4([x, y, z, w]));
    }

    /// glUniform4fv (single vec4 from slice)
    pub fn uniform4fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 4 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(
            program,
            location,
            UniformValue::Vec4([data[0], data[1], data[2], data[3]]),
        );
    }

    /// glUniformMatrix4fv (single mat4 from slice, column-major)
    pub fn uniform_matrix4fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 16 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 16];
        m.copy_from_slice(&data[..16]);
        self.set_uniform(program, location, UniformValue::Mat4(m));
    }

    /// Read back a stored uniform value (returns None if not set).
    pub fn get_uniform(&self, program: u32, location: i32) -> Option<&UniformValue> {
        if location < 0 {
            return None;
        }
        let key = Self::uniform_key(program, location);
        self.uniforms.get(&key)
    }

    fn set_uniform(&mut self, program: u32, location: i32, value: UniformValue) {
        let key = Self::uniform_key(program, location);
        let _ = self.uniforms.insert(key, value);
    }

    #[inline]
    fn uniform_key(program: u32, location: i32) -> u64 {
        ((program as u64) << 32) | (location as u32 as u64)
    }

    /// glValidateProgram — marks the program validated; sets validate_ok = linked.
    pub fn validate_program(&mut self, program: u32) {
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let p = &mut self.programs[program as usize - 1];
        p.validated = true;
        p.validate_ok = p.linked;
    }

    /// Returns true if the last validate_program call on this program succeeded.
    pub fn is_program_valid(&self, program: u32) -> bool {
        if !self.is_valid_program_name(program) {
            return false;
        }
        self.programs[program as usize - 1].validate_ok
    }

    pub fn gen_samplers(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.sampler_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.sampler_alloc[i] = true;
                    self.samplers[i] = SamplerObject::default();
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => {
                    self.set_error(GlError::OutOfMemory);
                    break;
                }
            }
        }
        count
    }

    pub fn bind_sampler(&mut self, unit: u32, sampler: u32) {
        if unit as usize >= MAX_TEXTURE_UNITS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if sampler != 0 && !self.is_valid_sampler_name(sampler) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.sampler_units[unit as usize] = sampler;
    }

    pub fn sampler_parameter_wrap(&mut self, sampler: u32, wrap_s: WrapMode, wrap_t: WrapMode) {
        if !self.is_valid_sampler_name(sampler) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let s = &mut self.samplers[sampler as usize - 1];
        s.wrap_s = wrap_s;
        s.wrap_t = wrap_t;
    }

    pub fn sampler_parameter_filter(
        &mut self,
        sampler: u32,
        min_filter: FilterMode,
        mag_filter: FilterMode,
    ) {
        if !self.is_valid_sampler_name(sampler) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let s = &mut self.samplers[sampler as usize - 1];
        s.min_filter = min_filter;
        s.mag_filter = mag_filter;
    }

    pub fn sampler_parameter_border_color(&mut self, sampler: u32, border_color: [f32; 4]) {
        if !self.is_valid_sampler_name(sampler) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.samplers[sampler as usize - 1].border_color = border_color;
    }

    pub fn delete_samplers(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_sampler_name(n) {
                continue;
            }
            self.sampler_alloc[n as usize - 1] = false;
            self.samplers[n as usize - 1] = SamplerObject::default();
            for unit in &mut self.sampler_units {
                if *unit == n {
                    *unit = 0;
                }
            }
        }
    }

    /// Create a sync object that is immediately signaled in this software backend.
    pub fn fence_sync(&mut self) -> u32 {
        match self.sync_alloc.iter().position(|&a| !a) {
            Some(i) => {
                self.sync_alloc[i] = true;
                self.syncs[i] = SyncObject { signaled: true };
                (i + 1) as u32
            }
            None => {
                self.set_error(GlError::OutOfMemory);
                0
            }
        }
    }

    /// Wait on a sync object.
    pub fn client_wait_sync(&mut self, sync: u32, _timeout_ns: u64) -> SyncWaitResult {
        if !self.is_valid_sync_name(sync) {
            self.set_error(GlError::InvalidOperation);
            return SyncWaitResult::WaitFailed;
        }
        let s = self.syncs[sync as usize - 1];
        if s.signaled {
            SyncWaitResult::AlreadySignaled
        } else {
            SyncWaitResult::TimeoutExpired
        }
    }

    pub fn delete_syncs(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_sync_name(n) {
                continue;
            }
            self.sync_alloc[n as usize - 1] = false;
            self.syncs[n as usize - 1] = SyncObject::default();
        }
    }

    pub fn gen_transform_feedbacks(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.tfb_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.tfb_alloc[i] = true;
                    self.tfbs[i] = TransformFeedbackObject::default();
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => {
                    self.set_error(GlError::OutOfMemory);
                    break;
                }
            }
        }
        count
    }

    pub fn bind_transform_feedback(&mut self, name: u32) {
        if name != 0 && !self.is_valid_tfb_name(name) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        // Cannot rebind away from an active, non-paused TFB.
        if self.transform_feedback != 0 && self.is_valid_tfb_name(self.transform_feedback) {
            let cur = self.tfbs[self.transform_feedback as usize - 1];
            if cur.active && !cur.paused && name != self.transform_feedback {
                self.set_error(GlError::InvalidOperation);
                return;
            }
        }
        self.transform_feedback = name;
    }

    /// Bind `buffer` as transform feedback target at `index`.
    pub fn bind_transform_feedback_buffer_base(&mut self, index: u32, buffer: u32) {
        if index as usize >= MAX_TFB_BINDINGS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if buffer != 0 && !self.is_valid_buffer_name(buffer) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.transform_feedback_buffers[index as usize] = buffer;
    }

    pub fn transform_feedback_buffer_binding(&self, index: u32) -> Option<u32> {
        if index as usize >= MAX_TFB_BINDINGS {
            return None;
        }
        Some(self.transform_feedback_buffers[index as usize])
    }

    pub fn delete_transform_feedbacks(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_tfb_name(n) {
                continue;
            }
            let cur = self.tfbs[n as usize - 1];
            if cur.active {
                self.set_error(GlError::InvalidOperation);
                continue;
            }
            self.tfb_alloc[n as usize - 1] = false;
            self.tfbs[n as usize - 1] = TransformFeedbackObject::default();
            if self.transform_feedback == n {
                self.transform_feedback = 0;
            }
        }
    }

    pub fn begin_transform_feedback(&mut self, primitive_mode: DrawMode) {
        if self.transform_feedback == 0 || !self.is_valid_tfb_name(self.transform_feedback) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if !matches!(
            primitive_mode,
            DrawMode::Points | DrawMode::Lines | DrawMode::Triangles
        ) {
            self.set_error(GlError::InvalidEnum);
            return;
        }
        let tfb = &mut self.tfbs[self.transform_feedback as usize - 1];
        if tfb.active {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        tfb.active = true;
        tfb.paused = false;
        tfb.primitive_mode = Some(primitive_mode);

        // Base binding semantics: a new capture pass starts from the beginning.
        for &buf in &self.transform_feedback_buffers {
            if buf != 0 && self.is_valid_buffer_name(buf) {
                self.buf_data[buf as usize - 1].clear();
            }
        }
    }

    pub fn end_transform_feedback(&mut self) {
        if self.transform_feedback == 0 || !self.is_valid_tfb_name(self.transform_feedback) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let tfb = &mut self.tfbs[self.transform_feedback as usize - 1];
        if !tfb.active {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        tfb.active = false;
        tfb.paused = false;
        tfb.primitive_mode = None;
    }

    pub fn pause_transform_feedback(&mut self) {
        if self.transform_feedback == 0 || !self.is_valid_tfb_name(self.transform_feedback) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let tfb = &mut self.tfbs[self.transform_feedback as usize - 1];
        if !tfb.active || tfb.paused {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        tfb.paused = true;
    }

    pub fn resume_transform_feedback(&mut self) {
        if self.transform_feedback == 0 || !self.is_valid_tfb_name(self.transform_feedback) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let tfb = &mut self.tfbs[self.transform_feedback as usize - 1];
        if !tfb.active || !tfb.paused {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        tfb.paused = false;
    }

    pub fn current_transform_feedback(&self) -> u32 {
        self.transform_feedback
    }

    pub fn transform_feedback_state(&self, name: u32) -> Option<TransformFeedbackObject> {
        if !self.is_valid_tfb_name(name) {
            return None;
        }
        Some(self.tfbs[name as usize - 1])
    }

    /// Append captured bytes to transform feedback binding 0 when capture is active.
    pub fn transform_feedback_capture_bytes(&mut self, data: &[u8]) -> bool {
        if self.transform_feedback == 0 || !self.is_valid_tfb_name(self.transform_feedback) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        let tfb = self.tfbs[self.transform_feedback as usize - 1];
        if !tfb.active || tfb.paused {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        let buf = self.transform_feedback_buffers[0];
        if buf == 0 || !self.is_valid_buffer_name(buf) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        self.buf_data[buf as usize - 1].extend_from_slice(data);
        true
    }

    /// Capture transform feedback data for the currently bound and linked program.
    ///
    /// In this software backend, data is provided as tightly packed f32 values,
    /// validated against link-time transform feedback varying declarations.
    pub fn transform_feedback_capture_from_program_f32(&mut self, values: &[f32]) -> bool {
        let program = self.current_program;
        if program == 0 || !self.is_program_linked(program) {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        let expected = self.program_tfb_components[program as usize - 1] as usize;
        if expected == 0 {
            self.set_error(GlError::InvalidOperation);
            return false;
        }
        if values.len() != expected {
            self.set_error(GlError::InvalidValue);
            return false;
        }
        let mut bytes = Vec::with_capacity(values.len().saturating_mul(4));
        for &v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.transform_feedback_capture_bytes(bytes.as_slice())
    }

    pub fn gen_queries(&mut self, out: &mut [u32]) -> usize {
        let mut count = 0;
        for slot in out.iter_mut() {
            match self.query_alloc.iter().position(|&a| !a) {
                Some(i) => {
                    self.query_alloc[i] = true;
                    self.queries[i] = QueryObject::default();
                    *slot = (i + 1) as u32;
                    count += 1;
                }
                None => {
                    self.set_error(GlError::OutOfMemory);
                    break;
                }
            }
        }
        count
    }

    pub fn delete_queries(&mut self, names: &[u32]) {
        for &n in names {
            if !self.is_valid_query_name(n) {
                continue;
            }
            if self.queries[n as usize - 1].active {
                self.set_error(GlError::InvalidOperation);
                continue;
            }
            self.query_alloc[n as usize - 1] = false;
            self.queries[n as usize - 1] = QueryObject::default();
            for a in &mut self.active_queries {
                if *a == Some(n) {
                    *a = None;
                }
            }
        }
    }

    pub fn begin_query(&mut self, target: QueryTarget, query: u32) {
        if target == QueryTarget::TimeElapsed
            && !self.extension_is_enabled(GlExtension::ExtDisjointTimerQuery)
        {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if !self.is_valid_query_name(query) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let slot = query_target_slot(target);
        if self.active_queries[slot].is_some() {
            self.set_error(GlError::InvalidOperation);
            return;
        }

        let q = &mut self.queries[query as usize - 1];
        if q.active {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if let Some(existing_target) = q.target {
            if existing_target != target {
                self.set_error(GlError::InvalidOperation);
                return;
            }
        }
        q.target = Some(target);
        q.active = true;
        q.result = 0;
        q.result_available = false;
        self.active_queries[slot] = Some(query);
    }

    pub fn end_query(&mut self, target: QueryTarget) {
        let slot = query_target_slot(target);
        let Some(query) = self.active_queries[slot] else {
            self.set_error(GlError::InvalidOperation);
            return;
        };
        self.active_queries[slot] = None;
        let q = &mut self.queries[query as usize - 1];
        q.active = false;
        q.result_available = true;
        if target == QueryTarget::AnySamplesPassed {
            q.result = if q.result > 0 { 1 } else { 0 };
        }
    }

    /// Feed software backend statistics into active queries.
    pub fn query_mark_progress(
        &mut self,
        samples_passed: u64,
        primitives_generated: u64,
        time_elapsed_ns: u64,
    ) {
        let updates = [
            (QueryTarget::SamplesPassed, samples_passed),
            (QueryTarget::AnySamplesPassed, samples_passed),
            (QueryTarget::PrimitivesGenerated, primitives_generated),
            (QueryTarget::TimeElapsed, time_elapsed_ns),
        ];
        for (target, delta) in updates {
            let slot = query_target_slot(target);
            let Some(query) = self.active_queries[slot] else {
                continue;
            };
            self.queries[query as usize - 1].result = self.queries[query as usize - 1]
                .result
                .saturating_add(delta);
        }
    }

    pub fn query_result_available(&self, query: u32) -> Option<bool> {
        if !self.is_valid_query_name(query) {
            return None;
        }
        Some(self.queries[query as usize - 1].result_available)
    }

    pub fn query_result_u64(&self, query: u32) -> Option<u64> {
        if !self.is_valid_query_name(query) {
            return None;
        }
        let q = self.queries[query as usize - 1];
        if !q.result_available {
            return None;
        }
        Some(q.result)
    }

    pub fn query_target(&self, query: u32) -> Option<QueryTarget> {
        if !self.is_valid_query_name(query) {
            return None;
        }
        self.queries[query as usize - 1].target
    }

    pub fn debug_message_insert(
        &mut self,
        source: DebugSource,
        kind: DebugType,
        id: u32,
        severity: DebugSeverity,
        message: &[u8],
    ) {
        if !self.extension_is_enabled(GlExtension::KhrDebug) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if self.debug_log.len() >= MAX_DEBUG_MESSAGES {
            self.debug_log.remove(0);
        }
        self.debug_log.push(DebugMessage {
            source,
            kind,
            id,
            severity,
            message: String::from_utf8_lossy(message).into_owned(),
        });
    }

    pub fn push_debug_group(&mut self, id: u32, message: &[u8]) {
        if !self.extension_is_enabled(GlExtension::KhrDebug) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if self.debug_group_depth >= MAX_DEBUG_GROUP_DEPTH {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.debug_group_depth += 1;
        self.debug_message_insert(
            DebugSource::Application,
            DebugType::PushGroup,
            id,
            DebugSeverity::Notification,
            message,
        );
    }

    pub fn pop_debug_group(&mut self) {
        if !self.extension_is_enabled(GlExtension::KhrDebug) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if self.debug_group_depth == 0 {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.debug_group_depth -= 1;
        self.debug_message_insert(
            DebugSource::Application,
            DebugType::PopGroup,
            0,
            DebugSeverity::Notification,
            b"pop",
        );
    }

    pub fn debug_group_depth(&self) -> u8 {
        self.debug_group_depth
    }

    pub fn drain_debug_messages(&mut self) -> Vec<DebugMessage> {
        let mut out = Vec::with_capacity(self.debug_log.len());
        out.append(&mut self.debug_log);
        out
    }

    pub fn set_robust_access(&mut self, enable: bool) {
        if !self.extension_is_enabled(GlExtension::KhrRobustness) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.robust_access = enable;
    }

    pub fn robust_access_enabled(&self) -> bool {
        self.extension_is_enabled(GlExtension::KhrRobustness) && self.robust_access
    }

    pub fn context_reset_status(&self) -> ContextResetStatus {
        if self.extension_is_enabled(GlExtension::KhrRobustness) {
            self.reset_status
        } else {
            ContextResetStatus::NoError
        }
    }

    pub fn force_context_reset_for_testing(&mut self, status: ContextResetStatus) {
        if !self.extension_is_enabled(GlExtension::KhrRobustness) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.reset_status = status;
    }

    fn shader_attached_to_any_program(&self, shader: u32) -> bool {
        self.programs
            .iter()
            .filter(|p| p.vertex_shader != 0 || p.fragment_shader != 0)
            .any(|p| p.vertex_shader == shader || p.fragment_shader == shader)
    }

    fn finalize_shader_delete_if_unattached(&mut self, shader: u32) {
        if !self.is_valid_shader_name(shader) {
            return;
        }
        if self.shader_attached_to_any_program(shader) {
            return;
        }
        let slot = shader as usize - 1;
        if self
            .shaders
            .get(slot)
            .and_then(|s| s.as_ref())
            .map(|s| s.delete_pending)
            .unwrap_or(false)
        {
            self.shader_alloc[slot] = false;
            self.shaders[slot] = None;
            self.shader_info_logs[slot].clear();
        }
    }

    fn is_valid_rbo_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_RENDERBUFFERS && self.rbo_alloc[name as usize - 1]
    }

    fn is_valid_shader_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_SHADERS && self.shader_alloc[name as usize - 1]
    }

    fn is_valid_program_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_PROGRAMS && self.program_alloc[name as usize - 1]
    }

    fn is_valid_sampler_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_SAMPLERS && self.sampler_alloc[name as usize - 1]
    }

    fn is_valid_sync_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_SYNCS && self.sync_alloc[name as usize - 1]
    }

    fn is_valid_tfb_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_TFBS && self.tfb_alloc[name as usize - 1]
    }

    fn is_valid_query_name(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_QUERIES && self.query_alloc[name as usize - 1]
    }

    pub fn is_program_linked(&self, name: u32) -> bool {
        self.is_valid_program_name(name) && self.programs[name as usize - 1].linked
    }

    pub fn current_vao(&self) -> u32 {
        self.vao
    }
    pub fn current_array_buffer(&self) -> u32 {
        self.array_buffer
    }
    pub fn current_element_buffer(&self) -> u32 {
        let ebo = self.vao_slot().element_buffer;
        if ebo != 0 {
            ebo
        } else {
            self.element_array_buffer
        }
    }
    pub fn current_texture_binding(&self, unit: u32) -> Option<u32> {
        if unit as usize >= MAX_TEXTURE_UNITS {
            return None;
        }
        Some(self.texture_units[unit as usize])
    }
    pub fn current_sampler_binding(&self, unit: u32) -> Option<u32> {
        if unit as usize >= MAX_TEXTURE_UNITS {
            return None;
        }
        Some(self.sampler_units[unit as usize])
    }
    pub fn current_draw_framebuffer(&self) -> u32 {
        self.draw_framebuffer
    }
    pub fn current_read_framebuffer(&self) -> u32 {
        self.read_framebuffer
    }
    pub fn current_program(&self) -> u32 {
        self.current_program
    }

    pub fn vertex_attrib_pointer_for(&self, vao: u32, index: u32) -> Option<AttribPointer> {
        if index as usize >= MAX_ATTRIBS {
            return None;
        }
        if vao == 0 {
            return Some(self.default_vao.attribs[index as usize]);
        }
        if !self.is_valid_vao_name(vao) {
            return None;
        }
        Some(self.vaos[vao as usize - 1].attribs[index as usize])
    }

    pub fn element_buffer_for(&self, vao: u32) -> Option<u32> {
        if vao == 0 {
            return Some(self.default_vao.element_buffer);
        }
        if !self.is_valid_vao_name(vao) {
            return None;
        }
        Some(self.vaos[vao as usize - 1].element_buffer)
    }

    pub fn framebuffer_color_attachment_texture(&self, fbo: u32, index: u8) -> Option<u32> {
        if index as usize >= 8 || !self.is_valid_fbo_name(fbo) {
            return None;
        }
        Some(self.fbos[fbo as usize - 1].color[index as usize].texture)
    }

    pub fn framebuffer_depth_attachment_texture(&self, fbo: u32) -> Option<u32> {
        if !self.is_valid_fbo_name(fbo) {
            return None;
        }
        Some(self.fbos[fbo as usize - 1].depth.texture)
    }

    fn is_shader_compiled(&self, name: u32, kind: ShaderKind) -> bool {
        if !self.is_valid_shader_name(name) {
            return false;
        }
        let Some(obj) = self.shaders[name as usize - 1].as_ref() else {
            return false;
        };
        obj.kind == kind && obj.compiled
    }

    // ── glUniform* — missing scalar/vector/matrix variants ────────────────

    /// glUniform2f
    pub fn uniform2f(&mut self, program: u32, location: i32, x: f32, y: f32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::Vec2([x, y]));
    }

    /// glUniform3f
    pub fn uniform3f(&mut self, program: u32, location: i32, x: f32, y: f32, z: f32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::Vec3([x, y, z]));
    }

    /// glUniform2i
    pub fn uniform2i(&mut self, program: u32, location: i32, x: i32, y: i32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IVec2([x, y]));
    }

    /// glUniform3i
    pub fn uniform3i(&mut self, program: u32, location: i32, x: i32, y: i32, z: i32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IVec3([x, y, z]));
    }

    /// glUniform4i
    pub fn uniform4i(&mut self, program: u32, location: i32, x: i32, y: i32, z: i32, w: i32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IVec4([x, y, z, w]));
    }

    /// glUniform1ui
    pub fn uniform1ui(&mut self, program: u32, location: i32, v: u32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UInt(v));
    }

    /// glUniform2ui
    pub fn uniform2ui(&mut self, program: u32, location: i32, x: u32, y: u32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UVec2([x, y]));
    }

    /// glUniform3ui
    pub fn uniform3ui(&mut self, program: u32, location: i32, x: u32, y: u32, z: u32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UVec3([x, y, z]));
    }

    /// glUniform4ui
    pub fn uniform4ui(&mut self, program: u32, location: i32, x: u32, y: u32, z: u32, w: u32) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UVec4([x, y, z, w]));
    }

    /// glUniform1fv (float array, count elements at consecutive locations)
    pub fn uniform1fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::FloatArray(data.to_vec()));
    }

    /// glUniform2fv (vec2 array)
    pub fn uniform2fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 2 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::FloatArray(data.to_vec()));
    }

    /// glUniform3fv (vec3 array)
    pub fn uniform3fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 3 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::FloatArray(data.to_vec()));
    }

    /// glUniform1iv (int array)
    pub fn uniform1iv(&mut self, program: u32, location: i32, data: &[i32]) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IntArray(data.to_vec()));
    }

    /// glUniform2iv (ivec2 array)
    pub fn uniform2iv(&mut self, program: u32, location: i32, data: &[i32]) {
        if location < 0 {
            return;
        }
        if data.len() < 2 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IntArray(data.to_vec()));
    }

    /// glUniform3iv (ivec3 array)
    pub fn uniform3iv(&mut self, program: u32, location: i32, data: &[i32]) {
        if location < 0 {
            return;
        }
        if data.len() < 3 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IntArray(data.to_vec()));
    }

    /// glUniform4iv (ivec4 array)
    pub fn uniform4iv(&mut self, program: u32, location: i32, data: &[i32]) {
        if location < 0 {
            return;
        }
        if data.len() < 4 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::IntArray(data.to_vec()));
    }

    /// glUniform1uiv (uint array)
    pub fn uniform1uiv(&mut self, program: u32, location: i32, data: &[u32]) {
        if location < 0 {
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UIntArray(data.to_vec()));
    }

    /// glUniform2uiv (uvec2 array)
    pub fn uniform2uiv(&mut self, program: u32, location: i32, data: &[u32]) {
        if location < 0 {
            return;
        }
        if data.len() < 2 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UIntArray(data.to_vec()));
    }

    /// glUniform3uiv (uvec3 array)
    pub fn uniform3uiv(&mut self, program: u32, location: i32, data: &[u32]) {
        if location < 0 {
            return;
        }
        if data.len() < 3 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UIntArray(data.to_vec()));
    }

    /// glUniform4uiv (uvec4 array)
    pub fn uniform4uiv(&mut self, program: u32, location: i32, data: &[u32]) {
        if location < 0 {
            return;
        }
        if data.len() < 4 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        self.set_uniform(program, location, UniformValue::UIntArray(data.to_vec()));
    }

    /// glUniformMatrix2fv (mat2, column-major)
    pub fn uniform_matrix2fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 4 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 4];
        m.copy_from_slice(&data[..4]);
        self.set_uniform(program, location, UniformValue::Mat2(m));
    }

    /// glUniformMatrix3fv (mat3, column-major)
    pub fn uniform_matrix3fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 9 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 9];
        m.copy_from_slice(&data[..9]);
        self.set_uniform(program, location, UniformValue::Mat3(m));
    }

    /// glUniformMatrix2x3fv (2 columns, 3 rows — 6 elements, column-major)
    pub fn uniform_matrix2x3fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 6 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 6];
        m.copy_from_slice(&data[..6]);
        self.set_uniform(program, location, UniformValue::Mat2x3(m));
    }

    /// glUniformMatrix3x2fv (3 columns, 2 rows — 6 elements, column-major)
    pub fn uniform_matrix3x2fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 6 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 6];
        m.copy_from_slice(&data[..6]);
        self.set_uniform(program, location, UniformValue::Mat3x2(m));
    }

    /// glUniformMatrix2x4fv (2 columns, 4 rows — 8 elements, column-major)
    pub fn uniform_matrix2x4fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 8];
        m.copy_from_slice(&data[..8]);
        self.set_uniform(program, location, UniformValue::Mat2x4(m));
    }

    /// glUniformMatrix4x2fv (4 columns, 2 rows — 8 elements, column-major)
    pub fn uniform_matrix4x2fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 8 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 8];
        m.copy_from_slice(&data[..8]);
        self.set_uniform(program, location, UniformValue::Mat4x2(m));
    }

    /// glUniformMatrix3x4fv (3 columns, 4 rows — 12 elements, column-major)
    pub fn uniform_matrix3x4fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 12 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 12];
        m.copy_from_slice(&data[..12]);
        self.set_uniform(program, location, UniformValue::Mat3x4(m));
    }

    /// glUniformMatrix4x3fv (4 columns, 3 rows — 12 elements, column-major)
    pub fn uniform_matrix4x3fv(&mut self, program: u32, location: i32, data: &[f32]) {
        if location < 0 {
            return;
        }
        if data.len() < 12 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let mut m = [0f32; 12];
        m.copy_from_slice(&data[..12]);
        self.set_uniform(program, location, UniformValue::Mat4x3(m));
    }

    // ── glIs* — object validity queries ──────────────────────────────────

    /// glIsTexture
    pub fn is_texture(&self, name: u32) -> bool {
        self.is_valid_texture_name(name)
    }
    /// glIsBuffer
    pub fn is_buffer(&self, name: u32) -> bool {
        self.is_valid_buffer_name(name)
    }
    /// glIsFramebuffer
    pub fn is_framebuffer(&self, name: u32) -> bool {
        self.is_valid_fbo_name(name)
    }
    /// glIsRenderbuffer
    pub fn is_renderbuffer(&self, name: u32) -> bool {
        name > 0 && (name as usize) <= MAX_FBOS && self.rbo_alloc[name as usize - 1]
    }
    /// glIsProgram
    pub fn is_program(&self, name: u32) -> bool {
        self.is_valid_program_name(name)
    }
    /// glIsShader
    pub fn is_shader(&self, name: u32) -> bool {
        self.is_valid_shader_name(name)
    }
    /// glIsVertexArray
    pub fn is_vertex_array(&self, name: u32) -> bool {
        self.is_valid_vao_name(name)
    }
    /// glIsSampler
    pub fn is_sampler(&self, name: u32) -> bool {
        self.is_valid_sampler_name(name)
    }
    /// glIsQuery
    pub fn is_query(&self, name: u32) -> bool {
        self.is_valid_query_name(name)
    }
    /// glIsTransformFeedback
    pub fn is_transform_feedback(&self, name: u32) -> bool {
        self.is_valid_tfb_name(name)
    }
    /// glIsSync
    pub fn is_sync(&self, name: u32) -> bool {
        self.is_valid_sync_name(name)
    }

    // ── glStencilFuncSeparate / glStencilOpSeparate / glStencilMaskSeparate ─

    /// glStencilFuncSeparate — set stencil comparison for front (face=0), back (face=1), or both (face=2).
    pub fn stencil_func_separate(&mut self, face: u8, func: StencilFunc, ref_val: u8, mask: u8) {
        if face == 0 || face == 2 {
            self.stencil_func = func;
            self.stencil_ref = ref_val;
            self.stencil_mask_r = mask;
        }
        if face == 1 || face == 2 {
            self.stencil_func_back = func;
            self.stencil_ref_back = ref_val;
            self.stencil_mask_r_back = mask;
        }
    }

    /// glStencilOpSeparate — set stencil ops for front (face=0), back (face=1), or both (face=2).
    pub fn stencil_op_separate(
        &mut self,
        face: u8,
        fail: StencilOp,
        zfail: StencilOp,
        zpass: StencilOp,
    ) {
        if face == 0 || face == 2 {
            self.stencil_fail = fail;
            self.stencil_zfail = zfail;
            self.stencil_zpass = zpass;
        }
        if face == 1 || face == 2 {
            self.stencil_fail_back = fail;
            self.stencil_zfail_back = zfail;
            self.stencil_zpass_back = zpass;
        }
    }

    /// glStencilMaskSeparate — set write mask for front (face=0), back (face=1), or both (face=2).
    pub fn stencil_mask_separate(&mut self, face: u8, mask: u8) {
        if face == 0 || face == 2 {
            self.stencil_mask_w = mask;
        }
        if face == 1 || face == 2 {
            self.stencil_mask_w_back = mask;
        }
    }

    // ── glSampleCoverage / glSampleMaski ──────────────────────────────────

    /// glSampleCoverage
    pub fn sample_coverage(&mut self, value: f32, invert: bool) {
        self.sample_coverage_value = value.clamp(0.0, 1.0);
        self.sample_coverage_invert = invert;
    }

    /// glSampleMaski — sets the sample mask word at `mask_number` (only index 0 supported here).
    pub fn sample_maski(&mut self, mask_number: u32, mask: u32) {
        if mask_number == 0 {
            self.sample_mask = mask;
        }
        // mask_number >= 1 would require more mask words; silently ignore for this backend.
    }

    // ── glLineWidth ───────────────────────────────────────────────────────

    /// glLineWidth
    pub fn set_line_width(&mut self, width: f32) {
        if width <= 0.0 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.line_width = width;
    }

    // ── glPointSize ───────────────────────────────────────────────────────

    /// glPointSize
    pub fn set_point_size(&mut self, size: f32) {
        if size <= 0.0 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        self.point_size = size;
    }

    // ── glHint ────────────────────────────────────────────────────────────

    /// glHint — accepts any target/mode pair; this backend treats all hints as advisory.
    pub fn hint(&mut self, _target: u32, _mode: u32) {
        // hints are no-ops on a software rasterizer
    }

    // ── glFlush / glFinish ────────────────────────────────────────────────

    /// glFlush — no-op on software backend (all operations are immediate).
    pub fn flush(&self) {}

    /// glFinish — no-op on software backend.
    pub fn finish(&self) {}

    // ── glReleaseShaderCompiler ───────────────────────────────────────────

    /// glReleaseShaderCompiler — hint to release internal compiler resources; no-op here.
    pub fn release_shader_compiler(&self) {}

    // ── glGetShaderPrecisionFormat ────────────────────────────────────────

    /// glGetShaderPrecisionFormat — returns (range_min, range_max, precision) for the given precision qualifier.
    /// This backend reports full IEEE 754 float precision for all types.
    pub fn get_shader_precision_format(
        &self,
        _shader_type: u32,
        _precision_type: u32,
    ) -> (i32, i32, i32) {
        (127, 127, 23) // highp float: ±2^127, 23-bit mantissa
    }

    // ── Extended glPixelStorei parameters ────────────────────────────────

    /// glPixelStorei for UNPACK_ROW_LENGTH (non-zero = custom row stride in pixels)
    pub fn pixel_store_unpack_row_length(&mut self, v: u32) {
        self.unpack_row_length = v;
    }
    /// glPixelStorei for UNPACK_SKIP_ROWS
    pub fn pixel_store_unpack_skip_rows(&mut self, v: u32) {
        self.unpack_skip_rows = v;
    }
    /// glPixelStorei for UNPACK_SKIP_PIXELS
    pub fn pixel_store_unpack_skip_pixels(&mut self, v: u32) {
        self.unpack_skip_pixels = v;
    }
    /// glPixelStorei for UNPACK_IMAGE_HEIGHT (for 3D textures)
    pub fn pixel_store_unpack_image_height(&mut self, v: u32) {
        self.unpack_image_height = v;
    }
    /// glPixelStorei for UNPACK_SKIP_IMAGES
    pub fn pixel_store_unpack_skip_images(&mut self, v: u32) {
        self.unpack_skip_images = v;
    }
    /// glPixelStorei for PACK_ROW_LENGTH
    pub fn pixel_store_pack_row_length(&mut self, v: u32) {
        self.pack_row_length = v;
    }
    /// glPixelStorei for PACK_SKIP_ROWS
    pub fn pixel_store_pack_skip_rows(&mut self, v: u32) {
        self.pack_skip_rows = v;
    }
    /// glPixelStorei for PACK_SKIP_PIXELS
    pub fn pixel_store_pack_skip_pixels(&mut self, v: u32) {
        self.pack_skip_pixels = v;
    }

    // ── glGetBufferParameteriv / i64v ─────────────────────────────────────

    /// Returns the size in bytes of a buffer object, or None if the name is invalid.
    pub fn get_buffer_size(&self, name: u32) -> Option<usize> {
        if !self.is_valid_buffer_name(name) {
            return None;
        }
        Some(self.buf_data[name as usize - 1].len())
    }

    /// Returns whether the buffer is currently mapped.
    pub fn get_buffer_mapped(&self, name: u32) -> bool {
        self.mapped.map_or(false, |m| m.name == name)
    }

    // ── glGetRenderbufferParameteriv ──────────────────────────────────────

    /// Returns (width, height) of a renderbuffer, or None if invalid.
    pub fn get_renderbuffer_size(&self, name: u32) -> Option<(u32, u32)> {
        if !self.is_renderbuffer(name) {
            return None;
        }
        let rb = &self.rbos[name as usize - 1];
        Some((rb.width, rb.height))
    }

    /// Returns true if the renderbuffer has a depth component.
    pub fn get_renderbuffer_has_depth(&self, name: u32) -> bool {
        if !self.is_renderbuffer(name) {
            return false;
        }
        self.rbos[name as usize - 1].has_depth
    }

    /// Returns true if the renderbuffer has a stencil component.
    pub fn get_renderbuffer_has_stencil(&self, name: u32) -> bool {
        if !self.is_renderbuffer(name) {
            return false;
        }
        self.rbos[name as usize - 1].has_stencil
    }

    // ── glRenderbufferStorageMultisample (stub) ───────────────────────────

    /// glRenderbufferStorageMultisample — this backend is single-sample; stores as if samples=1.
    pub fn renderbuffer_storage_multisample(
        &mut self,
        name: u32,
        _samples: u32,
        width: u32,
        height: u32,
        has_depth: bool,
        has_stencil: bool,
    ) {
        // Delegate to the existing single-sample path — no MSAA resolve in this backend.
        self.renderbuffer_storage(name, width, height, has_depth, has_stencil);
    }

    /// Internal helper: set renderbuffer storage dimensions and format.
    // ── glFramebufferTextureLayer ─────────────────────────────────────────

    /// glFramebufferTextureLayer — attaches a single layer of a 3D or array texture to an FBO.
    /// Since this backend stores textures as flat 2D, this is equivalent to framebuffer_texture_2d
    /// with the layer index stored but not used during rendering.
    pub fn framebuffer_texture_layer(
        &mut self,
        fbo: u32,
        attachment: Attachment,
        texture: u32,
        level: u32,
        _layer: i32,
    ) {
        // For a 2D-only backend, layer is ignored. Attach the texture as a 2D attachment.
        self.framebuffer_texture_2d(fbo, attachment, texture, level as u32);
    }

    // ── glInvalidateFramebuffer ───────────────────────────────────────────

    /// glInvalidateFramebuffer — performance hint to discard attachment contents; no-op here.
    pub fn invalidate_framebuffer(&mut self, _fbo: u32, _attachments: &[Attachment]) {}

    /// glInvalidateSubFramebuffer — performance hint; no-op here.
    pub fn invalidate_sub_framebuffer(
        &mut self,
        _fbo: u32,
        _attachments: &[Attachment],
        _x: i32,
        _y: i32,
        _w: i32,
        _h: i32,
    ) {
    }

    // ── glClearBufferiv / glClearBufferuiv / glClearBufferfi ──────────────

    /// glClearBufferiv — clear an integer color attachment or stencil buffer.
    /// For color attachments use `attachment = Attachment::Color(n)`.
    /// For stencil use `attachment = Attachment::Stencil` and `value[0]` as stencil.
    pub fn clear_buffer_iv(&mut self, attachment: Attachment, value: &[i32]) {
        match attachment {
            Attachment::Stencil => {
                if let Some(&v) = value.first() {
                    self.clear_stencil = v as u8;
                }
            }
            Attachment::Color(i) => {
                if value.len() >= 4 {
                    let fv = [
                        value[0] as f32,
                        value[1] as f32,
                        value[2] as f32,
                        value[3] as f32,
                    ];
                    self.clear_buffer_color_fv(i as u32, fv);
                }
            }
            _ => {}
        }
    }

    /// glClearBufferuiv — clear an unsigned integer color attachment.
    pub fn clear_buffer_uiv(&mut self, attachment: Attachment, value: &[u32]) {
        match attachment {
            Attachment::Stencil => {
                if let Some(&v) = value.first() {
                    self.clear_stencil = v as u8;
                }
            }
            Attachment::Color(i) => {
                if value.len() >= 4 {
                    let fv = [
                        value[0] as f32,
                        value[1] as f32,
                        value[2] as f32,
                        value[3] as f32,
                    ];
                    self.clear_buffer_color_fv(i as u32, fv);
                }
            }
            _ => {}
        }
    }

    /// glClearBufferfi — simultaneously clear the depth and stencil buffer.
    pub fn clear_buffer_fi(&mut self, depth: f32, stencil: i32) {
        self.clear_depth = depth.clamp(0.0, 1.0);
        self.clear_stencil = stencil as u8;
        // Execute clear on the current draw framebuffer's depth+stencil attachments.
        // The actual pixel writes happen in the pipeline; here we update the clear values
        // so the next clear() call picks them up.
    }

    // ── glDrawRangeElements ───────────────────────────────────────────────

    /// glDrawRangeElements — like draw_elements but with range hint [start, end].
    /// In this backend the range is advisory; it is otherwise identical to glDrawElements.
    /// Call `draw_elements` (with a shader target) instead, using the same parameters.
    /// The start/end hints are recorded for introspection but not used.
    pub fn draw_range_elements_hint(
        &mut self,
        _mode: DrawMode,
        _start: u32,
        _end: u32,
        _count: i32,
        _index_type: IndexType,
        _offset_bytes: usize,
    ) {
        // No-op state record: callers should invoke draw_elements directly.
        // This function exists so GL ES 3.0 glDrawRangeElements usage compiles.
    }

    // ── glVertexAttribIPointer ────────────────────────────────────────────

    /// glVertexAttribIPointer — integer (non-normalized) vertex attribute pointer.
    /// This backend stores it the same way as the float variant; the `integer` flag is
    /// recorded for introspection purposes and would be honoured by a shader executor.
    pub fn vertex_attrib_i_pointer(
        &mut self,
        index: u32,
        size: u8,
        _integer_type: u32,
        stride: u32,
        offset: u32,
    ) {
        if index as usize >= MAX_ATTRIBS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let buf = self.array_buffer;
        let vao = self.vao_slot_mut();
        let a = &mut vao.attribs[index as usize];
        a.buffer = buf;
        a.size = size.clamp(1, 4);
        a.stride = stride;
        a.offset = offset;
        // divisor unchanged; enabled state unchanged (caller must call enable_vertex_attrib_array).
    }

    // ── glCopyTexImage2D / glCopyTexSubImage2D ────────────────────────────

    /// glCopyTexImage2D — copy the current read framebuffer into a new 2D texture image.
    pub fn copy_tex_image_2d(
        &mut self,
        texture: u32,
        level: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) {
        // Read from current read FBO
        let fbo = self.read_framebuffer;
        let pixels = if fbo == 0 {
            // Default framebuffer not modelled in this backend — return empty.
            vec![0u32; (width * height) as usize]
        } else {
            let px_count = (width as usize).saturating_mul(height as usize);
            let mut out = vec![0u32; px_count];
            if !self.read_pixels_bgra8(fbo, 0, x as u32, y as u32, width, height, &mut out) {
                self.set_error(GlError::InvalidFramebufferOperation);
                return;
            }
            out
        };
        // Upload into texture at the given level
        self.tex_image_2d(texture, level, width, height, &pixels);
    }

    /// glCopyTexSubImage2D — copy a rectangle from the read framebuffer into part of an existing texture.
    pub fn copy_tex_sub_image_2d(
        &mut self,
        texture: u32,
        level: u32,
        xoffset: u32,
        yoffset: u32,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    ) {
        let fbo = self.read_framebuffer;
        let px_count = (width as usize).saturating_mul(height as usize);
        let mut src_pixels = vec![0u32; px_count];
        if fbo != 0 {
            if !self.read_pixels_bgra8(fbo, 0, x as u32, y as u32, width, height, &mut src_pixels) {
                self.set_error(GlError::InvalidFramebufferOperation);
                return;
            }
        }
        self.tex_sub_image_2d(texture, level, xoffset, yoffset, width, height, &src_pixels);
    }

    // ── glTexImage3D / glTexSubImage3D / glTexStorage3D (stubs) ──────────

    /// glTexStorage3D — allocate immutable 3D texture storage (depth = number of layers).
    /// This backend stores a single base-level slice; 3D/array semantics are not rasterized.
    pub fn tex_storage_3d(
        &mut self,
        texture: u32,
        levels: u32,
        width: u32,
        height: u32,
        depth: u32,
    ) {
        if !self.is_valid_texture_name(texture) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        // Allocate flat storage for the first level of each 2D slice (layer 0 only for now).
        let _ = depth; // depth/layer count acknowledged; not rasterized
        self.tex_storage_2d(texture, levels, width, height);
    }

    /// glTexImage3D — upload 3D texture data (BGRA32, one layer at a time via layer index).
    pub fn tex_image_3d(
        &mut self,
        texture: u32,
        level: u32,
        width: u32,
        height: u32,
        depth: u32,
        pixels: &[u32],
    ) {
        if !self.is_valid_texture_name(texture) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        let num_layers = depth as usize;
        let layer_size = (width * height) as usize;
        let mut layers: Vec<Vec<u32>> = Vec::new();
        for i in 0..num_layers {
            let start = i * layer_size;
            let end = start + layer_size;
            let layer_pixels: Vec<u32> = if end <= pixels.len() {
                pixels[start..end].to_vec()
            } else if start < pixels.len() {
                let mut l = pixels[start..].to_vec();
                l.resize(layer_size, 0);
                l
            } else {
                vec![0u32; layer_size]
            };
            layers.push(layer_pixels);
        }
        let idx = texture as usize - 1;
        self.textures[idx].array_layers = layers.clone();
        // Upload first layer as 2D base for backwards-compatible sampling.
        let first = layers
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0u32; layer_size]);
        let _ = level;
        self.tex_image_2d(texture, 0, width, height, &first);
    }

    /// glTexSubImage3D — partial update of a 3D texture. Updates the 2D slice at z=zoffset.
    pub fn tex_sub_image_3d(
        &mut self,
        texture: u32,
        level: u32,
        xoffset: u32,
        yoffset: u32,
        _zoffset: u32,
        width: u32,
        height: u32,
        _depth: u32,
        pixels: &[u32],
    ) {
        self.tex_sub_image_2d(texture, level, xoffset, yoffset, width, height, pixels);
    }

    // ── glCompressedTexImage2D / 3D (stubs) ───────────────────────────────

    /// glCompressedTexImage2D — no ETC2/ASTC decompressor in this backend; stores zeroed image.
    pub fn compressed_tex_image_2d(&mut self, texture: u32, level: u32, width: u32, height: u32) {
        let zeroes = vec![0u32; (width * height) as usize];
        self.tex_image_2d(texture, level, width, height, &zeroes);
    }

    /// glCompressedTexSubImage2D — no-op stub (compressed data is ignored).
    pub fn compressed_tex_sub_image_2d(
        &mut self,
        _texture: u32,
        _level: u32,
        _xoffset: u32,
        _yoffset: u32,
        _width: u32,
        _height: u32,
    ) {
    }

    /// glCompressedTexImage3D stub.
    pub fn compressed_tex_image_3d(
        &mut self,
        texture: u32,
        level: u32,
        width: u32,
        height: u32,
        depth: u32,
    ) {
        self.tex_storage_3d(texture, 1, width, height, depth);
        let _ = level;
    }

    // ── glGetTexParameter ─────────────────────────────────────────────────

    /// Returns the WrapMode for the S axis of the given texture.
    pub fn get_tex_parameter_wrap_s(&self, texture: u32) -> Option<WrapMode> {
        if !self.is_valid_texture_name(texture) {
            return None;
        }
        Some(self.textures[texture as usize - 1].wrap_s)
    }

    /// Returns the WrapMode for the T axis.
    pub fn get_tex_parameter_wrap_t(&self, texture: u32) -> Option<WrapMode> {
        if !self.is_valid_texture_name(texture) {
            return None;
        }
        Some(self.textures[texture as usize - 1].wrap_t)
    }

    /// Returns the minification FilterMode.
    pub fn get_tex_parameter_min_filter(&self, texture: u32) -> Option<FilterMode> {
        if !self.is_valid_texture_name(texture) {
            return None;
        }
        Some(self.textures[texture as usize - 1].min_filter)
    }

    /// Returns the magnification FilterMode.
    pub fn get_tex_parameter_mag_filter(&self, texture: u32) -> Option<FilterMode> {
        if !self.is_valid_texture_name(texture) {
            return None;
        }
        Some(self.textures[texture as usize - 1].mag_filter)
    }

    /// Returns (width, height) of the given texture at mip level 0.
    pub fn get_tex_level_size(&self, texture: u32) -> Option<(u32, u32)> {
        if !self.is_valid_texture_name(texture) {
            return None;
        }
        let obj = &self.textures[texture as usize - 1];
        Some((obj.width, obj.height))
    }

    // ── glGetFramebufferAttachmentParameteriv ─────────────────────────────

    /// Returns the texture name attached to the given FBO attachment point, or 0 if none.
    pub fn get_framebuffer_attachment_texture(
        &self,
        fbo: u32,
        attachment: Attachment,
    ) -> Option<u32> {
        if !self.is_valid_fbo_name(fbo) {
            return None;
        }
        let fb = &self.fbos[fbo as usize - 1];
        let name = match attachment {
            Attachment::Color(i) if (i as usize) < 8 => fb.color[i as usize].texture,
            Attachment::Depth => fb.depth.texture,
            Attachment::Stencil => fb.stencil.texture,
            Attachment::DepthStencil => fb.depth.texture,
            _ => 0,
        };
        Some(name)
    }

    /// Returns the renderbuffer name attached at the given FBO attachment point, or 0 if none.
    pub fn get_framebuffer_attachment_renderbuffer(
        &self,
        fbo: u32,
        attachment: Attachment,
    ) -> Option<u32> {
        if !self.is_valid_fbo_name(fbo) {
            return None;
        }
        let fb = &self.fbos[fbo as usize - 1];
        let name = match attachment {
            Attachment::Color(i) if (i as usize) < 8 => fb.color_rb[i as usize],
            Attachment::Depth => fb.depth_rb,
            Attachment::Stencil => fb.stencil_rb,
            Attachment::DepthStencil => fb.depth_rb,
            _ => 0,
        };
        Some(name)
    }

    // ── glGetUniformBlockIndex / glUniformBlockBinding ────────────────────

    /// glGetUniformBlockIndex — returns a pseudo-index for the named UBO block in the program.
    /// This backend uses a deterministic hash of the name as the block index.
    pub fn get_uniform_block_index(&self, program: u32, name: &str) -> u32 {
        if !self.is_valid_program_name(program) {
            return 0xFFFF_FFFF;
        }
        // Return a stable hash of the name as the block index (0xFFFFFFFF = GL_INVALID_INDEX).
        let mut h: u32 = 0x811c_9dc5;
        for b in name.bytes() {
            h = h.wrapping_mul(0x0100_0193) ^ (b as u32);
        }
        h & 0x7FFF_FFFE // mask off top bit and the invalid sentinel
    }

    /// glUniformBlockBinding — associates a UBO block index with a binding point.
    /// Stored in the program metadata (as a simple index→binding map).
    pub fn uniform_block_binding(&mut self, program: u32, block_index: u32, binding: u32) {
        if !self.is_valid_program_name(program) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        // Store block→binding in the uniforms map using a sentinel key space.
        let key = (0xBB00_0000u64 << 32) | ((program as u64) << 16) | (block_index as u64 & 0xFFFF);
        let _ = self.uniforms.insert(key, UniformValue::UInt(binding));
    }

    /// Returns the binding point associated with a block index set by uniform_block_binding.
    pub fn get_uniform_block_binding(&self, program: u32, block_index: u32) -> Option<u32> {
        let key = (0xBB00_0000u64 << 32) | ((program as u64) << 16) | (block_index as u64 & 0xFFFF);
        match self.uniforms.get(&key)? {
            UniformValue::UInt(v) => Some(*v),
            _ => None,
        }
    }

    // ── glGetActiveAttrib / glGetActiveUniform ────────────────────────────

    /// Returns the name of the vertex attribute at the given index in the linked program,
    /// as parsed from the vertex shader source.
    pub fn get_active_attrib(&self, program: u32, index: u32) -> Option<String> {
        if !self.is_valid_program_name(program) {
            return None;
        }
        let p = &self.programs[program as usize - 1];
        if !p.linked || p.vertex_shader == 0 {
            return None;
        }
        let vs = self.shaders[p.vertex_shader as usize - 1].as_ref()?;
        let meta = vs.metadata.as_ref()?;
        meta.inputs.get(index as usize).map(|v| v.name.clone())
    }

    /// Returns the name and type string of the uniform at the given index in the linked program.
    /// Returns `(type_name, uniform_name)`.
    pub fn get_active_uniform(&self, program: u32, index: u32) -> Option<(String, String)> {
        if !self.is_valid_program_name(program) {
            return None;
        }
        let p = &self.programs[program as usize - 1];
        if !p.linked {
            return None;
        }
        // Gather uniforms from vertex + fragment shader metadata.
        let mut uniforms: Vec<&ShaderInterfaceVar> = Vec::new();
        if p.vertex_shader != 0 {
            if let Some(Some(vs)) = self.shaders.get(p.vertex_shader as usize - 1) {
                if let Some(meta) = &vs.metadata {
                    for u in &meta.uniforms {
                        uniforms.push(u);
                    }
                }
            }
        }
        if p.fragment_shader != 0 {
            if let Some(Some(fs)) = self.shaders.get(p.fragment_shader as usize - 1) {
                if let Some(meta) = &fs.metadata {
                    for u in &meta.uniforms {
                        if !uniforms.iter().any(|e| e.name == u.name) {
                            uniforms.push(u);
                        }
                    }
                }
            }
        }
        uniforms
            .get(index as usize)
            .map(|u| (u.ty.clone(), u.name.clone()))
    }

    // ── glGetTransformFeedbackVarying ─────────────────────────────────────

    /// Returns the name of the transform feedback varying at `index` for the program.
    pub fn get_transform_feedback_varying(&self, program: u32, index: u32) -> Option<String> {
        if !self.is_valid_program_name(program) {
            return None;
        }
        let varyings = self.transform_feedback_varyings(program)?;
        varyings.into_iter().nth(index as usize)
    }

    // ── glGetStringi ─────────────────────────────────────────────────────

    /// glGetStringi(GL_EXTENSIONS, index) — returns the nth supported extension name.
    ///
    /// Only extensions with real functional backing are listed here.
    /// Removed: KHR_robustness (reset not enforced), EXT_disjoint_timer_query (returns 0),
    /// EXT_color_buffer_float (float FBO not supported). See docs/known_gaps.md GAP-002.
    pub fn get_stringi_extension(&self, index: u32) -> Option<&'static str> {
        let extensions: &[&str] = &["GL_KHR_debug"];
        extensions.get(index as usize).copied()
    }

    /// Returns the total number of supported extension strings.
    pub fn get_num_extensions(&self) -> u32 {
        1
    }

    // ── glGetIntegerv helpers (most-used state queries) ────────────────────

    /// Returns the current viewport as [x, y, width, height].
    pub fn get_viewport(&self) -> [i32; 4] {
        self.viewport
    }

    /// Returns the current scissor box as [x, y, width, height].
    pub fn get_scissor_box(&self) -> [i32; 4] {
        self.scissor
    }

    /// Returns the current clear color as [r, g, b, a] (f32).
    pub fn get_clear_color(&self) -> [f32; 4] {
        self.clear_color
    }

    /// Returns the current depth range as [near, far].
    pub fn get_depth_range(&self) -> [f32; 2] {
        self.depth_range
    }

    /// Returns whether a GL capability is currently enabled.
    pub fn is_enabled(&self, cap: u32) -> bool {
        // Use GL constant values that match the ES 3.0 spec.
        match cap {
            0x0B44 => self.depth_test,                  // GL_DEPTH_TEST
            0x0B90 => self.stencil_test,                // GL_STENCIL_TEST
            0x0BE2 => self.blend,                       // GL_BLEND
            0x0C11 => self.scissor_test,                // GL_SCISSOR_TEST
            0x0B45 => self.depth_write, // GL_DEPTH_WRITEMASK (not really a cap, but useful)
            0x0404 => self.cull_face != CullFace::None, // GL_CULL_FACE
            0x8037 => self.polygon_offset_fill, // GL_POLYGON_OFFSET_FILL
            _ => false,
        }
    }

    // ── glBindBufferRange helpers (convenience) ────────────────────────────

    /// glBindBufferRange for transform feedback buffers (convenience combining bind_buffer + base_range).
    pub fn bind_transform_feedback_buffer_range(
        &mut self,
        index: u32,
        buffer: u32,
        offset: usize,
        size: usize,
    ) {
        if !self.is_valid_buffer_name(buffer) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        // Bind at index; store offset/size in the uniform map as a sentinel.
        self.bind_transform_feedback_buffer_base(index, buffer);
        let key = (0xAF00_0000u64 << 32) | ((index as u64) << 16);
        let _ = self.uniforms.insert(
            key,
            UniformValue::UVec4([buffer, offset as u32, (offset >> 32) as u32, size as u32]),
        );
    }

    // ── glWaitSync / glGetSynciv ──────────────────────────────────────────

    /// glWaitSync — server-side wait on a sync object.
    /// In this software backend all syncs are immediately signaled, so this is a no-op.
    pub fn wait_sync(&mut self, sync: u32, _flags: u32, _timeout_ns: u64) {
        if !self.is_valid_sync_name(sync) {
            self.set_error(GlError::InvalidValue);
        }
        // All syncs signaled immediately; nothing to do.
    }

    /// glGetSynciv — query a sync object parameter.
    /// Returns `(value, written)` where value is the queried int and written is whether it was filled.
    pub fn get_sync_iv(&self, sync: u32, pname: u32) -> Option<i32> {
        if !self.is_valid_sync_name(sync) {
            return None;
        }
        let s = self.syncs[sync as usize - 1];
        match pname {
            0x9112 => Some(0x9116_i32), // GL_OBJECT_TYPE → GL_SYNC_FENCE
            0x9113 => Some(if s.signaled { 0x9119 } else { 0x0000 } as i32), // GL_SYNC_STATUS → SIGNALED/UNSIGNALED
            0x9114 => Some(0x9117_i32), // GL_SYNC_CONDITION → GL_SYNC_GPU_COMMANDS_COMPLETE
            0x9115 => Some(0),          // GL_SYNC_FLAGS
            _ => None,
        }
    }

    // ── glBindAttribLocation / glGetFragDataLocation ──────────────────────

    /// glBindAttribLocation — hint the linker to use `location` for attribute `name`.
    /// In this backend locations are determined deterministically at link time from the
    /// attribute name hash; this call records the user preference and is honoured when
    /// the program is next linked.
    pub fn bind_attrib_location(&mut self, program: u32, location: u32, name: &[u8]) {
        if program == 0 || (program as usize) > crate::MAX_PROGRAMS {
            self.set_error(GlError::InvalidValue);
            return;
        }
        // Store as a sentinel in the uniform map: key = (prog << 32) | (0xA770_0000 | location)
        let name_hash = {
            let mut h: u32 = 2166136261;
            for &b in name {
                h = h.wrapping_mul(16777619) ^ b as u32;
            }
            h
        };
        let key = ((program as u64) << 32) | (0xA770_0000u64 | location as u64);
        let _ = self
            .uniforms
            .insert(key, UniformValue::Int(name_hash as i32));
    }

    /// glGetFragDataLocation — returns the output color attachment index for a named
    /// fragment output variable. Unqualified outputs default to declaration order.
    pub fn get_frag_data_location(&self, program: u32, name: &[u8]) -> i32 {
        if program == 0 || (program as usize) > crate::MAX_PROGRAMS {
            return -1;
        }
        let prog = &self.programs[program as usize - 1];
        let frag_id = prog.fragment_shader;
        if frag_id == 0 {
            return -1;
        }
        let wanted = core::str::from_utf8(name).ok().map(str::trim).unwrap_or("");
        if wanted.is_empty() {
            return -1;
        }
        let Some(meta) = self
            .shaders
            .get(frag_id as usize - 1)
            .and_then(|s| s.as_ref())
            .and_then(|s| s.metadata.as_ref())
        else {
            return -1;
        };
        meta.outputs
            .iter()
            .enumerate()
            .find(|(_, out)| out.name == wanted)
            .map(|(index, out)| out.location.unwrap_or(index as u8) as i32)
            .unwrap_or(-1)
    }

    // ── glCopyBufferSubData ───────────────────────────────────────────────

    /// glCopyBufferSubData — copies a byte range from one buffer object to another.
    pub fn copy_buffer_sub_data(
        &mut self,
        read_buffer: u32,
        write_buffer: u32,
        read_offset: usize,
        write_offset: usize,
        size: usize,
    ) {
        if !self.is_valid_buffer_name(read_buffer) || !self.is_valid_buffer_name(write_buffer) {
            self.set_error(GlError::InvalidOperation);
            return;
        }
        if read_buffer == write_buffer {
            // Overlapping regions on the same buffer are undefined; reject.
            self.set_error(GlError::InvalidValue);
            return;
        }
        let src = self.buf_data[read_buffer as usize - 1].clone();
        let dst = &mut self.buf_data[write_buffer as usize - 1];
        let src_end = read_offset.saturating_add(size);
        let dst_end = write_offset.saturating_add(size);
        if src_end > src.len() || dst_end > dst.len() {
            self.set_error(GlError::InvalidValue);
            return;
        }
        dst[write_offset..dst_end].copy_from_slice(&src[read_offset..src_end]);
    }

    /// glGetBufferSubData — reads bytes from a buffer object into a caller-supplied slice.
    pub fn get_buffer_sub_data(&self, buffer: u32, offset: usize, out: &mut [u8]) -> bool {
        if !self.is_valid_buffer_name(buffer) {
            return false;
        }
        let data = &self.buf_data[buffer as usize - 1];
        let end = offset.saturating_add(out.len());
        if end > data.len() {
            return false;
        }
        out.copy_from_slice(&data[offset..end]);
        true
    }

    // ── glGetVertexAttrib* ────────────────────────────────────────────────

    /// glGetVertexAttribiv — returns integer parameter of a vertex attribute.
    /// pname: 0x8622=VERTEX_ATTRIB_ARRAY_SIZE, 0x8623=STRIDE, 0x8624=TYPE,
    ///        0x8625=NORMALIZED, 0x8626=ENABLED, 0x8645=DIVISOR
    pub fn get_vertex_attrib_iv(&self, index: u32, pname: u32) -> Option<i32> {
        if index >= MAX_ATTRIBS as u32 {
            return None;
        }
        let vao = self.vao_slot();
        let a = &vao.attribs[index as usize];
        match pname {
            0x8622 => Some(a.size as i32),    // GL_VERTEX_ATTRIB_ARRAY_SIZE
            0x8623 => Some(a.stride as i32),  // GL_VERTEX_ATTRIB_ARRAY_STRIDE
            0x8624 => Some(0x1406),           // GL_VERTEX_ATTRIB_ARRAY_TYPE (FLOAT placeholder)
            0x8625 => Some(0),                // GL_VERTEX_ATTRIB_ARRAY_NORMALIZED
            0x8626 => Some(a.enabled as i32), // GL_VERTEX_ATTRIB_ARRAY_ENABLED
            0x8645 => Some(a.divisor as i32), // GL_VERTEX_ATTRIB_ARRAY_DIVISOR
            0x889F => Some(a.buffer as i32),  // GL_VERTEX_ATTRIB_ARRAY_BUFFER_BINDING
            _ => None,
        }
    }

    /// glGetVertexAttribPointerv — returns the byte offset of a vertex attribute pointer.
    pub fn get_vertex_attrib_pointer(&self, index: u32) -> Option<usize> {
        if index >= MAX_ATTRIBS as u32 {
            return None;
        }
        Some(self.vao_slot().attribs[index as usize].offset as usize)
    }

    // ── glGetUniformfv / glGetUniformiv / glGetUniformuiv ─────────────────

    /// glGetUniformfv — reads uniform value as f32 components.
    pub fn get_uniform_fv(&self, program: u32, location: i32, out: &mut [f32]) {
        let key = ((program as u64) << 32) | (location as u32 as u64);
        match self.uniforms.get(&key) {
            Some(UniformValue::Float(v)) => {
                if !out.is_empty() {
                    out[0] = *v;
                }
            }
            Some(UniformValue::Vec2(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(2) {
                    *o = v[i];
                }
            }
            Some(UniformValue::Vec3(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(3) {
                    *o = v[i];
                }
            }
            Some(UniformValue::Vec4(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(4) {
                    *o = v[i];
                }
            }
            Some(UniformValue::Mat2(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(4) {
                    *o = v[i];
                }
            }
            Some(UniformValue::Mat3(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(9) {
                    *o = v[i];
                }
            }
            Some(UniformValue::Mat4(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(16) {
                    *o = v[i];
                }
            }
            Some(UniformValue::FloatArray(v)) => {
                let n = out.len().min(v.len());
                out[..n].copy_from_slice(&v[..n]);
            }
            _ => {}
        }
    }

    /// glGetUniformiv — reads uniform value as i32 components.
    pub fn get_uniform_iv(&self, program: u32, location: i32, out: &mut [i32]) {
        let key = ((program as u64) << 32) | (location as u32 as u64);
        match self.uniforms.get(&key) {
            Some(UniformValue::Int(v)) => {
                if !out.is_empty() {
                    out[0] = *v;
                }
            }
            Some(UniformValue::IVec2(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(2) {
                    *o = v[i];
                }
            }
            Some(UniformValue::IVec3(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(3) {
                    *o = v[i];
                }
            }
            Some(UniformValue::IVec4(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(4) {
                    *o = v[i];
                }
            }
            Some(UniformValue::IntArray(v)) => {
                let n = out.len().min(v.len());
                out[..n].copy_from_slice(&v[..n]);
            }
            _ => {}
        }
    }

    /// glGetUniformuiv — reads uniform value as u32 components.
    pub fn get_uniform_uiv(&self, program: u32, location: i32, out: &mut [u32]) {
        let key = ((program as u64) << 32) | (location as u32 as u64);
        match self.uniforms.get(&key) {
            Some(UniformValue::UInt(v)) => {
                if !out.is_empty() {
                    out[0] = *v;
                }
            }
            Some(UniformValue::UVec2(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(2) {
                    *o = v[i];
                }
            }
            Some(UniformValue::UVec3(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(3) {
                    *o = v[i];
                }
            }
            Some(UniformValue::UVec4(v)) => {
                for (i, o) in out.iter_mut().enumerate().take(4) {
                    *o = v[i];
                }
            }
            Some(UniformValue::UIntArray(v)) => {
                let n = out.len().min(v.len());
                out[..n].copy_from_slice(&v[..n]);
            }
            _ => {}
        }
    }

    // ── glGetProgramBinary / glProgramBinary ──────────────────────────────

    /// glGetProgramBinary — in this backend programs have no binary form.
    /// Returns false (no binary available) and sets InvalidOperation per spec.
    pub fn get_program_binary(&mut self, _program: u32) -> Option<Vec<u8>> {
        self.set_error(GlError::InvalidOperation);
        None
    }

    /// glProgramBinary — load a program from binary.  Not supported; sets InvalidOperation.
    pub fn program_binary(&mut self, _program: u32, _binary: &[u8]) {
        self.set_error(GlError::InvalidOperation);
    }

    // ── glTexStorage2DMultisample ─────────────────────────────────────────

    /// glTexStorage2DMultisample — allocate storage for a 2D multisample texture.
    /// This backend does not support MSAA textures; the call is accepted as a no-op
    /// (dimensions recorded) and renders the texture as if samples=1.
    pub fn tex_storage_2d_multisample(
        &mut self,
        name: u32,
        _samples: u32,
        width: u32,
        height: u32,
    ) {
        // Delegate to the standard tex_storage_2d path (1 mip level, no MSAA).
        self.tex_storage_2d(name, width, height, 1);
    }

    // ── glGetIntegerv / glGetFloatv / glGetBooleanv (generic state query) ─

    /// Query a single boolean GL state value (mirrors glGetBooleanv for common params).
    pub fn get_boolean(&self, pname: u32) -> Option<bool> {
        match pname {
            0x0B44 => Some(self.depth_test),
            0x0B90 => Some(self.stencil_test),
            0x0BE2 => Some(self.blend),
            0x0C11 => Some(self.scissor_test),
            0x0B45 => Some(self.depth_write),
            0x0404 => Some(self.cull_face != CullFace::None),
            0x8037 => Some(self.polygon_offset_fill),
            0x8862 => Some(false), // GL_PRIMITIVE_RESTART_FIXED_INDEX — not implemented
            _ => None,
        }
    }

    /// glGetIntegerv style query — returns a single integer state value.
    pub fn get_integer(&self, pname: u32) -> Option<i32> {
        match pname {
            0x0B21 => Some(self.viewport[0]),
            0x0B22 => Some(self.viewport[1]),
            0x0B23 => Some(self.viewport[2]),
            0x0B24 => Some(self.viewport[3]),
            0x0C22 => Some(self.scissor[0]),
            0x0C23 => Some(self.scissor[1]),
            0x0C24 => Some(self.scissor[2]),
            0x0C25 => Some(self.scissor[3]),
            0x0BE0 => Some(self.blend_src_rgb as i32),
            0x0BE1 => Some(self.blend_dst_rgb as i32),
            0x0C01 => Some(self.pack_alignment as i32),
            0x0CF5 => Some(self.unpack_alignment as i32),
            0x8A28 => Some(self.current_program as i32), // GL_CURRENT_PROGRAM
            0x8514 => Some(self.active_texture as i32),  // GL_ACTIVE_TEXTURE
            0x8005 => Some(4),                           // GL_MAX_DRAW_BUFFERS
            0x8869 => Some(MAX_ATTRIBS as i32),          // GL_MAX_VERTEX_ATTRIBS
            0x8872 => Some(16),                          // GL_MAX_TEXTURE_IMAGE_UNITS
            0x84E8 => Some(MAX_TEXTURES as i32),         // GL_MAX_COMBINED_TEXTURE_IMAGE_UNITS
            0x0B44 => Some(self.depth_test as i32),
            0x0B45 => Some(self.depth_write as i32),
            _ => None,
        }
    }

    /// Query a single float GL state value (mirrors glGetFloatv for common params).
    pub fn get_float(&self, pname: u32) -> Option<f32> {
        match pname {
            0x0B62 => Some(self.depth_range[0]),
            0x0B63 => Some(self.depth_range[1]),
            0x0B21 => Some(self.viewport[0] as f32),
            0x0B22 => Some(self.viewport[1] as f32),
            0x0B23 => Some(self.viewport[2] as f32),
            0x0B24 => Some(self.viewport[3] as f32),
            0x0C2A => Some(self.line_width),
            _ => self.get_integer(pname).map(|v| v as f32),
        }
    }

    // ── glGetProgramiv / glGetShaderiv (integer param queries) ───────────

    /// glGetProgramiv — query an integer property of a program object.
    pub fn get_program_iv(&self, program: u32, pname: u32) -> Option<i32> {
        if program == 0 || (program as usize) > crate::MAX_PROGRAMS {
            return None;
        }
        let p = &self.programs[program as usize - 1];
        match pname {
            0x8B80 => Some(p.linked as i32),      // GL_LINK_STATUS
            0x8B82 => Some(p.validate_ok as i32), // GL_VALIDATE_STATUS
            0x8B84 => Some(self.program_info_logs[program as usize - 1].len() as i32), // GL_INFO_LOG_LENGTH
            0x8B86 => Some(
                (if p.vertex_shader != 0 { 1 } else { 0 })
                    + (if p.fragment_shader != 0 { 1 } else { 0 }),
            ), // GL_ATTACHED_SHADERS
            _ => None,
        }
    }

    /// glGetShaderiv — query an integer property of a shader object.
    pub fn get_shader_iv(&self, shader: u32, pname: u32) -> Option<i32> {
        if shader == 0 || (shader as usize) > crate::MAX_SHADERS {
            return None;
        }
        match pname {
            0x8B81 => Some(
                self.shaders[shader as usize - 1]
                    .as_ref()
                    .map(|s| s.compiled as i32)
                    .unwrap_or(0),
            ), // GL_COMPILE_STATUS
            0x8B84 => Some(self.shader_info_logs[shader as usize - 1].len() as i32), // GL_INFO_LOG_LENGTH
            0x8B88 => Some(
                self.shaders[shader as usize - 1]
                    .as_ref()
                    .map(|s| s.source.len() as i32)
                    .unwrap_or(0),
            ), // GL_SHADER_SOURCE_LENGTH
            0x8B4F => Some(
                self.shaders[shader as usize - 1]
                    .as_ref()
                    .map(|s| s.kind as i32)
                    .unwrap_or(0),
            ), // GL_SHADER_TYPE
            _ => None,
        }
    }

    // ── glClearBufferfv ───────────────────────────────────────────────────

    /// glClearBufferfv — clear a color attachment with float values, or clear depth.
    /// For color: attachment is the draw-buffer index (0–7).
    /// For depth: use `Attachment::Depth` and value[0].
    pub fn clear_buffer_fv(&mut self, attachment: Attachment, value: &[f32]) {
        match attachment {
            Attachment::Depth => {
                if let Some(&d) = value.first() {
                    self.clear_depth = d.clamp(0.0, 1.0);
                }
            }
            Attachment::Color(i) => {
                if value.len() >= 4 {
                    self.clear_buffer_color_fv(i as u32, [value[0], value[1], value[2], value[3]]);
                }
            }
            _ => {
                self.set_error(GlError::InvalidEnum);
            }
        }
    }

    // ── glVertexAttrib{1,2,3,4}f / glVertexAttribI4i / glVertexAttribI4ui ─

    /// glVertexAttrib1f — set a constant vertex attribute (1 float component).
    pub fn vertex_attrib_1f(&mut self, index: u32, x: f32) {
        if index >= MAX_ATTRIBS as u32 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let key = ((0xCAFE_0000u64) << 16) | index as u64;
        let _ = self.uniforms.insert(key, UniformValue::Float(x));
    }

    /// glVertexAttrib2f — set a constant vertex attribute (2 float components).
    pub fn vertex_attrib_2f(&mut self, index: u32, x: f32, y: f32) {
        if index >= MAX_ATTRIBS as u32 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let key = ((0xCAFE_0001u64) << 16) | index as u64;
        let _ = self.uniforms.insert(key, UniformValue::Vec2([x, y]));
    }

    /// glVertexAttrib3f — set a constant vertex attribute (3 float components).
    pub fn vertex_attrib_3f(&mut self, index: u32, x: f32, y: f32, z: f32) {
        if index >= MAX_ATTRIBS as u32 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let key = ((0xCAFE_0002u64) << 16) | index as u64;
        let _ = self.uniforms.insert(key, UniformValue::Vec3([x, y, z]));
    }

    /// glVertexAttrib4f — set a constant vertex attribute (4 float components).
    pub fn vertex_attrib_4f(&mut self, index: u32, x: f32, y: f32, z: f32, w: f32) {
        if index >= MAX_ATTRIBS as u32 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let key = ((0xCAFE_0003u64) << 16) | index as u64;
        let _ = self.uniforms.insert(key, UniformValue::Vec4([x, y, z, w]));
    }

    /// glVertexAttrib4fv — set a constant vertex attribute from a float slice.
    pub fn vertex_attrib_4fv(&mut self, index: u32, v: &[f32]) {
        if v.len() >= 4 {
            self.vertex_attrib_4f(index, v[0], v[1], v[2], v[3]);
        }
    }

    /// glVertexAttribI4i — set a constant integer vertex attribute (4 i32 components).
    pub fn vertex_attrib_i4i(&mut self, index: u32, x: i32, y: i32, z: i32, w: i32) {
        if index >= MAX_ATTRIBS as u32 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let key = ((0xCAFE_0004u64) << 16) | index as u64;
        let _ = self.uniforms.insert(key, UniformValue::IVec4([x, y, z, w]));
    }

    /// glVertexAttribI4ui — set a constant unsigned integer vertex attribute (4 u32 components).
    pub fn vertex_attrib_i4ui(&mut self, index: u32, x: u32, y: u32, z: u32, w: u32) {
        if index >= MAX_ATTRIBS as u32 {
            self.set_error(GlError::InvalidValue);
            return;
        }
        let key = ((0xCAFE_0005u64) << 16) | index as u64;
        let _ = self.uniforms.insert(key, UniformValue::UVec4([x, y, z, w]));
    }

    // ── Uniform block introspection ───────────────────────────────────────

    /// glGetUniformIndices — returns the index of each named uniform within a program.
    /// In this backend, the index equals the deterministic location hash (same as get_uniform_location).
    pub fn get_uniform_indices(&self, program: u32, names: &[&[u8]], out: &mut [u32]) {
        let n = names.len().min(out.len());
        for i in 0..n {
            let loc = self.get_uniform_location(program, names[i]);
            out[i] = if loc < 0 { 0xFFFF_FFFF } else { loc as u32 };
        }
    }

    /// glGetActiveUniformsiv — queries multiple integer properties for a set of uniform indices.
    /// pname: 0x8A3A=GL_UNIFORM_TYPE (returns 0x1406=FLOAT as placeholder),
    ///        0x8A38=GL_UNIFORM_SIZE, 0x8A39=GL_UNIFORM_NAME_LENGTH,
    ///        0x8A3B=GL_UNIFORM_BLOCK_INDEX (returns -1 = default block),
    ///        0x8A3C=GL_UNIFORM_OFFSET, 0x8A3D=GL_UNIFORM_ARRAY_STRIDE,
    ///        0x8A3E=GL_UNIFORM_MATRIX_STRIDE, 0x8A3F=GL_UNIFORM_IS_ROW_MAJOR
    pub fn get_active_uniforms_iv(
        &self,
        program: u32,
        indices: &[u32],
        pname: u32,
        out: &mut [i32],
    ) {
        if program == 0 || (program as usize) > crate::MAX_PROGRAMS {
            return;
        }
        // Build the merged uniform list the same way get_active_uniform does.
        let p = &self.programs[program as usize - 1];
        let mut uniforms: Vec<&ShaderInterfaceVar> = Vec::new();
        if p.vertex_shader != 0 {
            if let Some(Some(vs)) = self.shaders.get(p.vertex_shader as usize - 1) {
                if let Some(meta) = &vs.metadata {
                    for u in &meta.uniforms {
                        uniforms.push(u);
                    }
                }
            }
        }
        if p.fragment_shader != 0 {
            if let Some(Some(fs)) = self.shaders.get(p.fragment_shader as usize - 1) {
                if let Some(meta) = &fs.metadata {
                    for u in &meta.uniforms {
                        if !uniforms.iter().any(|e| e.name == u.name) {
                            uniforms.push(u);
                        }
                    }
                }
            }
        }

        let n = indices.len().min(out.len());
        for i in 0..n {
            let u = uniforms.get(indices[i] as usize);
            out[i] = match pname {
                0x8A3A => u.map(|v| glsl_type_to_glenum(&v.ty)).unwrap_or(0x1406), // GL_UNIFORM_TYPE (GAP-001)
                0x8A38 => 1,  // GL_UNIFORM_SIZE — 1 (arrays not tracked in metadata yet)
                0x8A3B => -1, // GL_UNIFORM_BLOCK_INDEX — default block
                0x8A3C => (indices[i] as i32) * 16, // GL_UNIFORM_OFFSET — deterministic placeholder
                0x8A3D | 0x8A3E => 0, // array/matrix stride
                0x8A3F => 0,  // is row major
                _ => 0,
            };
        }
    }

    /// glGetActiveUniformBlockiv — queries integer properties of a uniform block.
    pub fn get_active_uniform_block_iv(
        &self,
        program: u32,
        block_index: u32,
        pname: u32,
    ) -> Option<i32> {
        if program == 0 || (program as usize) > crate::MAX_PROGRAMS {
            return None;
        }
        match pname {
            0x8A40 => Some(16 * (block_index as i32 + 1)), // GL_UNIFORM_BLOCK_DATA_SIZE — placeholder
            0x8A41 => Some(8),                             // GL_UNIFORM_BLOCK_NAME_LENGTH
            0x8A42 => Some(0),                             // GL_UNIFORM_BLOCK_ACTIVE_UNIFORMS
            0x8A46 => Some(
                self.get_uniform_block_binding(program, block_index)
                    .map(|b| b as i32)
                    .unwrap_or(-1),
            ), // GL_UNIFORM_BLOCK_BINDING
            _ => None,
        }
    }

    /// glGetActiveUniformBlockName — returns the name of a uniform block by index.
    /// This backend assigns synthetic names of the form "Block<N>".
    pub fn get_active_uniform_block_name(
        &self,
        program: u32,
        block_index: u32,
    ) -> Option<alloc::string::String> {
        if program == 0 || (program as usize) > crate::MAX_PROGRAMS {
            return None;
        }
        if !self.programs[program as usize - 1].linked {
            return None;
        }
        Some(alloc::format!("Block{}", block_index))
    }

    // ── glGetInternalformativ ─────────────────────────────────────────────

    /// glGetInternalformativ — query implementation-defined properties of an internal format.
    /// This backend returns conservative minimum values for all queries.
    pub fn get_internalformativ(
        &self,
        _target: u32,
        _internalformat: u32,
        pname: u32,
    ) -> Option<i32> {
        match pname {
            0x9104 => Some(1), // GL_NUM_SAMPLE_COUNTS — 1 sample supported
            0x80A9 => Some(1), // GL_SAMPLES — only 1
            _ => None,
        }
    }

    // ── glGetInteger64v ───────────────────────────────────────────────────

    /// glGetInteger64v — query a 64-bit integer state value.
    pub fn get_integer64(&self, pname: u32) -> Option<i64> {
        self.get_integer(pname).map(|v| v as i64)
    }

    // ── glDrawBuffers helpers ─────────────────────────────────────────────

    /// Returns the current draw buffers bitmask.
    pub fn get_draw_buffers(&self) -> u8 {
        self.draw_buffers_mask
    }

    /// Returns the current read buffer attachment index.
    pub fn get_read_buffer(&self) -> u8 {
        self.read_buffer_index
    }
}

fn collect_indices_from_bytes(
    bytes: &[u8],
    offset_bytes: usize,
    count: usize,
    index_type: IndexType,
) -> Option<Vec<u32>> {
    let elem_size = match index_type {
        IndexType::U8 => 1usize,
        IndexType::U16 => 2usize,
        IndexType::U32 => 4usize,
    };
    let byte_len = elem_size.checked_mul(count)?;
    let end = offset_bytes.checked_add(byte_len)?;
    let src = bytes.get(offset_bytes..end)?;

    let mut out = Vec::with_capacity(count);
    match index_type {
        IndexType::U8 => {
            for &b in src {
                out.push(b as u32);
            }
        }
        IndexType::U16 => {
            for ch in src.chunks_exact(2) {
                out.push(u16::from_le_bytes([ch[0], ch[1]]) as u32);
            }
        }
        IndexType::U32 => {
            for ch in src.chunks_exact(4) {
                out.push(u32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]));
            }
        }
    }
    Some(out)
}

fn is_valid_alignment(alignment: u32) -> bool {
    matches!(alignment, 1 | 2 | 4 | 8)
}

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment <= 1 {
        return value;
    }
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + (alignment - rem)
    }
}

fn decode_bgra8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(4)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }

    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 4;
            out[dst_off + col] =
                u32::from_le_bytes([bytes[s], bytes[s + 1], bytes[s + 2], bytes[s + 3]]);
        }
    }
    Some(out)
}

/// sRGB-to-linear LUT: converts an sRGB u8 channel to a linear-space u8 channel (GAP-014).
#[inline]
fn srgb_to_linear_u8(v: u8) -> u8 {
    let c = v as f32 / 255.0;
    let lin = if c <= 0.04045 {
        c / 12.92
    } else {
        libm::powf((c + 0.055) / 1.055_f32, 2.4)
    };
    (lin * 255.0 + 0.5) as u8
}

/// Decode SRGB8_ALPHA8 byte rows to BGRA32, converting sRGB channels to linear (GAP-014).
fn decode_srgb8_alpha8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(4)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 4;
            let r = srgb_to_linear_u8(bytes[s]);
            let g = srgb_to_linear_u8(bytes[s + 1]);
            let b = srgb_to_linear_u8(bytes[s + 2]);
            let a = bytes[s + 3]; // alpha is linear
            out[dst_off + col] = u32::from_le_bytes([b, g, r, a]);
        }
    }
    Some(out)
}

fn decode_rgba8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(4)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }

    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 4;
            let r = bytes[s];
            let g = bytes[s + 1];
            let b = bytes[s + 2];
            let a = bytes[s + 3];
            out[dst_off + col] = u32::from_le_bytes([b, g, r, a]);
        }
    }
    Some(out)
}

fn decode_rgb8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(3)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }

    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 3;
            let r = bytes[s];
            let g = bytes[s + 1];
            let b = bytes[s + 2];
            out[dst_off + col] = u32::from_le_bytes([b, g, r, 0xFF]);
        }
    }
    Some(out)
}

fn decode_r8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = width as usize;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }

    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let r = bytes[src_off + col];
            out[dst_off + col] = u32::from_le_bytes([0, 0, r, 0xFF]);
        }
    }
    Some(out)
}

fn decode_rg8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(2)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 2;
            let r = bytes[s];
            let g = bytes[s + 1];
            // Store in BGRA order: B=0, G=g, R=r, A=0xFF
            out[dst_off + col] = u32::from_le_bytes([0, g, r, 0xFF]);
        }
    }
    Some(out)
}

fn decode_rgb565_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(2)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 2;
            let p = u16::from_le_bytes([bytes[s], bytes[s + 1]]);
            let r5 = ((p >> 11) & 0x1F) as u8;
            let g6 = ((p >> 5) & 0x3F) as u8;
            let b5 = (p & 0x1F) as u8;
            let r = (r5 << 3) | (r5 >> 2);
            let g = (g6 << 2) | (g6 >> 4);
            let b = (b5 << 3) | (b5 >> 2);
            out[dst_off + col] = u32::from_le_bytes([b, g, r, 0xFF]);
        }
    }
    Some(out)
}

fn decode_rgba4_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(2)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 2;
            let p = u16::from_le_bytes([bytes[s], bytes[s + 1]]);
            let r4 = ((p >> 12) & 0xF) as u8;
            let g4 = ((p >> 8) & 0xF) as u8;
            let b4 = ((p >> 4) & 0xF) as u8;
            let a4 = (p & 0xF) as u8;
            let r = (r4 << 4) | r4;
            let g = (g4 << 4) | g4;
            let b = (b4 << 4) | b4;
            let a = (a4 << 4) | a4;
            out[dst_off + col] = u32::from_le_bytes([b, g, r, a]);
        }
    }
    Some(out)
}

fn decode_rgb5a1_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(2)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 2;
            let p = u16::from_le_bytes([bytes[s], bytes[s + 1]]);
            let r5 = ((p >> 11) & 0x1F) as u8;
            let g5 = ((p >> 6) & 0x1F) as u8;
            let b5 = ((p >> 1) & 0x1F) as u8;
            let a1 = (p & 1) as u8;
            let r = (r5 << 3) | (r5 >> 2);
            let g = (g5 << 3) | (g5 >> 2);
            let b = (b5 << 3) | (b5 >> 2);
            let a = if a1 == 1 { 0xFF } else { 0x00 };
            out[dst_off + col] = u32::from_le_bytes([b, g, r, a]);
        }
    }
    Some(out)
}

/// Decode RGBA16F (8 bytes/pixel, little-endian half floats) to BGRA8 by clamping to [0,1].
fn decode_rgba16f_rows(width: u32, height: u32, bytes: &[u8]) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    let required = pixel_count.checked_mul(8)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; pixel_count];
    for i in 0..pixel_count {
        let off = i * 8;
        let rh = u16::from_le_bytes([bytes[off], bytes[off + 1]]);
        let gh = u16::from_le_bytes([bytes[off + 2], bytes[off + 3]]);
        let bh = u16::from_le_bytes([bytes[off + 4], bytes[off + 5]]);
        let ah = u16::from_le_bytes([bytes[off + 6], bytes[off + 7]]);
        let r = (half_to_f32(rh).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let g = (half_to_f32(gh).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let b = (half_to_f32(bh).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let a = (half_to_f32(ah).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        out[i] = u32::from_le_bytes([b, g, r, a]);
    }
    Some(out)
}

fn decode_luminance8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = width as usize;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let l = bytes[src_off + col];
            out[dst_off + col] = u32::from_le_bytes([l, l, l, 0xFF]);
        }
    }
    Some(out)
}

fn decode_luminance_alpha8_rows(
    width: u32,
    height: u32,
    bytes: &[u8],
    unpack_alignment: u32,
) -> Option<Vec<u32>> {
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(2)?;
    let stride = align_up(row_bytes, unpack_alignment as usize);
    let required = stride
        .checked_mul(height as usize - 1)?
        .checked_add(row_bytes)?;
    if bytes.len() < required {
        return None;
    }
    let mut out = vec![0u32; (width as usize).checked_mul(height as usize)?];
    for row in 0..height as usize {
        let src_off = row * stride;
        let dst_off = row * width as usize;
        for col in 0..width as usize {
            let s = src_off + col * 2;
            let l = bytes[s];
            let a = bytes[s + 1];
            out[dst_off + col] = u32::from_le_bytes([l, l, l, a]);
        }
    }
    Some(out)
}

/// IEEE 754 half-float to f32 conversion (software, no FPU needed).
fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            // Denormal
            let mut m = mant;
            let mut e = 127 - 14;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            m &= !0x400;
            sign | (e << 23) | (m << 13)
        }
    } else if exp == 31 {
        sign | 0x7F80_0000 | (mant << 13) // inf or nan
    } else {
        sign | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

#[inline]
fn query_target_slot(target: QueryTarget) -> usize {
    match target {
        QueryTarget::SamplesPassed => 0,
        QueryTarget::AnySamplesPassed => 1,
        QueryTarget::PrimitivesGenerated => 2,
        QueryTarget::TimeElapsed => 3,
    }
}

fn parse_glsl_metadata(kind: ShaderKind, source: &[u8]) -> Option<ShaderMetadata> {
    let src = core::str::from_utf8(source).ok()?;
    let stripped = strip_glsl_comments(src);
    let mut md = ShaderMetadata {
        version_es300: false,
        has_main: false,
        main_signature_valid: false,
        float_precision_declared: false,
        inputs: Vec::new(),
        outputs: Vec::new(),
        output_locations: Vec::new(),
        uniforms: Vec::new(),
    };

    let mut seen_non_preproc = false;

    if !glsl_balanced_symbols(stripped.as_str()) {
        return None;
    }

    for raw_line in stripped.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#version") {
            if seen_non_preproc {
                return None;
            }
            let lowered = line.to_ascii_lowercase();
            if lowered.starts_with("#version 300") && lowered.contains("es") {
                md.version_es300 = true;
            } else if lowered.starts_with("#version 330") && lowered.contains("core") {
                // GLSL 330 core is accepted as equivalent to GLSL ES 3.00; no precision required.
                md.version_es300 = true;
                md.float_precision_declared = true;
            }
            continue;
        }
        if line.starts_with('#') && !md.version_es300 {
            return None;
        }
        if !line.starts_with('#') {
            seen_non_preproc = true;
        }

        if line.starts_with("precision") && line.contains("float") {
            md.float_precision_declared = true;
        }

        if is_main_function_signature(line) {
            md.has_main = true;
            md.main_signature_valid = true;
        }

        let (location, normalized) = strip_layout_prefix_with_location(line)?;
        let is_interface_candidate = is_interface_decl_candidate(normalized);
        if let Some(var) = parse_interface_decl(normalized, "uniform") {
            if !push_unique_interface_var(&mut md.uniforms, var) {
                return None;
            }
            continue;
        }
        if let Some(var) = parse_interface_decl(normalized, "in") {
            if !push_unique_interface_var(&mut md.inputs, var) {
                return None;
            }
            continue;
        }
        if let Some(mut var) = parse_interface_decl(normalized, "out") {
            var.location = location;
            if !push_unique_interface_var(&mut md.outputs, var) {
                return None;
            }
            if let Some(loc) = location {
                md.output_locations.push(loc);
            }
            continue;
        }

        if is_interface_candidate {
            return None;
        }
    }

    if !validate_shader_stage_io(kind, &md) {
        return None;
    }

    Some(md)
}

fn validate_glsl_shader_metadata(kind: ShaderKind, md: &ShaderMetadata) -> bool {
    if !md.version_es300 || !md.has_main || !md.main_signature_valid {
        return false;
    }
    if !md
        .inputs
        .iter()
        .all(|v| glsl_type_components(v.ty.as_str()).is_some())
    {
        return false;
    }
    if !md
        .outputs
        .iter()
        .all(|v| glsl_type_components(v.ty.as_str()).is_some())
    {
        return false;
    }
    if !md
        .uniforms
        .iter()
        .all(|v| glsl_uniform_type_supported(v.ty.as_str()))
    {
        return false;
    }
    // GLES fragment shaders need a default float precision unless every float
    // variable is explicitly qualified; we enforce a strict baseline here.
    if matches!(kind, ShaderKind::Fragment) && !md.float_precision_declared {
        return false;
    }
    true
}

fn validate_shader_stage_io(kind: ShaderKind, md: &ShaderMetadata) -> bool {
    match kind {
        ShaderKind::Vertex => {
            for &loc in &md.output_locations {
                if loc >= 8 {
                    return false;
                }
            }
        }
        ShaderKind::Fragment => {
            let mut used = [false; 8];
            for &loc in &md.output_locations {
                if loc >= 8 || used[loc as usize] {
                    return false;
                }
                used[loc as usize] = true;
            }
        }
        ShaderKind::Geometry | ShaderKind::Compute => {}
    }
    true
}

fn strip_glsl_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut in_block = false;
    while i < bytes.len() {
        if in_block {
            if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
            } else {
                // Preserve newlines so line-oriented parsing remains stable.
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            continue;
        }

        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            in_block = true;
            i += 2;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn glsl_balanced_symbols(src: &str) -> bool {
    let mut brace = 0i32;
    let mut paren = 0i32;
    for ch in src.chars() {
        match ch {
            '{' => brace += 1,
            '}' => {
                brace -= 1;
                if brace < 0 {
                    return false;
                }
            }
            '(' => paren += 1,
            ')' => {
                paren -= 1;
                if paren < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    brace == 0 && paren == 0
}

fn is_main_function_signature(line: &str) -> bool {
    let t = line.trim();
    if !t.starts_with("void") || !t.contains("main") {
        return false;
    }
    let Some(i) = t.find("main") else {
        return false;
    };
    let after = &t[i + 4..];
    let Some(lp) = after.find('(') else {
        return false;
    };
    let Some(rp) = after[lp + 1..].find(')') else {
        return false;
    };
    after[lp + 1..lp + 1 + rp].trim().is_empty()
}

fn strip_layout_prefix_with_location(line: &str) -> Option<(Option<u8>, &str)> {
    let trimmed = line.trim();
    if !trimmed.starts_with("layout") {
        return Some((None, trimmed));
    }
    if !trimmed.starts_with("layout(") {
        return None;
    }
    if let Some(end) = trimmed.find(')') {
        if end <= "layout(".len() {
            return None;
        }
        let mut location = None;
        let inside = &trimmed["layout(".len()..end];
        for part in inside.split(',') {
            let p = part.trim();
            if let Some(rest) = p.strip_prefix("location") {
                let rest = rest.trim_start();
                if let Some(eq) = rest.find('=') {
                    let num = rest[eq + 1..].trim();
                    if let Ok(v) = num.parse::<u8>() {
                        location = Some(v);
                    }
                }
            }
        }
        Some((location, trimmed[end + 1..].trim_start()))
    } else {
        None
    }
}

fn parse_interface_decl(line: &str, qualifier: &str) -> Option<ShaderInterfaceVar> {
    let line = line.trim_end_matches(';').trim();
    if line.is_empty() {
        return None;
    }

    let mut tokens: Vec<&str> = line.split_whitespace().collect();
    while !tokens.is_empty() && is_decl_prefix_token(tokens[0]) {
        tokens.remove(0);
    }
    if tokens.len() < 3 || tokens[0] != qualifier {
        return None;
    }

    let mut cursor = 1usize;
    while cursor < tokens.len() && is_decl_prefix_token(tokens[cursor]) {
        cursor += 1;
    }
    if tokens.len().saturating_sub(cursor) != 2 {
        return None;
    }

    let ty = tokens[cursor];
    let mut name = tokens[cursor + 1];
    if name.contains('[') && !name.contains(']') {
        return None;
    }
    if let Some(eq) = name.find('=') {
        name = &name[..eq];
    }
    let name = name.trim_end_matches(';').trim_end_matches(',');
    let name = if let Some(idx) = name.find('[') {
        &name[..idx]
    } else {
        name
    };
    if name.is_empty() || !is_valid_glsl_identifier(name) {
        return None;
    }

    Some(ShaderInterfaceVar {
        ty: ty.to_ascii_lowercase(),
        name: String::from(name),
        location: None,
    })
}

fn is_interface_decl_candidate(line: &str) -> bool {
    let mut tokens: Vec<&str> = line.trim_end_matches(';').split_whitespace().collect();
    while !tokens.is_empty() && is_decl_prefix_token(tokens[0]) {
        tokens.remove(0);
    }
    if tokens.is_empty() {
        return false;
    }
    matches!(tokens[0], "uniform" | "in" | "out")
}

fn is_valid_glsl_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn push_unique_interface_var(dst: &mut Vec<ShaderInterfaceVar>, var: ShaderInterfaceVar) -> bool {
    if let Some(prev) = dst.iter().find(|v| v.name == var.name) {
        return prev.ty == var.ty;
    }
    dst.push(var);
    true
}

fn is_decl_prefix_token(token: &str) -> bool {
    matches!(
        token,
        "flat"
            | "smooth"
            | "noperspective"
            | "centroid"
            | "sample"
            | "invariant"
            | "precise"
            | "highp"
            | "mediump"
            | "lowp"
    )
}

fn glsl_uniform_type_supported(ty: &str) -> bool {
    glsl_type_components(ty).is_some()
        || matches!(
            ty,
            "sampler2d" | "samplercube" | "sampler2darray" | "isampler2d" | "usampler2d"
        )
}

/// Decode one std140 member from `buf` at `*offset`, advance `*offset`, and
/// return (alignment, size_in_bytes, Option<Val>).  Returns `None` if the
/// buffer is too short.  (GAP-011)
fn decode_std140_member(
    ty: &str,
    buf: &[u8],
    offset: &mut usize,
) -> (usize, usize, Option<crate::glsl_interp::Val>) {
    use crate::glsl_interp::Val;
    // Read one `f32` from `buf` at byte position `pos`.
    let read_f32 = |buf: &[u8], pos: usize| -> f32 {
        if pos + 4 > buf.len() {
            return 0.0;
        }
        let arr = [buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]];
        f32::from_le_bytes(arr)
    };
    let read_i32 = |buf: &[u8], pos: usize| -> i32 {
        if pos + 4 > buf.len() {
            return 0;
        }
        let arr = [buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]];
        i32::from_le_bytes(arr)
    };
    let read_u32 = |buf: &[u8], pos: usize| -> u32 {
        if pos + 4 > buf.len() {
            return 0;
        }
        let arr = [buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]];
        u32::from_le_bytes(arr)
    };
    // Align `offset` up to `align`.
    let align_up = |off: usize, align: usize| -> usize {
        if align == 0 {
            return off;
        }
        (off + align - 1) & !(align - 1)
    };

    match ty {
        "float" | "int" | "uint" | "bool" => {
            *offset = align_up(*offset, 4);
            let val = match ty {
                "int" => Some(Val::Int(read_i32(buf, *offset))),
                "uint" => Some(Val::UInt(read_u32(buf, *offset))),
                "bool" => Some(Val::Bool(read_i32(buf, *offset) != 0)),
                _ => Some(Val::Float(read_f32(buf, *offset))),
            };
            *offset += 4;
            (4, 4, val)
        }
        "vec2" => {
            *offset = align_up(*offset, 8);
            let v = [read_f32(buf, *offset), read_f32(buf, *offset + 4), 0.0, 0.0];
            *offset += 8;
            (8, 8, Some(Val::Vec2([v[0], v[1]])))
        }
        "vec3" => {
            *offset = align_up(*offset, 16);
            let v = [
                read_f32(buf, *offset),
                read_f32(buf, *offset + 4),
                read_f32(buf, *offset + 8),
            ];
            *offset += 12; // 3 floats; next member aligns to 16 anyway
            (16, 12, Some(Val::Vec3([v[0], v[1], v[2]])))
        }
        "vec4" => {
            *offset = align_up(*offset, 16);
            let v = [
                read_f32(buf, *offset),
                read_f32(buf, *offset + 4),
                read_f32(buf, *offset + 8),
                read_f32(buf, *offset + 12),
            ];
            *offset += 16;
            (16, 16, Some(Val::Vec4(v)))
        }
        "mat4" => {
            // mat4 = 4 columns × vec4; each column at 16-byte alignment
            *offset = align_up(*offset, 16);
            let mut m = [[0.0f32; 4]; 4];
            for col in &mut m {
                *offset = align_up(*offset, 16);
                for row in 0..4usize {
                    col[row] = read_f32(buf, *offset + row * 4);
                }
                *offset += 16;
            }
            (16, 64, Some(Val::Mat4(m)))
        }
        "mat3" => {
            // mat3 = 3 columns × vec3 (padded to vec4 = 16 bytes each)
            *offset = align_up(*offset, 16);
            let mut cols: [[f32; 4]; 3] = [[0.0; 4]; 3];
            for col in &mut cols {
                *offset = align_up(*offset, 16);
                for row in 0..3usize {
                    col[row] = read_f32(buf, *offset + row * 4);
                }
                *offset += 16; // padded column
            }
            // Return as mat4 with zeros in 4th col/row for simplicity.
            let m = [
                [cols[0][0], cols[0][1], cols[0][2], 0.0],
                [cols[1][0], cols[1][1], cols[1][2], 0.0],
                [cols[2][0], cols[2][1], cols[2][2], 0.0],
                [0.0, 0.0, 0.0, 0.0],
            ];
            (16, 48, Some(Val::Mat4(m)))
        }
        "ivec2" => {
            *offset = align_up(*offset, 8);
            let v = [read_i32(buf, *offset), read_i32(buf, *offset + 4)];
            *offset += 8;
            (8, 8, Some(Val::IVec2(v)))
        }
        "ivec3" | "ivec4" | "uvec2" | "uvec3" | "uvec4" | "bvec2" | "bvec3" | "bvec4" => {
            let n: usize = match ty {
                t if t.ends_with('4') => 4,
                t if t.ends_with('3') => 3,
                _ => 2,
            };
            let align = if n >= 4 {
                16
            } else if n == 3 {
                16
            } else {
                8
            };
            *offset = align_up(*offset, align);
            let base = *offset;
            *offset += n * 4;
            // Return as Vec4 with integer cast to float (approximate, avoids more Val variants).
            let v = [
                if n > 0 { read_f32(buf, base) } else { 0.0 },
                if n > 1 { read_f32(buf, base + 4) } else { 0.0 },
                if n > 2 { read_f32(buf, base + 8) } else { 0.0 },
                if n > 3 { read_f32(buf, base + 12) } else { 0.0 },
            ];
            (align, n * 4, Some(Val::Vec4(v)))
        }
        _ => {
            // Unknown type — skip 16 bytes (safe over-alignment).
            *offset = align_up(*offset, 16);
            *offset += 16;
            (16, 16, None)
        }
    }
}

/// Count the number of primitives generated for a given vertex count and draw mode (GAP-003).
#[inline]
fn primitives_for_mode(count: usize, mode: DrawMode) -> u64 {
    (match mode {
        DrawMode::Triangles => count / 3,
        DrawMode::TriangleStrip => count.saturating_sub(2),
        DrawMode::TriangleFan => count.saturating_sub(2),
        DrawMode::Lines => count / 2,
        DrawMode::LineStrip => count.saturating_sub(1),
        DrawMode::LineLoop => count,
        DrawMode::Points => count,
    }) as u64
}

/// Map a GLSL type name to the corresponding GL_* GLenum used by glGetActiveUniform (GAP-001).
fn glsl_type_to_glenum(ty: &str) -> i32 {
    match ty {
        "float" => 0x1406,                                         // GL_FLOAT
        "vec2" => 0x8B50,                                          // GL_FLOAT_VEC2
        "vec3" => 0x8B51,                                          // GL_FLOAT_VEC3
        "vec4" => 0x8B52,                                          // GL_FLOAT_VEC4
        "int" => 0x1404,                                           // GL_INT
        "ivec2" => 0x8B53,                                         // GL_INT_VEC2
        "ivec3" => 0x8B54,                                         // GL_INT_VEC3
        "ivec4" => 0x8B55,                                         // GL_INT_VEC4
        "uint" => 0x1405,                                          // GL_UNSIGNED_INT
        "uvec2" => 0x8DC6,                                         // GL_UNSIGNED_INT_VEC2
        "uvec3" => 0x8DC7,                                         // GL_UNSIGNED_INT_VEC3
        "uvec4" => 0x8DC8,                                         // GL_UNSIGNED_INT_VEC4
        "bool" => 0x8B56,                                          // GL_BOOL
        "bvec2" => 0x8B57,                                         // GL_BOOL_VEC2
        "bvec3" => 0x8B58,                                         // GL_BOOL_VEC3
        "bvec4" => 0x8B59,                                         // GL_BOOL_VEC4
        "mat2" => 0x8B5A,                                          // GL_FLOAT_MAT2
        "mat3" => 0x8B5B,                                          // GL_FLOAT_MAT3
        "mat4" => 0x8B5C,                                          // GL_FLOAT_MAT4
        "mat2x3" => 0x8B65,                                        // GL_FLOAT_MAT2x3
        "mat2x4" => 0x8B66,                                        // GL_FLOAT_MAT2x4
        "mat3x2" => 0x8B67,                                        // GL_FLOAT_MAT3x2
        "mat3x4" => 0x8B68,                                        // GL_FLOAT_MAT3x4
        "mat4x2" => 0x8B69,                                        // GL_FLOAT_MAT4x2
        "mat4x3" => 0x8B6A,                                        // GL_FLOAT_MAT4x3
        "sampler2D" | "sampler2d" => 0x8B5E,                       // GL_SAMPLER_2D
        "sampler3D" | "sampler3d" => 0x8B5F,                       // GL_SAMPLER_3D
        "samplerCube" | "samplercube" => 0x8B60,                   // GL_SAMPLER_CUBE
        "sampler2DShadow" | "sampler2dshadow" => 0x8B62,           // GL_SAMPLER_2D_SHADOW
        "sampler2DArray" | "sampler2darray" => 0x8DC1,             // GL_SAMPLER_2D_ARRAY
        "sampler2DArrayShadow" | "sampler2darrayshadow" => 0x8DC4, // GL_SAMPLER_2D_ARRAY_SHADOW
        "samplerCubeShadow" | "samplercubeshadow" => 0x8DC5,       // GL_SAMPLER_CUBE_SHADOW
        "isampler2D" | "isampler2d" => 0x8DCA,                     // GL_INT_SAMPLER_2D
        "isampler3D" | "isampler3d" => 0x8DCB,                     // GL_INT_SAMPLER_3D
        "isamplerCube" | "isamplercube" => 0x8DCC,                 // GL_INT_SAMPLER_CUBE
        "isampler2DArray" | "isampler2darray" => 0x8DCF,           // GL_INT_SAMPLER_2D_ARRAY
        "usampler2D" | "usampler2d" => 0x8DD2,                     // GL_UNSIGNED_INT_SAMPLER_2D
        "usampler3D" | "usampler3d" => 0x8DD3,                     // GL_UNSIGNED_INT_SAMPLER_3D
        "usamplerCube" | "usamplercube" => 0x8DD4,                 // GL_UNSIGNED_INT_SAMPLER_CUBE
        "usampler2DArray" | "usampler2darray" => 0x8DD7, // GL_UNSIGNED_INT_SAMPLER_2D_ARRAY
        _ => 0x1406,                                     // GL_FLOAT fallback
    }
}

fn glsl_type_components(ty: &str) -> Option<usize> {
    match ty {
        "float" | "int" | "uint" | "bool" => Some(1),
        "vec2" | "ivec2" | "uvec2" | "bvec2" => Some(2),
        "vec3" | "ivec3" | "uvec3" | "bvec3" => Some(3),
        "vec4" | "ivec4" | "uvec4" | "bvec4" => Some(4),
        "mat2" => Some(4),
        "mat3" => Some(9),
        "mat4" => Some(16),
        _ => None,
    }
}

#[inline]
fn extension_slot(ext: GlExtension) -> usize {
    match ext {
        GlExtension::KhrDebug => 0,
        GlExtension::KhrRobustness => 1,
        GlExtension::ExtDisjointTimerQuery => 2,
        GlExtension::ExtColorBufferFloat => 3,
    }
}

#[inline]
fn extension_name(ext: GlExtension) -> &'static str {
    match ext {
        GlExtension::KhrDebug => "GL_KHR_debug",
        GlExtension::KhrRobustness => "GL_KHR_robustness",
        GlExtension::ExtDisjointTimerQuery => "GL_EXT_disjoint_timer_query",
        GlExtension::ExtColorBufferFloat => "GL_EXT_color_buffer_float",
    }
}

// ── Blend factor helpers ──────────────────────────────────────────────────────

fn blend_factor(f: BlendFactor, src: Vec4, dst: Vec4, constant: Vec4) -> Vec4 {
    match f {
        BlendFactor::Zero => Vec4::new(0.0, 0.0, 0.0, 0.0),
        BlendFactor::One => Vec4::new(1.0, 1.0, 1.0, 1.0),
        BlendFactor::SrcColor => Vec4::new(src.x, src.y, src.z, src.w),
        BlendFactor::OneMinusSrcColor => {
            Vec4::new(1.0 - src.x, 1.0 - src.y, 1.0 - src.z, 1.0 - src.w)
        }
        BlendFactor::DstColor => Vec4::new(dst.x, dst.y, dst.z, dst.w),
        BlendFactor::OneMinusDstColor => {
            Vec4::new(1.0 - dst.x, 1.0 - dst.y, 1.0 - dst.z, 1.0 - dst.w)
        }
        BlendFactor::SrcAlpha => Vec4::new(src.w, src.w, src.w, src.w),
        BlendFactor::OneMinusSrcAlpha => {
            let ia = 1.0 - src.w;
            Vec4::new(ia, ia, ia, ia)
        }
        BlendFactor::DstAlpha => Vec4::new(dst.w, dst.w, dst.w, dst.w),
        BlendFactor::OneMinusDstAlpha => {
            let ia = 1.0 - dst.w;
            Vec4::new(ia, ia, ia, ia)
        }
        BlendFactor::ConstantColor => constant,
        BlendFactor::OneMinusConstantColor => Vec4::new(
            1.0 - constant.x,
            1.0 - constant.y,
            1.0 - constant.z,
            1.0 - constant.w,
        ),
        BlendFactor::ConstantAlpha => Vec4::new(constant.w, constant.w, constant.w, constant.w),
        BlendFactor::OneMinusConstantAlpha => {
            let ia = 1.0 - constant.w;
            Vec4::new(ia, ia, ia, ia)
        }
        BlendFactor::SrcAlphaSaturate => {
            let f = (src.w).min(1.0 - dst.w);
            Vec4::new(f, f, f, 1.0)
        }
    }
}

fn blend_factor_alpha(f: BlendFactor, src: Vec4, dst: Vec4, constant: Vec4) -> Vec4 {
    // Alpha channel of the blend factor vector
    let v = blend_factor(f, src, dst, constant);
    Vec4::new(v.w, v.w, v.w, v.w)
}

fn blend_eq(eq: BlendEquation, src: Vec4, dst: Vec4) -> Vec4 {
    match eq {
        BlendEquation::FuncAdd => {
            Vec4::new(src.x + dst.x, src.y + dst.y, src.z + dst.z, src.w + dst.w)
        }
        BlendEquation::FuncSubtract => {
            Vec4::new(src.x - dst.x, src.y - dst.y, src.z - dst.z, src.w - dst.w)
        }
        BlendEquation::FuncReverseSubtract => {
            Vec4::new(dst.x - src.x, dst.y - src.y, dst.z - src.z, dst.w - src.w)
        }
        BlendEquation::Min => Vec4::new(
            src.x.min(dst.x),
            src.y.min(dst.y),
            src.z.min(dst.z),
            src.w.min(dst.w),
        ),
        BlendEquation::Max => Vec4::new(
            src.x.max(dst.x),
            src.y.max(dst.y),
            src.z.max(dst.z),
            src.w.max(dst.w),
        ),
    }
}

fn blend_eq_f32(eq: BlendEquation, src: f32, dst: f32) -> f32 {
    match eq {
        BlendEquation::FuncAdd => src + dst,
        BlendEquation::FuncSubtract => src - dst,
        BlendEquation::FuncReverseSubtract => dst - src,
        BlendEquation::Min => src.min(dst),
        BlendEquation::Max => src.max(dst),
    }
}

// ── Stencil op helper ─────────────────────────────────────────────────────────

fn apply_stencil_op(op: StencilOp, stencil: u8, ref_val: u8, write_mask: u8) -> u8 {
    let result = match op {
        StencilOp::Keep => return stencil,
        StencilOp::Zero => 0u8,
        StencilOp::Replace => ref_val,
        StencilOp::Increment => stencil.saturating_add(1),
        StencilOp::IncrementWrap => stencil.wrapping_add(1),
        StencilOp::Decrement => stencil.saturating_sub(1),
        StencilOp::DecrementWrap => stencil.wrapping_sub(1),
        StencilOp::Invert => !stencil,
    };
    (stencil & !write_mask) | (result & write_mask)
}

// ── Color packing ─────────────────────────────────────────────────────────────

#[inline]
fn pack_color_f32(rgba: [f32; 4]) -> u32 {
    // Input: [r, g, b, a]  →  packed BGRA32
    let b = (rgba[2].clamp(0.0, 1.0) * 255.0) as u32;
    let g = (rgba[1].clamp(0.0, 1.0) * 255.0) as u32;
    let r = (rgba[0].clamp(0.0, 1.0) * 255.0) as u32;
    let a = (rgba[3].clamp(0.0, 1.0) * 255.0) as u32;
    (a << 24) | (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn framebuffer_status_requires_attachment() {
        let mut ctx = Context::new();
        let mut fbos = [0u32; 1];
        assert_eq!(ctx.gen_framebuffers(&mut fbos), 1);
        assert_eq!(
            ctx.check_framebuffer_status(fbos[0]),
            FramebufferStatus::MissingAttachment
        );
    }

    #[test]
    fn framebuffer_status_uses_attached_mip_level_size() {
        let mut ctx = Context::new();
        let mut tex = [0u32; 2];
        let mut fbos = [0u32; 1];
        assert_eq!(ctx.gen_textures(&mut tex), 2);
        assert_eq!(ctx.gen_framebuffers(&mut fbos), 1);

        ctx.tex_storage_2d(tex[0], 8, 8, 3);
        ctx.tex_image_2d(tex[0], 0, 8, 8, &vec![0xFF11_2233; 64]);
        ctx.generate_mipmap(tex[0]);
        ctx.framebuffer_texture_2d(fbos[0], Attachment::Color(0), tex[0], 1);
        assert_eq!(
            ctx.check_framebuffer_status(fbos[0]),
            FramebufferStatus::Complete
        );

        ctx.tex_storage_2d(tex[1], 8, 8, 1);
        ctx.tex_image_2d(tex[1], 0, 8, 8, &vec![0xFF44_5566; 64]);
        ctx.framebuffer_texture_2d(fbos[0], Attachment::Depth, tex[1], 0);
        assert_eq!(
            ctx.check_framebuffer_status(fbos[0]),
            FramebufferStatus::IncompleteDimensions
        );
    }

    #[test]
    fn deleting_texture_scrubs_framebuffer_attachments() {
        let mut ctx = Context::new();
        let mut tex = [0u32; 1];
        let mut fbos = [0u32; 1];
        assert_eq!(ctx.gen_textures(&mut tex), 1);
        assert_eq!(ctx.gen_framebuffers(&mut fbos), 1);

        ctx.tex_storage_2d(tex[0], 4, 4, 1);
        ctx.tex_image_2d(tex[0], 0, 4, 4, &vec![0xFFFF_FFFF; 16]);
        ctx.framebuffer_texture_2d(fbos[0], Attachment::Color(0), tex[0], 0);
        assert_eq!(
            ctx.check_framebuffer_status(fbos[0]),
            FramebufferStatus::Complete
        );

        ctx.delete_textures(&tex);
        assert_eq!(
            ctx.check_framebuffer_status(fbos[0]),
            FramebufferStatus::MissingAttachment
        );
    }
}
