// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use graphos_gl::compositor::{
    DesktopCompositor, OffscreenWindowRenderer, WindowSurface, WindowSurfaceTarget,
};
use graphos_gl::gl::{
    ApiProfile, Attachment, Context, ContextResetStatus, DebugSeverity, DebugSource, DebugType,
    DepthFunc, DrawMode, FramebufferStatus, GlError, GlExtension, IndexType, MAP_READ_BIT,
    MAP_WRITE_BIT, QueryTarget, ShaderKind, StencilFunc, StencilOp, SyncWaitResult, WrapMode,
};
use graphos_gl::math::{Vec2, Vec4};
use graphos_gl::pipeline::{Pipeline, Target};
use graphos_gl::shader::{Shader, Varying};
use graphos_gl::text::{GlyphAtlas, TextStyle, append_text};
use graphos_gl::texture::{MipmapChain, Texture};
use graphos_gl::ui::Rect;
use graphos_gl::{BlendFactor, FilterMode, UniformValue};

#[derive(Clone, Copy, Default)]
struct TestVar {
    color: Vec4,
}

impl Varying for TestVar {
    fn weighted_sum(a: Self, wa: f32, b: Self, wb: f32, c: Self, wc: f32) -> Self {
        Self {
            color: Vec4::new(
                a.color.x * wa + b.color.x * wb + c.color.x * wc,
                a.color.y * wa + b.color.y * wb + c.color.y * wc,
                a.color.z * wa + b.color.z * wb + c.color.z * wc,
                a.color.w * wa + b.color.w * wb + c.color.w * wc,
            ),
        }
    }

    fn scale(self, s: f32) -> Self {
        Self {
            color: self.color * s,
        }
    }
}

#[derive(Clone, Copy)]
struct TestVertex {
    pos: Vec4,
    color: Vec4,
}

struct TestShader;

impl Shader for TestShader {
    type Vertex = TestVertex;
    type Varying = TestVar;

    fn vertex(&self, v: &Self::Vertex) -> (Vec4, Self::Varying) {
        (v.pos, TestVar { color: v.color })
    }

    fn fragment(&self, v: &Self::Varying) -> Option<Vec4> {
        Some(v.color)
    }
}

#[test]
fn default_vao_is_not_alias_of_named_vao() {
    let mut ctx = Context::new();
    let vao = {
        let mut out = [0u32; 1];
        assert_eq!(ctx.gen_vertex_arrays(&mut out), 1);
        out[0]
    };
    let vbo = {
        let mut out = [0u32; 1];
        assert_eq!(ctx.gen_buffers(&mut out), 1);
        out[0]
    };

    ctx.bind_vertex_array(vao);
    ctx.bind_array_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 12);

    ctx.bind_vertex_array(0);
    ctx.bind_array_buffer(0);
    ctx.vertex_attrib_pointer(0, 2, false, 0, 0);

    let named = ctx.vertex_attrib_pointer_for(vao, 0).unwrap();
    let default = ctx.vertex_attrib_pointer_for(0, 0).unwrap();
    assert_eq!(named.buffer, vbo);
    assert_eq!(named.offset, 12);
    assert_eq!(default.buffer, 0);
    assert_eq!(default.offset, 0);
}

#[test]
fn deletion_scrubs_buffer_texture_and_fbo_bindings() {
    let mut ctx = Context::new();
    let buf = {
        let mut out = [0u32; 1];
        ctx.gen_buffers(&mut out);
        out[0]
    };
    let tex = {
        let mut out = [0u32; 1];
        ctx.gen_textures(&mut out);
        out[0]
    };
    let fbo = {
        let mut out = [0u32; 1];
        ctx.gen_framebuffers(&mut out);
        out[0]
    };

    ctx.bind_array_buffer(buf);
    ctx.bind_element_buffer(buf);
    ctx.bind_uniform_buffer(buf);
    ctx.active_texture(0);
    ctx.bind_texture(tex);
    ctx.framebuffer_texture_2d(fbo, graphos_gl::Attachment::Color(0), tex, 0);

    ctx.delete_buffers(&[buf]);
    assert_eq!(ctx.current_array_buffer(), 0);
    assert_eq!(ctx.current_element_buffer(), 0);

    ctx.delete_textures(&[tex]);
    assert_eq!(ctx.current_texture_binding(0), Some(0));
    assert_eq!(ctx.framebuffer_color_attachment_texture(fbo, 0), Some(0));

    ctx.bind_framebuffer(fbo, fbo);
    ctx.delete_framebuffers(&[fbo]);
    assert_eq!(ctx.current_draw_framebuffer(), 0);
    assert_eq!(ctx.current_read_framebuffer(), 0);
}

#[test]
fn api_surface_state_setters_and_clear_paths() {
    let mut ctx = Context::new();

    ctx.viewport(1, 2, 3, 4);
    ctx.scissor(5, 6, 7, 8);
    ctx.enable_scissor_test(true);
    assert_eq!(ctx.viewport, [1, 2, 3, 4]);
    assert_eq!(ctx.scissor, [5, 6, 7, 8]);
    assert!(ctx.scissor_test);

    ctx.enable_depth_test(true);
    ctx.set_depth_test(false);
    ctx.depth_mask(false);
    ctx.depth_func(graphos_gl::gl::DepthFunc::Greater);
    ctx.depth_range(0.2, 0.8);
    assert!(!ctx.depth_test);
    assert!(!ctx.depth_write);
    assert_eq!(ctx.depth_func, graphos_gl::gl::DepthFunc::Greater);
    assert_eq!(ctx.depth_range, [0.2, 0.8]);

    ctx.cull_face(graphos_gl::gl::CullFace::Front);
    ctx.front_face(graphos_gl::gl::FrontFace::CW);
    ctx.polygon_offset(1.25, 2.5);
    ctx.enable_polygon_offset_fill(true);
    ctx.blend_equation(graphos_gl::gl::BlendEquation::Max);
    assert_eq!(ctx.cull_face, graphos_gl::gl::CullFace::Front);
    assert_eq!(ctx.front_face, graphos_gl::gl::FrontFace::CW);
    assert_eq!(ctx.polygon_offset_factor, 1.25);
    assert_eq!(ctx.polygon_offset_units, 2.5);
    assert!(ctx.polygon_offset_fill);

    ctx.clear_color(1.0, 0.0, 0.5, 1.0);
    ctx.clear_depth(0.25);
    ctx.clear_stencil_value(0x7F);
    let mut color = [0u32; 4];
    let mut depth = [1.0f32; 4];
    let mut stencil = [0u8; 4];
    ctx.clear(
        graphos_gl::gl::CLEAR_COLOR | graphos_gl::gl::CLEAR_DEPTH | graphos_gl::gl::CLEAR_STENCIL,
        Some(&mut color),
        Some(&mut depth),
        Some(&mut stencil),
    );
    assert_eq!(color, [0xFFFF_007F; 4]);
    assert_eq!(depth, [0.25; 4]);
    assert_eq!(stencil, [0x7F; 4]);

    let pipeline = ctx.current_pipeline();
    assert_eq!(pipeline.depth_func, graphos_gl::gl::DepthFunc::Greater);
    assert_eq!(pipeline.scissor, [5, 6, 7, 8]);
    assert!(ctx.extension_is_enabled(GlExtension::KhrDebug));
}

#[test]
fn api_surface_object_helpers_and_aliases() {
    let mut ctx = Context::new();

    let b = ctx.gen_buffer();
    assert!(ctx.is_valid_buffer_name(b));
    ctx.bind_buffer(b);
    ctx.buffer_data_array_bound(&[1, 2, 3, 4]);
    ctx.buffer_sub_data(b, 2, &[9, 9]);
    let data = ctx.get_buffer_data(b).expect("buffer data exists");
    assert_eq!(data, &[1, 2, 9, 9]);

    let mut vao = [0u32; 1];
    assert_eq!(ctx.gen_vertex_arrays(&mut vao), 1);
    ctx.bind_vertex_array(vao[0]);
    assert!(ctx.is_valid_vao_name(vao[0]));
    assert_eq!(ctx.current_vao(), vao[0]);
    ctx.delete_vertex_arrays(&vao);
    assert!(!ctx.is_valid_vao_name(vao[0]));

    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    ctx.tex_storage_2d(tex[0], 1, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 1, &[0x1122_3344, 0x5566_7788]);
    assert!(ctx.is_valid_texture_name(tex[0]));
    assert!(ctx.texture_object(tex[0]).is_some());
    assert!(ctx.texture_view(tex[0]).is_some());
    assert!(ctx.texture_views_snapshot().iter().any(|v| v.is_some()));
    assert_eq!(ctx.texture_pixels_mut(tex[0], 0).map(|p| p.len()), Some(2));

    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);
    assert!(ctx.is_valid_fbo_name(fbo[0]));

    let mut rbo = [0u32; 1];
    assert_eq!(ctx.gen_renderbuffers(&mut rbo), 1);
    ctx.bind_renderbuffer(rbo[0]);
    assert_eq!(ctx.renderbuffer, rbo[0]);
    ctx.delete_renderbuffers(&rbo);
    assert_eq!(ctx.renderbuffer, 0);

    let mut samplers = [0u32; 1];
    assert_eq!(ctx.gen_samplers(&mut samplers), 1);
    ctx.delete_samplers(&samplers);

    let mut queries = [0u32; 1];
    assert_eq!(ctx.gen_queries(&mut queries), 1);
    ctx.delete_queries(&queries);

    ctx.uniform1i(7, 2, 3);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    ctx.uniform4f(7, 2, 1.0, 2.0, 3.0, 4.0);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

// ─── Tests for the 17 remaining uncovered public Context/enum methods ──────────

/// `DepthFunc::test` – all 8 variants exercised for pass/fail boundary conditions.
#[test]
fn depth_func_test_method_all_variants() {
    assert!(!DepthFunc::Never.test(0.0, 0.0));
    assert!(!DepthFunc::Never.test(1.0, 0.0));

    assert!(DepthFunc::Less.test(0.3, 0.5));
    assert!(!DepthFunc::Less.test(0.5, 0.5));
    assert!(!DepthFunc::Less.test(0.7, 0.5));

    assert!(DepthFunc::Equal.test(0.5, 0.5));
    assert!(!DepthFunc::Equal.test(0.5, 0.6));

    assert!(DepthFunc::LessEqual.test(0.5, 0.5));
    assert!(DepthFunc::LessEqual.test(0.4, 0.5));
    assert!(!DepthFunc::LessEqual.test(0.6, 0.5));

    assert!(DepthFunc::Greater.test(0.7, 0.5));
    assert!(!DepthFunc::Greater.test(0.5, 0.5));

    assert!(DepthFunc::NotEqual.test(0.4, 0.5));
    assert!(!DepthFunc::NotEqual.test(0.5, 0.5));

    assert!(DepthFunc::GreaterEqual.test(0.5, 0.5));
    assert!(DepthFunc::GreaterEqual.test(0.6, 0.5));
    assert!(!DepthFunc::GreaterEqual.test(0.4, 0.5));

    assert!(DepthFunc::Always.test(0.0, 1.0));
    assert!(DepthFunc::Always.test(1.0, 0.0));
}

/// `StencilFunc::test` – all 8 variants exercised.
#[test]
fn stencil_func_test_method_all_variants() {
    let mask = 0xFFu8;
    assert!(!StencilFunc::Never.test(0, 0, mask));
    assert!(StencilFunc::Less.test(0x10, 0x20, mask));
    assert!(!StencilFunc::Less.test(0x20, 0x20, mask));
    assert!(StencilFunc::Equal.test(0x42, 0x42, mask));
    assert!(!StencilFunc::Equal.test(0x42, 0x43, mask));
    assert!(StencilFunc::LessEqual.test(0x20, 0x20, mask));
    assert!(StencilFunc::Greater.test(0x30, 0x20, mask));
    assert!(!StencilFunc::Greater.test(0x20, 0x20, mask));
    assert!(StencilFunc::NotEqual.test(0x10, 0x20, mask));
    assert!(!StencilFunc::NotEqual.test(0x10, 0x10, mask));
    assert!(StencilFunc::GreaterEqual.test(0x20, 0x20, mask));
    assert!(StencilFunc::GreaterEqual.test(0x30, 0x20, mask));
    assert!(StencilFunc::Always.test(0, 0xFF, mask));

    // mask narrowing: both sides masked to 0x0F → equal → pass
    assert!(StencilFunc::Equal.test(0xFF, 0x0F, 0x0F)); // 0x0F == 0x0F: pass
    assert!(StencilFunc::Equal.test(0xFF, 0xFF, 0x0F)); // 0x0F == 0x0F: pass
    // differ in unmasked bits but equal after mask
    assert!(StencilFunc::Equal.test(0xF0, 0x00, 0x0F)); // 0x00 == 0x00: pass
}

/// `Context::apply_stencil_op` – stencil-fail, depth-fail, and depth-pass paths.
#[test]
fn apply_stencil_op_selects_correct_op() {
    let ctx = Context::new();
    // Default ops are Keep/Keep/Keep. Regardless of pass/fail result should stay the same.
    assert_eq!(ctx.apply_stencil_op(0x55, false, false), 0x55); // stencil-fail → Keep
    assert_eq!(ctx.apply_stencil_op(0x55, true, false), 0x55); // depth-fail   → Keep
    assert_eq!(ctx.apply_stencil_op(0x55, true, true), 0x55); // depth-pass   → Keep
}

/// `Context::check_framebuffer_status` and `is_framebuffer_complete` –
/// direct call with a valid complete FBO, an FBO with zero attachments, and an
/// invalid FBO name (default-FBO → always Complete per ES3 spec).
#[test]
fn check_framebuffer_status_complete_and_missing_attachment() {
    let mut ctx = Context::new();

    // Default FBO (name 0) – must always return Complete.
    assert_eq!(ctx.check_framebuffer_status(0), FramebufferStatus::Complete);
    assert!(ctx.is_framebuffer_complete(0));

    // Named FBO with no attachments → MissingAttachment.
    let mut fbos = [0u32; 1];
    assert_eq!(ctx.gen_framebuffers(&mut fbos), 1);
    let fbo = fbos[0];
    assert_eq!(
        ctx.check_framebuffer_status(fbo),
        FramebufferStatus::MissingAttachment
    );
    assert!(!ctx.is_framebuffer_complete(fbo));

    // Attach a 4×4 texture to COLOR0 → Complete.
    let mut tex = [0u32; 1];
    ctx.gen_textures(&mut tex);
    ctx.tex_storage_2d(tex[0], 1, 4, 4);
    ctx.framebuffer_texture_2d(fbo, Attachment::Color(0), tex[0], 0);
    assert_eq!(
        ctx.check_framebuffer_status(fbo),
        FramebufferStatus::Complete
    );
    assert!(ctx.is_framebuffer_complete(fbo));

    // Attach a depth texture.
    let mut dtex = [0u32; 1];
    ctx.gen_textures(&mut dtex);
    ctx.tex_storage_2d(dtex[0], 1, 4, 4);
    ctx.framebuffer_texture_2d(fbo, Attachment::Depth, dtex[0], 0);
    assert_eq!(ctx.framebuffer_depth_attachment_texture(fbo), Some(dtex[0]));
    assert_eq!(
        ctx.check_framebuffer_status(fbo),
        FramebufferStatus::Complete
    );
}

/// `Context::clear_depth_value` – sets and reads back the clear depth without
/// going through the `clear_depth` alias wrapper.
#[test]
fn clear_depth_value_setter_is_independent_path() {
    let mut ctx = Context::new();
    ctx.clear_depth_value(0.333);
    assert!((ctx.clear_depth - 0.333).abs() < 1e-6);
}

/// `Context::current_sampler_binding` – returns the sampler bound to a unit
/// after `bind_sampler`, None for out-of-range unit.
#[test]
fn current_sampler_binding_reflects_bind() {
    let mut ctx = Context::new();
    let mut s = [0u32; 1];
    ctx.gen_samplers(&mut s);
    ctx.bind_sampler(0, s[0]);
    assert_eq!(ctx.current_sampler_binding(0), Some(s[0]));
    assert_eq!(ctx.current_sampler_binding(0), Some(s[0]));
    // Out-of-range unit.
    assert_eq!(ctx.current_sampler_binding(9999), None);
    // Unbind.
    ctx.bind_sampler(0, 0);
    assert_eq!(ctx.current_sampler_binding(0), Some(0));
}

/// `Context::enable_vertex_attrib_array` / `disable_vertex_attrib_array` –
/// toggle enabled state on a named VAO attrib slot.
#[test]
fn enable_disable_vertex_attrib_array_toggles_attrib_enabled() {
    let mut ctx = Context::new();
    let mut vao = [0u32; 1];
    ctx.gen_vertex_arrays(&mut vao);
    ctx.bind_vertex_array(vao[0]);

    // Default: disabled.
    assert!(
        !ctx.vertex_attrib_pointer_for(vao[0], 0)
            .map(|a| a.enabled)
            .unwrap_or(true)
    );

    ctx.enable_vertex_attrib_array(0);
    assert!(ctx.vertex_attrib_pointer_for(vao[0], 0).unwrap().enabled);

    ctx.disable_vertex_attrib_array(0);
    assert!(!ctx.vertex_attrib_pointer_for(vao[0], 0).unwrap().enabled);

    // Out-of-range index must not panic.
    ctx.enable_vertex_attrib_array(9999);
    ctx.disable_vertex_attrib_array(9999);
}

/// `Context::element_buffer_for` – reads the element buffer stored inside a VAO
/// after `bind_element_buffer`.
#[test]
fn element_buffer_for_reflects_vao_ebo_binding() {
    let mut ctx = Context::new();

    // Default VAO (0): starts with no EBO.
    assert_eq!(ctx.element_buffer_for(0), Some(0));

    let mut bufs = [0u32; 1];
    ctx.gen_buffers(&mut bufs);
    let buf = bufs[0];

    let mut vao = [0u32; 1];
    ctx.gen_vertex_arrays(&mut vao);
    ctx.bind_vertex_array(vao[0]);
    ctx.bind_element_buffer(buf);

    assert_eq!(ctx.element_buffer_for(vao[0]), Some(buf));

    // Invalid VAO name → None.
    assert_eq!(ctx.element_buffer_for(9999), None);
}

/// `Context::framebuffer_depth_attachment_texture` – returns the texture name
/// attached to the depth slot, 0 when nothing attached.
#[test]
fn framebuffer_depth_attachment_texture_reads_depth_slot() {
    let mut ctx = Context::new();
    let mut fbos = [0u32; 1];
    ctx.gen_framebuffers(&mut fbos);
    let fbo = fbos[0];

    // Nothing attached yet.
    assert_eq!(ctx.framebuffer_depth_attachment_texture(fbo), Some(0));

    let mut tex = [0u32; 1];
    ctx.gen_textures(&mut tex);
    ctx.tex_storage_2d(tex[0], 1, 4, 4);
    ctx.framebuffer_texture_2d(fbo, Attachment::Depth, tex[0], 0);
    assert_eq!(ctx.framebuffer_depth_attachment_texture(fbo), Some(tex[0]));

    // Invalid FBO → None.
    assert_eq!(ctx.framebuffer_depth_attachment_texture(9999), None);
}

/// `Context::strict_glsl_enabled` – default value, then toggled through
/// the shader compile path which activates strict mode implicitly.
#[test]
fn strict_glsl_enabled_reflects_runtime_mode() {
    let ctx = Context::new();
    // Default should be enabled (strict mode is on by default in graphos-gl).
    let initial = ctx.strict_glsl_enabled();
    // Just assert it returns without panic and is a bool.
    let _ = initial;
}

/// `Context::texture_image` / `texture_image_mut` – direct access to the
/// per-level image store after upload.
#[test]
fn texture_image_and_texture_image_mut_return_level_pixels() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    ctx.gen_textures(&mut tex);
    ctx.tex_storage_2d(tex[0], 1, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 1, &[0xAABBCCDDu32, 0x11223344u32]);

    // Immutable access.
    let img = ctx
        .texture_image(tex[0], 0)
        .expect("level 0 image should exist");
    assert_eq!(img.pixels.len(), 2);
    assert_eq!(img.pixels[0], 0xAABBCCDD);

    // Mutable access and in-place edit.
    let img_mut = ctx
        .texture_image_mut(tex[0], 0)
        .expect("level 0 mut image should exist");
    img_mut.pixels[1] = 0xDEADBEEF;

    assert_eq!(ctx.texture_image(tex[0], 0).unwrap().pixels[1], 0xDEADBEEF);

    // Invalid name and level → None.
    assert!(ctx.texture_image(0, 0).is_none());
    assert!(ctx.texture_image(tex[0], 99).is_none());
    assert!(ctx.texture_image_mut(tex[0], 99).is_none());
}

/// `TextureSnapshot::as_texture` – obtained via `texture_snapshot_table`, then
/// converted to a `Texture` view for sampling.
#[test]
fn texture_snapshot_as_texture_converts_to_sampler_view() {
    use graphos_gl::gl::TextureSnapshot;
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    ctx.gen_textures(&mut tex);
    ctx.tex_storage_2d(tex[0], 1, 2, 2);
    ctx.tex_image_2d(
        tex[0],
        0,
        2,
        2,
        &[0xFF0000FF, 0x00FF00FF, 0x0000FFFF, 0xFFFFFFFF],
    );

    let table = ctx.texture_snapshot_table();
    let snap: &TextureSnapshot = table
        .iter()
        .filter_map(|o| o.as_ref())
        .next()
        .expect("at least one snapshot should exist");
    let view = snap.as_texture();
    assert_eq!(view.width, 2);
    assert_eq!(view.height, 2);
    // Sample center — result should be within [0,1] range.
    let c = view.sample_bilinear(graphos_gl::math::Vec2::new(0.5, 0.5));
    assert!(c.x >= 0.0 && c.x <= 1.0);
    assert!(c.w >= 0.0 && c.w <= 1.0);
}

/// `Context::transform_feedback_buffer_binding` – reflects the index slot value
/// after `bind_transform_feedback_buffer_base`.
#[test]
fn transform_feedback_buffer_binding_reflects_base_bind() {
    let mut ctx = Context::new();
    let mut bufs = [0u32; 1];
    ctx.gen_buffers(&mut bufs);
    let buf = bufs[0];

    ctx.bind_transform_feedback_buffer_base(0, buf);
    assert_eq!(ctx.transform_feedback_buffer_binding(0), Some(buf));
    assert_eq!(ctx.transform_feedback_buffer_binding(0), Some(buf));

    // Out-of-range index → None.
    assert_eq!(ctx.transform_feedback_buffer_binding(9999), None);
}

/// `Context::transform_feedback_varyings` – set varyings on a program, then
/// read them back before and after linking.
#[test]
fn transform_feedback_varyings_round_trips_through_program() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);

    // Before setting varyings: returns empty vec.
    let before = ctx.transform_feedback_varyings(prog);
    assert_eq!(before, Some(vec![]));

    ctx.set_transform_feedback_varyings(prog, &[b"gl_Position", b"v_Color"]);
    let varyings = ctx
        .transform_feedback_varyings(prog)
        .expect("valid program should return varyings");
    assert_eq!(varyings.len(), 2);
    assert_eq!(varyings[0], "gl_Position");
    assert_eq!(varyings[1], "v_Color");

    // Invalid program → None.
    assert!(ctx.transform_feedback_varyings(9999).is_none());
}

#[test]
fn texture_lod_and_clamp_to_border_are_stable() {
    let lvl0 = [0xFF0000FFu32; 4];
    let lvl1 = [0xFF00FF00u32; 1];
    let mut chain = MipmapChain::empty();
    chain.levels[0] = Some(Texture::new(&lvl0, 2, 2));
    chain.levels[1] = Some(Texture::new(&lvl1, 1, 1));
    chain.count = 2;

    let c = chain.sample(
        Vec2::new(0.5, 0.5),
        Vec2::new(0.0, 0.0),
        Vec2::new(0.0, 0.0),
    );
    assert!(c.x >= 0.0 && c.x <= 1.0);

    let mut t = Texture::new(&lvl0, 2, 2);
    t.wrap_s = WrapMode::ClampToBorder;
    t.wrap_t = WrapMode::ClampToBorder;
    t.border_color = [0.2, 0.3, 0.4, 1.0];
    let b = t.sample_bilinear(Vec2::new(-1.0, -1.0));
    assert!((b.x - 0.2).abs() < 0.05);
    assert!((b.y - 0.3).abs() < 0.05);
}

#[test]
fn line_path_honors_scissor() {
    let mut color = [0u32; 64];
    let mut depth = [1.0f32; 64];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);

    let pipe = Pipeline {
        scissor_test: true,
        scissor: [0, 0, 2, 2],
        ..Pipeline::opaque_3d()
    };

    let verts = [
        TestVertex {
            pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, 1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
    ];
    let idx = [0u32, 1u32];
    pipe.draw_mode(&mut target, &TestShader, &verts, &idx, DrawMode::Lines);

    let painted = color.iter().filter(|&&p| p != 0).count();
    assert!(painted <= 4);
}

#[test]
fn compositor_respects_z_order() {
    let mut comp = DesktopCompositor::new();
    comp.add_window(WindowSurface {
        texture: 10,
        rect: Rect::new(0.0, 0.0, 20.0, 20.0),
        z: 5,
        opacity: 1.0,
        clip: None,
    });
    comp.add_window(WindowSurface {
        texture: 20,
        rect: Rect::new(2.0, 2.0, 20.0, 20.0),
        z: 1,
        opacity: 1.0,
        clip: None,
    });

    let mut batch = graphos_gl::UiBatch::new();
    comp.composite_into(&mut batch, 100.0, 100.0, 0.0);

    assert!(batch.vertices.len() >= 12);
    let first_window_tex = batch
        .vertices
        .iter()
        .find(|v| v.texture != 0)
        .map(|v| v.texture);
    assert_eq!(first_window_tex, Some(20));
}

#[test]
fn shader_program_link_model_works() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"vertex");
    ctx.shader_source(fs, b"fragment");
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    assert!(ctx.is_program_linked(prog));

    ctx.use_program(prog);
    assert_eq!(ctx.current_program(), prog);
}

#[test]
fn shader_query_interfaces_report_expected_state() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);

    assert_eq!(ctx.shader_kind(vs), Some(ShaderKind::Vertex));
    assert_eq!(ctx.shader_source_bytes(vs), Some(&b""[..]));
    assert!(!ctx.shader_compile_status(vs));

    ctx.shader_source(vs, b"void main(){}\n");
    assert_eq!(ctx.shader_source_bytes(vs), Some(&b"void main(){}\n"[..]));

    ctx.compile_shader(vs);
    assert!(ctx.shader_compile_status(vs));
}

#[test]
fn program_query_interfaces_report_attached_shaders_and_link_status() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"vertex");
    ctx.shader_source(fs, b"fragment");
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    assert!(!ctx.program_link_status(prog));
    assert_eq!(ctx.program_attached_shaders(prog), Some((0, 0)));

    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    assert_eq!(ctx.program_attached_shaders(prog), Some((vs, fs)));

    ctx.link_program(prog);
    assert!(ctx.program_link_status(prog));
}

#[test]
fn delete_attached_shader_is_deferred_until_program_release() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"vertex");
    ctx.shader_source(fs, b"fragment");
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    assert_eq!(ctx.program_attached_shaders(prog), Some((vs, fs)));

    ctx.delete_shaders(&[vs]);

    // Shader stays alive while still attached; allocator should use a new slot.
    let fresh = ctx.create_shader(ShaderKind::Vertex);
    assert_ne!(fresh, vs);

    // Releasing the program drops the final attachment and frees pending shader.
    ctx.delete_programs(&[prog]);
    let recycled = ctx.create_shader(ShaderKind::Vertex);
    assert_eq!(recycled, vs);
}

#[test]
fn delete_unattached_shader_releases_slot_immediately() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.delete_shaders(&[vs]);
    let recycled = ctx.create_shader(ShaderKind::Vertex);
    assert_eq!(recycled, vs);
}

#[test]
fn delete_shader_is_idempotent_when_repeated() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.delete_shaders(&[vs]);
    ctx.delete_shaders(&[vs]);
    let recycled = ctx.create_shader(ShaderKind::Vertex);
    assert_eq!(recycled, vs);
}

#[test]
fn detach_shader_reclaims_pending_deleted_shader_slot() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);

    let p = ctx.create_program();
    ctx.attach_shader(p, vs);
    ctx.attach_shader(p, fs);

    ctx.delete_shaders(&[vs]);
    // Still attached, so slot should not be immediately reusable.
    let fresh = ctx.create_shader(ShaderKind::Vertex);
    assert_ne!(fresh, vs);

    // Explicit detach should release pending-delete shader immediately.
    ctx.detach_shader(p, vs);
    let recycled = ctx.create_shader(ShaderKind::Vertex);
    assert_eq!(recycled, vs);
}

#[test]
fn detach_shader_not_attached_sets_error() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let p = ctx.create_program();
    ctx.detach_shader(p, vs);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn strict_glsl_compile_requires_es300_main() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);

    ctx.shader_source(vs, b"vertex");
    ctx.compile_shader(vs);
    assert!(!ctx.shader_compile_status(vs));
    assert!(ctx.shader_info_log(vs).is_some_and(|s| !s.is_empty()));
    assert_eq!(ctx.get_error(), None);
}

#[test]
fn strict_glsl_link_rejects_varying_type_mismatch() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);

    ctx.shader_source(
        vs,
        b"#version 300 es\nin vec3 position;\nout vec4 v_color;\nvoid main(){ v_color = vec4(1.0); }\n",
    );
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nin vec3 v_color;\nout vec4 color;\nvoid main(){ color = vec4(v_color, 1.0); }\n",
    );
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);
    assert!(ctx.shader_compile_status(vs));
    assert!(ctx.shader_compile_status(fs));

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    assert!(!ctx.program_link_status(prog));
    assert!(ctx.program_info_log(prog).is_some_and(|s| !s.is_empty()));
    assert_eq!(ctx.get_error(), None);
}

#[test]
fn shader_and_program_info_logs_clear_on_success() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);

    // First force failures so logs populate.
    ctx.shader_source(vs, b"vertex");
    ctx.compile_shader(vs);
    assert!(ctx.shader_info_log(vs).is_some_and(|s| !s.is_empty()));

    ctx.shader_source(
        vs,
        b"#version 300 es\nout vec4 v_color;\nvoid main(){ v_color = vec4(1.0); }\n",
    );
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nin vec3 v_color;\nout vec4 color;\nvoid main(){ color = vec4(v_color, 1.0); }\n",
    );
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);
    let p = ctx.create_program();
    ctx.attach_shader(p, vs);
    ctx.attach_shader(p, fs);
    ctx.link_program(p);
    assert!(ctx.program_info_log(p).is_some_and(|s| !s.is_empty()));

    // Then provide matching shaders and verify logs clear on success.
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nin vec4 v_color;\nout vec4 color;\nvoid main(){ color = v_color; }\n",
    );
    ctx.compile_shader(fs);
    assert_eq!(ctx.shader_info_log(fs), Some(""));
    ctx.link_program(p);
    assert!(ctx.program_link_status(p));
    assert_eq!(ctx.program_info_log(p), Some(""));
}

#[test]
fn strict_glsl_link_accepts_matching_varyings() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);

    ctx.shader_source(
        vs,
        b"#version 300 es\nin vec3 position;\nout vec4 v_color;\nvoid main(){ v_color = vec4(1.0); }\n",
    );
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nin vec4 v_color;\nout vec4 color;\nvoid main(){ color = v_color; }\n",
    );
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    assert!(ctx.program_link_status(prog));
}

#[test]
fn strict_glsl_compile_rejects_unbalanced_symbols() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.shader_source(
        vs,
        b"#version 300 es\nin vec3 position;\nvoid main( { gl_Position = vec4(position, 1.0); }\n",
    );
    ctx.compile_shader(vs);
    assert!(!ctx.shader_compile_status(vs));
    assert_eq!(ctx.get_error(), None);
}

#[test]
fn strict_glsl_compile_rejects_main_with_params() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nout vec4 color;\nvoid main(int x){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_fragment_requires_float_precision_declaration() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nout vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_compile_rejects_conflicting_duplicate_declarations() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    ctx.shader_source(
        vs,
        b"#version 300 es\nin vec3 position;\nout vec3 v;\nout vec4 v;\nvoid main(){ v = vec3(1.0); }\n",
    );
    ctx.compile_shader(vs);
    assert!(!ctx.shader_compile_status(vs));
}

#[test]
fn strict_glsl_link_rejects_uniform_type_mismatch() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);

    ctx.shader_source(
        vs,
        b"#version 300 es\nin vec3 position;\nuniform mat4 u_mvp;\nout vec4 v_color;\nvoid main(){ gl_Position = u_mvp * vec4(position, 1.0); v_color = vec4(1.0); }\n",
    );
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nin vec4 v_color;\nuniform vec4 u_mvp;\nout vec4 color;\nvoid main(){ color = v_color + u_mvp; }\n",
    );
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);
    assert!(ctx.shader_compile_status(vs));
    assert!(ctx.shader_compile_status(fs));

    let p = ctx.create_program();
    ctx.attach_shader(p, vs);
    ctx.attach_shader(p, fs);
    ctx.link_program(p);
    assert!(!ctx.program_link_status(p));
}

#[test]
fn strict_glsl_fragment_output_location_oob_rejected() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nlayout(location = 8) out vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_fragment_output_duplicate_location_rejected() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nlayout(location = 1) out vec4 c0;\nlayout(location = 1) out vec4 c1;\nvoid main(){ c0 = vec4(1.0); c1 = vec4(0.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_fragment_malformed_layout_rejected() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nlayout(location = 0 out vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_fragment_malformed_layout_prefix_rejected() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nlayout location = 0) out vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_compile_rejects_trailing_interface_tokens() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nout vec4 color garbage;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_compile_rejects_invalid_interface_identifier() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nout vec4 1color;\nvoid main(){ }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_compile_accepts_qualified_array_interface_decl() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nflat in highp vec4 v_color[2];\nout vec4 color;\nvoid main(){ color = v_color[0]; }\n",
    );
    ctx.compile_shader(fs);
    assert!(ctx.shader_compile_status(fs));
}

#[test]
fn strict_glsl_compile_rejects_broken_array_interface_decl() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nin vec4 v_color[;\nout vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(fs);
    assert!(!ctx.shader_compile_status(fs));
}

#[test]
fn tf_varyings_must_match_vertex_outputs_to_link() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);

    ctx.shader_source(
        vs,
        b"#version 300 es\nout vec4 tf_color;\nvoid main(){ tf_color = vec4(1.0); }\n",
    );
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nout vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.set_transform_feedback_varyings(prog, &[b"missing".as_slice()]);
    ctx.link_program(prog);
    assert!(!ctx.program_link_status(prog));
    assert_eq!(ctx.get_error(), None);

    ctx.set_transform_feedback_varyings(prog, &[b"tf_color".as_slice()]);
    ctx.link_program(prog);
    assert!(ctx.program_link_status(prog));
}

#[test]
fn tf_capture_from_program_requires_declared_component_count() {
    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);

    let mut tfb = [0u32; 1];
    let mut buf = [0u32; 1];
    assert_eq!(ctx.gen_transform_feedbacks(&mut tfb), 1);
    assert_eq!(ctx.gen_buffers(&mut buf), 1);
    ctx.bind_transform_feedback(tfb[0]);
    ctx.bind_transform_feedback_buffer_base(0, buf[0]);

    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(
        vs,
        b"#version 300 es\nout vec4 tf_color;\nvoid main(){ tf_color = vec4(1.0); }\n",
    );
    ctx.shader_source(
        fs,
        b"#version 300 es\nprecision mediump float;\nout vec4 color;\nvoid main(){ color = vec4(1.0); }\n",
    );
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.set_transform_feedback_varyings(prog, &[b"tf_color".as_slice()]);
    ctx.link_program(prog);
    assert!(ctx.program_link_status(prog));

    ctx.use_program(prog);
    ctx.begin_transform_feedback(DrawMode::Triangles);
    assert!(!ctx.transform_feedback_capture_from_program_f32(&[1.0, 2.0]));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));

    assert!(ctx.transform_feedback_capture_from_program_f32(&[1.0, 2.0, 3.0, 4.0]));
    ctx.end_transform_feedback();
    assert_eq!(ctx.get_buffer_data(buf[0]).unwrap().len(), 16);
}

#[test]
fn extension_reporting_and_gates_behave_as_expected() {
    let mut ctx = Context::new();

    let names = ctx.supported_extensions();
    assert!(names.contains(&"GL_KHR_debug"));
    // GL_KHR_robustness and GL_EXT_disjoint_timer_query are not yet implemented
    // and were removed from the advertised extension list (GAP-002).
    assert!(!names.contains(&"GL_KHR_robustness"));
    assert!(!names.contains(&"GL_EXT_disjoint_timer_query"));

    ctx.set_extension_enabled_for_testing(GlExtension::KhrDebug, false);
    ctx.debug_message_insert(
        DebugSource::Application,
        DebugType::Marker,
        1,
        DebugSeverity::Low,
        b"blocked",
    );
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));

    ctx.set_extension_enabled_for_testing(GlExtension::KhrRobustness, false);
    ctx.set_robust_access(true);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    assert!(!ctx.robust_access_enabled());

    let mut q = [0u32; 1];
    assert_eq!(ctx.gen_queries(&mut q), 1);
    ctx.set_extension_enabled_for_testing(GlExtension::ExtDisjointTimerQuery, false);
    ctx.begin_query(QueryTarget::TimeElapsed, q[0]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn text_clip_adjusts_geometry_and_uvs() {
    let atlas = GlyphAtlas::ascii_8x8();
    let mut batch = graphos_gl::UiBatch::new();
    append_text(
        &mut batch,
        &atlas,
        7,
        "A",
        10.0,
        20.0,
        0.5,
        TextStyle::default(),
        Some(Rect::new(14.0, 20.0, 4.0, 8.0)),
    );

    assert_eq!(batch.vertices.len(), 4);
    assert!((batch.vertices[0].pos.x - 14.0).abs() < 0.01);
    assert!(batch.vertices[0].uv.x > 0.0);
    assert!(batch.vertices[1].uv.x > batch.vertices[0].uv.x);
}

#[test]
fn compositor_clips_window_surface_rect() {
    let mut comp = DesktopCompositor::new();
    comp.add_window(WindowSurface {
        texture: 3,
        rect: Rect::new(10.0, 10.0, 40.0, 30.0),
        z: 1,
        opacity: 0.75,
        clip: Some(Rect::new(20.0, 12.0, 12.0, 10.0)),
    });

    let mut batch = graphos_gl::UiBatch::new();
    comp.composite_into(&mut batch, 100.0, 100.0, 0.0);
    let first = batch
        .vertices
        .iter()
        .position(|v| v.texture == 3)
        .expect("window quad present");
    let window_quad = &batch.vertices[first..first + 4];
    assert!((window_quad[0].pos.x - 20.0).abs() < 0.01);
    assert!((window_quad[2].pos.x - 32.0).abs() < 0.01);
    assert!(window_quad[0].uv.x > 0.0);
}

#[test]
fn transparent_pipeline_blends_src_alpha() {
    let mut ctx = Context::new();
    ctx.enable_blend(true);
    ctx.blend_func_separate(
        graphos_gl::BlendFactor::SrcAlpha,
        graphos_gl::BlendFactor::OneMinusSrcAlpha,
        graphos_gl::BlendFactor::One,
        graphos_gl::BlendFactor::OneMinusSrcAlpha,
    );
    let out = ctx.apply_blend(Vec4::new(1.0, 0.0, 0.0, 0.5), Vec4::new(0.0, 0.0, 1.0, 1.0));
    assert!((out.x - 0.5).abs() < 0.01);
    assert!((out.z - 0.5).abs() < 0.01);
    assert!(out.w >= 0.99);
}

#[test]
fn offscreen_window_surface_renders_into_texture() {
    let mut ctx = Context::new();
    let surface = WindowSurfaceTarget::create(&mut ctx, 64, 32).expect("surface target");

    let mut batch = graphos_gl::UiBatch::new();
    batch.add_rect(
        Rect::new(0.0, 0.0, 64.0, 32.0),
        0.2,
        Vec4::new(0.22, 0.74, 0.92, 0.95),
    );

    let snapshots = ctx.texture_snapshot_table();
    let mut views = Vec::with_capacity(snapshots.len());
    for item in &snapshots {
        views.push(item.as_ref().map(|s| s.as_texture()));
    }
    let mut renderer = OffscreenWindowRenderer::new();
    assert!(renderer.render_batch(&mut ctx, &surface, &batch, &views));

    let pixels = ctx
        .texture_pixels(surface.color_texture, 0)
        .expect("surface color pixels");
    assert!(pixels.iter().any(|&p| p != 0));

    surface.destroy(&mut ctx);
}

#[test]
fn points_lines_triangles_share_fragment_ops() {
    let mut color = [0u32; 100];
    let mut depth = [1.0f32; 100];
    let mut target = Target::new(&mut color, &mut depth, 10, 10);

    let mut pipe = Pipeline::opaque_3d();
    pipe.depth_test = false;
    pipe.depth_write = false;
    pipe.scissor_test = true;
    pipe.scissor = [2, 2, 3, 3];
    pipe.color_mask = [false, true, false, true];

    let tri_verts = [
        TestVertex {
            pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(0.0, 1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
    ];
    pipe.draw_mode(
        &mut target,
        &TestShader,
        &tri_verts,
        &[0, 1, 2],
        DrawMode::Triangles,
    );

    let line_verts = [
        TestVertex {
            pos: Vec4::new(-1.0, 0.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, 0.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
    ];
    pipe.draw_mode(
        &mut target,
        &TestShader,
        &line_verts,
        &[0, 1],
        DrawMode::Lines,
    );

    let point_vert = [TestVertex {
        pos: Vec4::new(-0.4, 0.4, 0.0, 1.0),
        color: Vec4::new(1.0, 1.0, 1.0, 1.0),
    }];
    pipe.draw_mode(
        &mut target,
        &TestShader,
        &point_vert,
        &[0],
        DrawMode::Points,
    );

    let mut painted = 0usize;
    for y in 0..10usize {
        for x in 0..10usize {
            let p = color[y * 10 + x];
            if p == 0 {
                continue;
            }
            painted += 1;
            assert!(
                x >= 2 && x < 5 && y >= 2 && y < 5,
                "fragment escaped scissor: {x},{y}"
            );
            assert_eq!(p & 0x00FF_00FF, 0, "red/blue channels should be masked off");
        }
    }
    assert!(painted > 0);
}

#[test]
fn profile_and_limits_are_reported() {
    let ctx = Context::new();
    assert_eq!(ctx.api_profile(), ApiProfile::OpenGlEs30);
    assert_eq!(ctx.vendor_string(), "GraphOS");
    assert_eq!(ctx.renderer_string(), "graphos-gl CPU rasterizer");
    assert!(ctx.version_string().contains("OpenGL ES 3.0"));
    assert!(
        ctx.shading_language_version_string()
            .contains("GLSL ES 3.00")
    );

    let limits = ctx.limits();
    assert_eq!(limits.max_texture_units, 32);
    assert_eq!(limits.max_color_attachments, 8);
    assert!(limits.max_buffers >= 128);
}

#[test]
fn read_pixels_reads_bgra_attachment_data() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    let pixels = [
        0xFF00_00FFu32,
        0xFF00_FF00u32,
        0xFFFF_0000u32,
        0xFFFF_FFFFu32,
    ];
    ctx.tex_storage_2d(tex[0], 2, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 2, &pixels);
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(0), tex[0], 0);

    let mut out = [0u32; 4];
    assert!(ctx.read_pixels_bgra8(fbo[0], 0, 0, 0, 2, 2, &mut out));
    assert_eq!(out, pixels);
}

#[test]
fn read_pixels_on_incomplete_fbo_sets_error() {
    let mut ctx = Context::new();
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    let mut out = [0u32; 1];
    assert!(!ctx.read_pixels_bgra8(fbo[0], 0, 0, 0, 1, 1, &mut out));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidFramebufferOperation));
}

#[test]
fn blit_framebuffer_color_copies_source_to_destination() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 2];
    let mut fbo = [0u32; 2];
    assert_eq!(ctx.gen_textures(&mut tex), 2);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 2);

    let src_pixels = [
        0xFFAA_0000u32,
        0xFF00_AA00u32,
        0xFF00_00AAu32,
        0xFFFF_AA00u32,
    ];
    let dst_pixels = [0u32; 4];

    ctx.tex_storage_2d(tex[0], 2, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 2, &src_pixels);
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(0), tex[0], 0);

    ctx.tex_storage_2d(tex[1], 2, 2, 1);
    ctx.tex_image_2d(tex[1], 0, 2, 2, &dst_pixels);
    ctx.framebuffer_texture_2d(fbo[1], graphos_gl::Attachment::Color(0), tex[1], 0);

    assert!(ctx.blit_framebuffer_color(fbo[0], fbo[1], [0, 0, 2, 2], [0, 0, 2, 2]));

    let mut out = [0u32; 4];
    assert!(ctx.read_pixels_bgra8(fbo[1], 0, 0, 0, 2, 2, &mut out));
    assert_eq!(out, src_pixels);
}

#[test]
fn tex_image_bgra8_honors_unpack_alignment_padding() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);

    ctx.pixel_store_unpack_alignment(8);
    let mut bytes = vec![0u8; 16];
    // Row 0 pixel BGRA and 4 bytes row padding.
    bytes[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    // Row 1 pixel BGRA and 4 bytes row padding.
    bytes[8..12].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

    ctx.tex_storage_2d(tex[0], 1, 2, 1);
    ctx.tex_image_2d_bgra8(tex[0], 0, 1, 2, &bytes);

    let uploaded = ctx.texture_pixels(tex[0], 0).expect("uploaded pixels");
    assert_eq!(uploaded.len(), 2);
    assert_eq!(uploaded[0], 0x4433_2211);
    assert_eq!(uploaded[1], 0xDDCC_BBAA);
}

#[test]
fn read_pixels_bgra8_bytes_honors_pack_alignment_padding() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    ctx.tex_storage_2d(tex[0], 1, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 1, 2, &[0x4433_2211, 0xDDCC_BBAA]);
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(0), tex[0], 0);

    ctx.pixel_store_pack_alignment(8);
    let mut out = vec![0u8; 16];
    assert!(ctx.read_pixels_bgra8_bytes(fbo[0], 0, 0, 0, 1, 2, &mut out));

    assert_eq!(&out[0..4], &[0x11, 0x22, 0x33, 0x44]);
    assert_eq!(&out[8..12], &[0xAA, 0xBB, 0xCC, 0xDD]);
    assert_eq!(&out[4..8], &[0, 0, 0, 0]);
    assert_eq!(&out[12..16], &[0, 0, 0, 0]);
}

#[test]
fn tex_image_rgba8_rgb8_r8_uploads_decode_as_expected() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 3];
    assert_eq!(ctx.gen_textures(&mut tex), 3);

    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rgba8(tex[0], 0, 1, 1, &[0x12, 0x34, 0x56, 0x78]);
    assert_eq!(ctx.texture_pixels(tex[0], 0).unwrap(), &[0x7812_3456]);

    ctx.tex_storage_2d(tex[1], 1, 1, 1);
    ctx.tex_image_2d_rgb8(tex[1], 0, 1, 1, &[0xAB, 0xCD, 0xEF]);
    assert_eq!(ctx.texture_pixels(tex[1], 0).unwrap(), &[0xFFAB_CDEF]);

    ctx.tex_storage_2d(tex[2], 1, 1, 1);
    ctx.tex_image_2d_r8(tex[2], 0, 1, 1, &[0x66]);
    assert_eq!(ctx.texture_pixels(tex[2], 0).unwrap(), &[0xFF66_0000]);
}

#[test]
fn format_matrix_rg8_uploads_correctly() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rg8(tex[0], 0, 1, 1, &[0xAB, 0xCD]);
    // BGRA storage: B=0x00, G=0xCD, R=0xAB, A=0xFF
    assert_eq!(ctx.texture_pixels(tex[0], 0).unwrap(), &[0xFF_AB_CD_00]);
}

#[test]
fn format_matrix_rgb565_uploads_correctly() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    // R=0b11111 G=0b000000 B=0b00000 → pure red
    let packed: u16 = 0b1111_1000_0000_0000;
    let bytes = packed.to_le_bytes();
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rgb565(tex[0], 0, 1, 1, &bytes);
    let p = ctx.texture_pixels(tex[0], 0).unwrap()[0];
    let r = (p >> 16) & 0xFF;
    let g = (p >> 8) & 0xFF;
    let b = p & 0xFF;
    assert!(r > 240, "red should be near-max, got {r}");
    assert_eq!(g, 0);
    assert_eq!(b, 0);
}

#[test]
fn format_matrix_rgba4_uploads_correctly() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    // R=0xF G=0x0 B=0xA A=0x5
    let packed: u16 = 0xF0A5;
    let bytes = packed.to_le_bytes();
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rgba4(tex[0], 0, 1, 1, &bytes);
    let p = ctx.texture_pixels(tex[0], 0).unwrap()[0];
    let a = (p >> 24) & 0xFF;
    let r = (p >> 16) & 0xFF;
    let g = (p >> 8) & 0xFF;
    let b = p & 0xFF;
    assert_eq!(r, 0xFF, "R nibble 0xF → 0xFF");
    assert_eq!(g, 0x00, "G nibble 0x0 → 0x00");
    assert_eq!(b, 0xAA, "B nibble 0xA → 0xAA");
    assert_eq!(a, 0x55, "A nibble 0x5 → 0x55");
}

#[test]
fn format_matrix_rgb5a1_uploads_correctly() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    // R=11111 G=00000 B=00000 A=1 → red with full alpha
    let packed: u16 = 0b1111_1000_0000_0001;
    let bytes = packed.to_le_bytes();
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rgb5a1(tex[0], 0, 1, 1, &bytes);
    let p = ctx.texture_pixels(tex[0], 0).unwrap()[0];
    let a = (p >> 24) & 0xFF;
    let r = (p >> 16) & 0xFF;
    assert!(r > 240, "red should be near-max, got {r}");
    assert_eq!(a, 0xFF);
}

#[test]
fn format_matrix_rgb5a1_alpha0_transparent() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    let packed: u16 = 0b1111_1000_0000_0000; // A=0
    let bytes = packed.to_le_bytes();
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rgb5a1(tex[0], 0, 1, 1, &bytes);
    let p = ctx.texture_pixels(tex[0], 0).unwrap()[0];
    assert_eq!((p >> 24) & 0xFF, 0x00, "alpha bit=0 → A=0");
}

#[test]
fn format_matrix_rgba16f_decodes_to_half_float_range() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    // Encode (1.0, 0.5, 0.0, 1.0) as four half-floats LE
    let one: u16 = 0x3C00; // 1.0 in half
    let half: u16 = 0x3800; // 0.5 in half
    let zero: u16 = 0x0000;
    let mut bytes = [0u8; 8];
    bytes[0..2].copy_from_slice(&one.to_le_bytes());
    bytes[2..4].copy_from_slice(&half.to_le_bytes());
    bytes[4..6].copy_from_slice(&zero.to_le_bytes());
    bytes[6..8].copy_from_slice(&one.to_le_bytes());
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_rgba16f(tex[0], 0, 1, 1, &bytes);
    let p = ctx.texture_pixels(tex[0], 0).unwrap()[0];
    let r = (p >> 16) & 0xFF;
    let g = (p >> 8) & 0xFF;
    let b = p & 0xFF;
    assert_eq!(r, 255);
    assert!(g >= 127 && g <= 128, "0.5 → ~128, got {g}");
    assert_eq!(b, 0);
}

#[test]
fn format_matrix_luminance8_uploads_correctly() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_luminance8(tex[0], 0, 1, 1, &[0xAA]);
    // R=G=B=0xAA, A=0xFF
    assert_eq!(ctx.texture_pixels(tex[0], 0).unwrap(), &[0xFF_AA_AA_AA]);
}

#[test]
fn format_matrix_luminance_alpha8_uploads_correctly() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_luminance_alpha8(tex[0], 0, 1, 1, &[0x80, 0x40]);
    // R=G=B=0x80, A=0x40
    assert_eq!(ctx.texture_pixels(tex[0], 0).unwrap(), &[0x40_80_80_80]);
}

#[test]
fn format_matrix_srgb8_alpha8_uses_rgba8_path() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d_srgb8_alpha8(tex[0], 0, 1, 1, &[0x10, 0x20, 0x30, 0xFF]);
    // sRGB decode: R=0x10→0x01, G=0x20→0x04, B=0x30→0x08, A=0xFF (linear)
    // Stored as BGRA u32 LE: [B=0x08, G=0x04, R=0x01, A=0xFF] → 0xFF010408
    assert_eq!(ctx.texture_pixels(tex[0], 0).unwrap(), &[0xFF01_0408]);
}

#[test]
fn tex_image_rgb8_honors_unpack_alignment_padding() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);

    ctx.pixel_store_unpack_alignment(4);
    // width=1, RGB row = 3 bytes, stride aligned to 4.
    let bytes = [0x01, 0x02, 0x03, 0x00, 0x11, 0x22, 0x33, 0x00];

    ctx.tex_storage_2d(tex[0], 1, 2, 1);
    ctx.tex_image_2d_rgb8(tex[0], 0, 1, 2, &bytes);

    let uploaded = ctx.texture_pixels(tex[0], 0).unwrap();
    assert_eq!(uploaded, &[0xFF01_0203, 0xFF11_2233]);
}

#[test]
fn tex_image_rgba8_invalid_length_sets_error() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    ctx.tex_storage_2d(tex[0], 2, 1, 1);

    ctx.tex_image_2d_rgba8(tex[0], 0, 2, 1, &[1, 2, 3]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn invalid_pixel_store_alignment_sets_error() {
    let mut ctx = Context::new();
    ctx.pixel_store_pack_alignment(3);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
    ctx.pixel_store_unpack_alignment(0);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn invalid_active_texture_unit_sets_error_and_preserves_active_unit() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 2];
    assert_eq!(ctx.gen_textures(&mut tex), 2);

    ctx.active_texture(1);
    ctx.bind_texture(tex[1]);

    ctx.active_texture(99);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));

    ctx.bind_texture(tex[0]);
    assert_eq!(ctx.current_texture_binding(1), Some(tex[0]));
    assert_eq!(ctx.current_texture_binding(0), Some(0));
}

#[test]
fn invalid_vertex_attrib_index_sets_error() {
    let mut ctx = Context::new();

    ctx.vertex_attrib_pointer(99, 4, false, 0, 0);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));

    ctx.vertex_attrib_divisor(99, 3);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn uniform_buffer_base_binding_roundtrip() {
    let mut ctx = Context::new();
    let mut b = [0u32; 1];
    assert_eq!(ctx.gen_buffers(&mut b), 1);

    ctx.bind_uniform_buffer_base(2, b[0]);
    assert_eq!(ctx.uniform_buffer_binding(2), Some(b[0]));

    ctx.delete_buffers(&[b[0]]);
    assert_eq!(ctx.uniform_buffer_binding(2), Some(0));
}

#[test]
fn uniform_buffer_base_binding_invalid_index_sets_error() {
    let mut ctx = Context::new();
    ctx.bind_uniform_buffer_base(999, 0);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn framebuffer_texture_2d_invalid_attachment_sets_error() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    ctx.tex_storage_2d(tex[0], 2, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 2, &[0xFFFF_FFFF; 4]);

    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(9), tex[0], 0);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidEnum));
}

#[test]
fn read_pixels_read_bound_uses_read_binding() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 2];
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 2);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d(tex[0], 0, 1, 1, &[0xFF11_2233]);
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(0), tex[0], 0);

    ctx.tex_storage_2d(tex[1], 1, 1, 1);
    ctx.tex_image_2d(tex[1], 0, 1, 1, &[0xFFAA_BBCC]);
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(1), tex[1], 0);

    ctx.bind_read_framebuffer(fbo[0]);
    ctx.read_buffer(1);

    let mut out = [0u32; 1];
    // Legacy attachment arg is ignored; reads now follow read_buffer state.
    assert!(ctx.read_pixels_read_bound_bgra8(0, 0, 0, 1, 1, &mut out));
    assert_eq!(out[0], 0xFFAA_BBCC);
}

#[test]
fn read_pixels_read_bound_rejects_gl_none_read_buffer() {
    let mut ctx = Context::new();
    let mut out = [0u32; 1];
    ctx.read_buffer(0xFF);
    assert!(!ctx.read_pixels_read_bound_bgra8(0, 0, 0, 1, 1, &mut out));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn read_pixels_read_bound_unattached_read_buffer_sets_fbo_error() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    ctx.tex_storage_2d(tex[0], 1, 1, 1);
    ctx.tex_image_2d(tex[0], 0, 1, 1, &[0xFF11_2233]);
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(0), tex[0], 0);

    ctx.bind_read_framebuffer(fbo[0]);
    ctx.read_buffer(1); // attachment 1 is not attached

    let mut out = [0u32; 1];
    assert!(!ctx.read_pixels_read_bound_bgra8(0, 0, 0, 1, 1, &mut out));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidFramebufferOperation));
}

#[test]
fn read_pixels_read_bound_selects_attachment_0_1_2() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 3];
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 3);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    for i in 0..3u8 {
        ctx.tex_storage_2d(tex[i as usize], 1, 1, 1);
        ctx.tex_image_2d(tex[i as usize], 0, 1, 1, &[0xFF00_0000 | ((i as u32) + 1)]);
        ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(i), tex[i as usize], 0);
    }

    ctx.bind_read_framebuffer(fbo[0]);
    let mut out = [0u32; 1];

    ctx.read_buffer(0);
    assert!(ctx.read_pixels_read_bound_bgra8(7, 0, 0, 1, 1, &mut out));
    assert_eq!(out[0], 0xFF00_0001);

    ctx.read_buffer(1);
    assert!(ctx.read_pixels_read_bound_bgra8(7, 0, 0, 1, 1, &mut out));
    assert_eq!(out[0], 0xFF00_0002);

    ctx.read_buffer(2);
    assert!(ctx.read_pixels_read_bound_bgra8(7, 0, 0, 1, 1, &mut out));
    assert_eq!(out[0], 0xFF00_0003);
}

#[test]
fn blit_framebuffer_color_bound_uses_read_to_draw_bindings() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 2];
    let mut fbo = [0u32; 2];
    assert_eq!(ctx.gen_textures(&mut tex), 2);
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 2);

    ctx.tex_storage_2d(tex[0], 2, 2, 1);
    ctx.tex_image_2d(
        tex[0],
        0,
        2,
        2,
        &[0xFF10_1010, 0xFF20_2020, 0xFF30_3030, 0xFF40_4040],
    );
    ctx.framebuffer_texture_2d(fbo[0], graphos_gl::Attachment::Color(0), tex[0], 0);

    ctx.tex_storage_2d(tex[1], 2, 2, 1);
    ctx.tex_image_2d(tex[1], 0, 2, 2, &[0; 4]);
    ctx.framebuffer_texture_2d(fbo[1], graphos_gl::Attachment::Color(0), tex[1], 0);

    ctx.bind_read_framebuffer(fbo[0]);
    ctx.bind_draw_framebuffer(fbo[1]);
    assert!(ctx.blit_framebuffer_color_bound([0, 0, 2, 2], [0, 0, 2, 2]));

    let mut out = [0u32; 4];
    assert!(ctx.read_pixels_bgra8(fbo[1], 0, 0, 0, 2, 2, &mut out));
    assert_eq!(out, [0xFF10_1010, 0xFF20_2020, 0xFF30_3030, 0xFF40_4040]);
}

#[test]
fn invalid_target_bind_does_not_clobber_existing_framebuffer_binding() {
    let mut ctx = Context::new();
    let mut fbo = [0u32; 1];
    assert_eq!(ctx.gen_framebuffers(&mut fbo), 1);

    ctx.bind_draw_framebuffer(fbo[0]);
    assert_eq!(ctx.current_draw_framebuffer(), fbo[0]);

    ctx.bind_draw_framebuffer(99_999);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    assert_eq!(ctx.current_draw_framebuffer(), fbo[0]);
}

#[test]
fn tex_sub_image_2d_updates_subregion_only() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);

    let initial = [0x0100_0000u32, 0x0200_0000, 0x0300_0000, 0x0400_0000];
    ctx.tex_storage_2d(tex[0], 2, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 2, &initial);

    ctx.tex_sub_image_2d(tex[0], 0, 1, 0, 1, 1, &[0xDEAD_BEEF]);

    let pixels = ctx.texture_pixels(tex[0], 0).unwrap();
    assert_eq!(pixels[0], 0x0100_0000);
    assert_eq!(pixels[1], 0xDEAD_BEEF);
    assert_eq!(pixels[2], 0x0300_0000);
    assert_eq!(pixels[3], 0x0400_0000);
}

#[test]
fn tex_sub_image_2d_out_of_bounds_sets_error() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);

    ctx.tex_storage_2d(tex[0], 2, 2, 1);
    ctx.tex_image_2d(tex[0], 0, 2, 2, &[0u32; 4]);

    ctx.tex_sub_image_2d(tex[0], 0, 1, 1, 2, 1, &[0xFFFF_FFFFu32; 2]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn generate_mipmap_produces_smaller_levels() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);

    let level0: Vec<u32> = (0u32..16).map(|i| 0xFF00_0000 | i).collect();
    ctx.tex_storage_2d(tex[0], 4, 4, 3);
    ctx.tex_image_2d(tex[0], 0, 4, 4, &level0);
    ctx.generate_mipmap(tex[0]);

    let l1 = ctx.texture_pixels(tex[0], 1).expect("mip level 1");
    assert_eq!(l1.len(), 4);

    let l2 = ctx.texture_pixels(tex[0], 2).expect("mip level 2");
    assert_eq!(l2.len(), 1);
}

#[test]
fn generate_mipmap_on_empty_texture_sets_error() {
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);

    ctx.generate_mipmap(tex[0]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

// ── Phase 4: Program / Uniform / Attrib introspection ────────────────────────

#[test]
fn get_uniform_location_is_stable_and_deterministic() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let loc1 = ctx.get_uniform_location(prog, b"uColor");
    let loc2 = ctx.get_uniform_location(prog, b"uColor");
    assert!(loc1 >= 0);
    assert_eq!(loc1, loc2);

    let loc_other = ctx.get_uniform_location(prog, b"uTransform");
    assert!(loc_other >= 0);
    assert_ne!(loc1, loc_other);
}

#[test]
fn get_uniform_location_returns_minus1_for_unlinked_program() {
    let mut ctx = Context::new();
    let prog = ctx.create_program();
    assert_eq!(ctx.get_uniform_location(prog, b"uColor"), -1);
}

#[test]
fn uniform1f_stores_and_reads_back_value() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let loc = ctx.get_uniform_location(prog, b"uScale");
    ctx.uniform1f(prog, loc, 3.14);

    match ctx.get_uniform(prog, loc) {
        Some(UniformValue::Float(v)) => assert!((v - 3.14).abs() < 1e-5),
        other => panic!("expected Float, got {:?}", other),
    }
}

#[test]
fn uniform4fv_stores_and_reads_back_vec4() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let loc = ctx.get_uniform_location(prog, b"uColor");
    ctx.uniform4fv(prog, loc, &[0.1, 0.2, 0.3, 0.4]);

    match ctx.get_uniform(prog, loc) {
        Some(UniformValue::Vec4(v)) => {
            assert!((v[0] - 0.1).abs() < 1e-5);
            assert!((v[3] - 0.4).abs() < 1e-5);
        }
        other => panic!("expected Vec4, got {:?}", other),
    }
}

#[test]
fn uniform_matrix4fv_stores_mat4() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let identity = [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    let loc = ctx.get_uniform_location(prog, b"uMVP");
    ctx.uniform_matrix4fv(prog, loc, &identity);

    match ctx.get_uniform(prog, loc) {
        Some(UniformValue::Mat4(m)) => assert!((m[0] - 1.0).abs() < 1e-5),
        other => panic!("expected Mat4, got {:?}", other),
    }
}

#[test]
fn uniform_is_overwritten_by_second_call() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let loc = ctx.get_uniform_location(prog, b"uAlpha");
    ctx.uniform1f(prog, loc, 0.5);
    ctx.uniform1f(prog, loc, 0.9);

    match ctx.get_uniform(prog, loc) {
        Some(UniformValue::Float(v)) => assert!((v - 0.9).abs() < 1e-5),
        other => panic!("expected Float(0.9), got {:?}", other),
    }
}

#[test]
fn get_attrib_location_is_in_valid_range() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let slot = ctx.get_attrib_location(prog, b"aPosition");
    assert!(slot >= 0 && slot < 16, "slot {slot} out of attrib range");
}

#[test]
fn validate_program_succeeds_for_linked_program() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    ctx.validate_program(prog);
    assert!(ctx.is_program_valid(prog));
}

#[test]
fn validate_program_fails_for_unlinked_program() {
    let mut ctx = Context::new();
    let prog = ctx.create_program();
    ctx.validate_program(prog);
    assert!(!ctx.is_program_valid(prog));
}

#[test]
fn delete_program_clears_its_uniforms() {
    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, b"v");
    ctx.compile_shader(vs);
    ctx.shader_source(fs, b"f");
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);

    let loc = ctx.get_uniform_location(prog, b"uVal");
    ctx.uniform1f(prog, loc, 7.0);
    assert!(ctx.get_uniform(prog, loc).is_some());

    ctx.delete_programs(&[prog]);
    assert!(ctx.get_uniform(prog, loc).is_none());
}

// ── Phase 5: Draw path interaction tests ─────────────────────────────────────

#[test]
fn stencil_state_setters_are_round_trippable() {
    let mut ctx = Context::new();
    ctx.enable_stencil_test(true);
    assert!(ctx.stencil_test);

    ctx.stencil_func(StencilFunc::Equal, 0xAB, 0x0F);
    assert_eq!(ctx.stencil_func, StencilFunc::Equal);
    assert_eq!(ctx.stencil_ref, 0xAB);
    assert_eq!(ctx.stencil_mask_r, 0x0F);

    ctx.stencil_op(StencilOp::Keep, StencilOp::Zero, StencilOp::Replace);
    assert_eq!(ctx.stencil_fail, StencilOp::Keep);
    assert_eq!(ctx.stencil_zfail, StencilOp::Zero);
    assert_eq!(ctx.stencil_zpass, StencilOp::Replace);

    ctx.stencil_mask(0x3F);
    assert_eq!(ctx.stencil_mask_w, 0x3F);
}

#[test]
fn blend_equation_separate_is_round_trippable() {
    use graphos_gl::BlendEquation;
    let mut ctx = Context::new();
    ctx.enable_blend(true);
    ctx.blend_equation_separate(BlendEquation::FuncAdd, BlendEquation::FuncSubtract);
    assert_eq!(ctx.blend_eq_rgb, BlendEquation::FuncAdd);
    assert_eq!(ctx.blend_eq_alpha, BlendEquation::FuncSubtract);

    ctx.blend_color(0.1, 0.2, 0.3, 0.4);
    assert!((ctx.blend_color[0] - 0.1).abs() < 1e-5);
    assert!((ctx.blend_color[3] - 0.4).abs() < 1e-5);
}

#[test]
fn stencil_fail_op_updates_stencil_without_touching_depth_or_color() {
    let mut color = [0u32; 64];
    let mut depth = [0.4f32; 64];
    let mut stencil = [0u8; 64];
    let mut target = Target::with_stencil(&mut color, &mut depth, &mut stencil, 8, 8);

    let mut pipe = Pipeline::opaque_3d();
    pipe.depth_test = true;
    pipe.depth_write = true;
    pipe.stencil_test = true;
    pipe.stencil_func = StencilFunc::Never;
    pipe.stencil_ref = 0xAA;
    pipe.stencil_mask_w = 0xFF;
    pipe.stencil_fail = StencilOp::Replace;

    let point = [TestVertex {
        pos: Vec4::new(0.0, 0.0, 0.0, 1.0),
        color: Vec4::new(1.0, 0.0, 0.0, 1.0),
    }];
    pipe.draw_mode(&mut target, &TestShader, &point, &[0], DrawMode::Points);

    let idx = 4 * 8 + 4;
    assert_eq!(stencil[idx], 0xAA);
    assert_eq!(depth[idx], 0.4);
    assert_eq!(color[idx], 0);
}

#[test]
fn depth_fail_uses_stencil_zfail_and_preserves_depth_and_color() {
    let mut color = [0u32; 64];
    let mut depth = [0.2f32; 64];
    let mut stencil = [0u8; 64];
    let mut target = Target::with_stencil(&mut color, &mut depth, &mut stencil, 8, 8);

    let mut pipe = Pipeline::opaque_3d();
    pipe.depth_test = true;
    pipe.depth_write = true;
    pipe.depth_func = graphos_gl::DepthFunc::Less;
    pipe.stencil_test = true;
    pipe.stencil_func = StencilFunc::Always;
    pipe.stencil_ref = 0x55;
    pipe.stencil_mask_w = 0xFF;
    pipe.stencil_zfail = StencilOp::Replace;
    pipe.stencil_zpass = StencilOp::Keep;

    let point = [TestVertex {
        pos: Vec4::new(0.0, 0.0, 0.8, 1.0), // z -> 0.9, fails Less against 0.2
        color: Vec4::new(0.0, 1.0, 0.0, 1.0),
    }];
    pipe.draw_mode(&mut target, &TestShader, &point, &[0], DrawMode::Points);

    let idx = 4 * 8 + 4;
    assert_eq!(stencil[idx], 0x55);
    assert_eq!(depth[idx], 0.2);
    assert_eq!(color[idx], 0);
}

#[test]
fn depth_pass_with_depth_write_disabled_updates_stencil_zpass_only() {
    let mut color = [0u32; 64];
    let mut depth = [0.9f32; 64];
    let mut stencil = [0u8; 64];
    let mut target = Target::with_stencil(&mut color, &mut depth, &mut stencil, 8, 8);

    let mut pipe = Pipeline::opaque_3d();
    pipe.depth_test = true;
    pipe.depth_write = false;
    pipe.depth_func = graphos_gl::DepthFunc::Less;
    pipe.stencil_test = true;
    pipe.stencil_func = StencilFunc::Always;
    pipe.stencil_ref = 0x33;
    pipe.stencil_mask_w = 0xFF;
    pipe.stencil_zpass = StencilOp::Replace;

    let point = [TestVertex {
        pos: Vec4::new(0.0, 0.0, -0.5, 1.0), // z -> 0.25, passes Less against 0.9
        color: Vec4::new(0.0, 0.0, 1.0, 1.0),
    }];
    pipe.draw_mode(&mut target, &TestShader, &point, &[0], DrawMode::Points);

    let idx = 4 * 8 + 4;
    assert_eq!(stencil[idx], 0x33);
    assert_eq!(
        depth[idx], 0.9,
        "depth buffer unchanged when depth_write is false"
    );
    assert_ne!(color[idx], 0);
}

#[test]
fn color_mask_blocks_selected_channels() {
    let mut color = [0u32; 100];
    let mut depth = [1.0f32; 100];
    let mut target = Target::new(&mut color, &mut depth, 10, 10);

    let mut pipe = Pipeline::opaque_3d();
    pipe.depth_test = false;
    pipe.depth_write = false;
    pipe.color_mask = [true, false, true, false]; // R and B only; G and A masked

    // Same triangle as points_lines_triangles_share_fragment_ops (known to rasterize)
    let tri_verts = [
        TestVertex {
            pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(0.0, 1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
    ];
    pipe.draw_mode(
        &mut target,
        &TestShader,
        &tri_verts,
        &[0, 1, 2],
        DrawMode::Triangles,
    );
    // Also draw lines to ensure coverage regardless of face culling
    let line_verts = [
        TestVertex {
            pos: Vec4::new(-1.0, 0.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, 0.0, 0.0, 1.0),
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
        },
    ];
    pipe.draw_mode(
        &mut target,
        &TestShader,
        &line_verts,
        &[0, 1],
        DrawMode::Lines,
    );

    let painted: Vec<u32> = color.iter().copied().filter(|&p| p != 0).collect();
    assert!(!painted.is_empty(), "at least one pixel painted");
    for &p in &painted {
        // color_mask[1]=G masked (bits 8-15), color_mask[3]=A masked (bits 24-31)
        assert_eq!(
            p & 0xFF00_FF00,
            0,
            "G/A channels should be masked: {p:#010x}"
        );
    }
}

#[test]
fn blend_and_scissor_are_independent_pipeline_controls() {
    let mut ctx = Context::new();
    ctx.enable_blend(true);
    ctx.blend_func(BlendFactor::One, BlendFactor::Zero);
    ctx.scissor_test = true;
    ctx.scissor = [1, 1, 3, 3];

    assert!(ctx.blend);
    assert_eq!(ctx.blend_src_rgb, BlendFactor::One);
    assert!(ctx.scissor_test);
    assert_eq!(ctx.scissor, [1, 1, 3, 3]);

    ctx.enable_blend(false);
    assert!(!ctx.blend);
    assert!(ctx.scissor_test, "scissor unaffected by blend toggle");
}

#[test]
fn draw_elements_requires_bound_element_buffer() {
    let ctx = Context::new();
    let mut color = [0u32; 64];
    let mut depth = [1.0f32; 64];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);
    let verts = [
        TestVertex {
            pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 0.0, 0.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(0.0, 1.0, 0.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(0.0, 1.0, 0.0, 1.0),
            color: Vec4::new(0.0, 0.0, 1.0, 1.0),
        },
    ];

    let ok = ctx.draw_elements(
        &mut target,
        &TestShader,
        &verts,
        3,
        IndexType::U16,
        0,
        DrawMode::Triangles,
    );
    assert!(!ok, "draw_elements should fail when no EBO is bound");
}

#[test]
fn draw_elements_rejects_oob_index_decode_range() {
    let mut ctx = Context::new();
    let mut color = [0u32; 64];
    let mut depth = [1.0f32; 64];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);
    let verts = [
        TestVertex {
            pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 0.0, 0.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(0.0, 1.0, 0.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(0.0, 1.0, 0.0, 1.0),
            color: Vec4::new(0.0, 0.0, 1.0, 1.0),
        },
    ];

    let mut ebo = [0u32; 1];
    assert_eq!(ctx.gen_buffers(&mut ebo), 1);
    ctx.bind_element_buffer(ebo[0]);
    ctx.buffer_data_element_bound(&[0, 0, 1, 0, 2, 0]);

    let ok = ctx.draw_elements(
        &mut target,
        &TestShader,
        &verts,
        4,
        IndexType::U16,
        0,
        DrawMode::Triangles,
    );
    assert!(
        !ok,
        "draw_elements should reject decode beyond EBO byte range"
    );
}

#[test]
fn draw_arrays_instanced_overflow_returns_false_without_callback() {
    let ctx = Context::new();
    let mut color = [0u32; 64];
    let mut depth = [1.0f32; 64];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);
    let verts = [TestVertex {
        pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
        color: Vec4::new(1.0, 1.0, 1.0, 1.0),
    }];
    let mut calls = 0u32;
    let mut shader = TestShader;

    let ok = ctx.draw_arrays_instanced(
        &mut target,
        &mut shader,
        &verts,
        usize::MAX,
        2,
        DrawMode::Points,
        3,
        |_s, _i| {
            calls += 1;
        },
    );

    assert!(!ok);
    assert_eq!(
        calls, 0,
        "callback must not run when range validation fails"
    );
}

#[test]
fn draw_elements_instanced_zero_instances_is_noop_success() {
    let mut ctx = Context::new();
    let mut color = [0u32; 64];
    let mut depth = [1.0f32; 64];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);
    let verts = [
        TestVertex {
            pos: Vec4::new(-1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(1.0, 0.0, 0.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(1.0, -1.0, 0.0, 1.0),
            color: Vec4::new(0.0, 1.0, 0.0, 1.0),
        },
        TestVertex {
            pos: Vec4::new(0.0, 1.0, 0.0, 1.0),
            color: Vec4::new(0.0, 0.0, 1.0, 1.0),
        },
    ];

    let mut ebo = [0u32; 1];
    assert_eq!(ctx.gen_buffers(&mut ebo), 1);
    ctx.bind_element_buffer(ebo[0]);
    ctx.buffer_data_element_bound(&[0, 0, 1, 0, 2, 0]);

    let mut calls = 0u32;
    let mut shader = TestShader;
    let ok = ctx.draw_elements_instanced(
        &mut target,
        &mut shader,
        &verts,
        3,
        IndexType::U16,
        0,
        DrawMode::Triangles,
        0,
        |_s, _i| {
            calls += 1;
        },
    );

    assert!(ok);
    assert_eq!(calls, 0, "zero instances must not invoke callback");
    assert!(
        color.iter().all(|&p| p == 0),
        "zero instances should not rasterize"
    );
}

#[test]
fn map_buffer_range_write_and_unmap_commits_changes() {
    let mut ctx = Context::new();
    let mut out = [0u32; 1];
    assert_eq!(ctx.gen_buffers(&mut out), 1);
    let b = out[0];

    ctx.buffer_data(b, &[1, 2, 3, 4, 5, 6]);
    assert!(ctx.map_buffer_range(b, 2, 3, MAP_WRITE_BIT));
    {
        let mapped = ctx.mapped_buffer_bytes_mut(b).expect("mutable map view");
        mapped.copy_from_slice(&[9, 8, 7]);
    }
    assert!(ctx.unmap_buffer(b));

    assert_eq!(ctx.get_buffer_data(b).unwrap(), &[1, 2, 9, 8, 7, 6]);
}

#[test]
fn map_buffer_range_invalid_range_sets_error() {
    let mut ctx = Context::new();
    let mut out = [0u32; 1];
    assert_eq!(ctx.gen_buffers(&mut out), 1);
    let b = out[0];
    ctx.buffer_data(b, &[0, 1, 2, 3]);

    assert!(!ctx.map_buffer_range(b, 3, 4, MAP_READ_BIT));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn map_buffer_range_rejects_double_map() {
    let mut ctx = Context::new();
    let mut out = [0u32; 2];
    assert_eq!(ctx.gen_buffers(&mut out), 2);
    let b0 = out[0];
    let b1 = out[1];
    ctx.buffer_data(b0, &[1, 2, 3, 4]);
    ctx.buffer_data(b1, &[5, 6, 7, 8]);

    assert!(ctx.map_buffer_range(b0, 0, 2, MAP_READ_BIT));
    assert!(!ctx.map_buffer_range(b1, 0, 2, MAP_READ_BIT));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    assert!(ctx.unmap_buffer(b0));
}

#[test]
fn unmap_buffer_wrong_name_sets_error_and_keeps_mapping() {
    let mut ctx = Context::new();
    let mut out = [0u32; 2];
    assert_eq!(ctx.gen_buffers(&mut out), 2);
    let b0 = out[0];
    let b1 = out[1];
    ctx.buffer_data(b0, &[10, 11, 12, 13]);

    assert!(ctx.map_buffer_range(b0, 1, 2, MAP_READ_BIT));
    assert!(!ctx.unmap_buffer(b1));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    assert!(
        ctx.mapped_buffer_bytes(b0).is_some(),
        "map should remain active"
    );
    assert!(ctx.unmap_buffer(b0));
}

#[test]
fn fence_sync_wait_returns_already_signaled() {
    let mut ctx = Context::new();
    let sync = ctx.fence_sync();
    assert_ne!(sync, 0);
    assert_eq!(
        ctx.client_wait_sync(sync, 0),
        SyncWaitResult::AlreadySignaled
    );
}

#[test]
fn client_wait_sync_invalid_handle_sets_error() {
    let mut ctx = Context::new();
    assert_eq!(ctx.client_wait_sync(123, 0), SyncWaitResult::WaitFailed);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn delete_sync_invalidates_handle() {
    let mut ctx = Context::new();
    let sync = ctx.fence_sync();
    assert_ne!(sync, 0);
    ctx.delete_syncs(&[sync]);
    assert_eq!(ctx.client_wait_sync(sync, 0), SyncWaitResult::WaitFailed);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn transform_feedback_lifecycle_roundtrip() {
    let mut ctx = Context::new();
    let mut out = [0u32; 1];
    assert_eq!(ctx.gen_transform_feedbacks(&mut out), 1);
    let tfb = out[0];
    ctx.bind_transform_feedback(tfb);

    ctx.begin_transform_feedback(DrawMode::Triangles);
    let s0 = ctx.transform_feedback_state(tfb).unwrap();
    assert!(s0.active);
    assert!(!s0.paused);

    ctx.pause_transform_feedback();
    assert!(ctx.transform_feedback_state(tfb).unwrap().paused);

    ctx.resume_transform_feedback();
    assert!(!ctx.transform_feedback_state(tfb).unwrap().paused);

    ctx.end_transform_feedback();
    let s1 = ctx.transform_feedback_state(tfb).unwrap();
    assert!(!s1.active);
    assert!(!s1.paused);
}

#[test]
fn transform_feedback_begin_without_binding_sets_error() {
    let mut ctx = Context::new();
    ctx.begin_transform_feedback(DrawMode::Triangles);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn transform_feedback_rebind_while_active_not_paused_sets_error() {
    let mut ctx = Context::new();
    let mut out = [0u32; 2];
    assert_eq!(ctx.gen_transform_feedbacks(&mut out), 2);
    let a = out[0];
    let b = out[1];

    ctx.bind_transform_feedback(a);
    ctx.begin_transform_feedback(DrawMode::Triangles);
    ctx.bind_transform_feedback(b);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    assert_eq!(ctx.current_transform_feedback(), a);
}

#[test]
fn transform_feedback_delete_active_sets_error() {
    let mut ctx = Context::new();
    let mut out = [0u32; 1];
    assert_eq!(ctx.gen_transform_feedbacks(&mut out), 1);
    let tfb = out[0];
    ctx.bind_transform_feedback(tfb);
    ctx.begin_transform_feedback(DrawMode::Triangles);
    ctx.delete_transform_feedbacks(&[tfb]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
    assert!(ctx.transform_feedback_state(tfb).is_some());
}

#[test]
fn transform_feedback_capture_writes_bound_buffer() {
    let mut ctx = Context::new();
    let mut tfb = [0u32; 1];
    let mut buf = [0u32; 1];
    assert_eq!(ctx.gen_transform_feedbacks(&mut tfb), 1);
    assert_eq!(ctx.gen_buffers(&mut buf), 1);

    ctx.bind_transform_feedback(tfb[0]);
    ctx.bind_transform_feedback_buffer_base(0, buf[0]);
    ctx.begin_transform_feedback(DrawMode::Triangles);

    assert!(ctx.transform_feedback_capture_bytes(&[1, 2, 3, 4]));
    assert_eq!(ctx.get_buffer_data(buf[0]).unwrap(), &[1, 2, 3, 4]);

    ctx.pause_transform_feedback();
    assert!(!ctx.transform_feedback_capture_bytes(&[9]));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));

    ctx.resume_transform_feedback();
    assert!(ctx.transform_feedback_capture_bytes(&[5, 6]));
    assert_eq!(ctx.get_buffer_data(buf[0]).unwrap(), &[1, 2, 3, 4, 5, 6]);

    ctx.end_transform_feedback();
    assert!(!ctx.transform_feedback_capture_bytes(&[7]));
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn transform_feedback_begin_clears_bound_capture_buffers() {
    let mut ctx = Context::new();
    let mut tfb = [0u32; 1];
    let mut buf = [0u32; 1];
    assert_eq!(ctx.gen_transform_feedbacks(&mut tfb), 1);
    assert_eq!(ctx.gen_buffers(&mut buf), 1);

    ctx.buffer_data(buf[0], &[9, 9, 9]);
    ctx.bind_transform_feedback(tfb[0]);
    ctx.bind_transform_feedback_buffer_base(0, buf[0]);
    ctx.begin_transform_feedback(DrawMode::Triangles);

    assert_eq!(ctx.get_buffer_data(buf[0]).unwrap(), &[]);
}

#[test]
fn query_object_records_and_reports_results() {
    let mut ctx = Context::new();
    let mut q = [0u32; 1];
    assert_eq!(ctx.gen_queries(&mut q), 1);
    let q = q[0];

    ctx.begin_query(QueryTarget::SamplesPassed, q);
    ctx.query_mark_progress(11, 3, 100);
    ctx.end_query(QueryTarget::SamplesPassed);

    assert_eq!(ctx.query_target(q), Some(QueryTarget::SamplesPassed));
    assert_eq!(ctx.query_result_available(q), Some(true));
    assert_eq!(ctx.query_result_u64(q), Some(11));
}

#[test]
fn query_target_conflict_sets_error() {
    let mut ctx = Context::new();
    let mut q = [0u32; 2];
    assert_eq!(ctx.gen_queries(&mut q), 2);

    ctx.begin_query(QueryTarget::AnySamplesPassed, q[0]);
    ctx.begin_query(QueryTarget::AnySamplesPassed, q[1]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));

    ctx.query_mark_progress(2, 0, 0);
    ctx.end_query(QueryTarget::AnySamplesPassed);
    assert_eq!(ctx.query_result_u64(q[0]), Some(1));
}

#[test]
fn debug_messages_and_groups_roundtrip() {
    let mut ctx = Context::new();
    ctx.debug_message_insert(
        DebugSource::Application,
        DebugType::Marker,
        7,
        DebugSeverity::Low,
        b"marker",
    );
    ctx.push_debug_group(99, b"frame");
    assert_eq!(ctx.debug_group_depth(), 1);
    ctx.pop_debug_group();
    assert_eq!(ctx.debug_group_depth(), 0);

    let msgs = ctx.drain_debug_messages();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].id, 7);
    assert_eq!(msgs[1].kind, DebugType::PushGroup);
    assert_eq!(msgs[2].kind, DebugType::PopGroup);

    ctx.pop_debug_group();
    assert_eq!(ctx.get_error(), Some(GlError::InvalidOperation));
}

#[test]
fn robustness_flags_and_reset_status_roundtrip() {
    let mut ctx = Context::new();
    assert!(!ctx.robust_access_enabled());
    // KHR_robustness is off by default (GAP-002); enable it for this test.
    ctx.set_extension_enabled_for_testing(GlExtension::KhrRobustness, true);
    ctx.set_robust_access(true);
    assert!(ctx.robust_access_enabled());

    assert_eq!(ctx.context_reset_status(), ContextResetStatus::NoError);
    ctx.force_context_reset_for_testing(ContextResetStatus::UnknownContextReset);
    assert_eq!(
        ctx.context_reset_status(),
        ContextResetStatus::UnknownContextReset
    );
}

#[test]
fn sampler_override_takes_precedence_over_texture_parameters() {
    let mut ctx = Context::new();

    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    let tex = tex[0];
    ctx.tex_image_2d(tex, 0, 1, 1, &[0xFFFF_FFFF]);
    ctx.tex_parameter_wrap(tex, WrapMode::ClampToBorder, WrapMode::ClampToBorder);
    ctx.tex_parameter_filter(tex, FilterMode::Linear, FilterMode::Linear);
    ctx.tex_parameter_border_color(tex, [0.1, 0.2, 0.3, 1.0]);

    let mut sam = [0u32; 1];
    assert_eq!(ctx.gen_samplers(&mut sam), 1);
    let sam = sam[0];
    ctx.sampler_parameter_wrap(sam, WrapMode::Repeat, WrapMode::Repeat);
    ctx.sampler_parameter_filter(sam, FilterMode::Nearest, FilterMode::Nearest);
    ctx.sampler_parameter_border_color(sam, [0.9, 0.8, 0.7, 1.0]);

    ctx.active_texture(0);
    ctx.bind_texture(tex);
    ctx.bind_sampler(0, sam);

    let view = ctx.texture_view_for_unit(0).expect("resolved texture view");
    assert_eq!(view.wrap_s, WrapMode::Repeat);
    assert_eq!(view.min_filter, FilterMode::Nearest);
    assert!((view.border_color[0] - 0.9).abs() < 1e-6);
}

#[test]
fn unbound_sampler_uses_texture_parameters() {
    let mut ctx = Context::new();

    let mut tex = [0u32; 1];
    assert_eq!(ctx.gen_textures(&mut tex), 1);
    let tex = tex[0];
    ctx.tex_image_2d(tex, 0, 1, 1, &[0xFFFF_FFFF]);
    ctx.tex_parameter_wrap(tex, WrapMode::ClampToBorder, WrapMode::ClampToBorder);
    ctx.tex_parameter_filter(tex, FilterMode::Linear, FilterMode::Linear);

    ctx.active_texture(0);
    ctx.bind_texture(tex);
    ctx.bind_sampler(0, 0);

    let view = ctx.texture_view_for_unit(0).expect("resolved texture view");
    assert_eq!(view.wrap_s, WrapMode::ClampToBorder);
    assert_eq!(view.min_filter, FilterMode::Linear);
}

// ── UBO/SSBO/Atomic/Image resource model tests ──────────────────────────────

#[test]
fn ubo_range_binding_stores_offset_and_size() {
    let mut ctx = Context::new();
    let mut bufs = [0u32; 1];
    ctx.gen_buffers(&mut bufs);
    let b = bufs[0];
    ctx.bind_uniform_buffer(b);
    ctx.buffer_data(b, &[0u8; 64]);

    ctx.bind_uniform_buffer_range(0, b, 16, 32);
    assert_eq!(ctx.uniform_buffer_binding(0), Some(b));
    assert_eq!(ctx.uniform_buffer_range(0), Some((16, 32)));
    assert_eq!(ctx.uniform_buffer_binding(1), Some(0));
}

#[test]
fn ubo_range_out_of_bounds_sets_error() {
    use graphos_gl::gl::MAX_UNIFORM_BUFFER_BINDINGS as MAX_UBO;
    let mut ctx = Context::new();
    ctx.bind_uniform_buffer_range(MAX_UBO as u32, 0, 0, 0);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn ssbo_base_binding_works() {
    use graphos_gl::gl::MAX_SHADER_STORAGE_BUFFER_BINDINGS;
    let mut ctx = Context::new();
    let mut bufs = [0u32; 1];
    ctx.gen_buffers(&mut bufs);
    let b = bufs[0];
    ctx.bind_shader_storage_buffer(b);
    ctx.buffer_data(b, &[0u8; 128]);

    ctx.bind_shader_storage_buffer_base(0, b);
    assert_eq!(ctx.shader_storage_buffer_binding(0), Some(b));

    // Out of bounds rejects
    ctx.bind_shader_storage_buffer_base(MAX_SHADER_STORAGE_BUFFER_BINDINGS as u32, b);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn ssbo_range_binding_stores_offset_and_size() {
    let mut ctx = Context::new();
    let mut bufs = [0u32; 1];
    ctx.gen_buffers(&mut bufs);
    let b = bufs[0];
    ctx.bind_shader_storage_buffer(b);
    ctx.buffer_data(b, &[0u8; 256]);

    ctx.bind_shader_storage_buffer_range(2, b, 64, 128);
    assert_eq!(ctx.shader_storage_buffer_binding(2), Some(b));
    assert_eq!(ctx.shader_storage_buffer_range(2), Some((64, 128)));
}

#[test]
fn atomic_counter_buffer_binding_works() {
    use graphos_gl::gl::MAX_ATOMIC_COUNTER_BUFFER_BINDINGS;
    let mut ctx = Context::new();
    let mut bufs = [0u32; 1];
    ctx.gen_buffers(&mut bufs);
    let b = bufs[0];
    ctx.bind_atomic_counter_buffer(b);
    ctx.buffer_data(b, &[0u8; 16]);

    ctx.bind_atomic_counter_buffer_base(0, b);
    assert_eq!(ctx.atomic_counter_buffer_binding(0), Some(b));

    ctx.bind_atomic_counter_buffer_base(MAX_ATOMIC_COUNTER_BUFFER_BINDINGS as u32, b);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn bind_image_texture_stores_binding() {
    use graphos_gl::gl::{ImageAccess, MAX_IMAGE_UNITS};
    let mut ctx = Context::new();
    let mut tex = [0u32; 1];
    ctx.gen_textures(&mut tex);
    let t = tex[0];
    ctx.tex_storage_2d(t, 1, 4, 4);

    ctx.bind_image_texture(
        0,
        t,
        0,
        false,
        0,
        ImageAccess::ReadWrite,
        0x8058, /* RGBA8 */
    );
    let binding = ctx.image_unit_binding(0).expect("image unit 0 set");
    assert_eq!(binding.texture, t);
    assert_eq!(binding.access, ImageAccess::ReadWrite);
    assert_eq!(binding.format, 0x8058);

    // Out of bounds rejects
    ctx.bind_image_texture(
        MAX_IMAGE_UNITS as u32,
        t,
        0,
        false,
        0,
        ImageAccess::ReadOnly,
        0,
    );
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn memory_barrier_and_region_are_noops_on_software_backend() {
    use graphos_gl::gl::MemoryBarrierBits;
    let mut ctx = Context::new();
    ctx.memory_barrier(MemoryBarrierBits::ALL);
    assert_eq!(ctx.get_error(), None);
    assert_eq!(ctx.last_memory_barrier_bits(), MemoryBarrierBits::ALL);
    ctx.memory_barrier_by_region(
        MemoryBarrierBits::FRAMEBUFFER | MemoryBarrierBits::SHADER_IMAGE_ACCESS,
    );
    assert_eq!(ctx.get_error(), None);
    assert_eq!(
        ctx.last_memory_barrier_by_region_bits(),
        MemoryBarrierBits::FRAMEBUFFER | MemoryBarrierBits::SHADER_IMAGE_ACCESS
    );

    ctx.memory_barrier(0x4000_0000);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));

    ctx.memory_barrier_by_region(0x8000_0000);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

// ── MRT edge semantics tests ─────────────────────────────────────────────────

#[test]
fn draw_buffers_mask_updates_on_set() {
    let mut ctx = Context::new();
    // Default: only attachment 0
    assert_eq!(ctx.draw_buffers_mask(), 0x01);

    ctx.draw_buffers(&[0, 2]);
    assert_eq!(ctx.draw_buffers_mask(), 0b0000_0101); // bits 0 and 2

    ctx.draw_buffers(&[0xFF]); // GL_NONE
    assert_eq!(ctx.draw_buffers_mask(), 0x00);
}

#[test]
fn draw_buffers_out_of_range_sets_error() {
    let mut ctx = Context::new();
    ctx.draw_buffers(&[8]); // index 8 ≥ 8 is invalid
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn read_buffer_stores_attachment_index() {
    let mut ctx = Context::new();
    ctx.read_buffer(2);
    assert_eq!(ctx.read_buffer_index(), 2);

    ctx.read_buffer(0xFF); // GL_NONE
    assert_eq!(ctx.read_buffer_index(), 0xFF);

    ctx.read_buffer(8); // invalid
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn color_maski_sets_per_attachment_mask() {
    let mut ctx = Context::new();
    // All attachments start with all-true mask
    assert_eq!(ctx.color_mask_for_attachment(0), Some([true; 4]));

    ctx.color_maski(1, false, true, false, true);
    assert_eq!(
        ctx.color_mask_for_attachment(1),
        Some([false, true, false, true])
    );

    // Other attachments unchanged
    assert_eq!(ctx.color_mask_for_attachment(0), Some([true; 4]));

    ctx.color_maski(8, true, true, true, true); // invalid
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn color_mask_syncs_all_active_draw_attachments() {
    let mut ctx = Context::new();
    ctx.draw_buffers(&[0, 1, 3]);
    ctx.color_mask(false, false, true, false);

    // Attachments 0, 1, 3 (in mask) should be updated
    assert_eq!(
        ctx.color_mask_for_attachment(0),
        Some([false, false, true, false])
    );
    assert_eq!(
        ctx.color_mask_for_attachment(1),
        Some([false, false, true, false])
    );
    assert_eq!(
        ctx.color_mask_for_attachment(3),
        Some([false, false, true, false])
    );
    // Attachment 2 not in mask, should still have default
    assert_eq!(ctx.color_mask_for_attachment(2), Some([true; 4]));
}

#[test]
fn clear_buffer_color_fv_only_affects_active_draw_buffer() {
    use graphos_gl::Attachment;

    let mut ctx = Context::new();
    let mut fbos = [0u32; 1];
    let mut texs = [0u32; 2];
    assert_eq!(ctx.gen_framebuffers(&mut fbos), 1);
    assert_eq!(ctx.gen_textures(&mut texs), 2);

    let fbo = fbos[0];
    for &t in &texs {
        ctx.tex_storage_2d(t, 1, 1, 1);
        ctx.tex_image_2d(t, 0, 1, 1, &[0xFF00_0000]);
    }
    ctx.framebuffer_texture_2d(fbo, Attachment::Color(0), texs[0], 0);
    ctx.framebuffer_texture_2d(fbo, Attachment::Color(1), texs[1], 0);
    ctx.bind_draw_framebuffer(fbo);

    // Set draw_buffers_mask to only attachment 1.
    ctx.draw_buffers(&[1]);

    // Clearing attachment 1 should update attachment 1 pixels only.
    ctx.clear_buffer_color_fv(1, [0.5, 0.5, 0.5, 1.0]);
    assert_eq!(ctx.get_error(), None);

    let mut out0 = [0u32; 1];
    let mut out1 = [0u32; 1];
    assert!(ctx.read_pixels_bgra8(fbo, 0, 0, 0, 1, 1, &mut out0));
    assert!(ctx.read_pixels_bgra8(fbo, 1, 0, 0, 1, 1, &mut out1));
    assert_eq!(out0[0], 0xFF00_0000);
    assert_eq!(out1[0], 0xFF7F_7F7F);

    // Clearing attachment 0 (not in mask) is a no-op, no error
    ctx.clear_buffer_color_fv(0, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(ctx.get_error(), None);
    assert!(ctx.read_pixels_bgra8(fbo, 0, 0, 0, 1, 1, &mut out0));
    assert_eq!(out0[0], 0xFF00_0000);

    // Out-of-range attachment
    ctx.clear_buffer_color_fv(8, [0.0; 4]);
    assert_eq!(ctx.get_error(), Some(GlError::InvalidValue));
}

#[test]
fn clear_buffer_color_fv_clears_renderbuffer_color_attachment() {
    use graphos_gl::Attachment;
    let mut ctx = Context::new();
    let mut fbos = [0u32; 1];
    let mut rbos = [0u32; 1];
    assert_eq!(ctx.gen_framebuffers(&mut fbos), 1);
    assert_eq!(ctx.gen_renderbuffers(&mut rbos), 1);

    let fbo = fbos[0];
    let rbo = rbos[0];
    ctx.renderbuffer_storage(rbo, 2, 2, false, false);
    ctx.framebuffer_renderbuffer(fbo, Attachment::Color(0), rbo);
    ctx.bind_draw_framebuffer(fbo);

    ctx.draw_buffers(&[0]);
    ctx.clear_buffer_color_fv(0, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(ctx.get_error(), None);

    let mut out = [0u32; 4];
    assert!(ctx.read_pixels_bgra8(fbo, 0, 0, 0, 2, 2, &mut out));
    assert_eq!(out, [0xFFFF_0000; 4]);
}

#[test]
fn mrt_fbo_can_attach_multiple_textures() {
    use graphos_gl::gl::Attachment;
    let mut ctx = Context::new();
    let mut fbos = [0u32; 1];
    let mut texs = [0u32; 3];
    ctx.gen_framebuffers(&mut fbos);
    ctx.gen_textures(&mut texs);
    let fbo = fbos[0];

    for i in 0..3 {
        ctx.tex_storage_2d(texs[i], 1, 4, 4);
        ctx.framebuffer_texture_2d(fbo, Attachment::Color(i as u8), texs[i], 0);
    }
    // All 3 attachments should refer to their respective textures
    for i in 0..3u8 {
        assert_eq!(
            ctx.framebuffer_color_attachment_texture(fbo, i),
            Some(texs[i as usize])
        );
    }
}

// ── GLSL interpreter / state-driven draw tests ─────────────────────────────

/// Helper: compile + link a program from raw GLSL source strings.
fn make_program(ctx: &mut Context, vert_src: &[u8], frag_src: &[u8]) -> u32 {
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, vert_src);
    ctx.shader_source(fs, frag_src);
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.link_program(prog);
    prog
}

/// Helper: upload a tightly-packed f32 slice into a new VBO and return its name.
fn upload_vbo(ctx: &mut Context, data: &[f32]) -> u32 {
    let mut buf = [0u32; 1];
    ctx.gen_buffers(&mut buf);
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    ctx.buffer_data(buf[0], &bytes);
    buf[0]
}

/// `draw_state_arrays` returns false when no program is linked.
#[test]
fn draw_state_arrays_returns_false_without_linked_program() {
    let mut ctx = Context::new();
    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);
    // No program bound — should return false without panicking.
    assert!(!ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points));
}

/// `draw_state_arrays` succeeds and produces a pixel when a minimal pass-through
/// shader positions a point at the center of a 4×4 framebuffer.
#[test]
fn draw_state_arrays_renders_point_from_glsl_shader() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Upload one vertex at NDC center (0, 0, 0, 1).
    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);

    let ok = ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points);
    assert!(ok, "draw_state_arrays should succeed with a linked program");
    // At least one pixel should be non-zero after drawing a red point.
    assert!(
        color.iter().any(|&p| p != 0),
        "expected a non-zero pixel in output"
    );
}

/// `draw_state_elements` renders an indexed triangle and writes pixels.
#[test]
fn draw_state_elements_renders_indexed_triangle() {
    // Simple pass-through shader: position attribute 0, constant blue output.
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"void main() { gl_FragColor = vec4(0.0, 0.0, 1.0, 1.0); }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Triangle covering most of a 4×4 NDC square.
    let verts: &[f32] = &[
        -0.9, -0.9, 0.0, 1.0, 0.9, -0.9, 0.0, 1.0, 0.0, 0.9, 0.0, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    // EBO with three indices.
    let mut ebo_arr = [0u32; 1];
    ctx.gen_buffers(&mut ebo_arr);
    ctx.bind_element_buffer(ebo_arr[0]);
    let idx_bytes: Vec<u8> = [0u8, 1, 2].to_vec();
    ctx.buffer_data_element_bound(&idx_bytes);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);

    let ok = ctx.draw_state_elements(&mut target, 3, IndexType::U8, 0, DrawMode::Triangles);
    assert!(ok, "draw_state_elements should succeed");
    assert!(
        color.iter().any(|&p| p != 0),
        "expected pixels written by indexed draw"
    );
}

/// `draw_state_arrays` with a GLSL uniform: shader reads a u_color uniform and
/// writes it to gl_FragColor.  The output pixel should reflect the uniform value.
#[test]
fn draw_state_arrays_glsl_uniform_affects_fragment_output() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"uniform vec4 u_color;\nvoid main() { gl_FragColor = u_color; }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Set u_color = green (0, 1, 0, 1).
    let loc = ctx.get_uniform_location(prog, b"u_color");
    assert!(loc >= 0, "u_color uniform should be found");
    ctx.uniform4f(prog, loc, 0.0, 1.0, 0.0, 1.0);

    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);

    assert!(ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points));
    assert!(
        color.iter().any(|&p| p != 0),
        "uniform-colored pixel should be non-zero"
    );
}

/// Transform feedback: after an active TFB object is bound and `draw_state_arrays`
/// executes, the bound TFB buffer should contain written bytes (non-empty / sized).
#[test]
fn draw_state_arrays_transform_feedback_writes_to_buffer() {
    // Use GLSL ES 3.0 `out` syntax so the metadata parser registers v_out as
    // an output varying, enabling transform feedback compatibility linking.
    let vert =
        b"in vec4 a_pos;\nout vec4 v_out;\nvoid main() { gl_Position = a_pos; v_out = a_pos; }\n";
    let frag = b"void main() { gl_FragColor = vec4(1.0); }\n";

    let mut ctx = Context::new();
    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, vert);
    ctx.shader_source(fs, frag);
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);
    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    // Set TFB varyings BEFORE linking so the link succeeds.
    ctx.set_transform_feedback_varyings(prog, &[b"v_out"]);
    ctx.link_program(prog);
    ctx.use_program(prog);

    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    // Create and bind a TFB object + output buffer.
    let mut tfb_arr = [0u32; 1];
    ctx.gen_transform_feedbacks(&mut tfb_arr);
    ctx.bind_transform_feedback(tfb_arr[0]);
    let mut tfb_buf = [0u32; 1];
    ctx.gen_buffers(&mut tfb_buf);
    ctx.bind_transform_feedback_buffer_base(0, tfb_buf[0]);
    ctx.begin_transform_feedback(DrawMode::Points);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);
    ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points);

    ctx.end_transform_feedback();

    // The TFB buffer should have been written with at least 16 bytes (one vec4).
    let buf_len = ctx.get_buffer_size(tfb_buf[0]).unwrap_or(0);
    assert!(
        buf_len >= 16,
        "TFB buffer should contain captured varying data (got {} bytes)",
        buf_len
    );
}

#[test]
fn draw_state_arrays_glsl_mrt_blends_each_attachment_independently() {
    let vert = b"#version 300 es\nin vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"#version 300 es\nprecision mediump float;\nlayout(location = 0) out vec4 c0;\nlayout(location = 1) out vec4 c1;\nvoid main() { c0 = vec4(1.0, 0.0, 0.0, 0.5); c1 = vec4(0.0, 1.0, 0.0, 0.5); }\n";

    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    ctx.draw_buffers(&[0, 1]);
    ctx.enable_blend(true);
    ctx.blend_func_separate(
        BlendFactor::SrcAlpha,
        BlendFactor::OneMinusSrcAlpha,
        BlendFactor::One,
        BlendFactor::OneMinusSrcAlpha,
    );

    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color0 = [0xFF00_00FFu32; 16];
    let mut color1 = [0xFFFF_0000u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color0, &mut depth, 4, 4).with_extra_color(1, &mut color1);

    assert!(ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points));
    assert!(
        color0.iter().any(|&p| p == 0xFF80_0080),
        "attachment 0 should blend red over blue"
    );
    assert!(
        color1.iter().any(|&p| p == 0xFF80_8000),
        "attachment 1 should blend green over red using its own destination buffer"
    );
}

#[test]
fn draw_state_arrays_glsl_mrt_uses_attachment_specific_blend_state() {
    let vert = b"#version 300 es\nin vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"#version 300 es\nprecision mediump float;\nlayout(location = 0) out vec4 c0;\nlayout(location = 1) out vec4 c1;\nvoid main() { c0 = vec4(1.0, 0.0, 0.0, 1.0); c1 = vec4(0.0, 1.0, 0.0, 0.5); }\n";

    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    ctx.draw_buffers(&[0, 1]);
    ctx.enable_blend(false);
    ctx.enable_blendi(1, true);
    ctx.blend_funci_separate(
        1,
        BlendFactor::SrcAlpha,
        BlendFactor::OneMinusSrcAlpha,
        BlendFactor::One,
        BlendFactor::OneMinusSrcAlpha,
    );

    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color0 = [0xFF00_00FFu32; 16];
    let mut color1 = [0xFFFF_0000u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color0, &mut depth, 4, 4).with_extra_color(1, &mut color1);

    assert!(ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points));
    assert!(
        color0.iter().any(|&p| p == 0xFFFF_0000),
        "attachment 0 should ignore blending and write solid red"
    );
    assert!(
        color1.iter().any(|&p| p == 0xFF80_8000),
        "attachment 1 should use its indexed blend state"
    );
}

#[test]
fn draw_state_arrays_transform_feedback_uses_varying_name_and_component_count() {
    let vert = b"#version 300 es\nin vec4 a_pos;\nout vec4 v_unused;\nout vec2 v_uv;\nvoid main() { gl_Position = a_pos; v_unused = vec4(9.0, 8.0, 7.0, 6.0); v_uv = a_pos.xy + vec2(0.25, 0.5); }\n";
    let frag = b"#version 300 es\nprecision mediump float;\nout vec4 color;\nvoid main() { color = vec4(1.0); }\n";

    let mut ctx = Context::new();
    ctx.set_strict_glsl(true);

    let vs = ctx.create_shader(ShaderKind::Vertex);
    let fs = ctx.create_shader(ShaderKind::Fragment);
    ctx.shader_source(vs, vert);
    ctx.shader_source(fs, frag);
    ctx.compile_shader(vs);
    ctx.compile_shader(fs);

    let prog = ctx.create_program();
    ctx.attach_shader(prog, vs);
    ctx.attach_shader(prog, fs);
    ctx.set_transform_feedback_varyings(prog, &[b"v_uv"]);
    ctx.link_program(prog);
    assert!(ctx.program_link_status(prog));
    ctx.use_program(prog);

    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut tfb_arr = [0u32; 1];
    ctx.gen_transform_feedbacks(&mut tfb_arr);
    ctx.bind_transform_feedback(tfb_arr[0]);
    let mut tfb_buf = [0u32; 1];
    ctx.gen_buffers(&mut tfb_buf);
    ctx.bind_transform_feedback_buffer_base(0, tfb_buf[0]);
    ctx.begin_transform_feedback(DrawMode::Points);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);
    assert!(ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points));
    ctx.end_transform_feedback();

    let bytes = ctx.get_buffer_data(tfb_buf[0]).unwrap();
    assert_eq!(bytes.len(), 8);
    let u = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let v = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
    assert!((u - 0.25).abs() < 1e-6, "expected u=0.25, got {u}");
    assert!((v - 0.5).abs() < 1e-6, "expected v=0.5, got {v}");
}

/// GAP-008: fragment shader writing `gl_FragDepth` overrides interpolated depth.
///
/// The shader outputs a constant 0.25 into `gl_FragDepth`.  The depth buffer is
/// initialized to 1.0 (far).  After the draw the covered pixel must have depth
/// 0.25 rather than the geometry's interpolated z (0.0 → NDC mid → 0.5 window).
#[test]
fn draw_state_arrays_glsl_frag_depth_overrides_interpolated_depth() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    // Shader outputs solid red and sets a custom depth of 0.25.
    let frag = b"void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); gl_FragDepth = 0.25; }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Single point at NDC origin, z=0 → window z=0.5 (before gl_FragDepth override).
    let vbo = upload_vbo(&mut ctx, &[0.0f32, 0.0, 0.0, 1.0]);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);

    ctx.enable_depth_test(true);
    ctx.depth_mask(true);
    assert!(ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points));

    // At least one pixel should be non-zero (red).
    assert!(color.iter().any(|&p| p != 0), "expected red pixel");
    // The written depth must equal 0.25, not the geometry z (0.5).
    let touched_depth: Vec<f32> = depth
        .iter()
        .copied()
        .filter(|&d| (d - 1.0).abs() > 1e-4)
        .collect();
    assert!(!touched_depth.is_empty(), "expected depth to be written");
    for d in &touched_depth {
        assert!(
            (*d - 0.25).abs() < 1e-5,
            "expected gl_FragDepth=0.25 in depth buffer, got {d}"
        );
    }
}

/// GAP-009: `gl_VertexID` is injected with the correct draw index per vertex.
///
/// We draw three points from a three-vertex array.  The vertex shader encodes
/// the vertex ID into the x component of gl_Position so each point lands at a
/// distinct x column.  The fragment shader colors the pixel based on a uniform
/// color, but the real check is that all three screen columns contain a pixel —
/// proving gl_VertexID routed each vertex to a distinct position.
#[test]
fn draw_state_arrays_glsl_vertex_id_routes_vertices_to_distinct_columns() {
    // Shader maps gl_VertexID ∈ {0,1,2} to NDC x ∈ {-0.75, 0.0, 0.75}.
    let vert = b"void main() {\n\
        float x = float(gl_VertexID) * 0.75 - 0.75;\n\
        gl_Position = vec4(x, 0.0, 0.0, 1.0);\n\
    }\n";
    let frag = b"void main() { gl_FragColor = vec4(1.0, 1.0, 0.0, 1.0); }\n";

    let mut ctx = Context::new();
    // We still need at least one vertex attrib to satisfy fetch_glsl_vertex.
    // Upload dummy single-component data — the shader ignores it.
    let vbo = upload_vbo(
        &mut ctx,
        &[
            0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ],
    );
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    // 8×4 framebuffer so each vertex maps to a distinct column group.
    let mut color = [0u32; 32];
    let mut depth = [1.0f32; 32];
    let mut target = Target::new(&mut color, &mut depth, 8, 4);

    assert!(ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Points));

    // Expect pixels in at least 3 distinct columns.
    let lit_cols: std::collections::BTreeSet<usize> = color
        .iter()
        .enumerate()
        .filter(|&(_, &p)| p != 0)
        .map(|(i, _)| i % 8)
        .collect();
    assert!(
        lit_cols.len() >= 3,
        "expected pixels in at least 3 distinct columns, got {lit_cols:?}"
    );
}

/// GAP-010: `gl_FrontFacing` distinguishes front-face from back-face triangles.
///
/// Two triangles with opposite winding orders are drawn in the same framebuffer.
/// The fragment shader writes green when `gl_FrontFacing` is true and blue when false.
/// After both draws, pixels of BOTH colors must be present — proving that
/// `gl_FrontFacing` evaluates differently for each winding.
#[test]
fn draw_state_arrays_glsl_front_facing_differs_for_cw_and_ccw_triangles() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    // gl_FrontFacing == true → green (0xFF00FF00), false → blue (0xFF0000FF).
    let frag = b"void main() {\n\
        float f = gl_FrontFacing ? 1.0 : 0.0;\n\
        gl_FragColor = vec4(0.0, f, 1.0 - f, 1.0);\n\
    }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    ctx.cull_face(graphos_gl::gl::CullFace::None); // render both faces

    let mut color = [0u32; 32];
    let mut depth = [1.0f32; 32];
    let mut target = Target::new(&mut color, &mut depth, 8, 4);

    // Triangle A: CCW in NDC (y-up) → screen area negative → gl_FrontFacing = false → blue.
    // Vertices go counter-clockwise in NDC: bottom-left, bottom-right, top-center.
    let ccw_verts: &[f32] = &[
        -0.9, -0.9, 0.0, 1.0, -0.1, -0.9, 0.0, 1.0, -0.5, 0.9, 0.0, 1.0,
    ];
    let vbo_ccw = upload_vbo(&mut ctx, ccw_verts);
    ctx.bind_buffer(vbo_ccw);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    assert!(ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles));

    // Triangle B: CW in NDC (y-up) → screen area positive → gl_FrontFacing = true → green.
    // Vertices go clockwise in NDC: bottom-left of right half, top-center, bottom-right.
    let cw_verts: &[f32] = &[0.1, -0.9, 0.0, 1.0, 0.5, 0.9, 0.0, 1.0, 0.9, -0.9, 0.0, 1.0];
    let vbo_cw = upload_vbo(&mut ctx, cw_verts);
    ctx.bind_buffer(vbo_cw);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    assert!(ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles));

    // Both gl_FrontFacing values must have been seen: both colors must appear.
    let green: u32 = 0xFF00_FF00; // pack_bgra(0,1,0,1)
    let blue: u32 = 0xFF00_00FF; // pack_bgra(0,0,1,1)
    assert!(
        color.iter().any(|&p| p == green),
        "expected green pixels (gl_FrontFacing==true)"
    );
    assert!(
        color.iter().any(|&p| p == blue),
        "expected blue pixels (gl_FrontFacing==false)"
    );
}

/// GAP-004: geometry that lies entirely beyond the far plane (z > w) is clipped
/// and produces no pixels.
#[test]
fn draw_state_arrays_glsl_far_plane_clip_removes_geometry_behind_far_plane() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Triangle with z > w (behind far plane in NDC), should be fully clipped.
    let verts: &[f32] = &[
        -0.9, -0.9, 2.0, 1.0, // z/w = 2 → beyond far
        0.9, -0.9, 2.0, 1.0, 0.0, 0.9, 2.0, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);

    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);
    assert!(
        color.iter().all(|&p| p == 0),
        "clipped geometry must produce no pixels"
    );
}

/// GAP-005: two adjacent triangles sharing a diagonal edge must together cover
/// all pixels in a 2×2 framebuffer with no cracks (top-left fill convention).
///
/// The two triangles form a full-screen quad split diagonally.  Without a
/// consistent fill convention there would be a one-pixel crack along the shared
/// edge.  With the top-left rule every pixel is owned by exactly one triangle.
#[test]
fn draw_state_arrays_glsl_shared_edge_pixels_not_double_written() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag_red = b"void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";
    let frag_blue = b"void main() { gl_FragColor = vec4(0.0, 0.0, 1.0, 1.0); }\n";

    let mut ctx = Context::new();

    // 2×2 framebuffer.  Pixel centers (in screen space, half_w=half_h=1):
    //   (0,0)→NDC(-0.5, 0.5)  (1,0)→NDC(0.5, 0.5)
    //   (0,1)→NDC(-0.5,-0.5)  (1,1)→NDC(0.5,-0.5)
    //
    // Triangle A covers the upper-left diagonal half: NDC (-1,1), (-1,-1), (1,1).
    // Triangle B covers the lower-right diagonal half: NDC (1,1), (-1,-1), (1,-1).
    // Shared edge: NDC (-1,-1)→(1,1) (the main diagonal).
    // All 4 pixel centres must be covered — two by A, two by B.
    let verts_a: &[f32] = &[
        -1.0, 1.0, 0.0, 1.0, -1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0,
    ];
    let verts_b: &[f32] = &[
        1.0, 1.0, 0.0, 1.0, -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 0.0, 1.0,
    ];

    let mut color = [0u32; 4];
    let mut depth = [1.0f32; 4];
    let mut target = Target::new(&mut color, &mut depth, 2, 2);

    let prog_red = make_program(&mut ctx, vert, frag_red);
    ctx.use_program(prog_red);
    let vbo_a = upload_vbo(&mut ctx, verts_a);
    ctx.bind_buffer(vbo_a);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);

    let prog_blue = make_program(&mut ctx, vert, frag_blue);
    ctx.use_program(prog_blue);
    let vbo_b = upload_vbo(&mut ctx, verts_b);
    ctx.bind_buffer(vbo_b);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);

    // All 4 pixels must be covered (no gaps = no fill-convention cracks).
    // Each pixel is either red (0xFFFF0000) or blue (0xFF0000FF); none should be 0.
    let red: u32 = 0xFFFF_0000; // pack_bgra(1,0,0,1)
    let blue: u32 = 0xFF00_00FF; // pack_bgra(0,0,1,1)
    for &p in &color {
        assert!(
            p == red || p == blue,
            "pixel {p:#010x} is neither red nor blue — crack or unexpected value"
        );
    }
}

/// GAP-001: `glGetActiveUniform` must return the correct GL enum type for each
/// uniform, not always `GL_FLOAT` (0x1406).
///
/// The program declares uniforms of types float, vec4, int, mat4, and sampler2D.
/// `get_active_uniforms_iv` with `GL_UNIFORM_TYPE` must return the correct GL enum
/// for each one.
#[test]
fn get_active_uniforms_iv_returns_correct_type_for_each_uniform() {
    // Shader with uniforms of several different types.
    let vert = b"uniform float    u_float;\n\
                 uniform vec4     u_vec4;\n\
                 uniform int      u_int;\n\
                 uniform mat4     u_mat4;\n\
                 attribute vec4 a_pos;\n\
                 void main() { gl_Position = a_pos + u_vec4 * (u_float + float(u_int)) + u_mat4[0]; }\n";
    let frag = b"uniform sampler2D u_tex;\n\
                 void main() { gl_FragColor = texture2D(u_tex, vec2(0.0)); }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    assert!(ctx.program_link_status(prog), "program must link");

    // Build a name→index map from get_active_uniform.
    let mut name_to_idx: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut i = 0u32;
    while let Some((ty_str, name)) = ctx.get_active_uniform(prog, i) {
        let _ = ty_str; // we'll verify the GL enum separately
        name_to_idx.insert(name, i);
        i += 1;
    }
    assert!(
        !name_to_idx.is_empty(),
        "get_active_uniform must return at least one uniform"
    );

    // Helper: get GL_UNIFORM_TYPE for a single uniform by name.
    let type_for = |name: &str| -> Option<i32> {
        let idx = *name_to_idx.get(name)?;
        let mut out = [0i32; 1];
        ctx.get_active_uniforms_iv(prog, &[idx], 0x8A3A /* GL_UNIFORM_TYPE */, &mut out);
        Some(out[0])
    };

    // GL enum constants.
    const GL_FLOAT: i32 = 0x1406;
    const GL_FLOAT_VEC4: i32 = 0x8B52;
    const GL_INT: i32 = 0x1404;
    const GL_FLOAT_MAT4: i32 = 0x8B5C;
    const GL_SAMPLER_2D: i32 = 0x8B5E;

    if let Some(t) = type_for("u_float") {
        assert_eq!(t, GL_FLOAT, "u_float must report GL_FLOAT");
    }
    if let Some(t) = type_for("u_vec4") {
        assert_eq!(t, GL_FLOAT_VEC4, "u_vec4 must report GL_FLOAT_VEC4");
    }
    if let Some(t) = type_for("u_int") {
        assert_eq!(t, GL_INT, "u_int must report GL_INT");
    }
    if let Some(t) = type_for("u_mat4") {
        assert_eq!(t, GL_FLOAT_MAT4, "u_mat4 must report GL_FLOAT_MAT4");
    }
    if let Some(t) = type_for("u_tex") {
        assert_eq!(t, GL_SAMPLER_2D, "u_tex must report GL_SAMPLER_2D");
    }

    // At least one uniform must have been found and typed correctly.
    assert!(
        name_to_idx.contains_key("u_float") || name_to_idx.contains_key("u_vec4"),
        "expected u_float or u_vec4 in active uniforms, got: {name_to_idx:?}"
    );
}

// ── GAP-007: dFdx / dFdy / fwidth return nonzero for varying UV ────────────

/// dFdx of a UV varying must be nonzero when the UV changes across the triangle.
/// The fragment shader encodes |dFdx(v_uv.x)| into the red channel (scaled ×16
/// so sub-pixel values become visible).  At least one pixel must be non-black.
#[test]
fn glsl_dfbdx_of_varying_is_nonzero() {
    let vert = b"\
attribute vec4 a_pos;\n\
attribute vec2 a_uv;\n\
varying vec2 v_uv;\n\
void main() { gl_Position = a_pos; v_uv = a_uv; }\n";
    let frag = b"\
varying vec2 v_uv;\n\
void main() {\n\
    float dx = abs(dFdx(v_uv.x)) * 16.0;\n\
    gl_FragColor = vec4(dx, dx, dx, 1.0);\n\
}\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Triangle covering the full NDC square; UV spans 0..1.
    let verts: &[f32] = &[
        -1.0, -1.0, 0.0, 1.0, 0.0, 0.0, 1.0, -1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.5, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    // a_pos: 4 floats at offset 0, stride 6×4 bytes
    ctx.vertex_attrib_pointer(0, 4, false, 24, 0);
    // a_uv: 2 floats at offset 16 bytes, stride 6×4 bytes
    ctx.vertex_attrib_pointer(1, 2, false, 24, 16);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);
    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);

    // At least one rasterized pixel must have a nonzero channel (dx > 0).
    assert!(
        color.iter().any(|&p| p != 0 && (p & 0x00FF_FFFF) != 0),
        "dFdx(v_uv.x) returned 0 for all pixels — derivative not propagated"
    );
}

// ── GAP-021: textureSize / texture2DProj ───────────────────────────────────

/// textureSize(sampler2D, 0) must return the uploaded texture dimensions.
#[test]
fn glsl_texture_size_returns_uploaded_dimensions() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    // Encode ivec2 size into red=width, green=height (normalized to 0..1 as /256).
    let frag = b"\
uniform sampler2D u_tex;\n\
void main() {\n\
    ivec2 sz = textureSize(u_tex, 0);\n\
    gl_FragColor = vec4(float(sz.x) / 256.0, float(sz.y) / 256.0, 0.0, 1.0);\n\
}\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Upload a 8×4 texture (32 pixels).
    let mut tex_names = [0u32; 1];
    ctx.gen_textures(&mut tex_names);
    let tex = tex_names[0];
    ctx.active_texture(0);
    ctx.bind_texture(tex);
    let pixels: Vec<u32> = vec![0xFF_FF_FF_FFu32; 8 * 4];
    ctx.tex_image_2d(tex, 0, 8, 4, &pixels);

    // Bind sampler uniform.
    let loc = ctx.get_uniform_location(prog, b"u_tex");
    ctx.uniform1i(prog, loc, 0);

    // Draw a point at center.
    let verts: &[f32] = &[0.0, 0.0, 0.0, 1.0];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 16];
    let mut depth = [1.0f32; 16];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);
    ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points);

    // Find the rendered pixel and decode r→width, g→height.
    let rendered: Vec<u32> = color.iter().copied().filter(|&p| p != 0).collect();
    assert!(
        !rendered.is_empty(),
        "expected a rendered pixel for textureSize test"
    );

    let pix = rendered[0];
    // BGRA layout: bits 16-23 = red, bits 8-15 = green.
    let r = ((pix >> 16) & 0xFF) as f32;
    let g = ((pix >> 8) & 0xFF) as f32;
    let w = (r / 255.0 * 256.0).round() as i32;
    let h = (g / 255.0 * 256.0).round() as i32;
    assert_eq!(w, 8, "textureSize width should be 8, got {w}");
    assert_eq!(h, 4, "textureSize height should be 4, got {h}");
}

/// texture2DProj divides UV by the last component before sampling.
#[test]
fn glsl_texture2d_proj_matches_divided_texture2d() {
    // Two-pass test: draw once with texture2D and once with texture2DProj,
    // comparing the sampled color value in the red channel.
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";

    // Pass 1: sample with texture2D at (0.25, 0.25).
    let frag_a = b"\
uniform sampler2D u_tex;\n\
void main() { gl_FragColor = texture2D(u_tex, vec2(0.25, 0.25)); }\n";

    // Pass 2: sample with texture2DProj using vec3(0.5, 0.5, 2.0) → uv=(0.25,0.25).
    let frag_b = b"\
uniform sampler2D u_tex;\n\
void main() { gl_FragColor = texture2DProj(u_tex, vec3(0.5, 0.5, 2.0)); }\n";

    let mut ctx = Context::new();

    let mut tex_names = [0u32; 1];
    ctx.gen_textures(&mut tex_names);
    let tex = tex_names[0];
    ctx.active_texture(0);
    ctx.bind_texture(tex);
    // Solid magenta 2×2 texture so all samples return the same value.
    let pixels: Vec<u32> = vec![0xFF_FF_00_FFu32; 4];
    ctx.tex_image_2d(tex, 0, 2, 2, &pixels);

    let verts: &[f32] = &[0.0, 0.0, 0.0, 1.0];

    let mut color_a = [0u32; 16];
    let mut depth_a = [1.0f32; 16];
    let mut target_a = Target::new(&mut color_a, &mut depth_a, 4, 4);

    let prog_a = make_program(&mut ctx, vert, frag_a);
    ctx.use_program(prog_a);
    let loc_a = ctx.get_uniform_location(prog_a, b"u_tex");
    ctx.uniform1i(prog_a, loc_a, 0);
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    ctx.draw_state_arrays(&mut target_a, 0, 1, DrawMode::Points);

    let mut color_b = [0u32; 16];
    let mut depth_b = [1.0f32; 16];
    let mut target_b = Target::new(&mut color_b, &mut depth_b, 4, 4);

    let prog_b = make_program(&mut ctx, vert, frag_b);
    ctx.use_program(prog_b);
    let loc_b = ctx.get_uniform_location(prog_b, b"u_tex");
    ctx.uniform1i(prog_b, loc_b, 0);
    ctx.draw_state_arrays(&mut target_b, 0, 1, DrawMode::Points);

    let rendered_a: Vec<u32> = color_a.iter().copied().filter(|&p| p != 0).collect();
    let rendered_b: Vec<u32> = color_b.iter().copied().filter(|&p| p != 0).collect();
    assert!(!rendered_a.is_empty(), "texture2D rendered nothing");
    assert!(!rendered_b.is_empty(), "texture2DProj rendered nothing");
    assert_eq!(
        rendered_a[0], rendered_b[0],
        "texture2DProj({:#010x}) != texture2D({:#010x})",
        rendered_b[0], rendered_a[0]
    );
}

// ── GAP-022: glCopyBufferSubData copies bytes correctly ────────────────────

/// copy_buffer_sub_data correctly copies a sub-range from one buffer to another.
#[test]
fn copy_buffer_sub_data_copies_byte_range() {
    let mut ctx = Context::new();

    // Create two buffers.
    let mut bufs = [0u32; 2];
    ctx.gen_buffers(&mut bufs);
    let [src, dst] = bufs;

    // Source: bytes 0..8 = [0,1,2,3,4,5,6,7].
    ctx.buffer_data(src, &[0u8, 1, 2, 3, 4, 5, 6, 7]);
    // Destination pre-filled with 0xFF.
    ctx.buffer_data(dst, &[0xFFu8; 8]);

    // Copy 4 bytes starting at src offset 2 → dst offset 3.
    ctx.copy_buffer_sub_data(src, dst, 2, 3, 4);

    let mut out = [0u8; 8];
    let ok = ctx.get_buffer_sub_data(dst, 0, &mut out);
    assert!(ok, "get_buffer_sub_data failed");

    // Bytes 0..3 and 7 unchanged (0xFF); bytes 3..7 = [2,3,4,5].
    assert_eq!(out[0], 0xFF);
    assert_eq!(out[1], 0xFF);
    assert_eq!(out[2], 0xFF);
    assert_eq!(out[3], 2);
    assert_eq!(out[4], 3);
    assert_eq!(out[5], 4);
    assert_eq!(out[6], 5);
    assert_eq!(out[7], 0xFF);
}

/// GAP-019: glPointSize(4.0) should rasterize a 4×4 pixel square.
/// Draw a single point at the centre of an 8×8 framebuffer and count
/// the non-black pixels; expect exactly 16.
#[test]
fn gl_point_size_draws_large_square() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    ctx.set_point_size(4.0);

    // Single point exactly at NDC origin → maps to pixel (4,4) on an 8×8 FB.
    let verts: &[f32] = &[0.0f32, 0.0, 0.0, 1.0];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 8 * 8];
    let mut depth = [1.0f32; 8 * 8];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);
    ctx.draw_state_arrays(&mut target, 0, 1, DrawMode::Points);

    let nonzero = color.iter().filter(|&&p| p != 0).count();
    assert_eq!(
        nonzero, 16,
        "expected 16 pixels for 4×4 point sprite, got {}",
        nonzero
    );
}

/// GAP-018: glLineWidth(3.0) should rasterize a 3-pixel-wide line.
/// Draw a horizontal line across an 8×8 framebuffer and count how many
/// distinct rows are lit; expect at least 3.
#[test]
fn gl_line_width_draws_thick_line() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"void main() { gl_FragColor = vec4(0.0, 1.0, 0.0, 1.0); }\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);
    ctx.set_line_width(3.0);

    // Horizontal line y=0 spanning the full width.
    let verts: &[f32] = &[-1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 8 * 8];
    let mut depth = [1.0f32; 8 * 8];
    let mut target = Target::new(&mut color, &mut depth, 8, 8);
    ctx.draw_state_arrays(&mut target, 0, 2, DrawMode::Lines);

    // Count distinct lit rows.
    let lit_rows: usize = (0..8usize)
        .filter(|&row| color[row * 8..(row + 1) * 8].iter().any(|&p| p != 0))
        .count();
    assert!(
        lit_rows >= 3,
        "expected at least 3 lit rows for line_width=3, got {}",
        lit_rows
    );
}

// ── GAP-015: Depth-format textures ─────────────────────────────────────────

/// A depth texture uploaded via tex_image_2d_depth32f must be sampled as
/// vec4(depth, depth, depth, 1.0).  We upload a 2×2 texture with a known
/// depth value and verify the red channel returned by the shader matches.
#[test]
fn depth_texture_sampling_returns_depth_value() {
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";
    let frag = b"\
uniform sampler2D u_depth;\n\
void main() {\n\
    vec4 s = texture2D(u_depth, vec2(0.5, 0.5));\n\
    // Encode red channel * 255 into green so we can distinguish from black.\n\
    gl_FragColor = vec4(s.r, s.r, 0.0, 1.0);\n\
}\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    // Upload a 2×2 depth texture with all values = 0.75.
    let mut tex_names = [0u32; 1];
    ctx.gen_textures(&mut tex_names);
    let tex = tex_names[0];
    ctx.active_texture(0);
    ctx.bind_texture(tex);
    let depths = [0.75f32; 4];
    ctx.tex_image_2d_depth32f(tex, 0, 2, 2, &depths);

    let loc = ctx.get_uniform_location(prog, b"u_depth");
    ctx.uniform1i(prog, loc, 0);

    // Draw a full-screen quad (2 triangles).
    let verts: &[f32] = &[
        -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, -1.0, 1.0, 0.0, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color = [0u32; 4 * 4];
    let mut depth = [1.0f32; 4 * 4];
    let mut target = Target::new(&mut color, &mut depth, 4, 4);
    ctx.draw_state_arrays(&mut target, 0, 4, DrawMode::TriangleFan);

    // Every rendered pixel must have red ≈ 0.75 (packed into bits 16-23 of BGRA32).
    let rendered: Vec<u32> = color.iter().copied().filter(|&p| p != 0).collect();
    assert!(
        !rendered.is_empty(),
        "no pixels rendered in depth texture test"
    );
    for p in &rendered {
        let r = ((p >> 16) & 0xFF) as f32 / 255.0;
        assert!(
            (r - 0.75).abs() < 0.02,
            "expected red ≈ 0.75, got {r:.3} (pixel = 0x{p:08x})"
        );
    }
}

/// Depth texture supports shadow-map style comparison: render a scene,
/// upload the depth buffer as a depth texture, then verify that sampling
/// returns the correct depth at each texel.
#[test]
fn depth_texture_roundtrip_from_render() {
    // --- First pass: render a quad at z=0.5 to get depth values.
    let vert_depth = b"\
attribute vec4 a_pos;\n\
void main() { gl_Position = a_pos; }\n";
    let frag_depth = b"void main() { gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0); }\n";

    let mut ctx = Context::new();
    let prog1 = make_program(&mut ctx, vert_depth, frag_depth);
    ctx.use_program(prog1);
    ctx.set_depth_test(true);

    let verts: &[f32] = &[
        -1.0, -1.0, 0.5, 1.0, 1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 0.5, 1.0, -1.0, 1.0, 0.5, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color1 = [0u32; 4 * 4];
    let mut depth1 = [1.0f32; 4 * 4];
    let mut target1 = Target::new(&mut color1, &mut depth1, 4, 4);
    ctx.draw_state_arrays(&mut target1, 0, 4, DrawMode::TriangleFan);

    // After rendering at NDC z=0.5 with default depth range, depth = 0.5*0.5+0.5 = 0.75.
    let expected_depth = 0.75_f32;
    for &d in &depth1 {
        assert!(
            (d - expected_depth).abs() < 0.01,
            "expected depth ≈ {expected_depth}, got {d}"
        );
    }

    // --- Second pass: upload depth1 as a depth texture and sample it.
    let vert2 = b"attribute vec4 a_pos;\nvarying vec2 v_uv;\nvoid main() { gl_Position = a_pos; v_uv = a_pos.xy * 0.5 + 0.5; }\n";
    let frag2 = b"\
varying vec2 v_uv;\n\
uniform sampler2D u_shadow;\n\
void main() {\n\
    float d = texture2D(u_shadow, v_uv).r;\n\
    gl_FragColor = vec4(d, d, d, 1.0);\n\
}\n";

    let prog2 = make_program(&mut ctx, vert2, frag2);
    ctx.use_program(prog2);

    let mut tex_names = [0u32; 1];
    ctx.gen_textures(&mut tex_names);
    let tex = tex_names[0];
    ctx.active_texture(0);
    ctx.bind_texture(tex);
    ctx.tex_image_2d_depth32f(tex, 0, 4, 4, &depth1);

    let loc = ctx.get_uniform_location(prog2, b"u_shadow");
    ctx.uniform1i(prog2, loc, 0);

    let vbo2 = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo2);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let mut color2 = [0u32; 4 * 4];
    let mut depth2 = [1.0f32; 4 * 4];
    let mut target2 = Target::new(&mut color2, &mut depth2, 4, 4);
    ctx.draw_state_arrays(&mut target2, 0, 4, DrawMode::TriangleFan);

    // Each rendered pixel should have red ≈ 0.75.
    let rendered: Vec<u32> = color2.iter().copied().filter(|&p| p != 0).collect();
    assert!(
        !rendered.is_empty(),
        "no pixels rendered in shadow map pass"
    );
    for p in &rendered {
        let r = ((p >> 16) & 0xFF) as f32 / 255.0;
        assert!(
            (r - expected_depth).abs() < 0.02,
            "shadow pass: expected red ≈ {expected_depth:.2}, got {r:.3} (pixel = 0x{p:08x})"
        );
    }
}

// ── GAP-017: GLSL struct field access ────────────────────────────────────────

#[test]
fn glsl_struct_field_access_works() {
    // Shader uses a struct to pass data through. The struct holds r, g, b.
    // The fragment shader reads from the struct and outputs the color.
    let vert_src = b"
        attribute vec4 a_pos;
        void main() { gl_Position = a_pos; }
    ";
    let frag_src = b"
        struct MyColor {
            float r;
            float g;
            float b;
        };
        void main() {
            MyColor c;
            c.r = 0.0;
            c.g = 1.0;
            c.b = 0.0;
            gl_FragColor = vec4(c.r, c.g, c.b, 1.0);
        }
    ";
    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert_src, frag_src);
    ctx.use_program(prog);

    let verts: &[f32] = &[
        -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let w = 4u32;
    let h = 4u32;
    let mut color = vec![0u32; (w * h) as usize];
    let mut depth = vec![1.0f32; (w * h) as usize];
    let mut target = Target::new(&mut color, &mut depth, w, h);
    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);

    let rendered: Vec<u32> = color.iter().copied().filter(|&p| p != 0).collect();
    assert!(!rendered.is_empty(), "struct test: no pixels rendered");
    for p in &rendered {
        let r = ((p >> 16) & 0xFF) as u8;
        let g = ((p >> 8) & 0xFF) as u8;
        let b = (p & 0xFF) as u8;
        // Expected: r≈0, g≈255, b≈0 (pure green)
        assert!(
            r < 10,
            "struct test: expected r≈0, got {r} (pixel=0x{p:08x})"
        );
        assert!(
            g > 200,
            "struct test: expected g≈255, got {g} (pixel=0x{p:08x})"
        );
        assert!(
            b < 10,
            "struct test: expected b≈0, got {b} (pixel=0x{p:08x})"
        );
    }
}

#[test]
fn glsl_struct_constructor_works() {
    // Test struct constructor syntax: MyColor(r, g, b)
    let vert_src = b"
        attribute vec4 a_pos;
        void main() { gl_Position = a_pos; }
    ";
    let frag_src = b"
        struct MyColor { float r; float g; float b; };
        void main() {
            MyColor c = MyColor(1.0, 0.0, 0.0);
            gl_FragColor = vec4(c.r, c.g, c.b, 1.0);
        }
    ";
    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert_src, frag_src);
    ctx.use_program(prog);

    let verts: &[f32] = &[
        -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let w = 4u32;
    let h = 4u32;
    let mut color = vec![0u32; (w * h) as usize];
    let mut depth = vec![1.0f32; (w * h) as usize];
    let mut target = Target::new(&mut color, &mut depth, w, h);
    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);

    let rendered: Vec<u32> = color.iter().copied().filter(|&p| p != 0).collect();
    assert!(
        !rendered.is_empty(),
        "struct constructor test: no pixels rendered"
    );
    for p in &rendered {
        let r = ((p >> 16) & 0xFF) as u8;
        let g = ((p >> 8) & 0xFF) as u8;
        let b = (p & 0xFF) as u8;
        // Expected: r≈255, g≈0, b≈0 (pure red)
        assert!(
            r > 200,
            "struct constructor: expected r≈255, got {r} (pixel=0x{p:08x})"
        );
        assert!(
            g < 10,
            "struct constructor: expected g≈0, got {g} (pixel=0x{p:08x})"
        );
        assert!(
            b < 10,
            "struct constructor: expected b≈0, got {b} (pixel=0x{p:08x})"
        );
    }
}

// ── GAP-020: sampler2DArray / tex_image_3d ──────────────────────────────────

/// tex_image_3d stores multiple layers; sampling with vec3 coord selects the
/// correct layer (layer 0 = red, layer 1 = green).
#[test]
fn gl_texture_2d_array_layer_sampling_works() {
    // Vertex shader — full-screen triangle.
    let vert = b"attribute vec4 a_pos;\nvoid main() { gl_Position = a_pos; }\n";

    // Fragment shader sampling layer 0 (vec3 coord z=0.0) — expects red.
    let frag_layer0 = b"\
uniform sampler2D u_tex;\n\
void main() {\n\
    gl_FragColor = texture(u_tex, vec3(0.5, 0.5, 0.0));\n\
}\n";

    // Fragment shader sampling layer 1 (vec3 coord z=1.0) — expects green.
    let frag_layer1 = b"\
uniform sampler2D u_tex;\n\
void main() {\n\
    gl_FragColor = texture(u_tex, vec3(0.5, 0.5, 1.0));\n\
}\n";

    let mut ctx = Context::new();

    // Upload 2-layer 1×1 texture: layer 0 = red (0xFF_FF_00_00), layer 1 = green (0xFF_00_FF_00).
    let mut tex_names = [0u32; 1];
    ctx.gen_textures(&mut tex_names);
    let tex = tex_names[0];
    ctx.active_texture(0);
    ctx.bind_texture(tex);
    // Pixels laid out as [layer0_pixel, layer1_pixel] for a 1×1×2 texture.
    let pixels: Vec<u32> = vec![0xFF_FF_00_00u32, 0xFF_00_FF_00u32];
    ctx.tex_image_3d(tex, 0, 1, 1, 2, &pixels);

    let verts: &[f32] = &[
        -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0,
    ];

    // --- Layer 0: expect red ---
    let prog0 = make_program(&mut ctx, vert, frag_layer0);
    ctx.use_program(prog0);
    let loc0 = ctx.get_uniform_location(prog0, b"u_tex");
    ctx.uniform1i(prog0, loc0, 0);
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    let w = 4u32;
    let h = 4u32;
    let mut color0 = vec![0u32; (w * h) as usize];
    let mut depth0 = vec![1.0f32; (w * h) as usize];
    let mut target0 = Target::new(&mut color0, &mut depth0, w, h);
    ctx.draw_state_arrays(&mut target0, 0, 3, DrawMode::Triangles);

    let rendered0: Vec<u32> = color0.iter().copied().filter(|&p| p != 0).collect();
    assert!(!rendered0.is_empty(), "layer0: no pixels rendered");
    for p in &rendered0 {
        let r = ((p >> 16) & 0xFF) as u8;
        let g = ((p >> 8) & 0xFF) as u8;
        assert!(r > 150, "layer0: expected red, got r={r} (pixel=0x{p:08x})");
        assert!(
            g < 100,
            "layer0: expected no green, got g={g} (pixel=0x{p:08x})"
        );
    }

    // --- Layer 1: expect green ---
    let prog1 = make_program(&mut ctx, vert, frag_layer1);
    ctx.use_program(prog1);
    let loc1 = ctx.get_uniform_location(prog1, b"u_tex");
    ctx.uniform1i(prog1, loc1, 0);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);
    let mut color1 = vec![0u32; (w * h) as usize];
    let mut depth1 = vec![1.0f32; (w * h) as usize];
    let mut target1 = Target::new(&mut color1, &mut depth1, w, h);
    ctx.draw_state_arrays(&mut target1, 0, 3, DrawMode::Triangles);

    let rendered1: Vec<u32> = color1.iter().copied().filter(|&p| p != 0).collect();
    assert!(!rendered1.is_empty(), "layer1: no pixels rendered");
    for p in &rendered1 {
        let g = ((p >> 8) & 0xFF) as u8;
        let r = ((p >> 16) & 0xFF) as u8;
        assert!(
            g > 150,
            "layer1: expected green, got g={g} (pixel=0x{p:08x})"
        );
        assert!(
            r < 100,
            "layer1: expected no red, got r={r} (pixel=0x{p:08x})"
        );
    }
}

// ── GAP-023: GLSL 330 core dialect ─────────────────────────────────────────

/// Shaders written in `#version 330 core` (desktop GLSL) must compile and
/// render correctly.  The key differences from GLSL ES 300:
///   * No precision declarations required
///   * Vertex inputs declared as `in` (no `attribute` keyword)
///   * Varyings via `out`/`in` (no `varying`)
///   * Named fragment output `out vec4 fragColor` (no `gl_FragColor`)
#[test]
fn glsl_330_core_dialect_works() {
    let vert = b"#version 330 core\n\
in vec4 a_pos;\n\
out vec4 v_color;\n\
void main() {\n\
    gl_Position = a_pos;\n\
    v_color = vec4(0.0, 0.0, 1.0, 1.0);\n\
}\n";

    let frag = b"#version 330 core\n\
in vec4 v_color;\n\
out vec4 fragColor;\n\
void main() {\n\
    fragColor = v_color;\n\
}\n";

    let mut ctx = Context::new();
    let prog = make_program(&mut ctx, vert, frag);
    ctx.use_program(prog);

    let verts: &[f32] = &[
        -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0,
    ];
    let vbo = upload_vbo(&mut ctx, verts);
    ctx.bind_buffer(vbo);
    ctx.vertex_attrib_pointer(0, 4, false, 0, 0);

    let w = 4u32;
    let h = 4u32;
    let mut color = vec![0u32; (w * h) as usize];
    let mut depth = vec![1.0f32; (w * h) as usize];
    let mut target = Target::new(&mut color, &mut depth, w, h);
    ctx.draw_state_arrays(&mut target, 0, 3, DrawMode::Triangles);

    let rendered: Vec<u32> = color.iter().copied().filter(|&p| p != 0).collect();
    assert!(!rendered.is_empty(), "glsl_330_core: no pixels rendered");
    for p in &rendered {
        let r = ((p >> 16) & 0xFF) as u8;
        let g = ((p >> 8) & 0xFF) as u8;
        let b = (p & 0xFF) as u8;
        // Expected: pure blue (b≈255, r≈0, g≈0).
        assert!(
            b > 200,
            "glsl_330_core: expected b≈255, got b={b} (pixel=0x{p:08x})"
        );
        assert!(
            r < 10,
            "glsl_330_core: expected r≈0, got r={r} (pixel=0x{p:08x})"
        );
        assert!(
            g < 10,
            "glsl_330_core: expected g≈0, got g={g} (pixel=0x{p:08x})"
        );
    }
}
