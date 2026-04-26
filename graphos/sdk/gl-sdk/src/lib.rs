// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphos-gl — real software 3D graphics pipeline.
//!
//! This is a modern, programmable, perspective-correct rasterizer written
//! entirely in `#![no_std]` Rust. It implements the same conceptual stages
//! as a hardware GPU executing OpenGL ES 3 / WebGL 2:
//!
//!   vertex shader  → clip       → perspective divide
//!                  → viewport   → triangle setup (edge functions)
//!                  → rasterize  → perspective-correct interpolation
//!                  → fragment shader (programmable)
//!                  → depth test → blend → color write
//!
//! On bare-metal GraphOS without a discrete-GPU driver, the rasterizer
//! runs on the CPU. The API is shaped so that a future virtio-gpu
//! command-buffer backend can replace [`pipeline::Pipeline::draw`]
//! without touching app code.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(clippy::many_single_char_names)]
extern crate alloc;

pub mod compositor;
pub mod demo;
pub mod gl;
pub mod glsl_interp;
pub mod math;
pub mod mesh;
pub mod pipeline;
pub mod shader;
pub mod text;
pub mod texture;
pub mod thread;
pub mod ui;

pub use compositor::{
    DesktopCompositor, OffscreenWindowRenderer, WindowSurface, WindowSurfaceTarget,
};
pub use demo::{DemoWindowScene, DesktopDemoScene, build_demo_scene, render_desktop_demo};
pub use gl::{
    ApiProfile, Attachment, AttribPointer, BlendEquation, BlendFactor, CLEAR_COLOR, CLEAR_DEPTH,
    CLEAR_STENCIL, Context, ContextResetStatus, CullFace, DebugMessage, DebugSeverity, DebugSource,
    DebugType, DepthFunc, DrawMode, FilterMode, FramebufferStatus, FrontFace, GlError, GlExtension,
    GlLimits, ImageAccess, ImageUnitBinding, IndexType, MAP_READ_BIT, MAP_WRITE_BIT,
    MAX_ATOMIC_COUNTER_BUFFER_BINDINGS, MAX_BUFFERS, MAX_DEBUG_GROUP_DEPTH, MAX_DEBUG_MESSAGES,
    MAX_FBOS, MAX_IMAGE_UNITS, MAX_PROGRAM_TFB_VARYINGS, MAX_PROGRAMS, MAX_QUERIES,
    MAX_RENDERBUFFERS, MAX_SAMPLERS, MAX_SHADER_STORAGE_BUFFER_BINDINGS, MAX_SHADERS, MAX_SYNCS,
    MAX_TEXTURES, MAX_TFBS, MAX_UNIFORM_BUFFER_BINDINGS, MAX_VAOS, MemoryBarrierBits,
    ProgramObject, QueryObject, QueryTarget, Renderbuffer, SamplerObject, ShaderKind, StencilFunc,
    StencilOp, SyncObject, SyncWaitResult, TextureObject, TextureSnapshot, TransformFeedbackObject,
    UniformBinding, UniformValue, VertexArray, WrapMode,
};
pub use math::{Mat4, Vec2, Vec3, Vec4};
pub use mesh::{Mesh, Vertex};
pub use pipeline::{Pipeline, Target};
pub use shader::{Shader, Varying};
pub use text::{
    GlyphAtlas, TextAlign, TextMetrics, TextStyle, append_text, append_text_aligned,
    append_text_lines, measure_text,
};
pub use texture::{MipmapChain, Texture, generate_mip_level};
pub use ui::{
    EdgeInsets, NineSlice, Rect, UiBatch, UiRenderer, UiShader, UiTextureView, UiVarying, UiVertex,
};
