// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! `GlContext` — the primary GL state machine and API entry point.
//!
//! Analogous to an OpenGL context.  One per compositor thread.
//! All GL objects are created and destroyed through this type.

extern crate alloc;
use alloc::{collections::BTreeMap, vec, vec::Vec};

use crate::command::{Color, CommandBuffer, PixelFormat, ResourceId, ResourceKind};
use crate::device::GpuDevice;
use crate::types::{BlendState, DepthState, IndexFormat, RasterState, Topology, VertexLayout};

use super::buffer::{BufferTarget, BufferUsage, GlBuffer};
use super::draw::{DrawParams, draw};
use super::error::GlError;
use super::framebuffer::{AttachPoint, Attachment, AttachmentSource, GlFbo};
use super::program::{GlProgram, ShaderStage, compile_shader, link_program};
use super::state::{
    BlendEquation, BlendFactor, Capability, CullFace, DepthFunc, FrontFace, RenderState,
    ScissorBox, StencilAction, StencilFunc, Viewport,
};
use super::texture::{
    FilterMode, GlRenderbuffer, GlTexture, SamplerParams, TextureTarget, WrapMode,
};
use super::uniform::UniformValue;
use super::vertex::{GlVao, VertexAttrib, VertexBinding};

// ── Object registries ─────────────────────────────────────────────────────────

struct Registry<T> {
    next: u32,
    map: BTreeMap<u32, T>,
}

impl<T> Registry<T> {
    fn new() -> Self {
        Self {
            next: 1,
            map: BTreeMap::new(),
        }
    }
    fn alloc(&mut self, obj: T) -> u32 {
        let id = self.next;
        self.next += 1;
        self.map.insert(id, obj);
        id
    }
    fn get(&self, id: u32) -> Option<&T> {
        self.map.get(&id)
    }
    fn get_mut(&mut self, id: u32) -> Option<&mut T> {
        self.map.get_mut(&id)
    }
    fn delete(&mut self, id: u32) -> Option<T> {
        self.map.remove(&id)
    }
}

// ── GlContext ─────────────────────────────────────────────────────────────────

/// GraphOS GL context — the stateful API surface.
///
/// ## Usage
/// ```
/// let mut gl = GlContext::new(device);
/// let prog = gl.create_program(VERT, FRAG).unwrap();
/// let vbo  = gl.gen_buffer();
/// gl.bind_buffer(BufferTarget::ArrayBuffer, vbo);
/// gl.buffer_data_raw(BufferTarget::ArrayBuffer, &vertex_bytes, BufferUsage::StaticDraw);
/// gl.use_program(prog);
/// gl.draw_arrays(Topology::Triangles, 0, 6);
/// gl.flush();
/// ```
pub struct GlContext {
    device: GpuDevice,

    // Object stores
    programs: Registry<GlProgram>,
    buffers: Registry<GlBuffer>,
    textures: Registry<GlTexture>,
    rbos: Registry<GlRenderbuffer>,
    vaos: Registry<GlVao>,
    fbos: Registry<GlFbo>,

    // Binding state
    active_program: u32,
    active_vao: u32,
    active_fbo: u32,
    bound_buf_array: u32,
    bound_buf_element: u32,
    bound_buf_uniform: u32,
    active_texture_unit: u8,
    bound_textures: [u32; 32],

    // Render state (mirrors glEnable/glDisable etc.)
    state: RenderState,

    // In-progress command buffer
    cmds: CommandBuffer,

    // Backbuffer (default FBO target)
    backbuffer: ResourceId,
}

impl GlContext {
    /// Create a new GL context over a kernel device.
    pub fn new(device: GpuDevice, backbuffer: ResourceId) -> Self {
        Self {
            device,
            programs: Registry::new(),
            buffers: Registry::new(),
            textures: Registry::new(),
            rbos: Registry::new(),
            vaos: Registry::new(),
            fbos: Registry::new(),
            active_program: 0,
            active_vao: 0,
            active_fbo: 0,
            bound_buf_array: 0,
            bound_buf_element: 0,
            bound_buf_uniform: 0,
            active_texture_unit: 0,
            bound_textures: [0u32; 32],
            state: RenderState::default(),
            cmds: CommandBuffer::new(),
            backbuffer,
        }
    }

    // ── Error ─────────────────────────────────────────────────────────────────

    /// glGetError equivalent (always returns the last error; resets it).
    pub fn get_error(&mut self) -> Option<GlError> {
        None
    }

    // ── State: enable / disable ───────────────────────────────────────────────

    pub fn enable(&mut self, cap: Capability) {
        match cap {
            Capability::Blend => self.state.blend = true,
            Capability::CullFace => self.state.cull_face = true,
            Capability::DepthTest => self.state.depth_test = true,
            Capability::ScissorTest => self.state.scissor_test = true,
            Capability::StencilTest => self.state.stencil_test = true,
            Capability::Multisample => self.state.multisample = true,
            Capability::DepthClamp => self.state.depth_clamp = true,
            Capability::PrimitiveRestart => self.state.primitive_restart = true,
            _ => {}
        }
    }

    pub fn disable(&mut self, cap: Capability) {
        match cap {
            Capability::Blend => self.state.blend = false,
            Capability::CullFace => self.state.cull_face = false,
            Capability::DepthTest => self.state.depth_test = false,
            Capability::ScissorTest => self.state.scissor_test = false,
            Capability::StencilTest => self.state.stencil_test = false,
            Capability::Multisample => self.state.multisample = false,
            Capability::DepthClamp => self.state.depth_clamp = false,
            Capability::PrimitiveRestart => self.state.primitive_restart = false,
            _ => {}
        }
    }

    // ── State: viewport / scissor ─────────────────────────────────────────────

    pub fn viewport(&mut self, x: i32, y: i32, w: u32, h: u32) {
        self.state.viewport = Viewport {
            x,
            y,
            width: w,
            height: h,
        };
        self.cmds
            .set_viewport(x as f32, y as f32, w as f32, h as f32);
    }

    pub fn scissor(&mut self, x: i32, y: i32, w: u32, h: u32) {
        self.state.scissor = ScissorBox {
            x,
            y,
            width: w,
            height: h,
        };
        let r = crate::command::Rect::new(x, y, w, h);
        self.cmds.set_scissor(r);
    }

    // ── State: blend ──────────────────────────────────────────────────────────

    pub fn blend_func(&mut self, src: BlendFactor, dst: BlendFactor) {
        self.blend_func_separate(src, dst, src, dst);
    }

    pub fn blend_func_separate(
        &mut self,
        src_rgb: BlendFactor,
        dst_rgb: BlendFactor,
        src_a: BlendFactor,
        dst_a: BlendFactor,
    ) {
        self.state.blend_src_rgb = src_rgb;
        self.state.blend_dst_rgb = dst_rgb;
        self.state.blend_src_alpha = src_a;
        self.state.blend_dst_alpha = dst_a;
        self.flush_blend_state();
    }

    pub fn blend_equation(&mut self, eq: BlendEquation) {
        self.state.blend_eq_rgb = eq;
        self.state.blend_eq_alpha = eq;
        self.flush_blend_state();
    }

    pub fn blend_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.state.blend_color = [r, g, b, a];
    }

    fn flush_blend_state(&mut self) {
        let bs = self.make_blend_state();
        self.cmds.set_blend_state(bs);
    }

    fn make_blend_state(&self) -> BlendState {
        use super::state::BlendFactor as SF;
        use crate::types::BlendFactor as TF;
        fn cvt(f: SF) -> TF {
            match f {
                SF::Zero => TF::Zero,
                SF::One => TF::One,
                SF::SrcAlpha => TF::SrcAlpha,
                SF::OneMinusSrcAlpha => TF::OneMinusSrcAlpha,
                SF::DstAlpha => TF::DstAlpha,
                SF::OneMinusDstAlpha => TF::OneMinusDstAlpha,
                SF::SrcColor => TF::SrcColor,
                SF::OneMinusSrcColor => TF::OneMinusSrcColor,
                _ => TF::One,
            }
        }
        use super::state::BlendEquation as SE;
        use crate::types::BlendOp as BO;
        fn ceq(e: SE) -> BO {
            match e {
                SE::FuncAdd => BO::Add,
                SE::FuncSubtract => BO::Subtract,
                SE::FuncReverseSubtract => BO::ReverseSubtract,
                SE::Min => BO::Min,
                SE::Max => BO::Max,
            }
        }
        BlendState {
            enabled: self.state.blend,
            src_color: cvt(self.state.blend_src_rgb),
            dst_color: cvt(self.state.blend_dst_rgb),
            color_op: ceq(self.state.blend_eq_rgb),
            src_alpha: cvt(self.state.blend_src_alpha),
            dst_alpha: cvt(self.state.blend_dst_alpha),
            alpha_op: ceq(self.state.blend_eq_alpha),
            write_mask: 0x0F,
        }
    }

    // ── State: depth ──────────────────────────────────────────────────────────

    pub fn depth_func(&mut self, f: DepthFunc) {
        self.state.depth_func = f;
        self.flush_depth_state();
    }

    pub fn depth_mask(&mut self, write: bool) {
        self.state.depth_mask = write;
        self.flush_depth_state();
    }

    fn flush_depth_state(&mut self) {
        use super::state::DepthFunc as SF;
        use crate::types::DepthOp as DO;
        let op = match self.state.depth_func {
            SF::Never => DO::Never,
            SF::Less => DO::Less,
            SF::Equal => DO::Equal,
            SF::LessOrEqual => DO::LessOrEqual,
            SF::Greater => DO::Greater,
            SF::NotEqual => DO::NotEqual,
            SF::GreaterOrEqual => DO::GreaterOrEqual,
            SF::Always => DO::Always,
        };
        let ds = DepthState {
            test_enable: self.state.depth_test,
            write_enable: self.state.depth_mask,
            compare_op: op,
        };
        self.cmds.set_depth_state(ds);
    }

    // ── State: cull ───────────────────────────────────────────────────────────

    pub fn cull_face(&mut self, face: CullFace) {
        self.state.cull_face_mode = face;
        self.flush_raster_state();
    }

    pub fn front_face(&mut self, winding: FrontFace) {
        self.state.front_face = winding;
        self.flush_raster_state();
    }

    fn flush_raster_state(&mut self) {
        use super::state::{CullFace as SC, FrontFace as SF};
        use crate::types::{CullMode, FillMode};
        let rs = RasterState {
            cull_mode: match self.state.cull_face_mode {
                SC::Front => CullMode::Front,
                SC::Back => CullMode::Back,
                SC::FrontAndBack => CullMode::None,
            },
            fill_mode: FillMode::Solid,
            front_ccw: self.state.front_face == SF::CCW,
            depth_clip: true,
        };
        self.cmds.set_raster_state(rs);
    }

    // ── State: clear ──────────────────────────────────────────────────────────

    pub fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.state.clear_color = [r, g, b, a];
    }

    pub fn clear(&mut self, color: bool, depth: bool, _stencil: bool) {
        let target = self.current_color_rt();
        if color {
            let [r, g, b, a] = self.state.clear_color;
            let ai = (a * 255.0) as u8;
            let ri = (r * 255.0) as u8;
            let gi = (g * 255.0) as u8;
            let bi = (b * 255.0) as u8;
            let c = Color::argb(ai, ri, gi, bi);
            let vp = self.state.viewport;
            let rect = crate::command::Rect::new(vp.x, vp.y, vp.width, vp.height);
            self.cmds.fill_rect(target, rect, c, 0);
        }
        if depth {
            self.cmds.clear_depth(self.state.clear_depth as f32);
        }
    }

    // ── Buffers ───────────────────────────────────────────────────────────────

    pub fn gen_buffer(&mut self) -> u32 {
        self.buffers.alloc(GlBuffer::new(self.buffers.next))
    }

    pub fn gen_buffers(&mut self, n: u32) -> Vec<u32> {
        (0..n).map(|_| self.gen_buffer()).collect()
    }

    pub fn delete_buffer(&mut self, name: u32) {
        if let Some(buf) = self.buffers.delete(name) {
            if buf.resource.is_valid() {
                self.cmds.free_resource(buf.resource);
            }
        }
    }

    pub fn bind_buffer(&mut self, target: BufferTarget, name: u32) {
        match target {
            BufferTarget::ArrayBuffer => self.bound_buf_array = name,
            BufferTarget::ElementArrayBuffer => self.bound_buf_element = name,
            BufferTarget::UniformBuffer => self.bound_buf_uniform = name,
            _ => {}
        }
        if self.active_vao != 0 {
            if let Some(vao) = self.vaos.get_mut(self.active_vao) {
                match target {
                    BufferTarget::ArrayBuffer => {
                        vao.vbos[0] = self
                            .buffers
                            .get(name)
                            .map(|buf| buf.resource)
                            .unwrap_or(ResourceId::INVALID);
                    }
                    BufferTarget::ElementArrayBuffer => {
                        vao.ibo = self
                            .buffers
                            .get(name)
                            .map(|buf| buf.resource)
                            .unwrap_or(ResourceId::INVALID);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Upload buffer data.  Allocates a GPU resource on first call.
    pub fn buffer_data_raw(&mut self, target: BufferTarget, data: &[u8], usage: BufferUsage) {
        let name = match target {
            BufferTarget::ArrayBuffer => self.bound_buf_array,
            BufferTarget::ElementArrayBuffer => self.bound_buf_element,
            BufferTarget::UniformBuffer => self.bound_buf_uniform,
            _ => return,
        };
        let size = data.len() as u32;
        let kind = target.to_kind();

        // Alloc GPU resource if needed.
        let resource = if let Some(buf) = self.buffers.get(name) {
            if buf.resource.is_valid() && buf.size >= size {
                buf.resource
            } else {
                match self.device.alloc_buffer(kind, size) {
                    Some(resource) => resource,
                    None => return,
                }
            }
        } else {
            return;
        };

        // Upload.
        let ptr = data.as_ptr() as u64;
        self.cmds.upload_buffer_raw(resource, 0, ptr, size);

        if let Some(buf) = self.buffers.get_mut(name) {
            buf.resource = resource;
            buf.size = size;
            buf.usage = usage;
        }
        if self.active_vao != 0 {
            if let Some(vao) = self.vaos.get_mut(self.active_vao) {
                match target {
                    BufferTarget::ArrayBuffer => vao.vbos[0] = resource,
                    BufferTarget::ElementArrayBuffer => vao.ibo = resource,
                    _ => {}
                }
            }
        }
    }

    pub fn buffer_sub_data_raw(&mut self, target: BufferTarget, offset: u32, data: &[u8]) {
        let name = match target {
            BufferTarget::ArrayBuffer => self.bound_buf_array,
            BufferTarget::ElementArrayBuffer => self.bound_buf_element,
            _ => return,
        };
        if let Some(buf) = self.buffers.get(name) {
            if buf.resource.is_valid() {
                let resource = buf.resource;
                self.cmds.upload_buffer_raw(
                    resource,
                    offset,
                    data.as_ptr() as u64,
                    data.len() as u32,
                );
            }
        }
    }

    // ── Textures ──────────────────────────────────────────────────────────────

    pub fn gen_texture(&mut self) -> u32 {
        self.textures
            .alloc(GlTexture::new(self.textures.next, TextureTarget::Texture2D))
    }

    pub fn gen_textures(&mut self, n: u32) -> Vec<u32> {
        (0..n).map(|_| self.gen_texture()).collect()
    }

    pub fn delete_texture(&mut self, name: u32) {
        if let Some(tex) = self.textures.delete(name) {
            if tex.resource.is_valid() {
                self.cmds.free_resource(tex.resource);
            }
        }
    }

    pub fn active_texture(&mut self, unit: u8) {
        self.active_texture_unit = unit;
    }

    pub fn bind_texture(&mut self, target: TextureTarget, name: u32) {
        let unit = self.active_texture_unit as usize;
        self.bound_textures[unit] = name;
        // The kernel bind is emitted at draw time.
    }

    /// Allocate a 2D texture and (optionally) upload pixel data.
    pub fn tex_image_2d(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
        data_ptr: Option<u64>,
        data_len: u32,
    ) {
        let name = self.bound_textures[self.active_texture_unit as usize];
        if name == 0 {
            return;
        }
        let resource =
            match self
                .device
                .alloc_resource(width, height, format, ResourceKind::RenderTarget)
            {
                Some(resource) => resource,
                None => return,
            };
        if let Some(ptr) = data_ptr {
            self.cmds.upload_buffer_raw(resource, 0, ptr, data_len);
        }
        if let Some(tex) = self.textures.get_mut(name) {
            tex.resource = resource;
            tex.width = width;
            tex.height = height;
            tex.format = format;
        }
    }

    pub fn generate_mipmap(&mut self, _target: TextureTarget) {
        // Emit a GenerateMips command (Phase 2; no-op in Phase 1).
        // The kernel executor will generate hardware mips when available.
    }

    pub fn tex_parameter_min_filter(&mut self, filter: FilterMode) {
        let name = self.bound_textures[self.active_texture_unit as usize];
        if let Some(tex) = self.textures.get_mut(name) {
            tex.sampler.min_filter = filter;
        }
    }

    pub fn tex_parameter_mag_filter(&mut self, filter: FilterMode) {
        let name = self.bound_textures[self.active_texture_unit as usize];
        if let Some(tex) = self.textures.get_mut(name) {
            tex.sampler.mag_filter = filter;
        }
    }

    pub fn tex_parameter_wrap_s(&mut self, wrap: WrapMode) {
        let name = self.bound_textures[self.active_texture_unit as usize];
        if let Some(tex) = self.textures.get_mut(name) {
            tex.sampler.wrap_s = wrap;
        }
    }

    pub fn tex_parameter_wrap_t(&mut self, wrap: WrapMode) {
        let name = self.bound_textures[self.active_texture_unit as usize];
        if let Some(tex) = self.textures.get_mut(name) {
            tex.sampler.wrap_t = wrap;
        }
    }

    // ── Renderbuffers ─────────────────────────────────────────────────────────

    pub fn gen_renderbuffer(&mut self) -> u32 {
        self.rbos.alloc(GlRenderbuffer::new(self.rbos.next))
    }

    pub fn bind_renderbuffer(&mut self, _name: u32) {}

    pub fn renderbuffer_storage(&mut self, name: u32, format: PixelFormat, w: u32, h: u32) {
        let kind = if format == PixelFormat::Bgra8Unorm || format == PixelFormat::Rgba8Unorm {
            ResourceKind::RenderTarget
        } else {
            ResourceKind::DepthStencil
        };
        let Some(resource) = self.device.alloc_resource(w, h, format, kind) else {
            return;
        };
        if let Some(rbo) = self.rbos.get_mut(name) {
            rbo.resource = resource;
            rbo.width = w;
            rbo.height = h;
            rbo.format = format;
        }
    }

    // ── Vertex arrays ─────────────────────────────────────────────────────────

    pub fn gen_vertex_array(&mut self) -> u32 {
        self.vaos.alloc(GlVao::new(self.vaos.next))
    }

    pub fn gen_vertex_arrays(&mut self, n: u32) -> Vec<u32> {
        (0..n).map(|_| self.gen_vertex_array()).collect()
    }

    pub fn bind_vertex_array(&mut self, name: u32) {
        self.active_vao = name;
    }

    pub fn vertex_attrib_pointer(
        &mut self,
        location: u32,
        components: u8,
        ty: super::vertex::AttribType,
        normalise: bool,
        stride: u32,
        offset: u32,
    ) {
        if let Some(vao) = self.vaos.get_mut(self.active_vao) {
            let slot = location as usize;
            if slot < 16 {
                vao.attribs[slot] = Some(super::vertex::VertexAttrib {
                    location,
                    components,
                    ty,
                    normalise,
                    offset,
                    binding: 0,
                });
                vao.bindings[0] = Some(VertexBinding {
                    stride,
                    step_rate: 0,
                });
                vao.infer_layout();
            }
        }
    }

    pub fn enable_vertex_attrib_array(&mut self, _location: u32) {}

    pub fn vertex_attrib_divisor(&mut self, location: u32, divisor: u32) {
        if let Some(vao) = self.vaos.get_mut(self.active_vao) {
            let slot = location as usize;
            if slot < 8 {
                if let Some(b) = &mut vao.bindings[slot] {
                    b.step_rate = divisor;
                }
            }
        }
    }

    // ── Framebuffers ─────────────────────────────────────────────────────────

    pub fn gen_framebuffer(&mut self) -> u32 {
        self.fbos.alloc(GlFbo::new(self.fbos.next))
    }

    pub fn bind_framebuffer(&mut self, name: u32) {
        self.active_fbo = name;
    }

    pub fn framebuffer_texture_2d(&mut self, attachment: AttachPoint, tex_name: u32, level: u8) {
        let resource = self
            .textures
            .get(tex_name)
            .map(|t| t.resource)
            .unwrap_or(ResourceId::INVALID);
        let (w, h) = self
            .textures
            .get(tex_name)
            .map(|t| (t.width, t.height))
            .unwrap_or((0, 0));
        if let Some(fbo) = self.fbos.get_mut(self.active_fbo) {
            fbo.attach(
                Attachment {
                    point: attachment,
                    source: AttachmentSource::Texture {
                        resource,
                        level,
                        layer: 0,
                    },
                },
                w,
                h,
            );
        }
    }

    pub fn framebuffer_renderbuffer(&mut self, attachment: AttachPoint, rbo_name: u32) {
        let resource = self
            .rbos
            .get(rbo_name)
            .map(|r| r.resource)
            .unwrap_or(ResourceId::INVALID);
        let (w, h) = self
            .rbos
            .get(rbo_name)
            .map(|r| (r.width, r.height))
            .unwrap_or((0, 0));
        if let Some(fbo) = self.fbos.get_mut(self.active_fbo) {
            fbo.attach(
                Attachment {
                    point: attachment,
                    source: AttachmentSource::Renderbuf { resource },
                },
                w,
                h,
            );
        }
    }

    pub fn check_framebuffer_status(&self, name: u32) -> super::framebuffer::FboStatus {
        self.fbos
            .get(name)
            .map(|f| f.status())
            .unwrap_or(super::framebuffer::FboStatus::Undefined)
    }

    // ── Programs ──────────────────────────────────────────────────────────────

    pub fn create_program(&mut self, vert_src: &str, frag_src: &str) -> Result<u32, GlError> {
        let vert = compile_shader(ShaderStage::Vertex, vert_src)?;
        let frag = compile_shader(ShaderStage::Fragment, frag_src)?;
        let mut prog = GlProgram::new(self.programs.next);
        prog.vertex = Some(vert);
        prog.fragment = Some(frag);
        link_program(&mut prog)?;
        let id = self.programs.alloc(prog);
        Ok(id)
    }

    pub fn create_compute_program(&mut self, comp_src: &str) -> Result<u32, GlError> {
        let comp = compile_shader(ShaderStage::Compute, comp_src)?;
        let mut prog = GlProgram::new(self.programs.next);
        prog.compute = Some(comp);
        link_program(&mut prog)?;
        let id = self.programs.alloc(prog);
        Ok(id)
    }

    pub fn delete_program(&mut self, name: u32) {
        self.programs.delete(name);
    }

    pub fn use_program(&mut self, name: u32) {
        self.active_program = name;
    }

    pub fn get_uniform_location(&self, prog: u32, name: &str) -> Option<u32> {
        self.programs.get(prog)?.uniform_location(name)
    }

    // ── Uniforms ──────────────────────────────────────────────────────────────

    pub fn uniform(&mut self, location: u32, value: UniformValue) {
        let packed = value.as_u32x4();
        self.cmds.set_uniform(location as u8, packed);
    }

    pub fn uniform_1f(&mut self, loc: u32, v: f32) {
        self.uniform(loc, UniformValue::Float(v));
    }

    pub fn uniform_1i(&mut self, loc: u32, v: i32) {
        self.uniform(loc, UniformValue::Int(v));
    }

    pub fn uniform_2f(&mut self, loc: u32, a: f32, b: f32) {
        self.uniform(loc, UniformValue::Vec2([a, b]));
    }

    pub fn uniform_3f(&mut self, loc: u32, a: f32, b: f32, c: f32) {
        self.uniform(loc, UniformValue::Vec3([a, b, c]));
    }

    pub fn uniform_4f(&mut self, loc: u32, a: f32, b: f32, c: f32, d: f32) {
        self.uniform(loc, UniformValue::Vec4([a, b, c, d]));
    }

    pub fn uniform_matrix4(&mut self, loc: u32, mat: [[f32; 4]; 4]) {
        self.uniform(loc, UniformValue::Mat4(mat));
    }

    // ── Textures bindings at draw time ────────────────────────────────────────

    fn emit_texture_bindings(&mut self) {
        for unit in 0..32u8 {
            let name = self.bound_textures[unit as usize];
            if name == 0 {
                continue;
            }
            let resource = self
                .textures
                .get(name)
                .map(|t| t.resource)
                .unwrap_or(ResourceId::INVALID);
            if resource.is_valid() {
                self.cmds.bind_texture(unit, resource);
            }
        }
    }

    fn emit_program_state(&mut self) {
        let hint = self
            .programs
            .get(self.active_program)
            .map(|p| p.hint() as u8)
            .unwrap_or(0);
        self.cmds.set_shader_hint(hint);
    }

    // ── Draw calls ────────────────────────────────────────────────────────────

    fn current_color_rt(&self) -> ResourceId {
        if self.active_fbo != 0 {
            self.fbos
                .get(self.active_fbo)
                .map(|f| f.color_rt[0])
                .unwrap_or(self.backbuffer)
        } else {
            self.backbuffer
        }
    }

    fn current_depth_rt(&self) -> ResourceId {
        if self.active_fbo != 0 {
            self.fbos
                .get(self.active_fbo)
                .map(|f| f.depth_rt)
                .unwrap_or(ResourceId::INVALID)
        } else {
            ResourceId::INVALID
        }
    }

    fn emit_render_target(&mut self) {
        let color = self.current_color_rt();
        let depth = self.current_depth_rt();
        self.cmds.set_render_target(color, depth);
    }

    fn resolve_vao(&self) -> (ResourceId, ResourceId, VertexLayout, IndexFormat) {
        if self.active_vao == 0 {
            return (
                ResourceId::INVALID,
                ResourceId::INVALID,
                VertexLayout::Pos2Uv2,
                IndexFormat::U16,
            );
        }
        let vao = match self.vaos.get(self.active_vao) {
            Some(v) => v,
            None => {
                return (
                    ResourceId::INVALID,
                    ResourceId::INVALID,
                    VertexLayout::Pos2Uv2,
                    IndexFormat::U16,
                );
            }
        };
        let vbo = vao.vbos[0];
        let ibo = vao.ibo;
        let layout = vao.layout.unwrap_or(VertexLayout::Pos2Uv2);
        let ifmt = vao.index_fmt;
        (vbo, ibo, layout, ifmt)
    }

    pub fn draw_arrays(&mut self, topology: Topology, first: u32, count: u32) {
        self.emit_render_target();
        self.emit_program_state();
        self.emit_texture_bindings();
        let (vbo, _, layout, _) = self.resolve_vao();
        draw(
            &mut self.cmds,
            DrawParams::arrays(vbo, layout, first, count).with_topology(topology),
        );
    }

    pub fn draw_elements(&mut self, topology: Topology, count: u32) {
        self.emit_render_target();
        self.emit_program_state();
        self.emit_texture_bindings();
        let (vbo, ibo, layout, ifmt) = self.resolve_vao();
        draw(
            &mut self.cmds,
            DrawParams::elements(vbo, ibo, layout, ifmt, 0, count).with_topology(topology),
        );
    }

    pub fn draw_arrays_instanced(
        &mut self,
        topology: Topology,
        first: u32,
        count: u32,
        instances: u32,
    ) {
        self.emit_render_target();
        self.emit_program_state();
        self.emit_texture_bindings();
        let (vbo, _, layout, _) = self.resolve_vao();
        draw(
            &mut self.cmds,
            DrawParams::arrays(vbo, layout, first, count)
                .with_topology(topology)
                .with_instances(instances),
        );
    }

    pub fn draw_elements_instanced(&mut self, topology: Topology, count: u32, instances: u32) {
        self.emit_render_target();
        self.emit_program_state();
        self.emit_texture_bindings();
        let (vbo, ibo, layout, ifmt) = self.resolve_vao();
        draw(
            &mut self.cmds,
            DrawParams::elements(vbo, ibo, layout, ifmt, 0, count)
                .with_topology(topology)
                .with_instances(instances),
        );
    }

    // ── Present ───────────────────────────────────────────────────────────────

    pub fn present(&mut self) {
        self.cmds.present(self.backbuffer);
    }

    // ── Flush ─────────────────────────────────────────────────────────────────

    /// Submit all queued commands to the kernel GPU executor.
    pub fn flush(&mut self) {
        self.device.submit(&self.cmds);
        self.cmds = CommandBuffer::new();
    }
}
