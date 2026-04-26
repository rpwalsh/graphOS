// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! The rasterization pipeline.
//!
//! [`Pipeline::draw`] consumes a vertex slice and an index slice (triangle
//! list), invokes the user's [`Shader`], clips against the near plane,
//! performs the perspective divide, runs the viewport transform, then
//! rasterizes each triangle with edge functions and perspective-correct
//! varying interpolation. Per-fragment depth/stencil tests update the
//! supplied buffers; surviving fragments are blended using the full
//! configurable OpenGL blend equations.

use alloc::vec::Vec;

use crate::gl::{
    BlendEquation, BlendFactor, CullFace, DepthFunc, DrawMode, StencilFunc, StencilOp,
};
use crate::math::{Vec3, Vec4};
use crate::shader::{FragmentOutputs, Shader, Varying};
use libm::{ceilf, floorf, roundf, sqrtf};

/// Color + depth + optional stencil render target.
///
/// `color` is packed BGRA32 storage; shader-visible colors remain RGBA.
/// `depth` is normalized [0,1] f32; `stencil` is u8.
/// All slices must be `width * height` elements.
pub struct Target<'a> {
    pub color: &'a mut [u32],
    pub extra_colors: Vec<(u8, &'a mut [u32])>,
    pub depth: &'a mut [f32],
    pub stencil: Option<&'a mut [u8]>,
    pub width: u32,
    pub height: u32,
}

impl<'a> Target<'a> {
    pub fn new(color: &'a mut [u32], depth: &'a mut [f32], width: u32, height: u32) -> Self {
        let expected = width as usize * height as usize;
        debug_assert_eq!(
            color.len(),
            expected,
            "color buffer len must equal width*height"
        );
        debug_assert_eq!(
            depth.len(),
            expected,
            "depth buffer len must equal width*height"
        );
        Self {
            color,
            extra_colors: Vec::new(),
            depth,
            stencil: None,
            width,
            height,
        }
    }

    pub fn with_stencil(
        color: &'a mut [u32],
        depth: &'a mut [f32],
        stencil: &'a mut [u8],
        width: u32,
        height: u32,
    ) -> Self {
        let expected = width as usize * height as usize;
        debug_assert_eq!(
            color.len(),
            expected,
            "color buffer len must equal width*height"
        );
        debug_assert_eq!(
            depth.len(),
            expected,
            "depth buffer len must equal width*height"
        );
        debug_assert_eq!(
            stencil.len(),
            expected,
            "stencil buffer len must equal width*height"
        );
        Self {
            color,
            extra_colors: Vec::new(),
            depth,
            stencil: Some(stencil),
            width,
            height,
        }
    }

    /// Attach an additional MRT color buffer to a specific color attachment index.
    pub fn with_extra_color(mut self, attachment: u8, color: &'a mut [u32]) -> Self {
        let expected = self.width as usize * self.height as usize;
        debug_assert!(attachment < 8, "attachment index must be < 8");
        debug_assert_ne!(attachment, 0, "use Target::new for attachment 0");
        debug_assert_eq!(
            color.len(),
            expected,
            "color buffer len must equal width*height"
        );
        self.extra_colors.push((attachment, color));
        self
    }

    /// Clear color (BGRA32) and/or depth and/or stencil.
    pub fn clear_all(&mut self, color: u32, depth: f32, stencil: u8) {
        self.color.fill(color);
        for (_, pixels) in &mut self.extra_colors {
            pixels.fill(color);
        }
        self.depth.fill(depth);
        if let Some(s) = &mut self.stencil {
            s.fill(stencil);
        }
    }

    /// Reset color to `clear_color` (BGRA32) and depth to `1.0` (far plane).
    pub fn clear(&mut self, clear_color: u32) {
        self.color.fill(clear_color);
        for (_, pixels) in &mut self.extra_colors {
            pixels.fill(clear_color);
        }
        self.depth.fill(1.0);
    }

    fn raw_color_ptrs(&mut self) -> ([*mut u32; 8], [bool; 8]) {
        let expected = self.width as usize * self.height as usize;
        let mut ptrs = [core::ptr::null_mut(); 8];
        let mut present = [false; 8];
        ptrs[0] = self.color.as_mut_ptr();
        present[0] = true;
        for (attachment, pixels) in &mut self.extra_colors {
            let index = *attachment as usize;
            debug_assert_eq!(
                pixels.len(),
                expected,
                "color buffer len must equal width*height"
            );
            if index < 8 {
                ptrs[index] = pixels.as_mut_ptr();
                present[index] = true;
            }
        }
        (ptrs, present)
    }
}

/// Full OpenGL rasterization pipeline state.
///
/// Build via [`Pipeline::opaque_3d()`] / [`Pipeline::transparent_3d()`] for
/// common cases, or construct via [`Context::current_pipeline()`] to mirror
/// the full GL state machine.
#[derive(Clone, Copy, Debug)]
pub struct Pipeline {
    // Culling
    pub cull_face: CullFace,
    // Depth
    pub depth_test: bool,
    pub depth_write: bool,
    pub depth_func: DepthFunc,
    // Stencil
    pub stencil_test: bool,
    pub stencil_func: StencilFunc,
    pub stencil_ref: u8,
    pub stencil_mask_r: u8,
    pub stencil_mask_w: u8,
    pub stencil_fail: StencilOp,
    pub stencil_zfail: StencilOp,
    pub stencil_zpass: StencilOp,
    // Blend
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
    // Scissor
    pub scissor_test: bool,
    pub scissor: [i32; 4],
    // Active color attachments.
    pub draw_buffers_mask: u8,
    // Color write mask for attachment 0 [R,G,B,A].
    pub color_mask: [bool; 4],
    // Per-attachment color write masks for MRT.
    pub color_masks: [[bool; 4]; 8],
    // Point and line primitive sizes.
    pub point_size: f32,
    pub line_width: f32,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::opaque_3d()
    }
}

impl Pipeline {
    /// Standard opaque 3-D rendering: backface cull, depth test, no blend.
    pub const fn opaque_3d() -> Self {
        Self {
            cull_face: CullFace::Back,
            depth_test: true,
            depth_write: true,
            depth_func: DepthFunc::Less,
            stencil_test: false,
            stencil_func: StencilFunc::Always,
            stencil_ref: 0,
            stencil_mask_r: 0xFF,
            stencil_mask_w: 0xFF,
            stencil_fail: StencilOp::Keep,
            stencil_zfail: StencilOp::Keep,
            stencil_zpass: StencilOp::Keep,
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
            scissor_test: false,
            scissor: [0; 4],
            draw_buffers_mask: 0x01,
            color_mask: [true; 4],
            color_masks: [[true; 4]; 8],
            point_size: 1.0,
            line_width: 1.0,
        }
    }

    /// Transparent 3-D: no cull, depth test (no depth write), src-alpha blend.
    pub const fn transparent_3d() -> Self {
        Self {
            cull_face: CullFace::None,
            depth_test: true,
            depth_write: false,
            blend: true,
            blend_src_rgb: BlendFactor::SrcAlpha,
            blend_dst_rgb: BlendFactor::OneMinusSrcAlpha,
            blend_src_alpha: BlendFactor::One,
            blend_dst_alpha: BlendFactor::OneMinusSrcAlpha,
            ..Self::opaque_3d()
        }
    }

    /// Legacy compat — used by existing call sites that set `cull_back`.
    #[allow(non_upper_case_globals)]
    pub const OPAQUE_3D: Pipeline = Self::opaque_3d();
    #[allow(non_upper_case_globals)]
    pub const TRANSPARENT_3D: Pipeline = Self::transparent_3d();

    /// Draw primitives using `mode` topology.
    ///
    /// `vertices` is the vertex buffer; `indices` is a triangle-list index buffer
    /// for `Triangles` mode, or sequential indices for strip/fan.
    pub fn draw<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        indices: &[u32],
    ) {
        self.draw_mode(target, shader, vertices, indices, DrawMode::Triangles);
    }

    /// Draw with explicit primitive topology.
    pub fn draw_mode<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        indices: &[u32],
        mode: DrawMode,
    ) {
        match mode {
            DrawMode::Triangles => {
                for tri in indices.chunks_exact(3) {
                    self.submit_triangle(target, shader, vertices, tri[0], tri[1], tri[2]);
                }
            }
            DrawMode::TriangleStrip => {
                for i in 0..indices.len().saturating_sub(2) {
                    // Alternate winding for even/odd
                    let (a, b, c) = if i & 1 == 0 {
                        (indices[i], indices[i + 1], indices[i + 2])
                    } else {
                        (indices[i + 1], indices[i], indices[i + 2])
                    };
                    self.submit_triangle(target, shader, vertices, a, b, c);
                }
            }
            DrawMode::TriangleFan => {
                if indices.len() < 3 {
                    return;
                }
                let pivot = indices[0];
                for i in 1..indices.len().saturating_sub(1) {
                    self.submit_triangle(
                        target,
                        shader,
                        vertices,
                        pivot,
                        indices[i],
                        indices[i + 1],
                    );
                }
            }
            DrawMode::Lines => {
                for pair in indices.chunks_exact(2) {
                    let Some(v0) = vertices.get(pair[0] as usize).copied() else {
                        continue;
                    };
                    let Some(v1) = vertices.get(pair[1] as usize).copied() else {
                        continue;
                    };
                    let (p0, y0) = shader.vertex(&v0);
                    let (p1, y1) = shader.vertex(&v1);
                    self.rasterize_line(target, shader, p0, y0, p1, y1);
                }
            }
            DrawMode::LineStrip => {
                for i in 0..indices.len().saturating_sub(1) {
                    let Some(v0) = vertices.get(indices[i] as usize).copied() else {
                        continue;
                    };
                    let Some(v1) = vertices.get(indices[i + 1] as usize).copied() else {
                        continue;
                    };
                    let (p0, y0) = shader.vertex(&v0);
                    let (p1, y1) = shader.vertex(&v1);
                    self.rasterize_line(target, shader, p0, y0, p1, y1);
                }
            }
            DrawMode::LineLoop => {
                let n = indices.len();
                if n < 2 {
                    return;
                }
                for i in 0..n {
                    let Some(v0) = vertices.get(indices[i] as usize).copied() else {
                        continue;
                    };
                    let Some(v1) = vertices.get(indices[(i + 1) % n] as usize).copied() else {
                        continue;
                    };
                    let (p0, y0) = shader.vertex(&v0);
                    let (p1, y1) = shader.vertex(&v1);
                    self.rasterize_line(target, shader, p0, y0, p1, y1);
                }
            }
            DrawMode::Points => {
                for &idx in indices {
                    let Some(v) = vertices.get(idx as usize).copied() else {
                        continue;
                    };
                    let (p, y) = shader.vertex(&v);
                    self.rasterize_point(target, shader, p, y);
                }
            }
        }
    }

    fn submit_triangle<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        vertices: &[S::Vertex],
        i0: u32,
        i1: u32,
        i2: u32,
    ) {
        let Some(v0) = vertices.get(i0 as usize).copied() else {
            return;
        };
        let Some(v1) = vertices.get(i1 as usize).copied() else {
            return;
        };
        let Some(v2) = vertices.get(i2 as usize).copied() else {
            return;
        };
        let (p0, y0) = shader.vertex(&v0);
        let (p1, y1) = shader.vertex(&v1);
        let (p2, y2) = shader.vertex(&v2);
        self.process_triangle(target, shader, p0, y0, p1, y1, p2, y2);
    }

    fn process_triangle<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        p0: Vec4,
        y0: S::Varying,
        p1: Vec4,
        y1: S::Varying,
        p2: Vec4,
        y2: S::Varying,
    ) {
        // Trivial reject — all vertices behind any single clip plane.
        if p0.w <= 0.0 && p1.w <= 0.0 && p2.w <= 0.0 {
            return;
        }

        // Full 6-plane Sutherland–Hodgman clip (ES 3.0 §2.13).
        // A triangle clipped against 6 planes can produce up to 7 vertices.
        let init = [(p0, y0), (p1, y1), (p2, y2)];
        let mut poly_a = [(Vec4::new(0.0, 0.0, 0.0, 1.0), y0); 9];
        let mut poly_b = [(Vec4::new(0.0, 0.0, 0.0, 1.0), y0); 9];
        let n = clip_all_planes(&init, &mut poly_a, &mut poly_b);
        if n < 3 {
            return;
        }

        // Fan-triangulate the clipped polygon and rasterize each triangle.
        let v0 = poly_a[0];
        let mut tri_buf: [(Vec4, S::Varying); 3] = [v0, poly_a[1], poly_a[2]];
        self.rasterize(target, shader, &tri_buf);
        for i in 2..n - 1 {
            tri_buf = [v0, poly_a[i], poly_a[i + 1]];
            self.rasterize(target, shader, &tri_buf);
        }
    }

    fn rasterize<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        tri: &[(Vec4, S::Varying); 3],
    ) {
        let w = target.width as i32;
        let h = target.height as i32;

        // Perspective divide → NDC, then viewport.
        let inv_w = [1.0 / tri[0].0.w, 1.0 / tri[1].0.w, 1.0 / tri[2].0.w];
        let ndc = [
            Vec3::new(
                tri[0].0.x * inv_w[0],
                tri[0].0.y * inv_w[0],
                tri[0].0.z * inv_w[0],
            ),
            Vec3::new(
                tri[1].0.x * inv_w[1],
                tri[1].0.y * inv_w[1],
                tri[1].0.z * inv_w[1],
            ),
            Vec3::new(
                tri[2].0.x * inv_w[2],
                tri[2].0.y * inv_w[2],
                tri[2].0.z * inv_w[2],
            ),
        ];
        let half_w = w as f32 * 0.5;
        let half_h = h as f32 * 0.5;
        let sx = [
            (ndc[0].x + 1.0) * half_w,
            (ndc[1].x + 1.0) * half_w,
            (ndc[2].x + 1.0) * half_w,
        ];
        let sy = [
            (1.0 - ndc[0].y) * half_h,
            (1.0 - ndc[1].y) * half_h,
            (1.0 - ndc[2].y) * half_h,
        ];
        let sz = [
            ndc[0].z * 0.5 + 0.5,
            ndc[1].z * 0.5 + 0.5,
            ndc[2].z * 0.5 + 0.5,
        ];

        // Edge function (twice the signed area of the triangle).
        let area = edge(sx[0], sy[0], sx[1], sy[1], sx[2], sy[2]);
        if area.abs() < 1e-4 {
            return;
        }

        // Propagate winding to varyings for gl_FrontFacing (GAP-010).
        // area > 0 means CCW screen-space = front face (GL y-down convention).
        let is_front = area > 0.0;
        let mut tri_varyings = [tri[0].1, tri[1].1, tri[2].1];
        tri_varyings[0].set_front_facing(is_front);
        tri_varyings[1].set_front_facing(is_front);
        tri_varyings[2].set_front_facing(is_front);
        let tri_refs: [(Vec4, S::Varying); 3] = [
            (tri[0].0, tri_varyings[0]),
            (tri[1].0, tri_varyings[1]),
            (tri[2].0, tri_varyings[2]),
        ];
        let tri = &tri_refs;

        // Face culling.
        match self.cull_face {
            crate::gl::CullFace::Back => {
                if area < 0.0 {
                    return;
                }
            }
            crate::gl::CullFace::Front => {
                if area > 0.0 {
                    return;
                }
            }
            crate::gl::CullFace::FrontAndBack => return,
            crate::gl::CullFace::None => {}
        }

        // Bounding box clipped to viewport.
        let mut min_x = floorf(sx[0].min(sx[1]).min(sx[2])).max(0.0) as i32;
        let mut min_y = floorf(sy[0].min(sy[1]).min(sy[2])).max(0.0) as i32;
        let mut max_x = ceilf(sx[0].max(sx[1]).max(sx[2])).min((w - 1) as f32) as i32;
        let mut max_y = ceilf(sy[0].max(sy[1]).max(sy[2])).min((h - 1) as f32) as i32;

        // Scissor clamp.
        if self.scissor_test {
            let [sx, sy, sw, sh] = self.scissor;
            min_x = min_x.max(sx);
            min_y = min_y.max(sy);
            max_x = max_x.min(sx + sw - 1);
            max_y = max_y.min(sy + sh - 1);
        }

        if min_x > max_x || min_y > max_y {
            return;
        }

        let inv_area = 1.0 / area;

        // Top-left fill convention (ES 3.0 §14.6.1).
        // An edge is "top-left" if it is a top edge (horizontal going right,
        // i.e. dy == 0 && dx > 0) or a left edge (going downward, dy > 0).
        // We add a tiny negative bias to the edge function for non-top-left
        // edges so that tie-breaking pixels are excluded rather than double-filled.
        //
        // Edge 0 spans v1→v2, edge 1 spans v2→v0, edge 2 spans v0→v1.
        // A positive area means CCW in screen space (y downward).
        const TL_BIAS: f32 = -1e-6;
        #[inline]
        fn tl_bias(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
            let dx = bx - ax;
            let dy = by - ay;
            // top edge: dy == 0 && dx > 0  OR  left edge: dy > 0
            if (dy == 0.0 && dx > 0.0) || dy > 0.0 {
                0.0
            } else {
                TL_BIAS
            }
        }
        let bias0 = tl_bias(sx[1], sy[1], sx[2], sy[2]);
        let bias1 = tl_bias(sx[2], sy[2], sx[0], sy[0]);
        let bias2 = tl_bias(sx[0], sy[0], sx[1], sy[1]);

        // Pre-scale varyings by 1/w for hyperbolic interpolation.
        let vy_over_w = [
            tri[0].1.scale(inv_w[0]),
            tri[1].1.scale(inv_w[1]),
            tri[2].1.scale(inv_w[2]),
        ];

        // GAP-007: compute per-triangle barycentric gradients for dFdx/dFdy.
        // For w_i = edge_i(cx,cy) / area, the screen-space gradients are
        // constant per triangle and follow directly from the edge function.
        // edge_i = (B.x-A.x)*(cy-A.y) - (B.y-A.y)*(cx-A.x)
        // d(edge_i)/dcx = -(B.y-A.y),  d(edge_i)/dcy = (B.x-A.x)
        let dw0dx = -(sy[2] - sy[1]) * inv_area;
        let dw1dx = -(sy[0] - sy[2]) * inv_area;
        let dw2dx = -(sy[1] - sy[0]) * inv_area;
        let dw0dy = (sx[2] - sx[1]) * inv_area;
        let dw1dy = (sx[0] - sx[2]) * inv_area;
        let dw2dy = (sx[1] - sx[0]) * inv_area;
        let d_inv_w_dx = dw0dx * inv_w[0] + dw1dx * inv_w[1] + dw2dx * inv_w[2];
        let d_inv_w_dy = dw0dy * inv_w[0] + dw1dy * inv_w[1] + dw2dy * inv_w[2];
        S::Varying::init_triangle_derivatives(
            &vy_over_w[0],
            &vy_over_w[1],
            &vy_over_w[2],
            dw0dx,
            dw1dx,
            dw2dx,
            dw0dy,
            dw1dy,
            dw2dy,
            d_inv_w_dx,
            d_inv_w_dy,
        );

        let tgt_w = target.width as usize;
        let (color_ptrs, color_present) = target.raw_color_ptrs();
        let depth_ptr = target.depth.as_mut_ptr();
        let stencil_ptr = target.stencil.as_deref_mut().map(|s| s.as_mut_ptr());
        debug_assert_eq!(
            target.color.len(),
            target.width as usize * target.height as usize
        );
        debug_assert_eq!(
            target.depth.len(),
            target.width as usize * target.height as usize
        );

        // ── Parallel row loop (bare-metal kernel threads) ────────────────────────
        //
        // On `x86_64-unknown-none` we use the GraphOS SYS_THREAD_SPAWN /
        // SYS_THREAD_JOIN syscalls to split the row range into up to N bands,
        // each processed by a kernel-scheduled thread.
        //
        // Safety invariants:
        //  1. Row bands are disjoint — thread t writes rows [band_min, band_max]
        //     only, which map to non-overlapping pixel indices.
        //  2. `init_triangle_derivatives` wrote TRIANGLE_DERIV *before* any
        //     thread is spawned; threads only *read* it — no race.
        //  3. `shader.fragment_outputs` is read-only during fragment execution.
        //  4. S::Varying: Copy — no reference types can be shared unsafely.
        //  5. All raw pointer arguments to band_rasterize_row_band are valid
        //     for the full duration of the parallel section; SYS_THREAD_JOIN
        //     provides the required happens-before edge before we return.
        #[cfg(target_os = "none")]
        {
            const MIN_ROWS_FOR_PAR: usize = usize::MAX; // disabled: serial is faster on TCG single-core
            const MAX_PAR_THREADS: usize = 4;

            let n_rows = (max_y - min_y + 1) as usize;
            if n_rows >= MIN_ROWS_FOR_PAR {
                let n_threads = n_rows.min(MAX_PAR_THREADS);
                let band_size = (n_rows + n_threads - 1) / n_threads;

                // Descriptor placed on the stack of this (parent) thread.
                // All spawned threads finish before we return from `rasterize`,
                // so lifetime is upheld by join.
                #[repr(C)]
                struct BandDesc<V: Copy> {
                    // rasterizer inputs (all read-only in workers)
                    pipeline: crate::pipeline::Pipeline,
                    shader_ptr: *const (),
                    color_ptrs: [*mut u32; 8],
                    color_present: [bool; 8],
                    depth_ptr: *mut f32,
                    stencil_ptr: *mut u8,
                    has_stencil: bool,
                    vy_over_w: [V; 3],
                    sx: [f32; 3],
                    sy: [f32; 3],
                    sz: [f32; 3],
                    inv_w: [f32; 3],
                    inv_area: f32,
                    bias0: f32,
                    bias1: f32,
                    bias2: f32,
                    tgt_w: usize,
                    min_x: i32,
                    max_x: i32,
                    // band limits
                    band_min_y: i32,
                    band_max_y: i32,
                    // per-pixel fragment fn
                    fragment_fn:
                        unsafe fn(*const (), *const V) -> Option<crate::shader::FragmentOutputs>,
                }

                // Per-pixel fragment_outputs trampoline — no captures, no closures.
                unsafe fn fragment_call<S: crate::shader::Shader>(
                    shader_ptr: *const (),
                    varying: *const S::Varying,
                ) -> Option<crate::shader::FragmentOutputs> {
                    let shader: &S = unsafe { &*(shader_ptr as *const S) };
                    let varying: &S::Varying = unsafe { &*varying };
                    shader.fragment_outputs(varying)
                }

                // Band entry point — called by each kernel thread.
                unsafe extern "C" fn band_rasterize<V: Copy + crate::shader::Varying>(arg: u64) {
                    let desc = unsafe { &*(arg as *const BandDesc<V>) };
                    let stencil_ptr = if desc.has_stencil {
                        Some(desc.stencil_ptr)
                    } else {
                        None
                    };
                    for py in desc.band_min_y..=desc.band_max_y {
                        let row_off = py as usize * desc.tgt_w;
                        for px in desc.min_x..=desc.max_x {
                            let cx = px as f32 + 0.5;
                            let cy = py as f32 + 0.5;
                            let w0 = edge(desc.sx[1], desc.sy[1], desc.sx[2], desc.sy[2], cx, cy)
                                * desc.inv_area
                                + desc.bias0;
                            let w1 = edge(desc.sx[2], desc.sy[2], desc.sx[0], desc.sy[0], cx, cy)
                                * desc.inv_area
                                + desc.bias1;
                            let w2 = edge(desc.sx[0], desc.sy[0], desc.sx[1], desc.sy[1], cx, cy)
                                * desc.inv_area
                                + desc.bias2;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }
                            let z = w0 * desc.sz[0] + w1 * desc.sz[1] + w2 * desc.sz[2];
                            if z < 0.0 || z > 1.0 {
                                continue;
                            }
                            let idx = row_off + px as usize;
                            let one_over_w =
                                w0 * desc.inv_w[0] + w1 * desc.inv_w[1] + w2 * desc.inv_w[2];
                            if one_over_w.abs() < 1e-8 {
                                continue;
                            }
                            let mut varying = V::weighted_sum(
                                desc.vy_over_w[0],
                                w0,
                                desc.vy_over_w[1],
                                w1,
                                desc.vy_over_w[2],
                                w2,
                            )
                            .scale(1.0 / one_over_w);
                            varying.finalize_pixel_derivatives(one_over_w);
                            let frag_out = unsafe {
                                (desc.fragment_fn)(desc.shader_ptr, &varying as *const V)
                            };
                            let _ = desc.pipeline.fragment_ops_raw(
                                px,
                                py,
                                idx,
                                z,
                                desc.color_ptrs,
                                desc.color_present,
                                desc.depth_ptr,
                                stencil_ptr,
                                || frag_out,
                            );
                        }
                    }
                }

                // Build one descriptor per band and spawn threads.
                let pipeline_copy = *self;
                // Erase the generic pipeline to the unit type for storage in BandDesc.
                // SAFETY: Pipeline<()> and Pipeline<S> are repr(C)-compatible because
                // Pipeline contains no S-typed field; the generic parameter is phantom.
                let pipeline_erased: crate::pipeline::Pipeline =
                    unsafe { core::mem::transmute_copy(&pipeline_copy) };

                const MAX_BANDS: usize = MAX_PAR_THREADS;
                let mut descs: [core::mem::MaybeUninit<BandDesc<S::Varying>>; MAX_BANDS] =
                    [const { core::mem::MaybeUninit::uninit() }; MAX_BANDS];
                let mut handles: [Option<crate::thread::ThreadHandle>; MAX_BANDS] =
                    [const { None }; MAX_BANDS];

                for t in 0..n_threads {
                    let band_min = min_y + (t * band_size) as i32;
                    let band_max = (band_min + band_size as i32 - 1).min(max_y);
                    if band_min > max_y {
                        break;
                    }

                    descs[t].write(BandDesc {
                        pipeline: pipeline_erased,
                        shader_ptr: shader as *const S as *const (),
                        color_ptrs,
                        color_present,
                        depth_ptr,
                        stencil_ptr: stencil_ptr.unwrap_or(core::ptr::null_mut()),
                        has_stencil: stencil_ptr.is_some(),
                        vy_over_w,
                        sx,
                        sy,
                        sz,
                        inv_w,
                        inv_area,
                        bias0,
                        bias1,
                        bias2,
                        tgt_w,
                        min_x,
                        max_x,
                        band_min_y: band_min,
                        band_max_y: band_max,
                        fragment_fn: fragment_call::<S>,
                    });

                    let arg = unsafe { descs[t].as_ptr() } as u64;
                    // The band_rasterize fn is monomorphised over S::Varying.
                    let entry: unsafe extern "C" fn(u64) = band_rasterize::<S::Varying>;
                    handles[t] = crate::thread::thread_spawn(entry, arg);
                }

                // Join all spawned threads (serial fallback for bands where
                // spawn failed, or when no kernel threads are available).
                for t in 0..n_threads {
                    if let Some(h) = handles[t].take() {
                        crate::thread::thread_join(h);
                    } else {
                        // Spawn failed — run this band synchronously.
                        let band_min = min_y + (t * band_size) as i32;
                        let band_max = (band_min + band_size as i32 - 1).min(max_y);
                        if band_min > max_y {
                            break;
                        }
                        let arg = unsafe { descs[t].as_ptr() } as u64;
                        unsafe { band_rasterize::<S::Varying>(arg) };
                    }
                    // Drop the descriptor (it was MaybeUninit — nothing to drop for POD).
                }
                return;
            }
        }

        for py in min_y..=max_y {
            let row_off = py as usize * tgt_w;
            for px in min_x..=max_x {
                let cx = px as f32 + 0.5;
                let cy = py as f32 + 0.5;

                let w0 = edge(sx[1], sy[1], sx[2], sy[2], cx, cy) * inv_area + bias0;
                let w1 = edge(sx[2], sy[2], sx[0], sy[0], cx, cy) * inv_area + bias1;
                let w2 = edge(sx[0], sy[0], sx[1], sy[1], cx, cy) * inv_area + bias2;

                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = w0 * sz[0] + w1 * sz[1] + w2 * sz[2];
                if z < 0.0 || z > 1.0 {
                    continue;
                }
                let idx = row_off + px as usize;
                let one_over_w = w0 * inv_w[0] + w1 * inv_w[1] + w2 * inv_w[2];
                if one_over_w.abs() < 1e-8 {
                    continue;
                }
                let mut varying = <S::Varying as Varying>::weighted_sum(
                    vy_over_w[0],
                    w0,
                    vy_over_w[1],
                    w1,
                    vy_over_w[2],
                    w2,
                )
                .scale(1.0 / one_over_w);
                varying.finalize_pixel_derivatives(one_over_w);
                let _ = self.fragment_ops_raw(
                    px,
                    py,
                    idx,
                    z,
                    color_ptrs,
                    color_present,
                    depth_ptr,
                    stencil_ptr,
                    || shader.fragment_outputs(&varying),
                );
            }
        }
    }

    fn rasterize_point<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        p: Vec4,
        y: S::Varying,
    ) {
        if p.w.abs() < 1e-8 {
            return;
        }
        let w = target.width as i32;
        let h = target.height as i32;
        let cx = ((p.x / p.w + 1.0) * w as f32 * 0.5) as i32;
        let cy = ((1.0 - p.y / p.w) * h as f32 * 0.5) as i32;
        let z = (p.z / p.w) * 0.5 + 0.5;
        if z < 0.0 || z > 1.0 {
            return;
        }

        let n = (floorf(self.point_size + 0.5) as i32).max(1);
        let lo = -(n / 2);
        let hi = lo + n - 1;

        let (color_ptrs, color_present) = target.raw_color_ptrs();
        let depth_ptr = target.depth.as_mut_ptr();
        let stencil_ptr = target.stencil.as_deref_mut().map(|s| s.as_mut_ptr());
        let tgt_w = target.width as usize;

        for dy in lo..=hi {
            let py = cy + dy;
            if py < 0 || py >= h {
                continue;
            }
            for dx in lo..=hi {
                let px = cx + dx;
                if px < 0 || px >= w {
                    continue;
                }
                let idx = py as usize * tgt_w + px as usize;
                let _ = self.fragment_ops_raw(
                    px,
                    py,
                    idx,
                    z,
                    color_ptrs,
                    color_present,
                    depth_ptr,
                    stencil_ptr,
                    || shader.fragment_outputs(&y),
                );
            }
        }
    }

    fn rasterize_line<S: Shader>(
        &self,
        target: &mut Target<'_>,
        shader: &S,
        p0: Vec4,
        y0: S::Varying,
        p1: Vec4,
        y1: S::Varying,
    ) {
        if p0.w.abs() < 1e-8 || p1.w.abs() < 1e-8 {
            return;
        }
        let w = target.width as i32;
        let h = target.height as i32;

        let sx0 = ((p0.x / p0.w + 1.0) * w as f32 * 0.5) as i32;
        let sy0 = ((1.0 - p0.y / p0.w) * h as f32 * 0.5) as i32;
        let sz0 = (p0.z / p0.w) * 0.5 + 0.5;
        let sx1 = ((p1.x / p1.w + 1.0) * w as f32 * 0.5) as i32;
        let sy1 = ((1.0 - p1.y / p1.w) * h as f32 * 0.5) as i32;
        let sz1 = (p1.z / p1.w) * 0.5 + 0.5;

        let (color_ptrs, color_present) = target.raw_color_ptrs();
        let depth_ptr = target.depth.as_mut_ptr();
        let stencil_ptr = target.stencil.as_deref_mut().map(|s| s.as_mut_ptr());
        let tgt_w = target.width as usize;

        // Parametric walk: step in the dominant axis direction.
        let dx_line = (sx1 - sx0) as f32;
        let dy_line = (sy1 - sy0) as f32;
        let steps = (sx1 - sx0).abs().max((sy1 - sy0).abs()).max(1);
        let line_len = sqrtf(dx_line * dx_line + dy_line * dy_line).max(1e-8);
        // Perpendicular unit vector (screen-space) for thick-line expansion.
        let perp_x = -dy_line / line_len;
        let perp_y = dx_line / line_len;
        let half_w = floorf(self.line_width * 0.5) as i32;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let bx = sx0 as f32 + dx_line * t;
            let by = sy0 as f32 + dy_line * t;
            let z = sz0 + (sz1 - sz0) * t;
            if z < 0.0 || z > 1.0 {
                continue;
            }
            let varying = <S::Varying as Varying>::weighted_sum(y0, 1.0 - t, y1, t, y0, 0.0);
            for off in -half_w..=half_w {
                let px = (bx + perp_x * off as f32 + 0.5) as i32;
                let py = (by + perp_y * off as f32 + 0.5) as i32;
                if px < 0 || px >= w || py < 0 || py >= h {
                    continue;
                }
                let idx = py as usize * tgt_w + px as usize;
                let _ = self.fragment_ops_raw(
                    px,
                    py,
                    idx,
                    z,
                    color_ptrs,
                    color_present,
                    depth_ptr,
                    stencil_ptr,
                    || shader.fragment_outputs(&varying),
                );
            }
        }
    }

    /// Per-pixel depth / stencil test, fragment shader invocation, and
    /// depth + color write.  Shared by the triangle, point, and line rasterizers.
    ///
    /// `get_frag` is invoked lazily only when both tests pass.
    /// Returns `true` if the pixel was written to at least one attachment.
    #[allow(clippy::too_many_arguments)]
    fn fragment_ops_raw<F: Fn() -> Option<FragmentOutputs>>(
        &self,
        px: i32,
        py: i32,
        idx: usize,
        z: f32,
        color_ptrs: [*mut u32; 8],
        color_present: [bool; 8],
        depth_ptr: *mut f32,
        stencil_ptr: Option<*mut u8>,
        get_frag: F,
    ) -> bool {
        // ── Scissor test ──────────────────────────────────────────────────────────
        if self.scissor_test {
            let [sx, sy, sw, sh] = self.scissor;
            if px < sx || px >= sx + sw || py < sy || py >= sy + sh {
                return false;
            }
        }
        // ── Stencil test ──────────────────────────────────────────────────────────
        let stencil_val = stencil_ptr.map(|sp| unsafe { *sp.add(idx) }).unwrap_or(0);
        if self.stencil_test {
            if !self
                .stencil_func
                .test(stencil_val, self.stencil_ref, self.stencil_mask_r)
            {
                if let Some(sp) = stencil_ptr {
                    let new_s = apply_stencil_op_fn(
                        self.stencil_fail,
                        stencil_val,
                        self.stencil_ref,
                        self.stencil_mask_w,
                    );
                    unsafe {
                        *sp.add(idx) = new_s;
                    }
                }
                return false;
            }
        }

        // ── Depth test ────────────────────────────────────────────────────────────
        let buf_z = unsafe { *depth_ptr.add(idx) };
        let depth_pass = !self.depth_test || self.depth_func.test(z, buf_z);

        // Stencil depth-pass / depth-fail write.
        if self.stencil_test {
            if let Some(sp) = stencil_ptr {
                let op = if depth_pass {
                    self.stencil_zpass
                } else {
                    self.stencil_zfail
                };
                let new_s =
                    apply_stencil_op_fn(op, stencil_val, self.stencil_ref, self.stencil_mask_w);
                unsafe {
                    *sp.add(idx) = new_s;
                }
            }
        }

        if !depth_pass {
            return false;
        }

        // ── Fragment shader ───────────────────────────────────────────────────────
        let Some(frag) = get_frag() else {
            return false;
        };

        // ── Depth write ───────────────────────────────────────────────────────────
        if self.depth_write {
            let write_z = frag.depth.unwrap_or(z);
            unsafe {
                *depth_ptr.add(idx) = write_z;
            }
        }

        // ── Color blend and write (per-attachment) ────────────────────────────────
        let constant = Vec4::new(
            self.blend_color[0],
            self.blend_color[1],
            self.blend_color[2],
            self.blend_color[3],
        );
        for attachment in 0..8usize {
            if !color_present[attachment] {
                continue;
            }
            if self.draw_buffers_mask & (1u8 << attachment) == 0 {
                continue;
            }
            let color_ptr = color_ptrs[attachment];
            if color_ptr.is_null() {
                continue;
            }
            let mask = if attachment == 0 {
                self.color_mask
            } else {
                self.color_masks[attachment]
            };
            if !mask[0] && !mask[1] && !mask[2] && !mask[3] {
                continue;
            }
            let src = match frag.colors[attachment] {
                Some(c) => c,
                None => continue,
            };
            let dst = unpack_bgra(unsafe { *color_ptr.add(idx) });
            let use_blend = self.blend || self.blend_attachments[attachment];
            let out = if use_blend {
                let (eq_rgb, eq_alpha, src_rgb_f, dst_rgb_f, src_alpha_f, dst_alpha_f) =
                    if self.blend_attachments[attachment] {
                        (
                            self.blend_eq_rgb_attachments[attachment],
                            self.blend_eq_alpha_attachments[attachment],
                            self.blend_src_rgb_attachments[attachment],
                            self.blend_dst_rgb_attachments[attachment],
                            self.blend_src_alpha_attachments[attachment],
                            self.blend_dst_alpha_attachments[attachment],
                        )
                    } else {
                        (
                            self.blend_eq_rgb,
                            self.blend_eq_alpha,
                            self.blend_src_rgb,
                            self.blend_dst_rgb,
                            self.blend_src_alpha,
                            self.blend_dst_alpha,
                        )
                    };
                let sf_rgb = blend_factor_fn(src_rgb_f, src, dst, constant);
                let df_rgb = blend_factor_fn(dst_rgb_f, src, dst, constant);
                let sf_a = blend_factor_alpha_fn(src_alpha_f, src, dst, constant);
                let df_a = blend_factor_alpha_fn(dst_alpha_f, src, dst, constant);
                let br = blend_eq_fn(eq_rgb, src.x * sf_rgb.x, dst.x * df_rgb.x);
                let bg = blend_eq_fn(eq_rgb, src.y * sf_rgb.y, dst.y * df_rgb.y);
                let bb = blend_eq_fn(eq_rgb, src.z * sf_rgb.z, dst.z * df_rgb.z);
                let ba = blend_eq_fn(eq_alpha, src.w * sf_a.w, dst.w * df_a.w);
                Vec4::new(
                    br.clamp(0.0, 1.0),
                    bg.clamp(0.0, 1.0),
                    bb.clamp(0.0, 1.0),
                    ba.clamp(0.0, 1.0),
                )
            } else {
                src
            };
            // Per-channel write mask.
            let existing = unpack_bgra(unsafe { *color_ptr.add(idx) });
            let final_color = Vec4::new(
                if mask[0] { out.x } else { existing.x },
                if mask[1] { out.y } else { existing.y },
                if mask[2] { out.z } else { existing.z },
                if mask[3] { out.w } else { existing.w },
            );
            unsafe {
                *color_ptr.add(idx) = pack_bgra(final_color);
            }
        }
        true
    }
}
#[inline]
fn edge(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

/// Full 6-plane Sutherland–Hodgman clip in homogeneous clip space (ES 3.0 §2.13).
///
/// Clips the polygon in `inp[..n_in]` against all six frustum planes:
///   -w ≤ x ≤ w,  -w ≤ y ≤ w,  -w ≤ z ≤ w
///
/// Uses two alternating scratch buffers (`buf_a`, `buf_b`) to avoid allocation.
/// Returns the number of vertices in `out` (which is `buf_a` after the last pass).
fn clip_all_planes<Y: Copy + LerpY>(
    inp: &[(Vec4, Y)],
    out: &mut [(Vec4, Y); 9],
    scratch: &mut [(Vec4, Y); 9],
) -> usize {
    // The six clip planes in the form (sign, component): keep if sign*comp >= -w  → keep if sign*comp + w >= 0
    // Plane 0: x >= -w  →  x + w >= 0
    // Plane 1: x <=  w  → -x + w >= 0
    // Plane 2: y >= -w  →  y + w >= 0
    // Plane 3: y <=  w  → -y + w >= 0
    // Plane 4: z >= -w  →  z + w >= 0
    // Plane 5: z <=  w  → -z + w >= 0
    #[inline]
    fn dist(p: Vec4, plane: usize) -> f32 {
        match plane {
            0 => p.x + p.w,  // x >= -w
            1 => -p.x + p.w, // x <=  w
            2 => p.y + p.w,  // y >= -w
            3 => -p.y + p.w, // y <=  w
            4 => p.z + p.w,  // z >= -w
            _ => -p.z + p.w, // z <=  w
        }
    }

    // Copy input into out buffer to start the ping-pong.
    let mut src_n = inp.len().min(9);
    for i in 0..src_n {
        scratch[i] = inp[i];
    }

    let mut cur = &mut *scratch as *mut [(Vec4, Y); 9];
    let mut next = out as *mut [(Vec4, Y); 9];

    for plane in 0..6usize {
        let c = unsafe { &*cur };
        let n = unsafe { &mut *next };
        let mut out_n = 0usize;
        for i in 0..src_n {
            let cur_v = c[i];
            let nxt_v = c[(i + 1) % src_n];
            let d_cur = dist(cur_v.0, plane);
            let d_nxt = dist(nxt_v.0, plane);
            let cur_in = d_cur >= 0.0;
            let nxt_in = d_nxt >= 0.0;
            if cur_in {
                if out_n < 9 {
                    n[out_n] = cur_v;
                    out_n += 1;
                }
            }
            if cur_in != nxt_in {
                // t such that d_cur + t*(d_nxt - d_cur) = 0
                let t = d_cur / (d_cur - d_nxt);
                let p = Vec4::new(
                    cur_v.0.x + (nxt_v.0.x - cur_v.0.x) * t,
                    cur_v.0.y + (nxt_v.0.y - cur_v.0.y) * t,
                    cur_v.0.z + (nxt_v.0.z - cur_v.0.z) * t,
                    cur_v.0.w + (nxt_v.0.w - cur_v.0.w) * t,
                );
                let y = Y::lerp_along(cur_v.1, nxt_v.1, t);
                if out_n < 9 {
                    n[out_n] = (p, y);
                    out_n += 1;
                }
            }
        }
        src_n = out_n;
        // Swap src and dst pointers for next plane.
        core::mem::swap(&mut cur, &mut next);
    }

    // After 6 planes (even number of swaps from the initial assignment),
    // the result is in `cur` which started as `scratch`. Copy to `out`.
    let result = unsafe { &*cur };
    for i in 0..src_n {
        out[i] = result[i];
    }
    src_n
}

/// Legacy near-plane-only clip — kept for reference / line rasterizer.
/// Writes clipped polygon into `out` and returns its vertex count (3 or 4).
fn clip_near<Y: Copy + LerpY>(inp: &[(Vec4, Y); 3], out: &mut [(Vec4, Y); 4]) -> usize {
    let mut n = 0usize;
    for i in 0..3 {
        let cur = inp[i];
        let nxt = inp[(i + 1) % 3];
        let cur_in = cur.0.z >= -cur.0.w;
        let nxt_in = nxt.0.z >= -nxt.0.w;
        if cur_in {
            out[n] = cur;
            n += 1;
        }
        if cur_in != nxt_in {
            let t = (cur.0.z + cur.0.w) / ((cur.0.z + cur.0.w) - (nxt.0.z + nxt.0.w));
            let p = cur.0 + (nxt.0 - cur.0) * t;
            let y = Y::lerp_along(cur.1, nxt.1, t);
            if n < 4 {
                out[n] = (p, y);
                n += 1;
            }
        }
    }
    n
}

/// Tiny helper trait for the clipper (Varying does not natively expose lerp).
pub trait LerpY: Copy {
    fn lerp_along(a: Self, b: Self, t: f32) -> Self;
}

impl<Y: Varying> LerpY for Y {
    fn lerp_along(a: Y, b: Y, t: f32) -> Y {
        Y::weighted_sum(a, 1.0 - t, b, t, a, 0.0)
    }
}

#[inline]
fn pack_bgra(c: Vec4) -> u32 {
    let r = (c.x.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let g = (c.y.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let b = (c.z.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let a = (c.w.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (a << 24) | (r << 16) | (g << 8) | b
}

#[inline]
fn unpack_bgra(p: u32) -> Vec4 {
    Vec4::new(
        ((p >> 16) & 0xFF) as f32 / 255.0, // R → x
        ((p >> 8) & 0xFF) as f32 / 255.0,  // G → y
        (p & 0xFF) as f32 / 255.0,         // B → z
        ((p >> 24) & 0xFF) as f32 / 255.0, // A → w
    )
}

// ── Stencil op ────────────────────────────────────────────────────────────────

fn apply_stencil_op_fn(op: StencilOp, s: u8, ref_val: u8, mask: u8) -> u8 {
    let new = match op {
        StencilOp::Keep => s,
        StencilOp::Zero => 0,
        StencilOp::Replace => ref_val,
        StencilOp::Increment => s.saturating_add(1),
        StencilOp::IncrementWrap => s.wrapping_add(1),
        StencilOp::Decrement => s.saturating_sub(1),
        StencilOp::DecrementWrap => s.wrapping_sub(1),
        StencilOp::Invert => !s,
    };
    (s & !mask) | (new & mask)
}

// ── Blend factor helpers ──────────────────────────────────────────────────────

fn blend_factor_fn(f: BlendFactor, src: Vec4, dst: Vec4, cc: Vec4) -> Vec4 {
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
        BlendFactor::SrcAlpha => {
            let a = src.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::OneMinusSrcAlpha => {
            let a = 1.0 - src.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::DstAlpha => {
            let a = dst.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::OneMinusDstAlpha => {
            let a = 1.0 - dst.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::ConstantColor => cc,
        BlendFactor::OneMinusConstantColor => {
            Vec4::new(1.0 - cc.x, 1.0 - cc.y, 1.0 - cc.z, 1.0 - cc.w)
        }
        BlendFactor::ConstantAlpha => {
            let a = cc.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::OneMinusConstantAlpha => {
            let a = 1.0 - cc.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::SrcAlphaSaturate => {
            let f = (src.w).min(1.0 - dst.w);
            Vec4::new(f, f, f, 1.0)
        }
    }
}

fn blend_factor_alpha_fn(f: BlendFactor, src: Vec4, dst: Vec4, cc: Vec4) -> Vec4 {
    // For alpha channel: SrcColor → SrcAlpha, etc.
    match f {
        BlendFactor::SrcColor | BlendFactor::SrcAlpha => {
            let a = src.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::OneMinusSrcColor | BlendFactor::OneMinusSrcAlpha => {
            let a = 1.0 - src.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::DstColor | BlendFactor::DstAlpha => {
            let a = dst.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::OneMinusDstColor | BlendFactor::OneMinusDstAlpha => {
            let a = 1.0 - dst.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::ConstantColor | BlendFactor::ConstantAlpha => {
            let a = cc.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::OneMinusConstantColor | BlendFactor::OneMinusConstantAlpha => {
            let a = 1.0 - cc.w;
            Vec4::new(a, a, a, a)
        }
        BlendFactor::SrcAlphaSaturate => Vec4::new(1.0, 1.0, 1.0, 1.0),
        _ => blend_factor_fn(f, src, dst, cc),
    }
}

fn blend_eq_fn(eq: BlendEquation, a: f32, b: f32) -> f32 {
    match eq {
        BlendEquation::FuncAdd => a + b,
        BlendEquation::FuncSubtract => a - b,
        BlendEquation::FuncReverseSubtract => b - a,
        BlendEquation::Min => a.min(b),
        BlendEquation::Max => a.max(b),
    }
}
