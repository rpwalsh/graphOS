// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Desktop compositor helpers for offscreen window surfaces.

use crate::gl::{Attachment, Context, FilterMode, FramebufferStatus, WrapMode};
use crate::math::{Vec2, Vec4};
use crate::texture::Texture;
use crate::ui::UiRenderer;
use crate::ui::{Rect, UiBatch};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug)]
pub struct WindowSurface {
    pub texture: u32,
    pub rect: Rect,
    pub z: i32,
    pub opacity: f32,
    pub clip: Option<Rect>,
}

pub struct DesktopCompositor {
    pub windows: Vec<WindowSurface>,
    pub background_top: Vec4,
    pub background_bottom: Vec4,
    pub shadow_color: Vec4,
    pub shadow_radius: f32,
    pub window_backdrop: Vec4,
}

impl DesktopCompositor {
    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            background_top: Vec4::new(0.05, 0.07, 0.12, 1.0),
            background_bottom: Vec4::new(0.01, 0.02, 0.05, 1.0),
            shadow_color: Vec4::new(0.0, 0.0, 0.0, 0.28),
            shadow_radius: 10.0,
            window_backdrop: Vec4::new(0.05, 0.08, 0.14, 0.16),
        }
    }

    pub fn clear(&mut self) {
        self.windows.clear();
    }

    pub fn add_window(&mut self, surface: WindowSurface) {
        self.windows.push(surface);
    }

    pub fn composite_into(&self, batch: &mut UiBatch, screen_w: f32, screen_h: f32, z_base: f32) {
        batch.add_gradient_rect(
            Rect::new(0.0, 0.0, screen_w, screen_h),
            z_base,
            self.background_top,
            self.background_top,
            self.background_bottom,
            self.background_bottom,
        );

        let mut ordered = self.windows.clone();
        ordered.sort_by_key(|w| w.z);

        for (idx, w) in ordered.iter().enumerate() {
            let z = z_base + 0.0005 + idx as f32 * 0.0001;
            let shadow = Rect::new(w.rect.x + 6.0, w.rect.y + 8.0, w.rect.w, w.rect.h);
            batch.add_shadow_rect(shadow, z, self.shadow_radius, self.shadow_color);
            batch.add_rounded_rect(w.rect, z + 0.000005, self.window_backdrop, 14.0, 8);

            let tint = Vec4::new(1.0, 1.0, 1.0, w.opacity.clamp(0.0, 1.0));
            batch.add_textured_rect_clipped(
                w.rect,
                z + 0.00001,
                tint,
                w.texture,
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 1.0),
                w.clip,
            );
        }
    }
}

impl Default for DesktopCompositor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WindowSurfaceTarget {
    pub fbo: u32,
    pub color_texture: u32,
    pub width: u32,
    pub height: u32,
}

impl WindowSurfaceTarget {
    pub fn create(ctx: &mut Context, width: u32, height: u32) -> Option<Self> {
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
        ctx.framebuffer_texture_2d(fbo, Attachment::Color(0), color_texture, 0);

        if ctx.check_framebuffer_status(fbo) != FramebufferStatus::Complete {
            ctx.delete_framebuffers(&[fbo]);
            ctx.delete_textures(&[color_texture]);
            return None;
        }

        Some(Self {
            fbo,
            color_texture,
            width,
            height,
        })
    }

    pub fn destroy(self, ctx: &mut Context) {
        ctx.delete_framebuffers(&[self.fbo]);
        ctx.delete_textures(&[self.color_texture]);
    }
}

pub struct OffscreenWindowRenderer {
    depth: Vec<f32>,
    stencil: Vec<u8>,
}

impl OffscreenWindowRenderer {
    pub fn new() -> Self {
        Self {
            depth: Vec::new(),
            stencil: Vec::new(),
        }
    }

    pub fn render_batch<'a>(
        &mut self,
        ctx: &mut Context,
        surface: &WindowSurfaceTarget,
        batch: &UiBatch,
        textures: &'a [Option<Texture<'a>>],
    ) -> bool {
        if ctx.check_framebuffer_status(surface.fbo) != FramebufferStatus::Complete {
            return false;
        }

        let px_count = (surface.width as usize).saturating_mul(surface.height as usize);
        if self.depth.len() < px_count {
            self.depth.resize(px_count, 1.0);
        }
        if self.stencil.len() < px_count {
            self.stencil.resize(px_count, 0);
        }

        let Some(color) = ctx.texture_pixels_mut(surface.color_texture, 0) else {
            return false;
        };
        let depth = &mut self.depth[..px_count];
        let stencil = &mut self.stencil[..px_count];

        let mut target = crate::pipeline::Target::with_stencil(
            color,
            depth,
            stencil,
            surface.width,
            surface.height,
        );
        target.clear_all(0x0000_0000, 1.0, 0);

        let renderer = UiRenderer::new(surface.width as f32, surface.height as f32, textures);
        renderer.render(&mut target, batch);
        true
    }
}

impl Default for OffscreenWindowRenderer {
    fn default() -> Self {
        Self::new()
    }
}
