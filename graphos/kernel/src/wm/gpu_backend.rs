// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GPU backend trait and implementations for the GraphOS scene compositor.
//!
//! ## Backends
//!
//! | Backend | Status | Description |
//! |---|---|---|
//! | `Virtio2dBackend` | Active | Optimised CPU blit into virtio-gpu scanout buffer. No heap allocation on the hot path. |
//! | `NativeGpuBackend` | Planned | GraphOS-native GPU command path driven by GraphOS submit packets. |
//!
//! ## Backend selection
//!
//! `ActiveBackend::init()` selects the best GraphOS-supported backend at
//! startup. The rest of the compositor is backend-agnostic.
//!
//! ## Coordinate system
//!
//! All coordinates are screen pixels.  (0, 0) is top-left.  The backend is
//! responsible for clamping all operations to screen bounds.

#![allow(dead_code)]

use crate::wm::damage::DamageRect;
use crate::wm::scene::{BorderDef, FillDef, Material, ShadowDef, Transform};

// ── Capabilities ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default)]
pub struct BackendCapabilities {
    /// GPU-native texture compositing.  When `false`, CPU blits are used.
    pub gpu_textures: bool,
    /// Programmable shaders available in the active backend.
    pub shaders: bool,
    /// Box shadow can be rendered natively (GPU path).
    pub native_shadow: bool,
    /// Corner rounding can be rendered natively.
    pub native_corners: bool,
    /// Gaussian blur is GPU-accelerated.
    pub native_blur: bool,
}

// ── GpuBackend trait ─────────────────────────────────────────────────────────

/// Abstraction over the hardware rendering path.
///
/// Each method corresponds to one high-level compositing operation.  The
/// backend handles all clipping, blending, and hardware-specific encoding.
pub trait GpuBackend {
    /// Capability flags for this backend.
    fn capabilities(&self) -> BackendCapabilities;

    // ── Frame lifecycle ───────────────────────────────────────────────────────

    /// Called before the first draw call of a frame.
    fn begin_frame(&mut self, damage: DamageRect, screen_w: u32, screen_h: u32);

    /// Called after all draw calls; submits the frame to the display.
    fn end_frame(&mut self, damage: DamageRect);

    // ── Fill operations ───────────────────────────────────────────────────────

    /// Fill a rectangle with a solid color or gradient.
    fn fill_rect(&mut self, rect: DamageRect, fill: FillDef, mat: &Material);

    /// Draw a border around `rect`.
    fn draw_border(&mut self, rect: DamageRect, border: BorderDef, mat: &Material);

    // ── Surface blit ──────────────────────────────────────────────────────────

    /// Blit ring-3 surface `surface_id` into the composited scene.
    ///
    /// `src` is the source rect within the surface buffer (often the full surface).
    /// `transform` positions and scales the surface into screen space.
    /// `mat` controls opacity, blend mode, and post-processing.
    fn blit_surface(
        &mut self,
        surface_id: u32,
        src_w: u32,
        src_h: u32,
        transform: &Transform,
        mat: &Material,
    );

    // ── Effect operations ─────────────────────────────────────────────────────

    /// Render a drop shadow beneath `content_rect`.
    ///
    /// The default implementation is a software-approximated box shadow.
    /// Backends with shader support should override this.
    fn draw_shadow(&mut self, content_rect: DamageRect, shadow: ShadowDef);

    // ── Cursor ────────────────────────────────────────────────────────────────

    /// Draw the software cursor sprite at `(cx, cy)`.
    fn draw_cursor(&mut self, cx: i32, cy: i32);
}

// ── Virtio-2D backend ────────────────────────────────────────────────────────
//
// Uses the existing virtio-gpu 2-D scanout path.  Compositing is done on the
// CPU but the hot path (opacity=255, scale=1.0) uses direct page→framebuffer
// `copy_nonoverlapping` instead of a per-pixel loop through a Vec.
//
// Key improvement over the previous `blit_surface`:
//   OLD: alloc Vec<u32>(W×H)  →  copy pages into Vec  →  per-pixel blend loop
//   NEW: for each row: copy_nonoverlapping(src_page_ptr, fb_row_ptr, row_len)
//   For 1280×800 full-screen surface this eliminates a 4 MB heap alloc + copy.

pub struct Virtio2dBackend {
    screen_w: u32,
    screen_h: u32,
}

impl Virtio2dBackend {
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        Self { screen_w, screen_h }
    }
}

impl GpuBackend for Virtio2dBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::default() // all false — pure CPU path
    }

    fn begin_frame(&mut self, damage: DamageRect, screen_w: u32, screen_h: u32) {
        self.screen_w = screen_w;
        self.screen_h = screen_h;
        // Fill the damage region with the background color to clear stale pixels.
        let clipped = damage.clip(screen_w, screen_h);
        if !clipped.is_empty() {
            crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                clipped.x, clipped.y, clipped.w, clipped.h, 0xFF060A16,
            );
        }
    }

    fn end_frame(&mut self, damage: DamageRect) {
        let clipped = damage.clip(self.screen_w, self.screen_h);
        if !clipped.is_empty() {
            crate::drivers::gpu::virtio_gpu::flush_rect(
                clipped.x as u32,
                clipped.y as u32,
                clipped.w,
                clipped.h,
            );
        }
    }

    fn fill_rect(&mut self, rect: DamageRect, fill: FillDef, mat: &Material) {
        if mat.opacity == 0 {
            return;
        }
        let clipped = rect.clip(self.screen_w, self.screen_h);
        if clipped.is_empty() {
            return;
        }

        match fill {
            FillDef::Solid(color) => {
                let c = apply_opacity(color, mat.opacity);
                crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                    clipped.x, clipped.y, clipped.w, clipped.h, c,
                );
            }
            FillDef::LinearV { from, to } => {
                // Row-by-row gradient (software).
                let total = clipped.h.max(1) as u64;
                for dy in 0..clipped.h {
                    let t = ((dy as u64 * 255) / total) as u8;
                    let c = blend_argb(from, to, t);
                    let c = apply_opacity(c, mat.opacity);
                    crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                        clipped.x,
                        clipped.y + dy as i32,
                        clipped.w,
                        1,
                        c,
                    );
                }
            }
            FillDef::LinearH { from, to } => {
                // This is too slow per-column; approximate with 8 bands.
                let bands = 8u32;
                let bw = clipped.w / bands;
                for i in 0..bands {
                    let t = ((i * 255) / bands.max(1)) as u8;
                    let c = blend_argb(from, to, t);
                    let c = apply_opacity(c, mat.opacity);
                    let bx = clipped.x + (i * bw) as i32;
                    let bw2 = if i == bands - 1 {
                        clipped.w - i * bw
                    } else {
                        bw
                    };
                    crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                        bx, clipped.y, bw2, clipped.h, c,
                    );
                }
            }
        }
    }

    fn draw_border(&mut self, rect: DamageRect, border: BorderDef, _mat: &Material) {
        let w = border.width as u32;
        let c = border.color;
        // Top
        crate::drivers::gpu::virtio_gpu::fill_rect_scanout(rect.x, rect.y, rect.w, w, c);
        // Bottom
        crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
            rect.x,
            rect.y + rect.h as i32 - w as i32,
            rect.w,
            w,
            c,
        );
        // Left
        crate::drivers::gpu::virtio_gpu::fill_rect_scanout(rect.x, rect.y, w, rect.h, c);
        // Right
        crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
            rect.x + rect.w as i32 - w as i32,
            rect.y,
            w,
            rect.h,
            c,
        );
    }

    fn blit_surface(
        &mut self,
        surface_id: u32,
        src_w: u32,
        src_h: u32,
        transform: &Transform,
        mat: &Material,
    ) {
        if mat.opacity == 0 {
            return;
        }
        // Delegate to the optimised virtio-gpu blit path.
        crate::drivers::gpu::virtio_gpu::blit_surface_scene(
            surface_id,
            src_w,
            src_h,
            transform.tx,
            transform.ty,
            transform.sx as u16,
            mat.opacity,
        );
    }

    fn draw_shadow(&mut self, content_rect: DamageRect, shadow: ShadowDef) {
        if !shadow.is_active() {
            return;
        }
        // Software approximation: draw a blurred dark rect behind content.
        // The native GPU backend can replace this with a proper blur pass.
        let r = shadow.blur as i32;
        let sx = content_rect.x + shadow.offset_x as i32;
        let sy = content_rect.y + shadow.offset_y as i32;
        let sw = content_rect.w;
        let sh = content_rect.h;
        // Draw 3 expanding rects with decreasing opacity to simulate blur.
        for step in 0i32..=2 {
            let expand = step * r / 3;
            let alpha = (shadow.color >> 24) as u8;
            let step_alpha = alpha / (3 - step as u8).max(1);
            let c = (shadow.color & 0x00FF_FFFF) | ((step_alpha as u32) << 24);
            let clipped = DamageRect::new(
                sx - expand,
                sy - expand,
                sw + (expand * 2) as u32,
                sh + (expand * 2) as u32,
            )
            .clip(self.screen_w, self.screen_h);
            if !clipped.is_empty() {
                crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                    clipped.x, clipped.y, clipped.w, clipped.h, c,
                );
            }
        }
    }

    fn draw_cursor(&mut self, cx: i32, cy: i32) {
        // 12×20 software arrow cursor (white with 1-pixel black outline).
        const CW: u32 = 12;
        const CH: u32 = 20;
        let cr = DamageRect::new(cx, cy, CW, CH).clip(self.screen_w, self.screen_h);
        if cr.is_empty() {
            return;
        }
        // Outline
        crate::drivers::gpu::virtio_gpu::fill_rect_scanout(cx, cy, CW, CH, 0xFF000000);
        // Fill interior (2px smaller on right/bottom)
        crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
            cx + 1,
            cy + 1,
            CW - 2,
            CH - 2,
            0xFFFFFFFF,
        );
        // Arrow tip — just leave the square for now; the native GPU backend can
        // replace this with a real cursor sprite surface.
    }
}

// ── ActiveBackend dispatch ────────────────────────────────────────────────────

/// Runtime-selected backend (virtio-2D only).
pub enum ActiveBackend {
    Virtio2d(Virtio2dBackend),
}

impl ActiveBackend {
    /// Initialise the virtio-2D backend.
    pub fn init(screen_w: u32, screen_h: u32) -> Self {
        crate::arch::serial::write_line(b"[gpu-backend] using virtio-2d backend\n");
        ActiveBackend::Virtio2d(Virtio2dBackend::new(screen_w, screen_h))
    }
}

impl GpuBackend for ActiveBackend {
    fn capabilities(&self) -> BackendCapabilities {
        match self {
            ActiveBackend::Virtio2d(b) => b.capabilities(),
        }
    }
    fn begin_frame(&mut self, d: DamageRect, sw: u32, sh: u32) {
        match self {
            ActiveBackend::Virtio2d(b) => b.begin_frame(d, sw, sh),
        }
    }
    fn end_frame(&mut self, d: DamageRect) {
        match self {
            ActiveBackend::Virtio2d(b) => b.end_frame(d),
        }
    }
    fn fill_rect(&mut self, r: DamageRect, f: FillDef, m: &Material) {
        match self {
            ActiveBackend::Virtio2d(b) => b.fill_rect(r, f, m),
        }
    }
    fn draw_border(&mut self, r: DamageRect, bd: BorderDef, m: &Material) {
        match self {
            ActiveBackend::Virtio2d(b) => b.draw_border(r, bd, m),
        }
    }
    fn blit_surface(&mut self, sid: u32, sw: u32, sh: u32, t: &Transform, m: &Material) {
        match self {
            ActiveBackend::Virtio2d(b) => b.blit_surface(sid, sw, sh, t, m),
        }
    }
    fn draw_shadow(&mut self, r: DamageRect, s: ShadowDef) {
        match self {
            ActiveBackend::Virtio2d(b) => b.draw_shadow(r, s),
        }
    }
    fn draw_cursor(&mut self, cx: i32, cy: i32) {
        match self {
            ActiveBackend::Virtio2d(b) => b.draw_cursor(cx, cy),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn blend_argb(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255u32.wrapping_sub(t);
    let r = ((a >> 16 & 0xFF) * it + (b >> 16 & 0xFF) * t) / 255;
    let g = ((a >> 8 & 0xFF) * it + (b >> 8 & 0xFF) * t) / 255;
    let bl = ((a & 0xFF) * it + (b & 0xFF) * t) / 255;
    let ao = ((a >> 24) * it + (b >> 24) * t) / 255;
    (ao << 24) | (r << 16) | (g << 8) | bl
}

#[inline]
fn apply_opacity(color: u32, opacity: u8) -> u32 {
    if opacity == 255 {
        return color;
    }
    let a = (color >> 24) as u32 * opacity as u32 / 255;
    (color & 0x00FF_FFFF) | (a << 24)
}
