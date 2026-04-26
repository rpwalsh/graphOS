// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! graphos-gl — Rust-native OpenGL-equivalent rendering API for GraphOS.
//!
//! This is a complete rendering API modelled on OpenGL 3.3 Core Profile,
//! implemented in pure Rust with no C library dependency.  It runs in ring-3
//! userspace and submits work to the kernel GPU executor via `GpuCmd` batches.
//!
//! ## Object model
//!
//! Every GPU object (program, buffer, texture, framebuffer, vertex array) is
//! created through `GlContext` and represented as a typed handle.  The context
//! owns the object registry and the current binding state.
//!
//! ## Shader model
//!
//! Shaders are written in GLSL (or a GraphOS IR defined below) and compiled
//! to a bytecode blob stored in `GlProgram`.  Phase 1: the kernel executor
//! uses built-in fixed-function equivalents keyed by the bytecode fingerprint.
//! Phase 2: the bytecode is forwarded to the hardware shader compiler.
//!
//! ## Thread safety
//!
//! `GlContext` is `!Send` and `!Sync` — one context per thread, matching the
//! OpenGL threading model.
//!
//! ## Usage
//!
//! ```rust
//! let mut gl = GlContext::new(device)?;
//!
//! let prog = gl.create_program(VERT_SRC, FRAG_SRC)?;
//! let vbo  = gl.create_buffer();
//! gl.bind_buffer(BufferTarget::Array, vbo);
//! gl.buffer_data(BufferTarget::Array, bytemuck::cast_slice(&verts), Usage::StaticDraw);
//!
//! let vao = gl.create_vertex_array();
//! gl.bind_vertex_array(vao);
//! gl.vertex_attrib_pointer(0, 3, AttribType::Float, false, 20, 0);
//! gl.enable_vertex_attrib_array(0);
//!
//! gl.use_program(prog);
//! gl.draw_arrays(Primitive::Triangles, 0, 3);
//! gl.flush(); // submits GpuCmd batch to kernel
//! ```

pub mod buffer;
pub mod context;
pub mod draw;
pub mod error;
pub mod framebuffer;
pub mod program;
pub mod state;
pub mod texture;
pub mod uniform;
pub mod vertex;

pub use buffer::{BufferTarget, BufferUsage, GlBuffer};
pub use context::GlContext;
pub use draw::{DrawParams, draw};
pub use error::GlError;
pub use framebuffer::{AttachPoint, FboStatus, GlFbo};
pub use program::{GlProgram, ShaderHint, ShaderStage};
pub use state::{
    BlendEquation, BlendFactor, Capability, CullFace, DepthFunc, FrontFace, PolygonOffset,
    RenderState, ScissorBox, StencilAction, StencilFunc, Viewport,
};
pub use texture::{FilterMode, GlRenderbuffer, GlTexture, SamplerParams, TextureTarget, WrapMode};
pub use uniform::UniformValue;
pub use vertex::{AttribType, GlVao, VertexAttrib, VertexBinding};
