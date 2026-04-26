// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Texture sampling and mipmap generation. BGRA32 source.
//!
//! [`Texture`] wraps an immutable BGRA32 pixel slice with nearest/bilinear
//! sampling and configurable wrap modes. [`MipmapChain`] stores up to 12
//! levels and selects the right level by a texel-footprint estimate.

use crate::gl::{FilterMode, WrapMode};
use crate::math::{Vec2, Vec4};

#[derive(Clone, Copy)]
pub struct Texture<'a> {
    pub pixels: &'a [u32],
    pub width: u32,
    pub height: u32,
    pub wrap_s: WrapMode,
    pub wrap_t: WrapMode,
    pub min_filter: FilterMode,
    pub mag_filter: FilterMode,
    pub border_color: [f32; 4],
}

/// A mipmap chain: up to 12 levels (covers 4096×4096 down to 1×1).
pub struct MipmapChain<'a> {
    pub levels: [Option<Texture<'a>>; 12],
    pub count: usize,
}

impl<'a> MipmapChain<'a> {
    pub const fn empty() -> Self {
        Self {
            levels: [None; 12],
            count: 0,
        }
    }

    /// Sample with automatic LOD selection.
    ///
    /// `ddx`/`ddy` are the UV-space derivatives per screen pixel;
    /// pass `[0.0, 0.0]` if unknown (falls back to base level).
    pub fn sample(&self, uv: Vec2, ddx: Vec2, ddy: Vec2) -> Vec4 {
        if self.count == 0 {
            return Vec4::new(1.0, 0.0, 1.0, 1.0);
        }
        // λ = 0.5 * log2(max(|ddx|², |ddy|²))
        let ddx2 = ddx.x * ddx.x + ddx.y * ddx.y;
        let ddy2 = ddy.x * ddy.x + ddy.y * ddy.y;
        let lambda = if ddx2 > 1e-12 || ddy2 > 1e-12 {
            0.5 * libm::log2f(ddx2.max(ddy2))
        } else {
            0.0
        }
        .max(0.0);

        let Some(base) = self.levels[0].as_ref() else {
            return Vec4::new(1.0, 0.0, 1.0, 1.0);
        };

        if lambda <= 0.0 {
            return sample_with_filter(base, uv, base.mag_filter);
        }

        let max_lod = (self.count - 1) as f32;
        let lambda = lambda.min(max_lod);
        let lod0 = libm::floorf(lambda) as usize;
        let lod1 = (lod0 + 1).min(self.count - 1);
        let frac = lambda - lod0 as f32;

        match base.min_filter {
            FilterMode::Nearest => sample_with_filter(base, uv, FilterMode::Nearest),
            FilterMode::Linear => sample_with_filter(base, uv, FilterMode::Linear),
            FilterMode::NearestMipmapNearest => {
                let lod = libm::floorf(lambda + 0.5) as usize;
                let tex = self.levels[lod.min(self.count - 1)]
                    .as_ref()
                    .unwrap_or(base);
                sample_with_filter(tex, uv, FilterMode::Nearest)
            }
            FilterMode::LinearMipmapNearest => {
                let lod = libm::floorf(lambda + 0.5) as usize;
                let tex = self.levels[lod.min(self.count - 1)]
                    .as_ref()
                    .unwrap_or(base);
                sample_with_filter(tex, uv, FilterMode::Linear)
            }
            FilterMode::NearestMipmapLinear => {
                let t0 = self.levels[lod0].as_ref().unwrap_or(base);
                let t1 = self.levels[lod1].as_ref().unwrap_or(t0);
                let c0 = sample_with_filter(t0, uv, FilterMode::Nearest);
                let c1 = sample_with_filter(t1, uv, FilterMode::Nearest);
                lerp4(c0, c1, frac)
            }
            FilterMode::LinearMipmapLinear => {
                let t0 = self.levels[lod0].as_ref().unwrap_or(base);
                let t1 = self.levels[lod1].as_ref().unwrap_or(t0);
                let c0 = sample_with_filter(t0, uv, FilterMode::Linear);
                let c1 = sample_with_filter(t1, uv, FilterMode::Linear);
                lerp4(c0, c1, frac)
            }
        }
    }
}

/// Generate a single 2× downsampled mipmap level into `dst`.
///
/// `src` is the source BGRA32 slice (`src_w × src_h`).
/// `dst` must have capacity for `(src_w/2) × (src_h/2)` pixels.
/// Uses a 2×2 box filter.
pub fn generate_mip_level(src: &[u32], src_w: u32, src_h: u32, dst: &mut [u32]) {
    let dw = (src_w / 2).max(1);
    let dh = (src_h / 2).max(1);
    for dy in 0..dh {
        for dx in 0..dw {
            let sx = dx * 2;
            let sy = dy * 2;
            let px = |x: u32, y: u32| -> [u32; 4] {
                let p = src[(y.min(src_h - 1) * src_w + x.min(src_w - 1)) as usize];
                [
                    (p >> 16) & 0xFF,
                    (p >> 8) & 0xFF,
                    p & 0xFF,
                    (p >> 24) & 0xFF,
                ]
            };
            let p00 = px(sx, sy);
            let p10 = px(sx + 1, sy);
            let p01 = px(sx, sy + 1);
            let p11 = px(sx + 1, sy + 1);
            let avg = |i: usize| -> u32 { (p00[i] + p10[i] + p01[i] + p11[i] + 2) / 4 };
            dst[(dy * dw + dx) as usize] = (avg(3) << 24) | (avg(0) << 16) | (avg(1) << 8) | avg(2);
        }
    }
}

impl<'a> Texture<'a> {
    pub const fn new(pixels: &'a [u32], width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
            wrap_s: WrapMode::Repeat,
            wrap_t: WrapMode::Repeat,
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            border_color: [0.0, 0.0, 0.0, 1.0],
        }
    }

    pub const fn with_filters(
        pixels: &'a [u32],
        width: u32,
        height: u32,
        wrap_s: WrapMode,
        wrap_t: WrapMode,
        min_filter: FilterMode,
        mag_filter: FilterMode,
    ) -> Self {
        Self {
            pixels,
            width,
            height,
            wrap_s,
            wrap_t,
            min_filter,
            mag_filter,
            border_color: [0.0, 0.0, 0.0, 1.0],
        }
    }

    fn wrap_coord(coord: f32, mode: WrapMode, size: u32) -> u32 {
        let s = size as f32;
        let t = match mode {
            WrapMode::Repeat => {
                let f = coord - libm::floorf(coord);
                f * s
            }
            WrapMode::MirroredRepeat => {
                let fi = libm::floorf(coord) as i32;
                let f = coord - fi as f32;
                let mirrored = if fi & 1 == 0 { f } else { 1.0 - f };
                mirrored * s
            }
            WrapMode::ClampToEdge | WrapMode::ClampToBorder => coord.clamp(0.0, 1.0) * s,
        };
        (t as u32).min(size - 1)
    }

    fn resolve_index(coord: i32, size: u32, mode: WrapMode) -> Option<u32> {
        let s = size as i32;
        match mode {
            WrapMode::Repeat => Some(coord.rem_euclid(s) as u32),
            WrapMode::MirroredRepeat => {
                let period = s * 2;
                let c = coord.rem_euclid(period);
                let mapped = if c >= s { (period - 1) - c } else { c };
                Some(mapped as u32)
            }
            WrapMode::ClampToEdge => Some(coord.clamp(0, s - 1) as u32),
            WrapMode::ClampToBorder => {
                if coord < 0 || coord >= s {
                    None
                } else {
                    Some(coord as u32)
                }
            }
        }
    }

    fn fetch_wrapped(&self, x: i32, y: i32) -> Vec4 {
        let Some(xi) = Self::resolve_index(x, self.width, self.wrap_s) else {
            return Vec4::new(
                self.border_color[0],
                self.border_color[1],
                self.border_color[2],
                self.border_color[3],
            );
        };
        let Some(yi) = Self::resolve_index(y, self.height, self.wrap_t) else {
            return Vec4::new(
                self.border_color[0],
                self.border_color[1],
                self.border_color[2],
                self.border_color[3],
            );
        };
        unpack(self.pixels[(yi * self.width + xi) as usize])
    }

    /// Sample using the texture's configured wrap and mag filter.
    pub fn sample(&self, uv: Vec2) -> Vec4 {
        match self.mag_filter {
            FilterMode::Nearest
            | FilterMode::NearestMipmapNearest
            | FilterMode::NearestMipmapLinear => self.sample_nearest(uv),
            _ => self.sample_bilinear(uv),
        }
    }

    /// Nearest-neighbour sample with texture's wrap mode.
    pub fn sample_nearest(&self, uv: Vec2) -> Vec4 {
        let x = Self::wrap_coord(uv.x, self.wrap_s, self.width);
        let y = Self::wrap_coord(uv.y, self.wrap_t, self.height);
        unpack(self.pixels[(y * self.width + x) as usize])
    }

    /// Bilinear sample with texture's wrap mode.
    pub fn sample_bilinear(&self, uv: Vec2) -> Vec4 {
        let u = uv.x * self.width as f32 - 0.5;
        let v = uv.y * self.height as f32 - 0.5;
        let x0 = libm::floorf(u) as i32;
        let y0 = libm::floorf(v) as i32;
        let fx = u - x0 as f32;
        let fy = v - y0 as f32;
        let c00 = self.fetch_wrapped(x0, y0);
        let c10 = self.fetch_wrapped(x0 + 1, y0);
        let c01 = self.fetch_wrapped(x0, y0 + 1);
        let c11 = self.fetch_wrapped(x0 + 1, y0 + 1);
        let cx0 = lerp4(c00, c10, fx);
        let cx1 = lerp4(c01, c11, fx);
        lerp4(cx0, cx1, fy)
    }
}

#[inline]
fn sample_with_filter(tex: &Texture<'_>, uv: Vec2, filter: FilterMode) -> Vec4 {
    match filter {
        FilterMode::Nearest
        | FilterMode::NearestMipmapNearest
        | FilterMode::NearestMipmapLinear => tex.sample_nearest(uv),
        _ => tex.sample_bilinear(uv),
    }
}

#[inline]
fn unpack(p: u32) -> Vec4 {
    Vec4::new(
        ((p >> 16) & 0xFF) as f32 / 255.0,
        ((p >> 8) & 0xFF) as f32 / 255.0,
        (p & 0xFF) as f32 / 255.0,
        ((p >> 24) & 0xFF) as f32 / 255.0,
    )
}

#[inline]
fn lerp4(a: Vec4, b: Vec4, t: f32) -> Vec4 {
    Vec4::new(
        a.x + (b.x - a.x) * t,
        a.y + (b.y - a.y) * t,
        a.z + (b.z - a.z) * t,
        a.w + (b.w - a.w) * t,
    )
}
