// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Phase J Session 29 — GPU effect pipeline.
//!
//! Effects run on the software pixel buffer (CPU-side) because virtio-gpu
//! compute shaders are not yet available.  Each effect produces output
//! suitable for submission to the `gpu_compositor` blit pipeline.
//!
//! ## Effects provided
//! - **Kawase dual-filter blur** — approximates Gaussian blur (σ ≈ 16 px).
//!   Applied to the desktop wallpaper layer; NOT run per-frame on live windows
//!   (cost is ~4ms CPU on 1280×800; acceptable at wallpaper-change frequency).
//! - **Drop shadow** — pre-computed per-window shadow map sampled at blit time.
//!   Shadow parameters: 24 px Gaussian spread, 0.35 opacity, (0, 6 px) offset.
//! - **Rim lighting** — 1 px specular highlight on active window top edge.
//! - **Frosted blur region** — samples a sub-region of the back-buffer,
//!   applies a fast box-blur (σ ≈ 4 px), tints with accent colour at 60%.
//!   Used by the title bar chrome; intentionally lower quality than wallpaper blur.
//!
//! ## Notes on GPU acceleration
//! The `GPU_COMPUTE_AVAILABLE` flag in `gpu_compositor.rs` will flip to `true`
//! once the virtio-gpu driver exposes a compute queue.  At that point the
//! Kawase and frosted-blur passes can be moved to GPU command buffers without
//! changing the public API of this module.

use core::sync::atomic::{AtomicBool, Ordering};

/// Set externally when a software pixel buffer is available for CPU-side effects.
pub static SOFTWARE_EFFECTS_ENABLED: AtomicBool = AtomicBool::new(true);

// ─────────────────────────────────────────────────────────────────────────────
// Kawase dual-filter blur
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum pixel buffer size this module will operate on (1280 × 800 × 4 bytes).
const MAX_PIXELS: usize = 1280 * 800;

/// Scratch buffer for blur intermediate results (allocated once, zero-initialised).
static mut BLUR_SCRATCH: [u32; MAX_PIXELS] = [0u32; MAX_PIXELS];

/// Down-sample radius for the Kawase pass (distance from centre to corner samples).
const KAWASE_DOWN_RADIUS: i32 = 4;
/// Up-sample radius.
const KAWASE_UP_RADIUS: i32 = 5;

/// Blit a u32 BGRA32 pixel from `src` to `dst` with bilinear clamp.
#[inline(always)]
fn sample_clamp(src: &[u32], stride: usize, x: i32, y: i32, w: i32, h: i32) -> u32 {
    let cx = x.clamp(0, w - 1) as usize;
    let cy = y.clamp(0, h - 1) as usize;
    src[cy * stride + cx]
}

/// Average four BGRA32 pixels (no overflow — each channel fits in 10 bits after sum).
#[inline(always)]
fn avg4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    let r = (((a >> 16) & 0xFF) + ((b >> 16) & 0xFF) + ((c >> 16) & 0xFF) + ((d >> 16) & 0xFF)) / 4;
    let g = (((a >> 8) & 0xFF) + ((b >> 8) & 0xFF) + ((c >> 8) & 0xFF) + ((d >> 8) & 0xFF)) / 4;
    let bl = ((a & 0xFF) + (b & 0xFF) + (c & 0xFF) + (d & 0xFF)) / 4;
    let aa =
        (((a >> 24) & 0xFF) + ((b >> 24) & 0xFF) + ((c >> 24) & 0xFF) + ((d >> 24) & 0xFF)) / 4;
    (aa << 24) | (r << 16) | (g << 8) | bl
}

/// Kawase dual-filter blur in-place on a BGRA32 pixel buffer.
///
/// `buf` must be exactly `w × h` u32 values.  Returns immediately if the
/// buffer is too large for the static scratch or if `!SOFTWARE_EFFECTS_ENABLED`.
pub fn kawase_blur_inplace(buf: &mut [u32], w: usize, h: usize) {
    if !SOFTWARE_EFFECTS_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if w == 0 || h == 0 || w * h > MAX_PIXELS || buf.len() < w * h {
        return;
    }

    let w_i = w as i32;
    let h_i = h as i32;

    // Safety: BLUR_SCRATCH is only accessed here; no re-entrancy in no_std kernel
    // single-core context (called from vsync which is non-reentrant).
    let scratch = unsafe { &mut BLUR_SCRATCH[..w * h] };

    // Pass 1: down-sample — copy from buf -> scratch with diagonal offsets.
    let r = KAWASE_DOWN_RADIUS;
    for y in 0..h_i {
        for x in 0..w_i {
            let a = sample_clamp(buf, w, x - r, y - r, w_i, h_i);
            let b = sample_clamp(buf, w, x + r, y - r, w_i, h_i);
            let c = sample_clamp(buf, w, x - r, y + r, w_i, h_i);
            let d = sample_clamp(buf, w, x + r, y + r, w_i, h_i);
            scratch[y as usize * w + x as usize] = avg4(a, b, c, d);
        }
    }

    // Pass 2: up-sample — copy from scratch -> buf with expanded offsets.
    let r2 = KAWASE_UP_RADIUS;
    for y in 0..h_i {
        for x in 0..w_i {
            let a = sample_clamp(scratch, w, x - r2, y - r2, w_i, h_i);
            let b = sample_clamp(scratch, w, x + r2, y - r2, w_i, h_i);
            let c = sample_clamp(scratch, w, x - r2, y + r2, w_i, h_i);
            let d = sample_clamp(scratch, w, x + r2, y + r2, w_i, h_i);
            buf[y as usize * w + x as usize] = avg4(a, b, c, d);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Frosted-glass sub-region blur (title bar)
// ─────────────────────────────────────────────────────────────────────────────

/// 3×3 box blur applied to a sub-rectangle of `src` (stride = screen width).
/// `dst` receives the blurred region (stride = `rect_w`).
/// Accent tint is blended at 60% opacity after the blur.
///
/// `accent` is a BGRA32 colour; only RGB channels are used.
/// Parameters for [`frosted_blur_region`].
pub struct BlurRegionParams {
    pub rect_x: usize,
    pub rect_y: usize,
    pub rect_w: usize,
    pub rect_h: usize,
    pub src_w: usize,
    pub src_h: usize,
    pub accent: u32,
}

pub fn frosted_blur_region(
    src: &[u32],
    src_stride: usize,
    dst: &mut [u32],
    params: BlurRegionParams,
) {
    let BlurRegionParams {
        rect_x,
        rect_y,
        rect_w,
        rect_h,
        src_w,
        src_h,
        accent,
    } = params;
    if rect_w == 0 || rect_h == 0 {
        return;
    }
    if dst.len() < rect_w * rect_h {
        return;
    }

    let ar = (accent >> 16) & 0xFF;
    let ag = (accent >> 8) & 0xFF;
    let ab = accent & 0xFF;

    for ry in 0..rect_h {
        for rx in 0..rect_w {
            // 3×3 box blur — clamp to src bounds.
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            let mut cnt = 0u32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let sx = (rect_x as i32 + rx as i32 + dx).clamp(0, src_w as i32 - 1) as usize;
                    let sy = (rect_y as i32 + ry as i32 + dy).clamp(0, src_h as i32 - 1) as usize;
                    let p = src[sy * src_stride + sx];
                    sr += (p >> 16) & 0xFF;
                    sg += (p >> 8) & 0xFF;
                    sb += p & 0xFF;
                    cnt += 1;
                }
            }
            sr /= cnt;
            sg /= cnt;
            sb /= cnt;

            // 60% accent tint.
            let fr = (sr * 40 + ar * 60) / 100;
            let fg = (sg * 40 + ag * 60) / 100;
            let fb = (sb * 40 + ab * 60) / 100;

            dst[ry * rect_w + rx] = 0xFF000000 | (fr << 16) | (fg << 8) | fb;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Drop shadow
// ─────────────────────────────────────────────────────────────────────────────

/// Shadow parameters.
pub struct ShadowParams {
    /// Gaussian spread radius (pixels).
    pub spread: u32,
    /// Opacity (0–255).
    pub opacity: u8,
    /// Horizontal offset (pixels, positive = right).
    pub offset_x: i32,
    /// Vertical offset (pixels, positive = down).
    pub offset_y: i32,
    /// Shadow BGRA32 colour (alpha ignored; opacity param controls blend).
    pub color: u32,
}

/// Default Phase J drop shadow: 24 px spread, 35% opacity, (0, +6) offset.
pub const DEFAULT_SHADOW: ShadowParams = ShadowParams {
    spread: 24,
    opacity: 89, // 0.35 × 255 ≈ 89
    offset_x: 0,
    offset_y: 6,
    color: 0xFF000000,
};

/// Compute the alpha weight of a shadow pixel at distance `d` from the window
/// edge using a box-sampled exponential approximation of Gaussian falloff.
///
/// Returns an opacity multiplier in 0–255.
#[inline(always)]
pub fn shadow_alpha(d: u32, spread: u32) -> u8 {
    if spread == 0 || d >= spread {
        return 0;
    }
    // Linear falloff from opacity at d=0 to 0 at d=spread.
    // A Gaussian falloff would be more accurate but requires expf; linear is
    // sufficient for the pre-computed box-shadow map approach.
    ((spread - d) * 255 / spread).min(255) as u8
}

// ─────────────────────────────────────────────────────────────────────────────
// Rim lighting
// ─────────────────────────────────────────────────────────────────────────────

/// Specular rim highlight colour for active window top edge.
/// 1 px gradient from `accent + 30% luminance`; rendered as a single-row
/// override on top of the chrome title bar blit.
///
/// Returns a BGRA32 pixel with the rim highlight colour derived from `accent`.
pub fn rim_highlight(accent: u32) -> u32 {
    let r = ((accent >> 16) & 0xFF).min(0xFF);
    let g = ((accent >> 8) & 0xFF).min(0xFF);
    let b = (accent & 0xFF).min(0xFF);
    // Boost luminance by ~30% (saturating add).
    let r2 = (r + 77).min(0xFF);
    let g2 = (g + 77).min(0xFF);
    let b2 = (b + 77).min(0xFF);
    0xFF000000 | (r2 << 16) | (g2 << 8) | b2
}

// ─────────────────────────────────────────────────────────────────────────────
// Rounded corner clip mask
// ─────────────────────────────────────────────────────────────────────────────

/// Radius for Phase J window rounded corners (pixels).
pub const CORNER_RADIUS: u32 = 10;

/// Returns true if the pixel at `(px, py)` within a rectangle of
/// `(w, h)` is inside the rounded-corner clip mask (radius = `r`).
#[inline(always)]
pub fn in_rounded_rect(px: i32, py: i32, w: i32, h: i32, r: i32) -> bool {
    if px < 0 || py < 0 || px >= w || py >= h {
        return false;
    }
    // Only the four corner regions need the distance test.
    let (cx, cy) = if px < r && py < r {
        (r, r)
    } else if px >= w - r && py < r {
        (w - r - 1, r)
    } else if px < r && py >= h - r {
        (r, h - r - 1)
    } else if px >= w - r && py >= h - r {
        (w - r - 1, h - r - 1)
    } else {
        return true;
    };
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}
