// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Bitmap-font text rendering helpers for desktop UI.
//!
//! This module builds a compact atlas and emits glyph quads into [`crate::ui::UiBatch`].

use crate::math::{Vec2, Vec4};
use crate::ui::{Rect, UiBatch};
use alloc::vec;
use alloc::vec::Vec;
use font8x8::UnicodeFonts;

pub struct GlyphAtlas {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub cell_w: u32,
    pub cell_h: u32,
    pub cols: u32,
    pub rows: u32,
}

impl GlyphAtlas {
    pub fn ascii_8x8() -> Self {
        let cell_w = 8;
        let cell_h = 8;
        let cols = 16;
        let rows = 16;
        let width = cell_w * cols;
        let height = cell_h * rows;
        let mut pixels = vec![0u32; (width * height) as usize];

        for code in 0u32..256 {
            let ch = core::char::from_u32(code).unwrap_or(' ');
            let glyph = font8x8::BASIC_FONTS.get(ch).unwrap_or([0u8; 8]);
            let gx = (code % cols) * cell_w;
            let gy = (code / cols) * cell_h;

            for (row, bits) in glyph.iter().enumerate() {
                let y = gy + row as u32;
                for x in 0..8u32 {
                    let on = (bits >> x) & 1;
                    let dst = (y * width + gx + x) as usize;
                    pixels[dst] = if on != 0 { 0xFFFFFFFF } else { 0x00FFFFFF };
                }
            }
        }

        Self {
            pixels,
            width,
            height,
            cell_w,
            cell_h,
            cols,
            rows,
        }
    }

    pub fn glyph_uv(&self, ch: char) -> (Vec2, Vec2) {
        let code = (ch as u32).min(255);
        let gx = (code % self.cols) * self.cell_w;
        let gy = (code / self.cols) * self.cell_h;
        let u0 = gx as f32 / self.width as f32;
        let v0 = gy as f32 / self.height as f32;
        let u1 = (gx + self.cell_w) as f32 / self.width as f32;
        let v1 = (gy + self.cell_h) as f32 / self.height as f32;
        (Vec2::new(u0, v0), Vec2::new(u1, v1))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    pub color: Vec4,
    pub scale: f32,
    pub line_height: f32,
    pub letter_spacing: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            color: Vec4::new(0.92, 0.94, 0.98, 1.0),
            scale: 1.0,
            line_height: 10.0,
            letter_spacing: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f32,
    pub height: f32,
    pub line_count: usize,
}

pub fn measure_text(atlas: &GlyphAtlas, text: &str, style: TextStyle) -> TextMetrics {
    let glyph_w = atlas.cell_w as f32 * style.scale + style.letter_spacing;
    let line_h = style.line_height * style.scale;
    let mut current = 0.0f32;
    let mut max_width = 0.0f32;
    let mut lines = 1usize;

    for ch in text.chars() {
        if ch == '\n' {
            max_width = max_width.max(current.max(0.0f32));
            current = 0.0;
            lines += 1;
        } else {
            current += glyph_w;
        }
    }
    max_width = max_width.max(current.max(0.0f32));

    TextMetrics {
        width: max_width.max(0.0f32),
        height: line_h * lines as f32,
        line_count: lines,
    }
}

pub fn append_text(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    text: &str,
    x: f32,
    y: f32,
    z: f32,
    style: TextStyle,
    clip: Option<Rect>,
) {
    let mut pen_x = x;
    let mut pen_y = y;
    let gw = atlas.cell_w as f32 * style.scale;
    let gh = atlas.cell_h as f32 * style.scale;

    for ch in text.chars() {
        if ch == '\n' {
            pen_x = x;
            pen_y += style.line_height * style.scale;
            continue;
        }

        let (uv0, uv1) = atlas.glyph_uv(ch);
        batch.add_textured_rect_clipped(
            Rect::new(pen_x, pen_y, gw, gh),
            z,
            style.color,
            atlas_texture,
            uv0,
            uv1,
            clip,
        );
        pen_x += gw + style.letter_spacing;
    }
}

pub fn append_text_aligned(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    text: &str,
    bounds: Rect,
    z: f32,
    style: TextStyle,
    align: TextAlign,
    clip: Option<Rect>,
) {
    let line_h = style.line_height * style.scale;
    let mut pen_y = bounds.y;
    for line in text.split('\n') {
        let metrics = measure_text(atlas, line, style);
        let pen_x = match align {
            TextAlign::Left => bounds.x,
            TextAlign::Center => bounds.center_x() - metrics.width * 0.5,
            TextAlign::Right => bounds.right() - metrics.width,
        };
        append_text(
            batch,
            atlas,
            atlas_texture,
            line,
            pen_x,
            pen_y,
            z,
            style,
            clip.or(Some(bounds)),
        );
        pen_y += line_h;
        if pen_y > bounds.bottom() {
            break;
        }
    }
}

pub fn append_text_lines(
    batch: &mut UiBatch,
    atlas: &GlyphAtlas,
    atlas_texture: u32,
    lines: &[&str],
    bounds: Rect,
    z: f32,
    style: TextStyle,
    clip: Option<Rect>,
) {
    let line_h = style.line_height * style.scale;
    let mut pen_y = bounds.y;
    for line in lines {
        if pen_y + line_h > bounds.bottom() {
            break;
        }
        append_text(
            batch,
            atlas,
            atlas_texture,
            line,
            bounds.x,
            pen_y,
            z,
            style,
            clip.or(Some(bounds)),
        );
        pen_y += line_h;
    }
}
