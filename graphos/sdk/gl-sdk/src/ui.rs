// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! 2D UI batching helpers built on top of the programmable pipeline.
//!
//! This module does not bypass the renderer. It generates vertex/index streams
//! for regular draw calls so windows/widgets/graphs can be rendered with the
//! same fragment pipeline semantics (scissor, depth/stencil, blend, color-mask).

use crate::gl::CullFace;
use crate::math::{Vec2, Vec3, Vec4};
use crate::pipeline::{Pipeline, Target};
use crate::shader::{Shader, Varying};
use crate::texture::Texture;
use alloc::vec::Vec;
use core::f32::consts::{FRAC_PI_2, PI};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(self) -> f32 {
        self.x + self.w
    }

    pub const fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub const fn center_x(self) -> f32 {
        self.x + self.w * 0.5
    }

    pub const fn center_y(self) -> f32 {
        self.y + self.h * 0.5
    }

    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            None
        } else {
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        }
    }

    pub fn inset(self, left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect::new(
            self.x + left,
            self.y + top,
            (self.w - left - right).max(0.0),
            (self.h - top - bottom).max(0.0),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EdgeInsets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl EdgeInsets {
    pub const fn all(v: f32) -> Self {
        Self {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NineSlice {
    pub texture: u32,
    pub source_size: Vec2,
    pub uv_min: Vec2,
    pub uv_max: Vec2,
    pub borders: EdgeInsets,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UiVertex {
    pub pos: Vec3,
    pub uv: Vec2,
    pub color: Vec4,
    pub texture: u32,
}

impl UiVertex {
    pub const fn new(pos: Vec3, uv: Vec2, color: Vec4, texture: u32) -> Self {
        Self {
            pos,
            uv,
            color,
            texture,
        }
    }
}

/// CPU-side batch for desktop/UI primitives.
pub struct UiBatch {
    pub vertices: Vec<UiVertex>,
    pub indices: Vec<u32>,
}

impl UiBatch {
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    pub fn with_capacity(vertex_cap: usize, index_cap: usize) -> Self {
        Self {
            vertices: Vec::with_capacity(vertex_cap),
            indices: Vec::with_capacity(index_cap),
        }
    }

    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
    }

    pub fn append(&mut self, other: &UiBatch) {
        if other.vertices.is_empty() || other.indices.is_empty() {
            return;
        }
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.reserve(other.indices.len());
        for &idx in &other.indices {
            self.indices.push(base + idx);
        }
    }

    pub fn push_quad(&mut self, v0: UiVertex, v1: UiVertex, v2: UiVertex, v3: UiVertex) {
        let base = self.vertices.len() as u32;
        self.vertices.push(v0);
        self.vertices.push(v1);
        self.vertices.push(v2);
        self.vertices.push(v3);
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn push_triangle(&mut self, v0: UiVertex, v1: UiVertex, v2: UiVertex) {
        let base = self.vertices.len() as u32;
        self.vertices.push(v0);
        self.vertices.push(v1);
        self.vertices.push(v2);
        self.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }

    pub fn add_rect(&mut self, rect: Rect, z: f32, color: Vec4) {
        self.add_textured_rect(rect, z, color, 0, Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0));
    }

    pub fn add_gradient_rect(
        &mut self,
        rect: Rect,
        z: f32,
        tl: Vec4,
        tr: Vec4,
        br: Vec4,
        bl: Vec4,
    ) {
        let v0 = UiVertex::new(Vec3::new(rect.x, rect.y, z), Vec2::new(0.0, 0.0), tl, 0);
        let v1 = UiVertex::new(
            Vec3::new(rect.right(), rect.y, z),
            Vec2::new(1.0, 0.0),
            tr,
            0,
        );
        let v2 = UiVertex::new(
            Vec3::new(rect.right(), rect.bottom(), z),
            Vec2::new(1.0, 1.0),
            br,
            0,
        );
        let v3 = UiVertex::new(
            Vec3::new(rect.x, rect.bottom(), z),
            Vec2::new(0.0, 1.0),
            bl,
            0,
        );
        self.push_quad(v0, v1, v2, v3);
    }

    pub fn add_textured_rect(
        &mut self,
        rect: Rect,
        z: f32,
        color: Vec4,
        texture: u32,
        uv_min: Vec2,
        uv_max: Vec2,
    ) {
        let v0 = UiVertex::new(
            Vec3::new(rect.x, rect.y, z),
            Vec2::new(uv_min.x, uv_min.y),
            color,
            texture,
        );
        let v1 = UiVertex::new(
            Vec3::new(rect.right(), rect.y, z),
            Vec2::new(uv_max.x, uv_min.y),
            color,
            texture,
        );
        let v2 = UiVertex::new(
            Vec3::new(rect.right(), rect.bottom(), z),
            Vec2::new(uv_max.x, uv_max.y),
            color,
            texture,
        );
        let v3 = UiVertex::new(
            Vec3::new(rect.x, rect.bottom(), z),
            Vec2::new(uv_min.x, uv_max.y),
            color,
            texture,
        );
        self.push_quad(v0, v1, v2, v3);
    }

    pub fn add_textured_rect_clipped(
        &mut self,
        rect: Rect,
        z: f32,
        color: Vec4,
        texture: u32,
        uv_min: Vec2,
        uv_max: Vec2,
        clip: Option<Rect>,
    ) {
        let clipped = if let Some(clip_rect) = clip {
            match rect.intersect(clip_rect) {
                Some(r) => r,
                None => return,
            }
        } else {
            rect
        };

        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }

        let u_span = uv_max.x - uv_min.x;
        let v_span = uv_max.y - uv_min.y;
        let x0 = (clipped.x - rect.x) / rect.w;
        let y0 = (clipped.y - rect.y) / rect.h;
        let x1 = (clipped.right() - rect.x) / rect.w;
        let y1 = (clipped.bottom() - rect.y) / rect.h;
        let clipped_uv_min = Vec2::new(uv_min.x + u_span * x0, uv_min.y + v_span * y0);
        let clipped_uv_max = Vec2::new(uv_min.x + u_span * x1, uv_min.y + v_span * y1);
        self.add_textured_rect(clipped, z, color, texture, clipped_uv_min, clipped_uv_max);
    }

    pub fn add_border(&mut self, rect: Rect, z: f32, thickness: f32, color: Vec4) {
        if thickness <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let t = thickness.min(rect.w * 0.5).min(rect.h * 0.5);
        self.add_rect(Rect::new(rect.x, rect.y, rect.w, t), z, color);
        self.add_rect(Rect::new(rect.x, rect.bottom() - t, rect.w, t), z, color);
        self.add_rect(Rect::new(rect.x, rect.y + t, t, rect.h - 2.0 * t), z, color);
        self.add_rect(
            Rect::new(rect.right() - t, rect.y + t, t, rect.h - 2.0 * t),
            z,
            color,
        );
    }

    pub fn add_line(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        z: f32,
        thickness: f32,
        color: Vec4,
    ) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = libm::sqrtf(dx * dx + dy * dy);
        if len <= 1e-5 || thickness <= 0.0 {
            return;
        }
        let nx = -dy / len;
        let ny = dx / len;
        let hx = nx * (thickness * 0.5);
        let hy = ny * (thickness * 0.5);

        let v0 = UiVertex::new(
            Vec3::new(x0 - hx, y0 - hy, z),
            Vec2::new(0.0, 0.0),
            color,
            0,
        );
        let v1 = UiVertex::new(
            Vec3::new(x1 - hx, y1 - hy, z),
            Vec2::new(1.0, 0.0),
            color,
            0,
        );
        let v2 = UiVertex::new(
            Vec3::new(x1 + hx, y1 + hy, z),
            Vec2::new(1.0, 1.0),
            color,
            0,
        );
        let v3 = UiVertex::new(
            Vec3::new(x0 + hx, y0 + hy, z),
            Vec2::new(0.0, 1.0),
            color,
            0,
        );
        self.push_quad(v0, v1, v2, v3);
    }

    pub fn add_window_chrome(
        &mut self,
        rect: Rect,
        z: f32,
        bg: Vec4,
        title_bar: Vec4,
        border: Vec4,
        title_h: f32,
        border_w: f32,
    ) {
        self.add_rect(rect, z, bg);
        self.add_rect(
            Rect::new(rect.x, rect.y, rect.w, title_h.max(0.0).min(rect.h)),
            z + 0.0001,
            title_bar,
        );
        self.add_border(rect, z + 0.0002, border_w, border);
    }

    pub fn add_graph_node(&mut self, cx: f32, cy: f32, r: f32, z: f32, color: Vec4) {
        self.add_circle(cx, cy, r, z, color, 18);
    }

    pub fn add_graph_edge(
        &mut self,
        from: Vec2,
        to: Vec2,
        z: f32,
        thickness: f32,
        color: Vec4,
        glow: Option<Vec4>,
    ) {
        if let Some(glow_color) = glow {
            self.add_line(from.x, from.y, to.x, to.y, z, thickness * 2.8, glow_color);
        }
        self.add_line(from.x, from.y, to.x, to.y, z + 0.0001, thickness, color);
    }

    pub fn add_circle(&mut self, cx: f32, cy: f32, r: f32, z: f32, color: Vec4, segments: usize) {
        if r <= 0.0 {
            return;
        }
        let segs = segments.max(8);
        let base = self.vertices.len() as u32;
        self.vertices.push(UiVertex::new(
            Vec3::new(cx, cy, z),
            Vec2::new(0.5, 0.5),
            color,
            0,
        ));
        for i in 0..=segs {
            let t = i as f32 / segs as f32;
            let a = t * PI * 2.0;
            let x = cx + libm::cosf(a) * r;
            let y = cy + libm::sinf(a) * r;
            self.vertices.push(UiVertex::new(
                Vec3::new(x, y, z),
                Vec2::new(0.5 + libm::cosf(a) * 0.5, 0.5 + libm::sinf(a) * 0.5),
                color,
                0,
            ));
        }
        for i in 0..segs as u32 {
            self.indices
                .extend_from_slice(&[base, base + i + 1, base + i + 2]);
        }
    }

    pub fn add_rounded_rect(
        &mut self,
        rect: Rect,
        z: f32,
        color: Vec4,
        radius: f32,
        segments: usize,
    ) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let radius = radius.min(rect.w * 0.5).min(rect.h * 0.5);
        if radius <= 0.5 {
            self.add_rect(rect, z, color);
            return;
        }

        let mut points = Vec::new();
        rounded_rect_points(rect, radius, segments.max(3), &mut points);
        let base = self.vertices.len() as u32;
        self.vertices.push(UiVertex::new(
            Vec3::new(rect.center_x(), rect.center_y(), z),
            Vec2::new(0.5, 0.5),
            color,
            0,
        ));
        for p in &points {
            self.vertices.push(UiVertex::new(
                Vec3::new(p.x, p.y, z),
                Vec2::new(0.0, 0.0),
                color,
                0,
            ));
        }
        for i in 0..points.len() as u32 {
            let next = if i + 1 == points.len() as u32 {
                0
            } else {
                i + 1
            };
            self.indices
                .extend_from_slice(&[base, base + i + 1, base + next + 1]);
        }
    }

    pub fn add_rounded_border(
        &mut self,
        rect: Rect,
        z: f32,
        color: Vec4,
        radius: f32,
        thickness: f32,
        segments: usize,
    ) {
        if thickness <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let outer_radius = radius.min(rect.w * 0.5).min(rect.h * 0.5);
        if outer_radius <= 0.5 {
            self.add_border(rect, z, thickness, color);
            return;
        }

        let inner = rect.inset(thickness, thickness, thickness, thickness);
        if inner.w <= 0.0 || inner.h <= 0.0 {
            self.add_rounded_rect(rect, z, color, outer_radius, segments);
            return;
        }

        let inner_radius = (outer_radius - thickness).max(0.0);
        let mut outer = Vec::new();
        let mut inner_points = Vec::new();
        let segs = segments.max(3);
        rounded_rect_points(rect, outer_radius, segs, &mut outer);
        rounded_rect_points(inner, inner_radius, segs, &mut inner_points);
        let base = self.vertices.len() as u32;
        for p in &outer {
            self.vertices.push(UiVertex::new(
                Vec3::new(p.x, p.y, z),
                Vec2::new(0.0, 0.0),
                color,
                0,
            ));
        }
        for p in &inner_points {
            self.vertices.push(UiVertex::new(
                Vec3::new(p.x, p.y, z),
                Vec2::new(0.0, 0.0),
                color,
                0,
            ));
        }
        let n = outer.len() as u32;
        for i in 0..n {
            let next = if i + 1 == n { 0 } else { i + 1 };
            let o0 = base + i;
            let o1 = base + next;
            let i0 = base + n + i;
            let i1 = base + n + next;
            self.indices.extend_from_slice(&[o0, o1, i1, o0, i1, i0]);
        }
    }

    pub fn add_shadow_rect(&mut self, rect: Rect, z: f32, blur_radius: f32, color: Vec4) {
        if blur_radius <= 0.0 || color.w <= 0.0 {
            return;
        }
        let layers = libm::ceilf(blur_radius) as usize;
        for i in 0..layers {
            let t = i as f32 / layers as f32;
            let expand = (i + 1) as f32;
            let alpha = color.w * (1.0 - t) * 0.35;
            self.add_rounded_border(
                Rect::new(
                    rect.x - expand,
                    rect.y - expand,
                    rect.w + expand * 2.0,
                    rect.h + expand * 2.0,
                ),
                z + i as f32 * 0.00001,
                Vec4::new(color.x, color.y, color.z, alpha),
                10.0 + expand,
                1.0,
                6,
            );
        }
    }

    pub fn add_nine_slice(&mut self, rect: Rect, z: f32, color: Vec4, slice: NineSlice) {
        if rect.w <= 0.0
            || rect.h <= 0.0
            || slice.source_size.x <= 0.0
            || slice.source_size.y <= 0.0
        {
            return;
        }

        let left = slice.borders.left.min(rect.w * 0.5);
        let right = slice.borders.right.min(rect.w * 0.5);
        let top = slice.borders.top.min(rect.h * 0.5);
        let bottom = slice.borders.bottom.min(rect.h * 0.5);

        let xs = [rect.x, rect.x + left, rect.right() - right, rect.right()];
        let ys = [rect.y, rect.y + top, rect.bottom() - bottom, rect.bottom()];

        let u0 = slice.uv_min.x;
        let v0 = slice.uv_min.y;
        let u1 = slice.uv_max.x;
        let v1 = slice.uv_max.y;
        let du = u1 - u0;
        let dv = v1 - v0;
        let us = [
            u0,
            u0 + du * (slice.borders.left / slice.source_size.x),
            u1 - du * (slice.borders.right / slice.source_size.x),
            u1,
        ];
        let vs = [
            v0,
            v0 + dv * (slice.borders.top / slice.source_size.y),
            v1 - dv * (slice.borders.bottom / slice.source_size.y),
            v1,
        ];

        for yi in 0..3 {
            for xi in 0..3 {
                let part = Rect::new(xs[xi], ys[yi], xs[xi + 1] - xs[xi], ys[yi + 1] - ys[yi]);
                if part.w <= 0.0 || part.h <= 0.0 {
                    continue;
                }
                self.add_textured_rect(
                    part,
                    z,
                    color,
                    slice.texture,
                    Vec2::new(us[xi], vs[yi]),
                    Vec2::new(us[xi + 1], vs[yi + 1]),
                );
            }
        }
    }

    pub fn add_terminal_panel(&mut self, rect: Rect, z: f32, bg: Vec4, border: Vec4, accent: Vec4) {
        self.add_rounded_rect(rect, z, bg, 12.0, 8);
        self.add_rounded_border(rect, z + 0.0001, border, 12.0, 1.0, 8);
        self.add_rect(
            Rect::new(rect.x, rect.y, rect.w, 24.0_f32.min(rect.h)),
            z + 0.0002,
            accent,
        );
        self.add_circle(
            rect.x + 14.0,
            rect.y + 12.0,
            4.0,
            z + 0.0003,
            Vec4::new(0.99, 0.38, 0.40, 0.95),
            12,
        );
        self.add_circle(
            rect.x + 28.0,
            rect.y + 12.0,
            4.0,
            z + 0.0003,
            Vec4::new(0.98, 0.78, 0.22, 0.95),
            12,
        );
        self.add_circle(
            rect.x + 42.0,
            rect.y + 12.0,
            4.0,
            z + 0.0003,
            Vec4::new(0.35, 0.90, 0.56, 0.95),
            12,
        );
    }

    pub fn add_package_card(
        &mut self,
        rect: Rect,
        z: f32,
        bg: Vec4,
        border: Vec4,
        accent: Vec4,
        thumbnail_texture: Option<u32>,
    ) {
        self.add_rounded_rect(rect, z, bg, 14.0, 8);
        self.add_rounded_border(rect, z + 0.0001, border, 14.0, 1.0, 8);
        self.add_rect(Rect::new(rect.x, rect.y, 4.0, rect.h), z + 0.0002, accent);
        let thumb = rect.inset(16.0, 16.0, 16.0, rect.h * 0.42);
        if let Some(texture) = thumbnail_texture {
            self.add_textured_rect(
                thumb,
                z + 0.0002,
                Vec4::new(1.0, 1.0, 1.0, 0.95),
                texture,
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 1.0),
            );
        } else {
            self.add_gradient_rect(
                thumb,
                z + 0.0002,
                Vec4::new(0.14, 0.18, 0.28, 0.95),
                Vec4::new(0.18, 0.26, 0.42, 0.95),
                Vec4::new(0.08, 0.11, 0.18, 0.95),
                Vec4::new(0.06, 0.09, 0.16, 0.95),
            );
        }
    }
}

impl Default for UiBatch {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
pub struct UiTextureView<'a> {
    pub texture: Texture<'a>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UiVarying {
    pub uv: Vec2,
    pub color: Vec4,
    pub texture: f32,
}

impl Varying for UiVarying {
    fn weighted_sum(a: Self, wa: f32, b: Self, wb: f32, c: Self, wc: f32) -> Self {
        Self {
            uv: Vec2::new(
                a.uv.x * wa + b.uv.x * wb + c.uv.x * wc,
                a.uv.y * wa + b.uv.y * wb + c.uv.y * wc,
            ),
            color: Vec4::new(
                a.color.x * wa + b.color.x * wb + c.color.x * wc,
                a.color.y * wa + b.color.y * wb + c.color.y * wc,
                a.color.z * wa + b.color.z * wb + c.color.z * wc,
                a.color.w * wa + b.color.w * wb + c.color.w * wc,
            ),
            texture: a.texture * wa + b.texture * wb + c.texture * wc,
        }
    }

    fn scale(self, s: f32) -> Self {
        Self {
            uv: Vec2::new(self.uv.x * s, self.uv.y * s),
            color: self.color * s,
            texture: self.texture * s,
        }
    }
}

pub struct UiShader<'a> {
    screen_w: f32,
    screen_h: f32,
    textures: &'a [Option<Texture<'a>>],
}

impl<'a> UiShader<'a> {
    pub fn new(screen_w: f32, screen_h: f32, textures: &'a [Option<Texture<'a>>]) -> Self {
        Self {
            screen_w: screen_w.max(1.0),
            screen_h: screen_h.max(1.0),
            textures,
        }
    }

    fn sample_texture(&self, texture_name: u32, uv: Vec2) -> Vec4 {
        if texture_name == 0 {
            return Vec4::new(1.0, 1.0, 1.0, 1.0);
        }
        self.textures
            .get(texture_name as usize - 1)
            .and_then(|slot| slot.as_ref())
            .map(|tex| tex.sample(uv))
            .unwrap_or(Vec4::new(1.0, 0.0, 1.0, 1.0))
    }
}

impl Shader for UiShader<'_> {
    type Vertex = UiVertex;
    type Varying = UiVarying;

    fn vertex(&self, v: &Self::Vertex) -> (Vec4, Self::Varying) {
        let ndc_x = (v.pos.x / self.screen_w) * 2.0 - 1.0;
        let ndc_y = 1.0 - (v.pos.y / self.screen_h) * 2.0;
        let ndc_z = v.pos.z.clamp(0.0, 1.0) * 2.0 - 1.0;
        (
            Vec4::new(ndc_x, ndc_y, ndc_z, 1.0),
            UiVarying {
                uv: v.uv,
                color: v.color,
                texture: v.texture as f32,
            },
        )
    }

    fn fragment(&self, v: &Self::Varying) -> Option<Vec4> {
        let texture_name = if v.texture <= 0.5 {
            0
        } else {
            libm::roundf(v.texture) as u32
        };
        let sample = self.sample_texture(texture_name, v.uv);
        Some(Vec4::new(
            sample.x * v.color.x,
            sample.y * v.color.y,
            sample.z * v.color.z,
            sample.w * v.color.w,
        ))
    }
}

pub struct UiRenderer<'a> {
    pub pipeline: Pipeline,
    pub shader: UiShader<'a>,
}

impl<'a> UiRenderer<'a> {
    pub fn new(screen_w: f32, screen_h: f32, textures: &'a [Option<Texture<'a>>]) -> Self {
        let mut pipeline = Pipeline::transparent_3d();
        pipeline.cull_face = CullFace::None;
        pipeline.depth_test = false;
        pipeline.depth_write = false;
        Self {
            pipeline,
            shader: UiShader::new(screen_w, screen_h, textures),
        }
    }

    pub fn render(&self, target: &mut Target<'_>, batch: &UiBatch) {
        self.pipeline
            .draw(target, &self.shader, &batch.vertices, &batch.indices);
    }
}

fn rounded_rect_points(rect: Rect, radius: f32, segments: usize, out: &mut Vec<Vec2>) {
    out.clear();
    let radius = radius.max(0.0);
    if radius <= 0.0 {
        out.push(Vec2::new(rect.x, rect.y));
        out.push(Vec2::new(rect.right(), rect.y));
        out.push(Vec2::new(rect.right(), rect.bottom()));
        out.push(Vec2::new(rect.x, rect.bottom()));
        return;
    }

    append_arc(
        out,
        Vec2::new(rect.right() - radius, rect.y + radius),
        radius,
        -FRAC_PI_2,
        0.0,
        segments,
    );
    append_arc(
        out,
        Vec2::new(rect.right() - radius, rect.bottom() - radius),
        radius,
        0.0,
        FRAC_PI_2,
        segments,
    );
    append_arc(
        out,
        Vec2::new(rect.x + radius, rect.bottom() - radius),
        radius,
        FRAC_PI_2,
        PI,
        segments,
    );
    append_arc(
        out,
        Vec2::new(rect.x + radius, rect.y + radius),
        radius,
        PI,
        PI + FRAC_PI_2,
        segments,
    );
}

fn append_arc(
    points: &mut Vec<Vec2>,
    center: Vec2,
    radius: f32,
    start: f32,
    end: f32,
    segments: usize,
) {
    for i in 0..=segments {
        if !points.is_empty() && i == 0 {
            continue;
        }
        let t = i as f32 / segments as f32;
        let angle = start + (end - start) * t;
        points.push(Vec2::new(
            center.x + libm::cosf(angle) * radius,
            center.y + libm::sinf(angle) * radius,
        ));
    }
}
