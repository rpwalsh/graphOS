// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::str;

use graphos_compositor::{CompositorState, ThemeTone};
use graphos_gl::compositor::{OffscreenWindowRenderer, WindowSurfaceTarget};
use graphos_gl::math::{Vec2, Vec3, Vec4};
use graphos_gl::mesh::{StdVarying, Vertex, build_sphere};
use graphos_gl::shader::Shader;
use graphos_gl::text::{GlyphAtlas, TextAlign, TextStyle, append_text, append_text_aligned, append_text_lines};
use graphos_gl::texture::Texture;
use graphos_gl::ui::{Rect, UiBatch, UiRenderer, UiVertex};
use graphos_gl::{
    Context, DrawMode, FilterMode, IndexType, Mat4, Target, WrapMode, MAX_TEXTURES,
};
use libm::{cosf, sinf};

use crate::runtime;

fn create_window_surface_with_trace(
    ctx: &mut Context,
    width: u32,
    height: u32,
    stage_id: u8,
) -> Option<WindowSurfaceTarget> {
    if width == 0 || height == 0 {
        return None;
    }

    let mut textures = [0u32; 1];
    if ctx.gen_textures(&mut textures) != 1 {
        return None;
    }
    let color_texture = textures[0];

    ctx.tex_storage_2d(color_texture, width, height, 1);

    ctx.tex_parameter_wrap(color_texture, WrapMode::ClampToEdge, WrapMode::ClampToEdge);
    ctx.tex_parameter_filter(color_texture, FilterMode::Linear, FilterMode::Linear);

    let mut fbos = [0u32; 1];
    if ctx.gen_framebuffers(&mut fbos) != 1 {
        ctx.delete_textures(&[color_texture]);
        return None;
    }
    let fbo = fbos[0];
    ctx.framebuffer_texture_2d(fbo, graphos_gl::Attachment::Color(0), color_texture, 0);

    let status = ctx.check_framebuffer_status(fbo);
    if status != graphos_gl::FramebufferStatus::Complete {
        ctx.delete_framebuffers(&[fbo]);
        ctx.delete_textures(&[color_texture]);
        return None;
    }
    let _ = stage_id;

    Some(WindowSurfaceTarget {
        fbo,
        color_texture,
        width,
        height,
    })
}

const fn empty_window_target() -> WindowSurfaceTarget {
    WindowSurfaceTarget {
        fbo: 0,
        color_texture: 0,
        width: 0,
        height: 0,
    }
}

const WINDOW_COUNT: usize = 4;
const SPHERE_LON: u32 = 18;
const SPHERE_LAT: u32 = 14;
const SPHERE_VERTS: usize = ((SPHERE_LON + 1) * (SPHERE_LAT + 1)) as usize;
const SPHERE_INDS: usize = (SPHERE_LON * SPHERE_LAT * 6) as usize;
const WINDOW_GRID_X: usize = 6;
const WINDOW_GRID_Y: usize = 5;
const CAMERA_Z: f32 = 1600.0;
const CAMERA_BASELINE_Y: f32 = 0.60;
const REFLECTION_PLANE_Y: f32 = -264.0;

#[derive(Clone, Copy)]
struct GlowSphereShader {
    mvp: Mat4,
    model: Mat4,
    tint: Vec3,
    glow: Vec3,
}

impl Shader for GlowSphereShader {
    type Vertex = Vertex;
    type Varying = StdVarying;

    fn vertex(&self, v: &Self::Vertex) -> (Vec4, Self::Varying) {
        let world = self.model.mul_vec4(v.pos.extend(1.0));
        let n = self.model.upper3_transposed();
        let normal = Vec3::new(
            n[0].x * v.normal.x + n[1].x * v.normal.y + n[2].x * v.normal.z,
            n[0].y * v.normal.x + n[1].y * v.normal.y + n[2].y * v.normal.z,
            n[0].z * v.normal.x + n[1].z * v.normal.y + n[2].z * v.normal.z,
        )
        .normalize();
        (
            self.mvp.mul_vec4(v.pos.extend(1.0)),
            StdVarying {
                world_pos: world.xyz(),
                normal,
                uv: v.uv,
                color: self.tint,
            },
        )
    }

    fn fragment(&self, v: &Self::Varying) -> Option<Vec4> {
        let n = v.normal.normalize();
        let light_a = Vec3::new(-0.48, 0.72, 0.45).normalize();
        let light_b = Vec3::new(0.62, -0.28, 0.74).normalize();
        let key = (n.dot(light_a)).max(0.0);
        let bounce = (n.dot(light_b)).max(0.0) * 0.45;
        let view = Vec3::new(0.0, 0.0, 1.0);
        let rim = (1.0 - n.dot(view).max(0.0)).max(0.0);
        let rim2 = rim * rim;
        let rgb = self.tint * (0.24 + key * 0.78 + bounce) + self.glow * (rim2 * 0.72);
        Some(Vec4::new(
            rgb.z.clamp(0.0, 1.0),
            rgb.y.clamp(0.0, 1.0),
            rgb.x.clamp(0.0, 1.0),
            1.0,
        ))
    }
}

#[derive(Clone, Copy)]
struct Palette {
    sky_top: u32,
    sky_bottom: u32,
    glass_top: Vec4,
    glass_bottom: Vec4,
    panel_fill: Vec4,
    panel_fill_alt: Vec4,
    panel_border: Vec4,
    copper: Vec4,
    copper_soft: Vec4,
    cyan: Vec4,
    cyan_soft: Vec4,
    text: Vec4,
    text_muted: Vec4,
    positive: Vec4,
}

#[derive(Clone, Copy)]
struct SpatialWindowSpec {
    texture: u32,
    title: &'static str,
    subtitle: &'static str,
    center: Vec3,
    size: Vec2,
    yaw: f32,
    pitch: f32,
    opacity: f32,
    accent: Vec4,
    focused: bool,
    sort_key: i32,
}

#[derive(Clone, Copy)]
struct ProjectedWindow {
    texture: u32,
    title: &'static str,
    subtitle: &'static str,
    corners: [Vec2; 4],
    reflection: Option<[Vec2; 4]>,
    bounds: Rect,
    opacity: f32,
    accent: Vec4,
    focused: bool,
    sort_key: i32,
}

pub struct CompositorRenderer {
    width: u32,
    height: u32,
    ctx: Context,
    depth: Vec<f32>,
    atlas: GlyphAtlas,
    atlas_texture: u32,
    offscreen: OffscreenWindowRenderer,
    window_targets: [WindowSurfaceTarget; WINDOW_COUNT],
    window_batches: [UiBatch; WINDOW_COUNT],
    backdrop_batch: UiBatch,
    final_batch: UiBatch,
    sphere_vertices: Vec<Vertex>,
    sphere_index_count: usize,
    sphere_ebo: u32,
    rich_ready: bool,
    rich_disabled: bool,
}

impl CompositorRenderer {
    pub fn new(width: u32, height: u32) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }

        let mut ctx = Context::new();
        ctx.viewport(0, 0, width as i32, height as i32);
        ctx.enable_depth_test(true);
        ctx.depth_mask(true);
        ctx.clear_depth_value(1.0);

        Some(Self {
            width,
            height,
            ctx,
            depth: Vec::new(),
            atlas: GlyphAtlas::ascii_8x8(),
            atlas_texture: 0,
            offscreen: OffscreenWindowRenderer::new(),
            window_targets: core::array::from_fn(|_| empty_window_target()),
            window_batches: core::array::from_fn(|_| UiBatch::new()),
            backdrop_batch: UiBatch::new(),
            final_batch: UiBatch::new(),
            sphere_vertices: Vec::new(),
            sphere_index_count: 0,
            sphere_ebo: 0,
            rich_ready: false,
            rich_disabled: false,
        })
    }

    pub fn render_frame(
        &mut self,
        pixels: &mut [u32],
        state: &CompositorState,
        frame: u32,
    ) -> bool {
        let trace = frame == 0;
        let pixel_count = (self.width as usize).saturating_mul(self.height as usize);
        if pixels.len() < pixel_count {
            return false;
        }
        if !self.ensure_rich_renderer(pixel_count, frame, trace) {
            self.render_fallback_frame(pixels, state, frame);
            return true;
        }
        if self.render_rich_frame(pixels, state, frame, trace) {
            return true;
        }

        runtime::write_line(
            b"[compositor] rich renderer failed during frame; reverting to fallback mode\n",
        );
        self.rich_ready = false;
        self.rich_disabled = true;
        self.render_fallback_frame(pixels, state, frame);
        true
    }

    fn ensure_rich_renderer(&mut self, pixel_count: usize, frame: u32, trace: bool) -> bool {
        if self.rich_ready {
            return true;
        }

        if self.rich_disabled {
            return false;
        }

        if frame == 0 {
            if trace {
                runtime::write_line(
                    b"[compositor][frame0] deferring rich renderer bootstrap until post-claim frame\n",
                );
            }
            return false;
        }

        if self.depth.len() < pixel_count {
            let additional = pixel_count.saturating_sub(self.depth.len());
            if self.depth.try_reserve_exact(additional).is_err() {
                runtime::write_line(
                    b"[compositor] depth allocation failed; staying in fallback mode\n",
                );
                self.rich_disabled = true;
                return false;
            }
            self.depth.resize(pixel_count, 1.0);
            runtime::write_line(b"[compositor] depth buffer ready\n");
        }

        if !self.init_rich_renderer() {
            runtime::write_line(
                b"[compositor] rich renderer bootstrap failed; staying in fallback mode\n",
            );
            self.rich_disabled = true;
            return false;
        }

        self.rich_ready = true;
        runtime::write_line(b"[compositor] rich renderer ready\n");
        true
    }

    fn init_rich_renderer(&mut self) -> bool {
        if self.atlas_texture != 0 && self.sphere_ebo != 0 {
            return true;
        }

        let mut tex_names = [0u32; 1];
        if self.ctx.gen_textures(&mut tex_names) != 1 {
            return false;
        }
        let atlas_texture = tex_names[0];
        self.ctx
            .tex_storage_2d(atlas_texture, self.atlas.width, self.atlas.height, 1);
        self.ctx.tex_image_2d(
            atlas_texture,
            0,
            self.atlas.width,
            self.atlas.height,
            self.atlas.pixels.as_slice(),
        );
        self.ctx.tex_parameter_wrap(
            atlas_texture,
            WrapMode::ClampToEdge,
            WrapMode::ClampToEdge,
        );
        self.ctx.tex_parameter_filter(
            atlas_texture,
            FilterMode::Nearest,
            FilterMode::Nearest,
        );
        self.atlas_texture = atlas_texture;

        let sizes = window_target_sizes(self.width, self.height);
        let surface_0 =
            create_window_surface_with_trace(&mut self.ctx, sizes[0].0, sizes[0].1, 0);
        let surface_1 =
            create_window_surface_with_trace(&mut self.ctx, sizes[1].0, sizes[1].1, 1);
        let surface_2 =
            create_window_surface_with_trace(&mut self.ctx, sizes[2].0, sizes[2].1, 2);
        let surface_3 =
            create_window_surface_with_trace(&mut self.ctx, sizes[3].0, sizes[3].1, 3);
        let (
            Some(surface_0),
            Some(surface_1),
            Some(surface_2),
            Some(surface_3),
        ) = (surface_0, surface_1, surface_2, surface_3)
        else {
            return false;
        };
        self.window_targets = [surface_0, surface_1, surface_2, surface_3];
        runtime::write_line(b"[compositor][new] surfaces ready\n");

        let mut buffer_names = [0u32; 1];
        if self.ctx.gen_buffers(&mut buffer_names) != 1 {
            return false;
        }
        self.sphere_ebo = buffer_names[0];
        runtime::write_line(b"[compositor][new] ebo ready\n");

        let mut sphere_vertices = vec![
            Vertex::new(Vec3::ZERO, Vec3::ZERO, Vec2::new(0.0, 0.0), Vec3::ONE);
            SPHERE_VERTS
        ];
        let mut sphere_indices = vec![0u32; SPHERE_INDS];
        let (vertex_count, index_count) = build_sphere(
            &mut sphere_vertices,
            &mut sphere_indices,
            SPHERE_LON,
            SPHERE_LAT,
            Vec3::ONE,
        );
        runtime::write_line(b"[compositor][new] sphere ready\n");
        sphere_vertices.truncate(vertex_count);
        let index_bytes = unsafe {
            core::slice::from_raw_parts(
                sphere_indices.as_ptr() as *const u8,
                index_count * core::mem::size_of::<u32>(),
            )
        };
        self.ctx.buffer_data(self.sphere_ebo, index_bytes);
        self.ctx.bind_element_buffer(self.sphere_ebo);
        self.sphere_vertices = sphere_vertices;
        self.sphere_index_count = index_count;
        runtime::write_line(b"[compositor][new] sphere uploaded\n");

        true
    }

    fn render_rich_frame(
        &mut self,
        pixels: &mut [u32],
        state: &CompositorState,
        frame: u32,
        trace: bool,
    ) -> bool {
        let pixel_count = (self.width as usize).saturating_mul(self.height as usize);
        if self.depth.len() < pixel_count {
            return false;
        }
        if trace {
            runtime::write_line(b"[compositor][frame0] buffers ready\n");
        }

        let palette = palette_for(state.tone);
        let telemetry = state.telemetry();

        pixels[..pixel_count].fill(palette.sky_bottom);
        self.depth[..pixel_count].fill(1.0);
        self.ctx.viewport(0, 0, self.width as i32, self.height as i32);
        self.ctx.enable_depth_test(true);
        self.ctx.depth_mask(true);
        self.ctx.bind_element_buffer(self.sphere_ebo);
        if trace {
            runtime::write_line(b"[compositor][frame0] state primed\n");
        }

        self.populate_window_batches(state, telemetry, palette, frame);
        if trace {
            runtime::write_line(b"[compositor][frame0] window batches ready\n");
        }
        if !self.render_window_surfaces() {
            return false;
        }
        if trace {
            runtime::write_line(b"[compositor][frame0] window surfaces rendered\n");
        }

        self.final_batch.clear();
        Self::append_stage_plane(
            &mut self.final_batch,
            self.width as f32,
            self.height as f32,
            palette,
            frame,
        );

        let focus_idx = (telemetry.focused_surface as usize) % WINDOW_COUNT;
        let mut windows = Vec::with_capacity(WINDOW_COUNT);
        for spec in spatial_window_specs(&self.window_targets, focus_idx, palette, frame) {
            if let Some(window) = project_window_spec(spec, self.width as f32, self.height as f32)
            {
                windows.push(window);
            }
        }
        windows.sort_by_key(|window| window.sort_key);
        for window in &windows {
            Self::append_window_reflection(&mut self.final_batch, *window, palette);
        }
        for window in &windows {
            Self::append_spatial_window(&mut self.final_batch, *window, palette);
        }
        for window in &windows {
            self.append_window_banner(*window, palette);
        }
        self.append_surface_ledger(state, palette);
        self.append_chart_ledger(state, palette, frame);
        self.append_shell_overlay(state, telemetry, palette, frame);
        if trace {
            runtime::write_line(b"[compositor][frame0] final batch ready\n");
        }

        let atlas_index = self.atlas_texture.saturating_sub(1) as usize;
        if atlas_index >= MAX_TEXTURES {
            return false;
        }

        let width = self.width;
        let height = self.height;
        let atlas = &self.atlas;
        let window_targets = &self.window_targets;
        let final_batch = &self.final_batch;
        let sphere_vertices = &self.sphere_vertices;
        let sphere_index_count = self.sphere_index_count;
        let ctx = &mut self.ctx;
        let backdrop_batch = &mut self.backdrop_batch;
        let mut target = Target::new(
            &mut pixels[..pixel_count],
            &mut self.depth[..pixel_count],
            width,
            height,
        );
        if trace {
            runtime::write_line(b"[compositor][frame0] target ready\n");
        }
        Self::render_background_3d(
            ctx,
            backdrop_batch,
            sphere_vertices,
            sphere_index_count,
            width,
            height,
            &mut target,
            palette,
            frame,
        );
        if trace {
            runtime::write_line(b"[compositor][frame0] background rendered\n");
        }

        let mut views: [Option<Texture<'_>>; MAX_TEXTURES] = [None; MAX_TEXTURES];
        views[atlas_index] = Some(Self::atlas_texture_view(atlas));
        for target_surface in window_targets {
            let idx = target_surface.color_texture.saturating_sub(1) as usize;
            if idx >= MAX_TEXTURES {
                return false;
            }
            views[idx] = ctx.texture_view(target_surface.color_texture);
        }

        let ui = UiRenderer::new(width as f32, height as f32, &views);
        ui.render(&mut target, final_batch);
        if trace {
            runtime::write_line(b"[compositor][frame0] final composite rendered\n");
        }
        true
    }

    fn render_fallback_frame(&self, pixels: &mut [u32], state: &CompositorState, frame: u32) {
        let width = self.width as usize;
        let height = self.height as usize;
        if width == 0 || height == 0 {
            return;
        }
        let pixel_count = width.saturating_mul(height);
        if pixels.len() < pixel_count {
            return;
        }

        let palette = palette_for(state.tone);
        pixels[..pixel_count].fill(palette.sky_bottom);

        // Draw a lightweight skyline band and heartbeat strip so the display is visibly alive
        // even when the full 3D/depth pipeline is disabled under memory pressure.
        let band_top = height / 5;
        let band_bottom = (height / 5).saturating_add((height / 7).max(1));
        for y in band_top..band_bottom.min(height) {
            let row = y * width;
            pixels[row..row + width].fill(palette.sky_top);
        }

        let beat = ((frame / 2) as usize) % width.max(1);
        let strip_y = (height.saturating_sub(24)).min(height.saturating_sub(1));
        let strip_row = strip_y * width;
        let strip_end = (strip_row + width).min(pixel_count);
        pixels[strip_row..strip_end].fill(palette.sky_top);
        let marker_start = strip_row + beat.saturating_sub(8);
        let marker_end = (strip_row + (beat + 8).min(width)).min(pixel_count);
        if marker_start < marker_end {
            pixels[marker_start..marker_end].fill(palette.sky_bottom);
        }
    }

    fn atlas_texture_view(atlas: &GlyphAtlas) -> Texture<'_> {
        Texture {
            pixels: atlas.pixels.as_slice(),
            width: atlas.width,
            height: atlas.height,
            wrap_s: WrapMode::ClampToEdge,
            wrap_t: WrapMode::ClampToEdge,
            min_filter: FilterMode::Nearest,
            mag_filter: FilterMode::Nearest,
            border_color: [0.0, 0.0, 0.0, 0.0],
        }
    }

    fn render_window_surfaces(&mut self) -> bool {
        let atlas_index = self.atlas_texture.saturating_sub(1) as usize;
        if atlas_index >= MAX_TEXTURES {
            return false;
        }
        let mut atlas_views: [Option<Texture<'_>>; MAX_TEXTURES] = [None; MAX_TEXTURES];
        atlas_views[atlas_index] = Some(Self::atlas_texture_view(&self.atlas));

        let offscreen = &mut self.offscreen;
        let ctx = &mut self.ctx;
        let window_targets = &self.window_targets;
        let window_batches = &self.window_batches;

        for idx in 0..WINDOW_COUNT {
            if !offscreen.render_batch(
                ctx,
                &window_targets[idx],
                &window_batches[idx],
                &atlas_views,
            ) {
                return false;
            }
        }
        true
    }

    fn append_stage_plane(
        batch: &mut UiBatch,
        width: f32,
        height: f32,
        palette: Palette,
        frame: u32,
    ) {
        let Some(stage) = project_scene_quad(
            width,
            height,
            [
                Vec3::new(-980.0, REFLECTION_PLANE_Y, -260.0),
                Vec3::new(980.0, REFLECTION_PLANE_Y, -260.0),
                Vec3::new(1380.0, REFLECTION_PLANE_Y - 168.0, 620.0),
                Vec3::new(-1380.0, REFLECTION_PLANE_Y - 168.0, 620.0),
            ],
        ) else {
            return;
        };

        add_warped_quad(
            batch,
            stage,
            0.14,
            0,
            [
                Vec4::new(
                    palette.glass_top.x,
                    palette.glass_top.y,
                    palette.glass_top.z,
                    0.08,
                ),
                Vec4::new(
                    palette.glass_top.x,
                    palette.glass_top.y,
                    palette.glass_top.z,
                    0.08,
                ),
                Vec4::new(
                    palette.glass_bottom.x,
                    palette.glass_bottom.y,
                    palette.glass_bottom.z,
                    0.28,
                ),
                Vec4::new(
                    palette.glass_bottom.x,
                    palette.glass_bottom.y,
                    palette.glass_bottom.z,
                    0.28,
                ),
            ],
            4,
            2,
        );

        for lane in 0..5 {
            let nx = lane as f32 / 4.0;
            let x = -680.0 + nx * 1360.0;
            if let Some([from, to]) = project_scene_line(
                width,
                height,
                [
                    Vec3::new(x, REFLECTION_PLANE_Y - 10.0, -220.0),
                    Vec3::new(x * 1.18, REFLECTION_PLANE_Y - 126.0, 520.0),
                ],
            ) {
                batch.add_line(
                    from.x,
                    from.y,
                    to.x,
                    to.y,
                    0.145,
                    1.1,
                    Vec4::new(
                        palette.cyan.x,
                        palette.cyan.y,
                        palette.cyan.z,
                        0.08 + nx * 0.06,
                    ),
                );
            }
        }

        let pulse = 0.08 + ((sinf(frame as f32 * 0.024) + 1.0) * 0.5) * 0.12;
        if let Some([from, to]) = project_scene_line(
            width,
            height,
            [
                Vec3::new(-180.0, REFLECTION_PLANE_Y + 14.0, 60.0),
                Vec3::new(220.0, REFLECTION_PLANE_Y + 14.0, 320.0),
            ],
        ) {
            batch.add_line(
                from.x,
                from.y,
                to.x,
                to.y,
                0.146,
                2.2,
                Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, pulse),
            );
        }
    }

    fn append_window_reflection(batch: &mut UiBatch, window: ProjectedWindow, palette: Palette) {
        let Some(reflection) = window.reflection else {
            return;
        };

        let alpha = if window.focused { 0.16 } else { 0.11 } * window.opacity;
        add_warped_quad(
            batch,
            reflection,
            0.18,
            window.texture,
            [
                Vec4::new(0.42, 0.52, 0.64, alpha),
                Vec4::new(0.42, 0.52, 0.64, alpha),
                Vec4::new(
                    palette.glass_bottom.x,
                    palette.glass_bottom.y,
                    palette.glass_bottom.z,
                    0.0,
                ),
                Vec4::new(
                    palette.glass_bottom.x,
                    palette.glass_bottom.y,
                    palette.glass_bottom.z,
                    0.0,
                ),
            ],
            WINDOW_GRID_X,
            WINDOW_GRID_Y,
        );
    }

    fn append_spatial_window(batch: &mut UiBatch, window: ProjectedWindow, palette: Palette) {
        let shadow_bounds = window.bounds.inset(-16.0, -10.0, -16.0, -22.0);
        batch.add_shadow_rect(
            Rect::new(
                shadow_bounds.x + 8.0,
                shadow_bounds.y + 12.0,
                shadow_bounds.w,
                shadow_bounds.h,
            ),
            0.205,
            18.0,
            Vec4::new(0.0, 0.0, 0.0, if window.focused { 0.30 } else { 0.24 }),
        );

        add_warped_quad(
            batch,
            scale_quad(window.corners, if window.focused { 1.038 } else { 1.028 }),
            0.212,
            0,
            [
                Vec4::new(
                    palette.cyan_soft.x,
                    palette.cyan_soft.y,
                    palette.cyan_soft.z,
                    if window.focused { 0.18 } else { 0.10 },
                ),
                Vec4::new(
                    palette.copper_soft.x,
                    palette.copper_soft.y,
                    palette.copper_soft.z,
                    if window.focused { 0.16 } else { 0.08 },
                ),
                Vec4::new(0.02, 0.03, 0.05, 0.10),
                Vec4::new(0.02, 0.03, 0.05, 0.10),
            ],
            2,
            2,
        );

        add_warped_quad(
            batch,
            window.corners,
            0.22,
            window.texture,
            [Vec4::new(1.0, 1.0, 1.0, window.opacity); 4],
            WINDOW_GRID_X,
            WINDOW_GRID_Y,
        );

        for i in 0..4 {
            let next = (i + 1) % 4;
            batch.add_line(
                window.corners[i].x,
                window.corners[i].y,
                window.corners[next].x,
                window.corners[next].y,
                0.222,
                if i == 0 { 2.2 } else { 1.2 },
                if i == 0 {
                    Vec4::new(window.accent.x, window.accent.y, window.accent.z, 0.84)
                } else {
                    Vec4::new(0.82, 0.90, 1.0, 0.30)
                },
            );
        }

        let tag_w = (window.bounds.w * 0.54).clamp(132.0, 236.0);
        let tag = Rect::new(
            (window.bounds.center_x() - tag_w * 0.5)
                .clamp(24.0, (window.bounds.right() - tag_w).max(24.0)),
            (window.bounds.y - 30.0).max(92.0),
            tag_w,
            24.0,
        );
        batch.add_rounded_rect(
            tag,
            0.224,
            Vec4::new(0.04, 0.06, 0.10, if window.focused { 0.90 } else { 0.76 }),
            12.0,
            8,
        );
        batch.add_rounded_border(
            tag,
            0.225,
            Vec4::new(window.accent.x, window.accent.y, window.accent.z, 0.72),
            12.0,
            1.0,
            8,
        );
    }

    fn render_background_3d(
        ctx: &mut Context,
        backdrop_batch: &mut UiBatch,
        sphere_vertices: &[Vertex],
        sphere_index_count: usize,
        width: u32,
        height: u32,
        target: &mut Target<'_>,
        palette: Palette,
        frame: u32,
    ) {
        Self::draw_backdrop_layers(backdrop_batch, width, height, target, palette, frame);

        let orbit = frame as f32 * 0.014;
        let aspect = width as f32 / height.max(1) as f32;
        let proj = Mat4::perspective(1.02, aspect, 0.1, 100.0);
        let eye = Vec3::new(0.0, 0.25, 6.1);
        let center = Vec3::new(0.0, 0.1, 0.0);
        let view = Mat4::look_at(eye, center, Vec3::Y);
        let world = Mat4::rotation_x(0.14).mul_mat(&Mat4::rotation_y(orbit * 0.42));
        let vp = proj.mul_mat(&view).mul_mat(&world);

        let core_model = Mat4::scale(Vec3::new(1.16, 1.16, 1.16));
        let core_shader = GlowSphereShader {
            model: core_model,
            mvp: vp.mul_mat(&core_model),
            tint: Vec3::new(0.94, 0.63, 0.28),
            glow: Vec3::new(0.46, 0.84, 1.0),
        };
        let _ = ctx.draw_elements(
            target,
            &core_shader,
            sphere_vertices,
            sphere_index_count,
            IndexType::U32,
            0,
            DrawMode::Triangles,
        );

        for i in 0..9usize {
            let phase = orbit + i as f32 * 0.67;
            let radius = 2.2 + (i % 3) as f32 * 0.34;
            let pos = Vec3::new(
                cosf(phase) * radius,
                sinf(phase * 1.3) * 0.42 + ((i as i32 % 3) as f32 - 1.0) * 0.22,
                sinf(phase) * radius,
            );
            let scale = 0.18 + (i % 4) as f32 * 0.03;
            let shader = GlowSphereShader {
                model: Mat4::translation(pos).mul_mat(&Mat4::scale(Vec3::splat(scale))),
                mvp: vp.mul_mat(&Mat4::translation(pos).mul_mat(&Mat4::scale(Vec3::splat(scale)))),
                tint: if i & 1 == 0 {
                    Vec3::new(0.44, 0.82, 1.0)
                } else {
                    Vec3::new(0.96, 0.68, 0.34)
                },
                glow: Vec3::new(0.18, 0.48, 0.86),
            };
            let _ = ctx.draw_elements(
                target,
                &shader,
                sphere_vertices,
                sphere_index_count,
                IndexType::U32,
                0,
                DrawMode::Triangles,
            );
        }
    }

    fn draw_backdrop_layers(
        backdrop_batch: &mut UiBatch,
        width: u32,
        height: u32,
        target: &mut Target<'_>,
        palette: Palette,
        frame: u32,
    ) {
        let sweep = sinf(frame as f32 * 0.012) * 24.0;
        backdrop_batch.clear();
        backdrop_batch.add_gradient_rect(
            Rect::new(0.0, 0.0, width as f32, height as f32),
            0.0,
            unpack_rgba(palette.sky_top),
            unpack_rgba(palette.sky_top),
            unpack_rgba(palette.sky_bottom),
            unpack_rgba(palette.sky_bottom),
        );
        backdrop_batch.add_gradient_rect(
            Rect::new(-64.0, -80.0 + sweep * 0.18, width as f32 * 0.72, height as f32 * 0.66),
            0.001,
            Vec4::new(0.98, 0.54, 0.22, 0.18),
            Vec4::new(0.74, 0.28, 0.10, 0.02),
            Vec4::new(0.10, 0.08, 0.12, 0.00),
            Vec4::new(0.26, 0.14, 0.10, 0.12),
        );
        backdrop_batch.add_gradient_rect(
            Rect::new(width as f32 * 0.36, height as f32 * 0.08 - sweep * 0.12, width as f32 * 0.72, height as f32 * 0.64),
            0.0012,
            Vec4::new(0.18, 0.56, 0.92, 0.06),
            Vec4::new(0.32, 0.72, 1.0, 0.18),
            Vec4::new(0.03, 0.08, 0.18, 0.02),
            Vec4::new(0.08, 0.22, 0.34, 0.08),
        );

        const STARS: &[(f32, f32, f32)] = &[
            (0.08, 0.11, 1.6), (0.14, 0.22, 2.0), (0.18, 0.08, 1.2), (0.26, 0.18, 1.5),
            (0.31, 0.12, 1.3), (0.37, 0.27, 1.7), (0.44, 0.09, 1.4), (0.51, 0.16, 1.8),
            (0.58, 0.12, 1.4), (0.64, 0.24, 1.5), (0.73, 0.18, 1.9), (0.81, 0.10, 1.2),
            (0.87, 0.22, 1.5), (0.92, 0.15, 1.4), (0.78, 0.30, 1.7), (0.22, 0.31, 1.3),
        ];
        for &(nx, ny, radius) in STARS {
            let pulse = 0.62 + sinf(frame as f32 * 0.02 + nx * 9.0 + ny * 6.0) * 0.18;
            backdrop_batch.add_graph_node(
                nx * width as f32,
                ny * height as f32,
                radius,
                0.002,
                Vec4::new(0.86, 0.94, 1.0, pulse.clamp(0.0, 1.0)),
            );
        }

        let ui = UiRenderer::new(width as f32, height as f32, &[]);
        ui.render(target, backdrop_batch);

    }

    fn populate_window_batches(
        &mut self,
        state: &CompositorState,
        telemetry: graphos_compositor::SceneTelemetry,
        palette: Palette,
        frame: u32,
    ) {
        for batch in &mut self.window_batches {
            batch.clear();
        }

        build_command_window(
            &mut self.window_batches[0],
            &self.atlas,
            self.atlas_texture,
            self.window_targets[0].width as f32,
            self.window_targets[0].height as f32,
            telemetry,
            palette,
            frame,
        );
        build_ai_window(
            &mut self.window_batches[1],
            &self.atlas,
            self.atlas_texture,
            self.window_targets[1].width as f32,
            self.window_targets[1].height as f32,
            telemetry,
            palette,
            frame,
        );
        build_topology_window(
            &mut self.window_batches[2],
            &self.atlas,
            self.atlas_texture,
            self.window_targets[2].width as f32,
            self.window_targets[2].height as f32,
            palette,
            frame,
        );
        build_release_window(
            &mut self.window_batches[3],
            &self.atlas,
            self.atlas_texture,
            self.window_targets[3].width as f32,
            self.window_targets[3].height as f32,
            state,
            telemetry,
            palette,
            frame,
        );
    }

    fn append_shell_overlay(
        &mut self,
        state: &CompositorState,
        telemetry: graphos_compositor::SceneTelemetry,
        palette: Palette,
        frame: u32,
    ) {
        let full = Rect::new(0.0, 0.0, self.width as f32, self.height as f32);
        let header = Rect::new(28.0, 18.0, self.width as f32 - 56.0, 64.0);
        self.final_batch.add_rounded_rect(
            header,
            0.92,
            Vec4::new(0.04, 0.06, 0.10, 0.52),
            24.0,
            10,
        );
        self.final_batch.add_rounded_border(
            header,
            0.921,
            Vec4::new(0.34, 0.52, 0.70, 0.64),
            24.0,
            1.0,
            10,
        );
        self.final_batch.add_gradient_rect(
            Rect::new(header.x + 8.0, header.y + 8.0, header.w * 0.42, header.h - 16.0),
            0.922,
            palette.copper_soft,
            Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, 0.08),
            Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.04),
            palette.cyan_soft,
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            "GRAPHOS // IMMERSIVE FABRIC DESKTOP",
            46.0,
            34.0,
            0.93,
            TextStyle {
                color: palette.text,
                scale: 1.65,
                line_height: 12.0,
                letter_spacing: 0.5,
            },
            None,
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            "virtio-gpu scanout  |  graphos-gl compositor  |  native ring-3 desktop takeover",
            48.0,
            54.0,
            0.93,
            TextStyle {
                color: palette.text_muted,
                scale: 0.92,
                line_height: 10.0,
                letter_spacing: 0.0,
            },
            None,
        );

        let dock = Rect::new(28.0, self.height as f32 - 86.0, self.width as f32 - 56.0, 58.0);
        self.final_batch.add_rounded_rect(
            dock,
            0.91,
            Vec4::new(0.03, 0.05, 0.08, 0.64),
            20.0,
            10,
        );
        self.final_batch.add_rounded_border(
            dock,
            0.911,
            Vec4::new(0.26, 0.39, 0.58, 0.62),
            20.0,
            1.0,
            10,
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            "OPS MESH   AI SUBSTRATE   RELEASE FLOW   TOPOLOGY VIEW   SHELL3D",
            dock.x + 24.0,
            dock.y + 18.0,
            0.915,
            TextStyle {
                color: palette.text,
                scale: 1.05,
                line_height: 10.0,
                letter_spacing: 0.25,
            },
            Some(dock),
        );

        let mut metric_buf = [0u8; 40];
        let mut chip_x = self.width as f32 - 324.0;
        for &(label, value, color) in &[
            (b"surfaces " as &[u8], telemetry.visible_surfaces as u32, palette.cyan),
            (b"charts " as &[u8], telemetry.charts as u32, palette.copper),
            (b"epoch " as &[u8], telemetry.scene_epoch, palette.positive),
        ] {
            let chip = Rect::new(chip_x, 98.0, 88.0, 34.0);
            self.final_batch.add_rounded_rect(
                chip,
                0.905,
                Vec4::new(0.05, 0.07, 0.11, 0.72),
                14.0,
                8,
            );
            self.final_batch.add_rounded_border(
                chip,
                0.906,
                Vec4::new(color.x, color.y, color.z, 0.62),
                14.0,
                1.0,
                8,
            );
            let text = metric_line(&mut metric_buf, label, value);
            append_text_aligned(
                &mut self.final_batch,
                &self.atlas,
                self.atlas_texture,
                text,
                chip.inset(8.0, 10.0, 8.0, 8.0),
                0.907,
                TextStyle {
                    color: palette.text,
                    scale: 0.9,
                    line_height: 10.0,
                    letter_spacing: 0.0,
                },
                TextAlign::Center,
                Some(chip),
            );
            chip_x += 96.0;
        }

        let pulse = 0.52 + sinf(frame as f32 * 0.028) * 0.18;
        let hub = [
            Vec2::new(full.w * 0.15, full.h * 0.24),
            Vec2::new(full.w * 0.32, full.h * 0.16),
            Vec2::new(full.w * 0.58, full.h * 0.19),
            Vec2::new(full.w * 0.82, full.h * 0.28),
            Vec2::new(full.w * 0.74, full.h * 0.56),
            Vec2::new(full.w * 0.28, full.h * 0.64),
        ];
        for i in 0..hub.len() {
            for j in i + 1..hub.len() {
                if (i + j) % 2 == 0 {
                    self.final_batch.add_graph_edge(
                        hub[i],
                        hub[j],
                        0.12,
                        1.4,
                        Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.18),
                        Some(Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.06)),
                    );
                }
            }
        }
        for point in hub {
            self.final_batch.add_graph_node(
                point.x,
                point.y,
                7.0,
                0.121,
                Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.82),
            );
            self.final_batch.add_graph_node(
                point.x,
                point.y,
                15.0,
                0.1205,
                Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, pulse.clamp(0.0, 1.0) * 0.10),
            );
        }

        let focus_chip = Rect::new(self.width as f32 - 244.0, 144.0, 216.0, 48.0);
        self.final_batch.add_rounded_rect(
            focus_chip,
            0.905,
            Vec4::new(0.04, 0.06, 0.10, 0.76),
            18.0,
            8,
        );
        self.final_batch.add_rounded_border(
            focus_chip,
            0.906,
            Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, 0.68),
            18.0,
            1.0,
            8,
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            "active theme",
            focus_chip.x + 16.0,
            focus_chip.y + 10.0,
            0.907,
            TextStyle {
                color: palette.text_muted,
                scale: 0.9,
                line_height: 10.0,
                letter_spacing: 0.0,
            },
            Some(focus_chip),
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            theme_name(state.tone),
            focus_chip.x + 16.0,
            focus_chip.y + 24.0,
            0.907,
            TextStyle {
                color: palette.text,
                scale: 1.05,
                line_height: 10.0,
                letter_spacing: 0.1,
            },
            Some(focus_chip),
        );
    }

    fn append_surface_ledger(&mut self, state: &CompositorState, palette: Palette) {
        let panel = Rect::new(28.0, 104.0, 220.0, 196.0);
        self.final_batch.add_rounded_rect(
            panel,
            0.892,
            Vec4::new(0.03, 0.05, 0.08, 0.68),
            18.0,
            8,
        );
        self.final_batch.add_rounded_border(
            panel,
            0.893,
            Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.46),
            18.0,
            1.0,
            8,
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            "fabric surfaces",
            panel.x + 16.0,
            panel.y + 14.0,
            0.894,
            TextStyle {
                color: palette.text,
                scale: 1.0,
                line_height: 10.0,
                letter_spacing: 0.15,
            },
            Some(panel),
        );

        for (idx, surface) in state
            .surfaces
            .iter()
            .filter(|surface| surface.visible)
            .take(5)
            .enumerate()
        {
            let row = Rect::new(panel.x + 12.0, panel.y + 40.0 + idx as f32 * 30.0, panel.w - 24.0, 24.0);
            let accent = if surface.focused { palette.copper } else { palette.cyan };
            self.final_batch.add_rounded_rect(
                row,
                0.895,
                Vec4::new(0.06, 0.08, 0.12, 0.88),
                10.0,
                8,
            );
            self.final_batch.add_rect(Rect::new(row.x, row.y, 3.0, row.h), 0.896, accent);
            append_text(
                &mut self.final_batch,
                &self.atlas,
                self.atlas_texture,
                bytes_to_str(surface.title()),
                row.x + 12.0,
                row.y + 7.0,
                0.897,
                TextStyle {
                    color: palette.text,
                    scale: 0.90,
                    line_height: 10.0,
                    letter_spacing: 0.0,
                },
                Some(panel),
            );
            append_text(
                &mut self.final_batch,
                &self.atlas,
                self.atlas_texture,
                bytes_to_str(surface.kind.as_bytes()),
                row.right() - 74.0,
                row.y + 7.0,
                0.897,
                TextStyle {
                    color: palette.text_muted,
                    scale: 0.82,
                    line_height: 10.0,
                    letter_spacing: 0.0,
                },
                Some(panel),
            );
        }
    }

    fn append_chart_ledger(&mut self, state: &CompositorState, palette: Palette, frame: u32) {
        let panel = Rect::new(self.width as f32 - 252.0, 208.0, 224.0, 168.0);
        self.final_batch.add_rounded_rect(
            panel,
            0.892,
            Vec4::new(0.03, 0.05, 0.08, 0.66),
            18.0,
            8,
        );
        self.final_batch.add_rounded_border(
            panel,
            0.893,
            Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, 0.48),
            18.0,
            1.0,
            8,
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            "live charts",
            panel.x + 16.0,
            panel.y + 14.0,
            0.894,
            TextStyle {
                color: palette.text,
                scale: 1.0,
                line_height: 10.0,
                letter_spacing: 0.15,
            },
            Some(panel),
        );

        for (idx, chart) in state.charts.iter().take(4).enumerate() {
            let row = Rect::new(panel.x + 12.0, panel.y + 40.0 + idx as f32 * 28.0, panel.w - 24.0, 22.0);
            let glow = 0.10 + ((sinf(frame as f32 * 0.03 + idx as f32 * 0.7) + 1.0) * 0.5) * 0.10;
            self.final_batch.add_rounded_rect(
                row,
                0.895,
                Vec4::new(0.06, 0.08, 0.12, 0.84),
                10.0,
                8,
            );
            self.final_batch.add_graph_node(
                row.x + 10.0,
                row.y + row.h * 0.5,
                4.0,
                0.896,
                Vec4::new(palette.positive.x, palette.positive.y, palette.positive.z, 0.92),
            );
            self.final_batch.add_graph_node(
                row.x + 10.0,
                row.y + row.h * 0.5,
                9.0,
                0.8955,
                Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, glow),
            );
            append_text(
                &mut self.final_batch,
                &self.atlas,
                self.atlas_texture,
                bytes_to_str(chart.title()),
                row.x + 22.0,
                row.y + 6.0,
                0.897,
                TextStyle {
                    color: palette.text,
                    scale: 0.88,
                    line_height: 10.0,
                    letter_spacing: 0.0,
                },
                Some(panel),
            );
        }
    }

    fn append_window_banner(&mut self, window: ProjectedWindow, palette: Palette) {
        let tag_w = (window.bounds.w * 0.54).clamp(132.0, 236.0);
        let tag = Rect::new(
            (window.bounds.center_x() - tag_w * 0.5)
                .clamp(24.0, (window.bounds.right() - tag_w).max(24.0)),
            (window.bounds.y - 30.0).max(92.0),
            tag_w,
            24.0,
        );
        append_text_aligned(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            window.title,
            tag.inset(10.0, 7.0, 10.0, 6.0),
            0.226,
            TextStyle {
                color: if window.focused {
                    palette.text
                } else {
                    palette.text_muted
                },
                scale: 0.9,
                line_height: 10.0,
                letter_spacing: 0.15,
            },
            TextAlign::Center,
            Some(tag),
        );
        append_text(
            &mut self.final_batch,
            &self.atlas,
            self.atlas_texture,
            window.subtitle,
            window.bounds.x + 14.0,
            window.bounds.bottom() - 18.0,
            0.223,
            TextStyle {
                color: Vec4::new(palette.text.x, palette.text.y, palette.text.z, 0.72),
                scale: 0.76,
                line_height: 10.0,
                letter_spacing: 0.0,
            },
            Some(window.bounds),
        );
    }
}

fn window_target_sizes(screen_w: u32, screen_h: u32) -> [(u32, u32); WINDOW_COUNT] {
    [
        ((screen_w as f32 * 0.42) as u32, (screen_h as f32 * 0.34) as u32),
        ((screen_w as f32 * 0.34) as u32, (screen_h as f32 * 0.30) as u32),
        ((screen_w as f32 * 0.38) as u32, (screen_h as f32 * 0.32) as u32),
        ((screen_w as f32 * 0.32) as u32, (screen_h as f32 * 0.24) as u32),
    ]
}

fn spatial_window_specs(
    window_targets: &[WindowSurfaceTarget; WINDOW_COUNT],
    focus_idx: usize,
    palette: Palette,
    frame: u32,
) -> [SpatialWindowSpec; WINDOW_COUNT] {
    let phase = frame as f32 * 0.014;
    let sway = sinf(phase);
    let drift = cosf(phase * 0.82);
    let primary_boost = if focus_idx == 0 { 56.0 } else { 0.0 };
    let ai_boost = if focus_idx == 1 { 52.0 } else { 0.0 };
    let topo_boost = if focus_idx == 2 { 60.0 } else { 0.0 };
    let release_boost = if focus_idx == 3 { 54.0 } else { 0.0 };

    [
        SpatialWindowSpec {
            texture: window_targets[0].color_texture,
            title: "COMMAND MESH",
            subtitle: "ring-3 control fabric",
            center: Vec3::new(-360.0 + drift * 18.0, 138.0 + sway * 16.0, 208.0 + primary_boost),
            size: scaled_window_size(&window_targets[0], if focus_idx == 0 { 0.96 } else { 0.92 }),
            yaw: 0.20,
            pitch: -0.10,
            opacity: if focus_idx == 0 { 1.0 } else { 0.96 },
            accent: palette.copper,
            focused: focus_idx == 0,
            sort_key: (208.0 + primary_boost) as i32,
        },
        SpatialWindowSpec {
            texture: window_targets[1].color_texture,
            title: "AI ORCHESTRATION",
            subtitle: "agent substrate lane",
            center: Vec3::new(342.0 + sway * 20.0, 152.0 + drift * 10.0, 78.0 + ai_boost),
            size: scaled_window_size(&window_targets[1], if focus_idx == 1 { 1.02 } else { 0.96 }),
            yaw: -0.22,
            pitch: -0.09,
            opacity: if focus_idx == 1 { 1.0 } else { 0.94 },
            accent: palette.cyan,
            focused: focus_idx == 1,
            sort_key: (78.0 + ai_boost) as i32,
        },
        SpatialWindowSpec {
            texture: window_targets[2].color_texture,
            title: "TOPOLOGY FIELD",
            subtitle: "graph-aware dependency view",
            center: Vec3::new(-256.0 + drift * 14.0, -106.0 + sway * 10.0, -54.0 + topo_boost),
            size: scaled_window_size(&window_targets[2], if focus_idx == 2 { 1.03 } else { 0.94 }),
            yaw: 0.28,
            pitch: -0.07,
            opacity: if focus_idx == 2 { 0.99 } else { 0.92 },
            accent: palette.cyan,
            focused: focus_idx == 2,
            sort_key: (-54.0 + topo_boost) as i32,
        },
        SpatialWindowSpec {
            texture: window_targets[3].color_texture,
            title: "RELEASE FLOW",
            subtitle: "deployment and handoff lane",
            center: Vec3::new(402.0 + sway * 14.0, -172.0 + drift * 8.0, 122.0 + release_boost),
            size: scaled_window_size(&window_targets[3], if focus_idx == 3 { 1.06 } else { 0.98 }),
            yaw: -0.12,
            pitch: -0.12,
            opacity: if focus_idx == 3 { 0.99 } else { 0.95 },
            accent: palette.positive,
            focused: focus_idx == 3,
            sort_key: (122.0 + release_boost) as i32,
        },
    ]
}

fn scaled_window_size(target: &WindowSurfaceTarget, scale: f32) -> Vec2 {
    Vec2::new(target.width as f32 * scale, target.height as f32 * scale)
}

fn project_window_spec(
    spec: SpatialWindowSpec,
    screen_w: f32,
    screen_h: f32,
) -> Option<ProjectedWindow> {
    let corners = project_window_quad(screen_w, screen_h, spec.center, spec.size, spec.yaw, spec.pitch)?;
    let reflection_center = Vec3::new(
        spec.center.x,
        REFLECTION_PLANE_Y - (spec.center.y - REFLECTION_PLANE_Y),
        spec.center.z - 90.0,
    );
    let reflection = project_window_quad(
        screen_w,
        screen_h,
        reflection_center,
        Vec2::new(spec.size.x * 0.96, spec.size.y * 0.82),
        spec.yaw,
        -spec.pitch * 0.8,
    );
    Some(ProjectedWindow {
        texture: spec.texture,
        title: spec.title,
        subtitle: spec.subtitle,
        corners,
        reflection,
        bounds: quad_bounds(corners),
        opacity: spec.opacity,
        accent: spec.accent,
        focused: spec.focused,
        sort_key: spec.sort_key,
    })
}

fn project_window_quad(
    screen_w: f32,
    screen_h: f32,
    center: Vec3,
    size: Vec2,
    yaw: f32,
    pitch: f32,
) -> Option<[Vec2; 4]> {
    let hw = size.x * 0.5;
    let hh = size.y * 0.5;
    project_scene_quad(
        screen_w,
        screen_h,
        [
            center + rotate_window_local(Vec3::new(-hw, hh, 0.0), yaw, pitch),
            center + rotate_window_local(Vec3::new(hw, hh, 0.0), yaw, pitch),
            center + rotate_window_local(Vec3::new(hw, -hh, 0.0), yaw, pitch),
            center + rotate_window_local(Vec3::new(-hw, -hh, 0.0), yaw, pitch),
        ],
    )
}

fn rotate_window_local(local: Vec3, yaw: f32, pitch: f32) -> Vec3 {
    let cy = cosf(yaw);
    let sy = sinf(yaw);
    let x = local.x * cy - local.z * sy;
    let z = local.x * sy + local.z * cy;

    let cx = cosf(pitch);
    let sx = sinf(pitch);
    let y = local.y * cx - z * sx;
    let z = local.y * sx + z * cx;
    Vec3::new(x, y, z)
}

fn project_scene_point(screen_w: f32, screen_h: f32, point: Vec3) -> Option<Vec2> {
    let depth = CAMERA_Z - point.z;
    if depth <= 120.0 {
        return None;
    }
    let scale = CAMERA_Z / depth;
    Some(Vec2::new(
        screen_w * 0.5 + point.x * scale,
        screen_h * CAMERA_BASELINE_Y - point.y * scale,
    ))
}

fn project_scene_quad(screen_w: f32, screen_h: f32, points: [Vec3; 4]) -> Option<[Vec2; 4]> {
    Some([
        project_scene_point(screen_w, screen_h, points[0])?,
        project_scene_point(screen_w, screen_h, points[1])?,
        project_scene_point(screen_w, screen_h, points[2])?,
        project_scene_point(screen_w, screen_h, points[3])?,
    ])
}

fn project_scene_line(screen_w: f32, screen_h: f32, points: [Vec3; 2]) -> Option<[Vec2; 2]> {
    Some([
        project_scene_point(screen_w, screen_h, points[0])?,
        project_scene_point(screen_w, screen_h, points[1])?,
    ])
}

fn quad_bounds(corners: [Vec2; 4]) -> Rect {
    let min_x = corners
        .iter()
        .fold(corners[0].x, |value, point| value.min(point.x));
    let max_x = corners
        .iter()
        .fold(corners[0].x, |value, point| value.max(point.x));
    let min_y = corners
        .iter()
        .fold(corners[0].y, |value, point| value.min(point.y));
    let max_y = corners
        .iter()
        .fold(corners[0].y, |value, point| value.max(point.y));
    Rect::new(min_x, min_y, max_x - min_x, max_y - min_y)
}

fn scale_quad(corners: [Vec2; 4], scale: f32) -> [Vec2; 4] {
    let center = Vec2::new(
        (corners[0].x + corners[1].x + corners[2].x + corners[3].x) * 0.25,
        (corners[0].y + corners[1].y + corners[2].y + corners[3].y) * 0.25,
    );
    [
        scale_point(corners[0], center, scale),
        scale_point(corners[1], center, scale),
        scale_point(corners[2], center, scale),
        scale_point(corners[3], center, scale),
    ]
}

fn scale_point(point: Vec2, center: Vec2, scale: f32) -> Vec2 {
    Vec2::new(
        center.x + (point.x - center.x) * scale,
        center.y + (point.y - center.y) * scale,
    )
}

fn add_warped_quad(
    batch: &mut UiBatch,
    corners: [Vec2; 4],
    z: f32,
    texture: u32,
    colors: [Vec4; 4],
    x_segments: usize,
    y_segments: usize,
) {
    let seg_x = x_segments.max(1);
    let seg_y = y_segments.max(1);
    let base = batch.vertices.len() as u32;

    for yi in 0..=seg_y {
        let ty = yi as f32 / seg_y as f32;
        let left = lerp_vec2(corners[0], corners[3], ty);
        let right = lerp_vec2(corners[1], corners[2], ty);
        let left_color = lerp_vec4(colors[0], colors[3], ty);
        let right_color = lerp_vec4(colors[1], colors[2], ty);

        for xi in 0..=seg_x {
            let tx = xi as f32 / seg_x as f32;
            let pos = lerp_vec2(left, right, tx);
            let color = lerp_vec4(left_color, right_color, tx);
            batch.vertices.push(UiVertex::new(
                Vec3::new(pos.x, pos.y, z),
                Vec2::new(tx, ty),
                color,
                texture,
            ));
        }
    }

    let stride = (seg_x + 1) as u32;
    for yi in 0..seg_y as u32 {
        for xi in 0..seg_x as u32 {
            let i0 = base + yi * stride + xi;
            let i1 = i0 + 1;
            let i2 = i0 + stride + 1;
            let i3 = i0 + stride;
            batch.indices.extend_from_slice(&[i0, i1, i2, i0, i2, i3]);
        }
    }
}

fn lerp_vec2(a: Vec2, b: Vec2, t: f32) -> Vec2 {
    Vec2::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn lerp_vec4(a: Vec4, b: Vec4, t: f32) -> Vec4 {
    Vec4::new(
        a.x + (b.x - a.x) * t,
        a.y + (b.y - a.y) * t,
        a.z + (b.z - a.z) * t,
        a.w + (b.w - a.w) * t,
    )
}

fn bytes_to_str(bytes: &[u8]) -> &str {
    str::from_utf8(bytes).unwrap_or("")
}

fn build_command_window(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    w: f32,
    h: f32,
    telemetry: graphos_compositor::SceneTelemetry,
    palette: Palette,
    frame: u32,
) {
    let bounds = Rect::new(0.0, 0.0, w, h);
    draw_window_frame(batch, bounds, "COMMAND MESH", atlas, atlas_texture, palette);

    let body = bounds.inset(18.0, 42.0, 18.0, 18.0);
    let lines = [
        "graphos@fabric:~$ boot graph-shell --immersive",
        "[ok] compositor       graphos-gl scene online",
        "[ok] scanout          virtio-gpu runtime claimed",
        "[ok] desktop mesh     orbital surfaces synchronized",
        "[ok] tooling          operator panels hydrated",
        "[run] shell3d         ring-3 surface streaming",
    ];
    append_text_lines(
        batch,
        atlas,
        atlas_texture,
        &lines,
        Rect::new(body.x, body.y, body.w, body.h * 0.52),
        0.06,
        TextStyle {
            color: Vec4::new(0.72, 0.96, 0.84, 1.0),
            scale: 1.0,
            line_height: 11.0,
            letter_spacing: 0.0,
        },
        Some(bounds),
    );

    let panel = Rect::new(body.x, body.y + body.h * 0.58, body.w, body.h * 0.30);
    batch.add_rounded_rect(panel, 0.058, palette.panel_fill_alt, 14.0, 8);
    batch.add_rounded_border(panel, 0.059, palette.panel_border, 14.0, 1.0, 8);
    let mut buf = [0u8; 32];
    append_text(
        batch,
        atlas,
        atlas_texture,
        metric_line(&mut buf, b"epoch ", telemetry.scene_epoch),
        panel.x + 16.0,
        panel.y + 14.0,
        0.06,
        TextStyle {
            color: palette.text,
            scale: 1.0,
            line_height: 10.0,
            letter_spacing: 0.0,
        },
        Some(panel),
    );
    append_text(
        batch,
        atlas,
        atlas_texture,
        metric_line(&mut buf, b"visible ", telemetry.visible_surfaces as u32),
        panel.x + 16.0,
        panel.y + 28.0,
        0.06,
        TextStyle {
            color: palette.text_muted,
            scale: 0.94,
            line_height: 10.0,
            letter_spacing: 0.0,
        },
        Some(panel),
    );

    let rail = Rect::new(panel.right() - 112.0, panel.y + 12.0, 88.0, panel.h - 24.0);
    batch.add_gradient_rect(
        rail,
        0.061,
        palette.copper_soft,
        Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, 0.24),
        Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.08),
        palette.cyan_soft,
    );
    let rise = ((sinf(frame as f32 * 0.038) + 1.0) * 0.5) * (rail.h - 12.0);
    batch.add_rect(
        Rect::new(rail.x + 18.0, rail.y + rail.h - rise - 6.0, 48.0, rise.max(6.0)),
        0.062,
        Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, 0.86),
    );
}

fn build_ai_window(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    w: f32,
    h: f32,
    telemetry: graphos_compositor::SceneTelemetry,
    palette: Palette,
    frame: u32,
) {
    let bounds = Rect::new(0.0, 0.0, w, h);
    draw_window_frame(batch, bounds, "AI ORCHESTRATION", atlas, atlas_texture, palette);

    let cards = [
        Rect::new(18.0, 50.0, w * 0.28, 72.0),
        Rect::new(26.0 + w * 0.30, 50.0, w * 0.28, 72.0),
        Rect::new(34.0 + w * 0.60, 50.0, w * 0.22, 72.0),
    ];
    for (idx, card) in cards.iter().enumerate() {
        batch.add_package_card(
            *card,
            0.055,
            palette.panel_fill,
            palette.panel_border,
            if idx == 1 { palette.copper } else { palette.cyan },
            None,
        );
    }
    append_text(batch, atlas, atlas_texture, "latency 14ms", cards[0].x + 14.0, cards[0].y + 42.0, 0.06, TextStyle { color: palette.text, scale: 0.96, line_height: 10.0, letter_spacing: 0.0 }, Some(bounds));
    append_text(batch, atlas, atlas_texture, "agents 03", cards[1].x + 14.0, cards[1].y + 42.0, 0.06, TextStyle { color: palette.text, scale: 0.96, line_height: 10.0, letter_spacing: 0.0 }, Some(bounds));
    append_text(batch, atlas, atlas_texture, if telemetry.dirty_surfaces == 0 { "stable" } else { "updating" }, cards[2].x + 14.0, cards[2].y + 42.0, 0.06, TextStyle { color: palette.text, scale: 0.96, line_height: 10.0, letter_spacing: 0.0 }, Some(bounds));

    let strip = Rect::new(18.0, 140.0, w - 36.0, h - 158.0);
    batch.add_rounded_rect(strip, 0.056, palette.panel_fill_alt, 16.0, 8);
    batch.add_rounded_border(strip, 0.057, palette.panel_border, 16.0, 1.0, 8);
    let mut buf = [0u8; 32];
    append_text(batch, atlas, atlas_texture, metric_line(&mut buf, b"workspace px ", telemetry.workspace_pixels), strip.x + 16.0, strip.y + 14.0, 0.058, TextStyle { color: palette.text, scale: 0.92, line_height: 10.0, letter_spacing: 0.0 }, Some(strip));
    append_text(batch, atlas, atlas_texture, "planner tracks shell, store, fabric, and release lanes", strip.x + 16.0, strip.y + 30.0, 0.058, TextStyle { color: palette.text_muted, scale: 0.88, line_height: 10.0, letter_spacing: 0.0 }, Some(strip));
    let wave_y = strip.y + strip.h - 24.0;
    for i in 0..8u32 {
        let x = strip.x + 16.0 + i as f32 * ((strip.w - 32.0) / 8.0);
        let height = 10.0 + ((sinf(frame as f32 * 0.06 + i as f32 * 0.7) + 1.0) * 0.5) * 22.0;
        batch.add_rect(
            Rect::new(x, wave_y - height, 18.0, height),
            0.059,
            if i & 1 == 0 { palette.cyan } else { palette.copper },
        );
    }
}

fn build_topology_window(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    w: f32,
    h: f32,
    palette: Palette,
    frame: u32,
) {
    let bounds = Rect::new(0.0, 0.0, w, h);
    draw_window_frame(batch, bounds, "TOPOLOGY FIELD", atlas, atlas_texture, palette);

    let points = [
        Vec2::new(w * 0.18, h * 0.28),
        Vec2::new(w * 0.34, h * 0.20),
        Vec2::new(w * 0.56, h * 0.26),
        Vec2::new(w * 0.78, h * 0.20),
        Vec2::new(w * 0.80, h * 0.58),
        Vec2::new(w * 0.56, h * 0.72),
        Vec2::new(w * 0.30, h * 0.66),
        Vec2::new(w * 0.18, h * 0.48),
    ];
    for i in 0..points.len() {
        for j in i + 1..points.len() {
            if (i + j) % 2 == 0 {
                batch.add_graph_edge(
                    points[i],
                    points[j],
                    0.055,
                    1.4,
                    Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.76),
                    Some(Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, 0.16)),
                );
            }
        }
    }
    for (idx, point) in points.iter().enumerate() {
        let pulse = 0.75 + sinf(frame as f32 * 0.05 + idx as f32 * 0.8) * 0.15;
        batch.add_graph_node(point.x, point.y, 8.0, 0.057, Vec4::new(palette.cyan.x, palette.cyan.y, palette.cyan.z, pulse));
        batch.add_graph_node(point.x, point.y, 18.0, 0.056, Vec4::new(palette.copper.x, palette.copper.y, palette.copper.z, 0.10));
    }
    append_text(
        batch,
        atlas,
        atlas_texture,
        "secure services, desktop shell, and tooling surfaces resolved in one graph",
        18.0,
        h - 28.0,
        0.06,
        TextStyle {
            color: palette.text_muted,
            scale: 0.9,
            line_height: 10.0,
            letter_spacing: 0.0,
        },
        Some(bounds),
    );
}

fn build_release_window(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    w: f32,
    h: f32,
    state: &CompositorState,
    telemetry: graphos_compositor::SceneTelemetry,
    palette: Palette,
    frame: u32,
) {
    let bounds = Rect::new(0.0, 0.0, w, h);
    draw_window_frame(batch, bounds, "RELEASE FLOW", atlas, atlas_texture, palette);

    let lane = Rect::new(18.0, 48.0, w - 36.0, 28.0);
    batch.add_rounded_rect(lane, 0.055, palette.panel_fill_alt, 14.0, 8);
    batch.add_rounded_border(lane, 0.056, palette.panel_border, 14.0, 1.0, 8);
    let progress = ((sinf(frame as f32 * 0.02) + 1.0) * 0.5).clamp(0.15, 0.88);
    batch.add_gradient_rect(
        Rect::new(lane.x + 4.0, lane.y + 4.0, (lane.w - 8.0) * progress, lane.h - 8.0),
        0.057,
        palette.copper,
        palette.cyan,
        palette.cyan_soft,
        palette.copper_soft,
    );

    let mut buf = [0u8; 36];
    append_text(
        batch,
        atlas,
        atlas_texture,
        metric_line(&mut buf, b"focus ", telemetry.focused_surface as u32),
        20.0,
        86.0,
        0.058,
        TextStyle {
            color: palette.text,
            scale: 0.92,
            line_height: 10.0,
            letter_spacing: 0.0,
        },
        Some(bounds),
    );
    append_text(
        batch,
        atlas,
        atlas_texture,
        theme_name(state.tone),
        w - 128.0,
        86.0,
        0.058,
        TextStyle {
            color: palette.text_muted,
            scale: 0.92,
            line_height: 10.0,
            letter_spacing: 0.0,
        },
        Some(bounds),
    );

    let releases = [
        ("shell3d", palette.cyan),
        ("ai-console", palette.copper),
        ("fabric tools", palette.positive),
    ];
    for (idx, (label, accent)) in releases.iter().enumerate() {
        let row = Rect::new(18.0, 112.0 + idx as f32 * 32.0, w - 36.0, 24.0);
        batch.add_rounded_rect(row, 0.059, palette.panel_fill, 12.0, 8);
        batch.add_rect(Rect::new(row.x, row.y, 4.0, row.h), 0.060, *accent);
        append_text(
            batch,
            atlas,
            atlas_texture,
            label,
            row.x + 16.0,
            row.y + 7.0,
            0.061,
            TextStyle {
                color: palette.text,
                scale: 0.92,
                line_height: 10.0,
                letter_spacing: 0.0,
            },
            Some(bounds),
        );
    }
}

fn draw_window_frame(
    batch: &mut UiBatch,
    bounds: Rect,
    title: &str,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    palette: Palette,
) {
    batch.add_rounded_rect(bounds, 0.05, Vec4::new(0.04, 0.06, 0.10, 0.98), 18.0, 10);
    batch.add_rounded_border(bounds, 0.051, palette.panel_border, 18.0, 1.0, 10);
    batch.add_gradient_rect(
        Rect::new(bounds.x, bounds.y, bounds.w, 28.0),
        0.052,
        Vec4::new(0.09, 0.12, 0.20, 1.0),
        Vec4::new(0.11, 0.16, 0.24, 1.0),
        Vec4::new(0.06, 0.08, 0.14, 1.0),
        Vec4::new(0.08, 0.10, 0.18, 1.0),
    );
    batch.add_rect(Rect::new(bounds.x, bounds.y, bounds.w * 0.18, 2.0), 0.053, palette.copper);
    append_text(
        batch,
        atlas,
        atlas_texture,
        title,
        16.0,
        10.0,
        0.054,
        TextStyle {
            color: palette.text,
            scale: 1.0,
            line_height: 10.0,
            letter_spacing: 0.2,
        },
        Some(bounds),
    );
}

fn palette_for(tone: ThemeTone) -> Palette {
    match tone {
        ThemeTone::Dark => Palette {
            sky_top: 0xFF182230,
            sky_bottom: 0xFF060A11,
            glass_top: Vec4::new(0.08, 0.10, 0.16, 0.08),
            glass_bottom: Vec4::new(0.02, 0.04, 0.08, 0.22),
            panel_fill: Vec4::new(0.06, 0.08, 0.12, 0.94),
            panel_fill_alt: Vec4::new(0.08, 0.10, 0.16, 0.90),
            panel_border: Vec4::new(0.29, 0.42, 0.60, 0.88),
            copper: Vec4::new(0.92, 0.60, 0.30, 0.92),
            copper_soft: Vec4::new(0.54, 0.25, 0.12, 0.18),
            cyan: Vec4::new(0.46, 0.82, 1.0, 0.92),
            cyan_soft: Vec4::new(0.10, 0.24, 0.34, 0.18),
            text: Vec4::new(0.95, 0.97, 1.0, 1.0),
            text_muted: Vec4::new(0.67, 0.76, 0.88, 1.0),
            positive: Vec4::new(0.44, 0.86, 0.62, 0.92),
        },
        ThemeTone::Light => Palette {
            sky_top: 0xFFF6F8FC,
            sky_bottom: 0xFFCDD7E6,
            glass_top: Vec4::new(0.92, 0.96, 1.0, 0.18),
            glass_bottom: Vec4::new(0.70, 0.80, 0.94, 0.32),
            panel_fill: Vec4::new(0.96, 0.98, 1.0, 0.96),
            panel_fill_alt: Vec4::new(0.90, 0.94, 1.0, 0.96),
            panel_border: Vec4::new(0.50, 0.60, 0.76, 0.88),
            copper: Vec4::new(0.84, 0.48, 0.16, 0.90),
            copper_soft: Vec4::new(0.96, 0.86, 0.78, 0.32),
            cyan: Vec4::new(0.28, 0.62, 0.94, 0.92),
            cyan_soft: Vec4::new(0.76, 0.88, 0.98, 0.34),
            text: Vec4::new(0.10, 0.16, 0.24, 1.0),
            text_muted: Vec4::new(0.34, 0.46, 0.60, 1.0),
            positive: Vec4::new(0.16, 0.64, 0.44, 0.92),
        },
        ThemeTone::HighContrast => Palette {
            sky_top: 0xFF111725,
            sky_bottom: 0xFF04070D,
            glass_top: Vec4::new(0.12, 0.16, 0.22, 0.16),
            glass_bottom: Vec4::new(0.04, 0.06, 0.12, 0.28),
            panel_fill: Vec4::new(0.05, 0.07, 0.11, 0.98),
            panel_fill_alt: Vec4::new(0.08, 0.10, 0.14, 0.98),
            panel_border: Vec4::new(0.66, 0.80, 0.96, 0.96),
            copper: Vec4::new(1.0, 0.74, 0.36, 0.96),
            copper_soft: Vec4::new(0.42, 0.20, 0.10, 0.28),
            cyan: Vec4::new(0.56, 0.88, 1.0, 0.96),
            cyan_soft: Vec4::new(0.12, 0.26, 0.38, 0.26),
            text: Vec4::new(1.0, 1.0, 1.0, 1.0),
            text_muted: Vec4::new(0.84, 0.90, 0.98, 1.0),
            positive: Vec4::new(0.60, 0.96, 0.72, 0.96),
        },
    }
}

fn metric_line<'a>(buf: &'a mut [u8], prefix: &[u8], value: u32) -> &'a str {
    let mut len = 0usize;
    for &byte in prefix {
        if len < buf.len() {
            buf[len] = byte;
            len += 1;
        }
    }
    len += write_u32(&mut buf[len..], value);
    str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("")
}

fn write_u32(buf: &mut [u8], mut value: u32) -> usize {
    if buf.is_empty() {
        return 0;
    }
    if value == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut digits = [0u8; 10];
    let mut len = 0usize;
    while value > 0 && len < digits.len() {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    let mut written = 0usize;
    while len > 0 && written < buf.len() {
        len -= 1;
        buf[written] = digits[len];
        written += 1;
    }
    written
}

fn theme_name(tone: ThemeTone) -> &'static str {
    match tone {
        ThemeTone::Dark => "dark glass",
        ThemeTone::Light => "light frost",
        ThemeTone::HighContrast => "high contrast",
    }
}

fn unpack_rgba(argb: u32) -> Vec4 {
    let a = ((argb >> 24) & 0xFF) as f32 / 255.0;
    let r = ((argb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((argb >> 8) & 0xFF) as f32 / 255.0;
    let b = (argb & 0xFF) as f32 / 255.0;
    Vec4::new(r, g, b, a)
}
