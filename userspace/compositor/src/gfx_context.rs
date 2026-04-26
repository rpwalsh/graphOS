// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graphics context — CPU blitter and GPU command encoder.
//!
//! ## Architecture
//!
//! `GfxContext` is the abstraction over two execution paths:
//!
//! | Implementation   | When used                              | Mechanism                              |
//! |------------------|----------------------------------------|----------------------------------------|
//! | `CpuBlitter`     | Always (Phase 1)                       | Direct writes to surface pixel buffer  |
//! | `GpuCmdEncoder`  | When GPU path is enabled               | Encodes GraphOS-native commands → SYS_GPU_SUBMIT |
//!
//! Both implement the `GfxContext` trait so that pass implementations in
//! `passes.rs` are portable across both paths.
//!
//! ## Command encoding (GPU path)
//!
//! GPU commands are encoded as typed GraphOS-native commands and serialized
//! by `graphos-gfx::wire` before submission to the kernel via `SYS_GPU_SUBMIT`.
//!
//! ## CPU blitter
//!
//! The CPU blitter operates on a surface pixel buffer obtained via a
//! `SYS_SURFACE_GET_PIXELS` syscall (mapped shared memory).  It implements:
//! - `fill_rect(x, y, w, h, argb)` — solid color fill with rounded corners
//! - `blit_surface(sid, ...)` — copy pixels from source surface
//! - `alpha_composite(dst, src, opacity)` — Porter-Duff src-over
//! - `blur_rect(x, y, w, h, radius)` — box blur approximation (3 passes)
//! - `gradient_fill(...)` — vertical/horizontal gradient fill

extern crate alloc;
use alloc::vec::Vec;

use graphos_gfx::wire;
use graphos_gfx::{
    Color as GfxColor, CommandBuffer as NativeCommandBuffer, GradientDir, Rect as GfxRect,
    ResourceId,
};

/// Submit a GraphOS-native byte-wire command buffer.
const SYS_GPU_SUBMIT: u64 = 0x508;

// ── Native GPU command buffer ────────────────────────────────────────────────

/// Encodes GraphOS-native GPU commands and serializes them through graphos-gfx wire format.
pub struct NativeGpuCommandBuf {
    pub cmds: NativeCommandBuffer,
    wire: Vec<u8>,
}

impl NativeGpuCommandBuf {
    pub fn new() -> Self {
        Self {
            cmds: NativeCommandBuffer::with_capacity(128),
            wire: Vec::with_capacity(4096),
        }
    }

    pub fn reset(&mut self, w: u32, h: u32) {
        self.cmds.clear();
        self.cmds.set_viewport(0.0, 0.0, w as f32, h as f32);
    }

    pub fn draw_rect(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, radius: u8) {
        self.cmds.fill_rect(
            ResourceId::INVALID,
            GfxRect::new(x, y, w, h),
            GfxColor(argb),
            radius,
        );
    }

    pub fn draw_gradient(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        argb_a: u32,
        argb_b: u32,
        dir: u8,
    ) {
        let grad_dir = match dir {
            1 => GradientDir::LeftToRight,
            2 => GradientDir::Diagonal,
            3 => GradientDir::Radial,
            _ => GradientDir::TopToBottom,
        };
        self.cmds.fill_gradient(
            ResourceId::INVALID,
            GfxRect::new(x, y, w, h),
            GfxColor(argb_a),
            GfxColor(argb_b),
            grad_dir,
        );
    }

    pub fn draw_shadow(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        argb: u32,
        ox: i8,
        oy: i8,
        sigma: u8,
    ) {
        self.cmds.shadow(
            ResourceId::INVALID,
            GfxRect::new(x, y, w, h),
            GfxColor(argb),
            ox,
            oy,
            sigma,
            0,
        );
    }

    pub fn draw_border(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        argb: u32,
        width: u8,
        radius: u8,
    ) {
        self.cmds.border(
            ResourceId::INVALID,
            GfxRect::new(x, y, w, h),
            GfxColor(argb),
            width,
            radius,
        );
    }

    pub fn import_surface(&mut self, surface_id: u32) {
        self.cmds.import_surface(surface_id, ResourceId(surface_id));
    }

    pub fn blit(
        &mut self,
        src_res: u32,
        dst_res: u32,
        sx: u32,
        sy: u32,
        sw: u32,
        sh: u32,
        dx: i32,
        dy: i32,
        dw: u32,
        dh: u32,
        opacity: u8,
    ) {
        self.cmds.blit(
            ResourceId(src_res),
            GfxRect::new(sx as i32, sy as i32, sw, sh),
            ResourceId(dst_res),
            GfxRect::new(dx, dy, dw, dh),
            opacity,
        );
    }

    pub fn blur_rect(&mut self, target: u32, x: i32, y: i32, w: u32, h: u32, sigma: u8) {
        self.cmds
            .blur(ResourceId(target), GfxRect::new(x, y, w, h), sigma);
    }

    pub fn present(&mut self, src_res: u32) {
        self.cmds.present(ResourceId(src_res));
    }

    pub fn submit(&mut self) {
        self.wire.clear();
        wire::encode(&self.cmds, &mut self.wire);

        #[cfg(target_os = "none")]
        {
            unsafe {
                core::arch::asm!(
                    "int 0x80",
                    in("rax") SYS_GPU_SUBMIT,
                    in("rdi") self.wire.as_ptr() as u64,
                    in("rsi") self.wire.len() as u64,
                    options(nostack),
                );
            }
        }

        self.cmds.clear();
    }
}

impl Default for NativeGpuCommandBuf {
    fn default() -> Self {
        Self::new()
    }
}

// ── GfxContext trait ──────────────────────────────────────────────────────────

/// Unified interface over CPU and GPU rendering paths.
///
/// Pass implementations call these methods; the concrete implementation
/// decides whether to do CPU work inline or encode a GPU command.
pub trait GfxContext {
    /// Fill a rectangle with a solid ARGB color, optionally with rounded corners.
    fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, corner_radius: u8);

    /// Fill a rectangle with a two-stop linear gradient.
    fn fill_gradient(&mut self, x: i32, y: i32, w: u32, h: u32, from: u32, to: u32, dir: u8);

    /// Draw a drop shadow for a rectangle.
    fn draw_shadow(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, ox: i8, oy: i8, blur: u8);

    /// Draw a border around a rectangle.
    fn draw_border(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, width: u8, radius: u8);

    /// Blit an imported surface at `(dst_x, dst_y)` scaled to `(dst_w, dst_h)` with opacity.
    fn blit_surface(
        &mut self,
        surface_id: u32,
        src_w: u32,
        src_h: u32,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
        opacity: u8,
    );

    /// Apply a dual-Kawase blur to `(x, y, w, h)` in the current render target.
    fn blur_rect(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        radius: u8,
        iterations: u8,
        downsample: u8,
    );

    /// Submit the current frame to the display.
    fn present(&mut self);

    /// Returns true when native GPU command submission is enabled.
    fn gpu_available(&self) -> bool;

    /// Returns the screen dimensions.
    fn screen_dims(&self) -> (u32, u32);
}

// ── CPU blitter ───────────────────────────────────────────────────────────────

/// Software GfxContext that writes directly into a mapped surface pixel buffer.
///
/// The buffer is a BGRA8 framebuffer.  All operations are inlined — no syscalls
/// mid-frame except for `present()` which calls `SYS_SURFACE_COMMIT`.
pub struct CpuBlitter {
    /// BGRA8 pixel buffer — mapped shared memory for the compositor's scanout surface.
    pub pixels: *mut u32,
    pub stride: u32,
    pub width: u32,
    pub height: u32,
    /// Kernel surface ID for `SYS_SURFACE_COMMIT`.
    pub surface_id: u32,
}

// SAFETY: The CPU blitter is single-threaded — the compositor event loop is
// sequential and `pixels` is a private framebuffer mapping.
unsafe impl Send for CpuBlitter {}

impl CpuBlitter {
    /// Create a new CPU blitter wrapping a framebuffer mapping.
    ///
    /// # Safety
    ///
    /// `pixels` must remain valid and exclusively writable for the lifetime of
    /// this `CpuBlitter`.  The buffer must be at least `stride * height` u32s.
    pub unsafe fn new(
        pixels: *mut u32,
        stride: u32,
        width: u32,
        height: u32,
        surface_id: u32,
    ) -> Self {
        Self {
            pixels,
            stride,
            width,
            height,
            surface_id,
        }
    }

    /// Write a single pixel at (x, y) — unsafe inline for inner loops.
    #[inline(always)]
    fn set_pixel(&mut self, x: u32, y: u32, argb: u32) {
        if x < self.width && y < self.height {
            // SAFETY: bounds checked above.
            unsafe {
                let ptr = self.pixels.add((y * self.stride + x) as usize);
                *ptr = argb_to_bgra(argb);
            }
        }
    }

    /// Read a pixel at (x, y).
    #[inline(always)]
    fn get_pixel(&self, x: u32, y: u32) -> u32 {
        if x < self.width && y < self.height {
            // SAFETY: bounds checked.
            let bgra = unsafe { *self.pixels.add((y * self.stride + x) as usize) };
            bgra_to_argb(bgra)
        } else {
            0
        }
    }

    /// Alpha-composite `src` (ARGB) over the pixel at (x, y).
    #[inline(always)]
    fn composite_pixel(&mut self, x: u32, y: u32, src: u32) {
        let a = (src >> 24) as u32;
        if a == 0 {
            return;
        }
        if a == 255 {
            self.set_pixel(x, y, src);
            return;
        }
        let dst = self.get_pixel(x, y);
        let ia = 255 - a;
        let r = ((src >> 16) & 0xFF) * a / 255 + ((dst >> 16) & 0xFF) * ia / 255;
        let g = ((src >> 8) & 0xFF) * a / 255 + ((dst >> 8) & 0xFF) * ia / 255;
        let b = (src & 0xFF) * a / 255 + (dst & 0xFF) * ia / 255;
        let da = 255u32.max((dst >> 24) & 0xFF);
        self.set_pixel(x, y, (da << 24) | (r << 16) | (g << 8) | b);
    }

    /// Box blur — three-pass approximation of a Gaussian.
    fn box_blur(&mut self, x: i32, y: i32, w: u32, h: u32, radius: u8) {
        let r = radius as u32;
        let mut row_buf: Vec<u32> = Vec::with_capacity(w as usize);

        // Horizontal pass
        for py in y.max(0) as u32..((y + h as i32).min(self.height as i32) as u32) {
            row_buf.clear();
            for px in x.max(0) as u32..((x + w as i32).min(self.width as i32) as u32) {
                let mut ra = 0u32;
                let mut rr = 0u32;
                let mut rg = 0u32;
                let mut rb = 0u32;
                let mut count = 0u32;
                for kx in 0..=r * 2 {
                    let sx = px as i64 + kx as i64 - r as i64;
                    if sx >= x as i64 && sx < (x + w as i32) as i64 {
                        let p = self.get_pixel(sx as u32, py);
                        ra += (p >> 24) & 0xFF;
                        rr += (p >> 16) & 0xFF;
                        rg += (p >> 8) & 0xFF;
                        rb += p & 0xFF;
                        count += 1;
                    }
                }
                if count > 0 {
                    row_buf.push(
                        ((ra / count) << 24)
                            | ((rr / count) << 16)
                            | ((rg / count) << 8)
                            | (rb / count),
                    );
                } else {
                    row_buf.push(self.get_pixel(px, py));
                }
            }
            let bx = x.max(0) as u32;
            for (i, &c) in row_buf.iter().enumerate() {
                self.set_pixel(bx + i as u32, py, c);
            }
        }

        // Vertical pass
        let mut col_buf: Vec<u32> = Vec::with_capacity(h as usize);
        for px in x.max(0) as u32..((x + w as i32).min(self.width as i32) as u32) {
            col_buf.clear();
            for py in y.max(0) as u32..((y + h as i32).min(self.height as i32) as u32) {
                let mut ra = 0u32;
                let mut rr = 0u32;
                let mut rg = 0u32;
                let mut rb = 0u32;
                let mut count = 0u32;
                for ky in 0..=r * 2 {
                    let sy = py as i64 + ky as i64 - r as i64;
                    if sy >= y as i64 && sy < (y + h as i32) as i64 {
                        let p = self.get_pixel(px, sy as u32);
                        ra += (p >> 24) & 0xFF;
                        rr += (p >> 16) & 0xFF;
                        rg += (p >> 8) & 0xFF;
                        rb += p & 0xFF;
                        count += 1;
                    }
                }
                if count > 0 {
                    col_buf.push(
                        ((ra / count) << 24)
                            | ((rr / count) << 16)
                            | ((rg / count) << 8)
                            | (rb / count),
                    );
                } else {
                    col_buf.push(self.get_pixel(px, py));
                }
            }
            let by = y.max(0) as u32;
            for (i, &c) in col_buf.iter().enumerate() {
                self.set_pixel(px, by + i as u32, c);
            }
        }
    }
}

impl GfxContext for CpuBlitter {
    fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, corner_radius: u8) {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + w as i32) as u32).min(self.width);
        let y1 = ((y + h as i32) as u32).min(self.height);
        let cr = corner_radius as i32;
        // Center of corners for radius test (relative to rect)
        for py in y0..y1 {
            for px in x0..x1 {
                let rx = px as i32 - x;
                let ry = py as i32 - y;
                // Rounded corner rejection
                if cr > 0 {
                    let cx = if rx < cr {
                        cr
                    } else if rx >= w as i32 - cr {
                        w as i32 - cr - 1
                    } else {
                        rx
                    };
                    let cy = if ry < cr {
                        cr
                    } else if ry >= h as i32 - cr {
                        h as i32 - cr - 1
                    } else {
                        ry
                    };
                    let dx = (rx - cx).abs();
                    let dy = (ry - cy).abs();
                    if dx > 0 && dy > 0 {
                        // Outside corner arc
                        let d2 = dx * dx + dy * dy;
                        if d2 > cr * cr {
                            continue;
                        }
                    }
                }
                self.composite_pixel(px, py, argb);
            }
        }
    }

    fn fill_gradient(&mut self, x: i32, y: i32, w: u32, h: u32, from: u32, to: u32, _dir: u8) {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + w as i32) as u32).min(self.width);
        let y1 = ((y + h as i32) as u32).min(self.height);
        for py in y0..y1 {
            let t = if h > 0 {
                ((py - y0) * 255 / h) as u8
            } else {
                0
            };
            let c = lerp_argb(from, to, t);
            for px in x0..x1 {
                self.composite_pixel(px, py, c);
            }
        }
    }

    fn draw_shadow(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, ox: i8, oy: i8, blur: u8) {
        // Draw shadow rect offset then blur it
        let sx = x + ox as i32;
        let sy = y + oy as i32;
        self.fill_rect(sx, sy, w, h, argb, 0);
        if blur > 0 {
            self.box_blur(sx, sy, w, h, blur);
        }
    }

    fn draw_border(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, width: u8, _radius: u8) {
        let bw = width as i32;
        // Top
        self.fill_rect(x, y, w, bw as u32, argb, 0);
        // Bottom
        self.fill_rect(x, y + h as i32 - bw, w, bw as u32, argb, 0);
        // Left
        self.fill_rect(x, y + bw, bw as u32, h - bw as u32 * 2, argb, 0);
        // Right
        self.fill_rect(
            x + w as i32 - bw,
            y + bw,
            bw as u32,
            h - bw as u32 * 2,
            argb,
            0,
        );
    }

    fn blit_surface(
        &mut self,
        _surface_id: u32,
        _src_w: u32,
        _src_h: u32,
        _dst_x: i32,
        _dst_y: i32,
        _dst_w: u32,
        _dst_h: u32,
        _opacity: u8,
    ) {
        // CPU-path surface blit deferred to `wm::gpu_compositor::blit_surface_scene()`
        // which operates in the kernel with direct page access.
        // The compositor calls this when it has a pointer to the source surface.
        // For Phase 1, the kernel handles surface blitting; this is a no-op here.
    }

    fn blur_rect(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        radius: u8,
        iterations: u8,
        _downsample: u8,
    ) {
        for _ in 0..iterations {
            self.box_blur(x, y, w, h, radius);
        }
    }

    fn present(&mut self) {
        // Phase 1: the kernel handles the final scanout via SYS_SURFACE_COMMIT.
        // The compositor binary calls this syscall after building the frame.
        // `surface_id` is submitted via the syscall interface.
        #[cfg(target_os = "none")]
        {
            // SAFETY: This is a raw syscall in our own OS kernel.
            unsafe {
                core::arch::asm!(
                    "int 0x80",
                    in("rax") 0x401u64,   // SYS_SURFACE_COMMIT
                    in("rdi") self.surface_id as u64,
                    options(nostack),
                );
            }
        }
    }

    fn gpu_available(&self) -> bool {
        false
    }

    fn screen_dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

// ── GPU command encoder ───────────────────────────────────────────────────────

/// GPU-path GfxContext — encodes GraphOS-native commands into `NativeGpuCommandBuf`.
///
/// Commands are accumulated during a frame and submitted in bulk via
/// `SYS_GPU_SUBMIT` in `present()`.
pub struct GpuCmdEncoder {
    pub buf: NativeGpuCommandBuf,
    pub screen_w: u32,
    pub screen_h: u32,
}

impl GpuCmdEncoder {
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        let mut enc = Self {
            buf: NativeGpuCommandBuf::new(),
            screen_w,
            screen_h,
        };
        enc.buf.reset(screen_w, screen_h);
        enc
    }

    pub fn reset(&mut self) {
        self.buf.reset(self.screen_w, self.screen_h);
    }
}

impl GfxContext for GpuCmdEncoder {
    fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, corner_radius: u8) {
        self.buf.draw_rect(x, y, w, h, argb, corner_radius);
    }

    fn fill_gradient(&mut self, x: i32, y: i32, w: u32, h: u32, from: u32, to: u32, dir: u8) {
        self.buf.draw_gradient(x, y, w, h, from, to, dir);
    }

    fn draw_shadow(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, ox: i8, oy: i8, blur: u8) {
        self.buf.draw_shadow(x, y, w, h, argb, ox, oy, blur);
    }

    fn draw_border(&mut self, x: i32, y: i32, w: u32, h: u32, argb: u32, width: u8, radius: u8) {
        self.buf.draw_border(x, y, w, h, argb, width, radius);
    }

    fn blit_surface(
        &mut self,
        surface_id: u32,
        src_w: u32,
        src_h: u32,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
        opacity: u8,
    ) {
        self.buf.import_surface(surface_id);
        self.buf.blit(
            surface_id, 0, 0, 0, src_w, src_h, dst_x, dst_y, dst_w, dst_h, opacity,
        );
    }

    fn blur_rect(
        &mut self,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        radius: u8,
        iterations: u8,
        downsample: u8,
    ) {
        let _ = (iterations, downsample);
        self.buf.blur_rect(0, x, y, w, h, radius);
    }

    fn present(&mut self) {
        self.buf.present(0);
        self.buf.submit();
        self.reset();
    }

    fn gpu_available(&self) -> bool {
        true
    }

    fn screen_dims(&self) -> (u32, u32) {
        (self.screen_w, self.screen_h)
    }
}

// ── Color helpers ─────────────────────────────────────────────────────────────

fn argb_to_bgra(argb: u32) -> u32 {
    let a = (argb >> 24) & 0xFF;
    let r = (argb >> 16) & 0xFF;
    let g = (argb >> 8) & 0xFF;
    let b = argb & 0xFF;
    (a << 24) | (r << 8) | (g << 16) | b
}

fn bgra_to_argb(bgra: u32) -> u32 {
    let a = (bgra >> 24) & 0xFF;
    let r = (bgra >> 8) & 0xFF;
    let g = (bgra >> 16) & 0xFF;
    let b = bgra & 0xFF;
    (a << 24) | (r << 16) | (g << 8) | b
}

fn lerp_argb(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255 - t;
    let aa = ((a >> 24) & 0xFF) * it / 255 + ((b >> 24) & 0xFF) * t / 255;
    let ar = ((a >> 16) & 0xFF) * it / 255 + ((b >> 16) & 0xFF) * t / 255;
    let ag = ((a >> 8) & 0xFF) * it / 255 + ((b >> 8) & 0xFF) * t / 255;
    let ab = (a & 0xFF) * it / 255 + (b & 0xFF) * t / 255;
    (aa << 24) | (ar << 16) | (ag << 8) | ab
}
