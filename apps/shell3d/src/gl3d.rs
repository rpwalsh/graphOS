// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GL3D rendering module for shell3d scene.
//!
//! This path renders a software-rasterized 3D fusion core and orbiting nodes
//! using graphos-gl's OpenGL-style `Context` API.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use graphos_app_sdk::canvas::Canvas;
use graphos_gl::mesh::{StdVarying, build_sphere};
use graphos_gl::{
    CLEAR_DEPTH, Context, DrawMode, IndexType, Mat4, Shader, Target, Vec2, Vec3, Vec4, Vertex,
};
use libm::{cosf, sinf};

const SPH_LON: u32 = 14;
const SPH_LAT: u32 = 10;
const SPH_VERTS: usize = ((SPH_LON + 1) * (SPH_LAT + 1)) as usize;
const SPH_INDS: usize = (SPH_LON * SPH_LAT * 6) as usize;

#[derive(Clone, Copy)]
struct FlatShader {
    mvp: Mat4,
    tint_rgb: Vec3,
}

impl Shader for FlatShader {
    type Vertex = Vertex;
    type Varying = StdVarying;

    fn vertex(&self, v: &Self::Vertex) -> (Vec4, Self::Varying) {
        (
            self.mvp.mul_vec4(v.pos.extend(1.0)),
            StdVarying {
                world_pos: v.pos,
                normal: v.normal,
                uv: Vec2::new(v.uv.x, v.uv.y),
                color: self.tint_rgb,
            },
        )
    }

    fn fragment(&self, v: &Self::Varying) -> Option<Vec4> {
        let n = v.normal.normalize();
        let light = Vec3::new(-0.35, 0.75, 0.56).normalize();
        let ndotl = (n.dot(light)).max(0.0);
        let shade = 0.30 + ndotl * 0.70;
        let rgb = v.color * shade;
        // graphos-gl expects BGRA channel order.
        Some(Vec4::new(rgb.z, rgb.y, rgb.x, 1.0))
    }
}

pub struct GlScene {
    ctx: Context,
    ebo: u32,
    verts: Vec<Vertex>,
    depth: Vec<f32>,
    index_count: usize,
}

impl GlScene {
    pub fn new(width: u32, height: u32) -> Option<Self> {
        let mut verts = vec![
            Vertex {
                pos: Vec3::ZERO,
                normal: Vec3::ZERO,
                uv: Vec2::new(0.0, 0.0),
                color: Vec3::ONE,
            };
            SPH_VERTS
        ];
        let mut inds = vec![0u32; SPH_INDS];
        let (v_count, i_count) = build_sphere(&mut verts, &mut inds, SPH_LON, SPH_LAT, Vec3::ONE);
        verts.truncate(v_count);

        let mut ctx = Context::new();
        ctx.viewport(0, 0, width as i32, height as i32);
        ctx.enable_depth_test(true);
        ctx.depth_mask(true);
        ctx.clear_depth_value(1.0);

        let mut names = [0u32; 1];
        if ctx.gen_buffers(&mut names) != 1 {
            return None;
        }
        let ebo = names[0];

        // SAFETY: inds is a contiguous Vec<u32> and we only create a read-only byte slice.
        let index_bytes = unsafe {
            core::slice::from_raw_parts(
                inds.as_ptr() as *const u8,
                i_count * core::mem::size_of::<u32>(),
            )
        };
        ctx.buffer_data(ebo, index_bytes);
        ctx.bind_element_buffer(ebo);

        Some(Self {
            ctx,
            ebo,
            verts,
            depth: vec![1.0; (width * height) as usize],
            index_count: i_count,
        })
    }

    pub fn render(
        &mut self,
        canvas: &mut Canvas<'_>,
        orbit_x_deg: i32,
        orbit_y_deg: i32,
        hover: Option<usize>,
        scene_top: u32,
        scene_h: u32,
    ) {
        let w = canvas.width();
        let h = canvas.height();
        let px_count = (w * h) as usize;
        if self.depth.len() != px_count {
            self.depth.resize(px_count, 1.0);
        }

        let viewport_h = scene_h.min(h).max(1);
        let viewport_y = scene_top.min(h.saturating_sub(1));

        self.ctx
            .viewport(0, viewport_y as i32, w as i32, viewport_h as i32);
        self.ctx
            .scissor(0, viewport_y as i32, w as i32, viewport_h as i32);
        self.ctx.enable_scissor_test(true);
        self.ctx.enable_depth_test(true);
        self.ctx.depth_mask(true);
        self.ctx.clear_depth_value(1.0);
        self.ctx.bind_element_buffer(self.ebo);

        self.ctx
            .clear(CLEAR_DEPTH, None, Some(&mut self.depth), None);

        let mut target = Target::new(canvas.pixels_mut(), &mut self.depth, w, h);

        let aspect = w as f32 / viewport_h as f32;
        let proj = Mat4::perspective(1.05, aspect, 0.1, 100.0);
        let view = Mat4::look_at(Vec3::new(0.0, 0.1, 4.2), Vec3::new(0.0, 0.0, 0.0), Vec3::Y);
        let orbit = Mat4::rotation_x((orbit_x_deg as f32) * 0.0174533)
            .mul_mat(&Mat4::rotation_y((orbit_y_deg as f32) * 0.0174533));
        let vp = proj.mul_mat(&view).mul_mat(&orbit);

        // Central fusion core.
        let core_model = Mat4::scale(Vec3::new(0.95, 0.95, 0.95));
        let core_shader = FlatShader {
            mvp: vp.mul_mat(&core_model),
            tint_rgb: Vec3::new(1.0, 0.83, 0.45),
        };
        let _ = self.ctx.draw_elements(
            &mut target,
            &core_shader,
            &self.verts,
            self.index_count,
            IndexType::U32,
            0,
            DrawMode::Triangles,
        );

        // Orbiting nodes.
        for i in 0..8usize {
            let ang = (i as f32) * 0.7853982;
            let pos = Vec3::new(
                cosf(ang) * 2.05,
                ((i as i32 % 3) as f32 - 1.0) * 0.28,
                sinf(ang) * 2.05,
            );
            let scale = if hover == Some(i) { 0.52 } else { 0.40 };
            let model = Mat4::translation(pos).mul_mat(&Mat4::scale(Vec3::splat(scale)));
            let tint = if hover == Some(i) {
                Vec3::new(0.78, 0.88, 1.0)
            } else {
                Vec3::new(0.36 + (i as f32 * 0.04), 0.62, 0.95 - (i as f32 * 0.03))
            };
            let shader = FlatShader {
                mvp: vp.mul_mat(&model),
                tint_rgb: tint,
            };
            let _ = self.ctx.draw_elements(
                &mut target,
                &shader,
                &self.verts,
                self.index_count,
                IndexType::U32,
                0,
                DrawMode::Triangles,
            );
        }
    }
}
