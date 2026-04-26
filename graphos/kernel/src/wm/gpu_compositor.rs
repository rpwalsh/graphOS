// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GPU command-list compositor pipeline — Phase J (Session 28).
//!
//! Scene-graph -> blit command list -> software fallback (GPU ring in Session 29).
//!
//! Layer order (back to front):
//!   0 Background  � wallpaper surface or solid fill
//!   1 Shadow      � per-window drop shadow (dimmed blit)
//!   2 Window      � app surfaces with spring-physics transforms
//!   3 Overlay     � notifications, menus, tooltips
//!   4 Cursor      � hardware cursor sprite
//!
//! Integration: desktop.rs vsync loop calls `frame_tick()` at 60 Hz.
//! frame_tick() drains the present queue, advances spring physics,
//! builds the BlitList, and executes via software blit (virtio-gpu ring TODO).

//!
//! ## Event-driven API (preferred)
//!
//! `compose_for_damage(damage)` — build and execute the blit list for only
//! the supplied damage rectangle, then flush exactly that rect to virtio-gpu.
//! The caller (desktop::run) invokes this only when the damage accumulator is
//! non-empty.  No timer drives this call.
//!
//! `tick_springs_if_active()` — advance spring physics only when at least one
//! window is animating.  Returns `true` if springs are still running so the
//! caller can re-arm an animation-timer source node and post scene damage.
//!
//! `frame_tick()` — legacy full-frame path retained for compatibility.
//! Prefer `compose_for_damage` for all new callers.

use alloc::vec::Vec;

use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::wm::gpu::{
    EXPOSE_MODE, MAX_GPU_SURFACES, expose_tile_for, surface_id_snapshot, surface_transform,
    tick_animations,
};
use crate::wm::surface_table::{
    MAX_SURFACE_FRAMES, present_queue_pop, surface_dimensions, surface_exists, surface_frames,
};

// -- Feature flags -------------------------------------------------------------

/// True once the virtio-gpu driver confirms GPU compute is available.
pub static GPU_COMPUTE_AVAILABLE: AtomicBool = AtomicBool::new(false);
static TRACE_FIRST_ACTIVE_FRAME: AtomicBool = AtomicBool::new(true);
static DAMAGE_FLUSH_LOG_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
static DAMAGE_MINIMAL_SCENE_LOG_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
static SCANOUT_SMOKE_TEST_PENDING: AtomicBool = AtomicBool::new(true);

// -- Layer constants ------------------------------------------------------------

pub const LAYER_BACKGROUND: u8 = 0;
pub const LAYER_SHADOW: u8 = 1;
pub const LAYER_WINDOW: u8 = 2;
pub const LAYER_OVERLAY: u8 = 3;
pub const LAYER_CURSOR: u8 = 4;

// -- Blit command --------------------------------------------------------------

/// Maximum blit commands per frame.
pub const MAX_BLIT_CMDS: usize = MAX_GPU_SURFACES * 3 + 8;

/// One blit operation in the frame command list.
#[derive(Clone, Copy)]
pub struct BlitCmd {
    /// Source surface ID.  0 = solid fill with `fill_color`.
    pub src_surface_id: u32,
    pub src_w: u16,
    pub src_h: u16,
    /// Destination top-left on screen (pixels).
    pub dst_x: i32,
    pub dst_y: i32,
    /// Scale x1024 (1024 = 1.0).
    pub scale_fp: u16,
    /// Opacity x255 (255 = fully opaque).
    pub opacity: u8,
    /// Layer (back-to-front sort key).
    pub layer: u8,
    /// BGRA32 fill colour for src_surface_id == 0.
    pub fill_color: u32,
}

impl BlitCmd {
    const DEFAULT: Self = Self {
        src_surface_id: 0,
        src_w: 0,
        src_h: 0,
        dst_x: 0,
        dst_y: 0,
        scale_fp: 1024,
        opacity: 255,
        layer: LAYER_WINDOW,
        fill_color: 0xFF1A1A2E,
    };
}

/// Per-frame blit command list.
pub struct BlitList {
    cmds: [BlitCmd; MAX_BLIT_CMDS],
    len: usize,
}

impl BlitList {
    const fn new() -> Self {
        Self {
            cmds: [BlitCmd::DEFAULT; MAX_BLIT_CMDS],
            len: 0,
        }
    }

    pub fn push(&mut self, cmd: BlitCmd) {
        if self.len < MAX_BLIT_CMDS {
            self.cmds[self.len] = cmd;
            self.len += 1;
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn as_slice(&self) -> &[BlitCmd] {
        &self.cmds[..self.len]
    }

    /// In-place insertion sort by layer (stable, small N).
    pub fn sort_by_layer(&mut self) {
        for i in 1..self.len {
            let key = self.cmds[i];
            let mut j = i;
            while j > 0 && self.cmds[j - 1].layer > key.layer {
                self.cmds[j] = self.cmds[j - 1];
                j -= 1;
            }
            self.cmds[j] = key;
        }
    }
}

// -- Pipeline state ------------------------------------------------------------

struct PipelineState {
    screen_w: u32,
    screen_h: u32,
    background_surface_id: u32,
    cursor_surface_id: u32,
    cursor_x: i32,
    cursor_y: i32,
}

impl PipelineState {
    const fn new() -> Self {
        Self {
            screen_w: 1280,
            screen_h: 800,
            background_surface_id: 0,
            cursor_surface_id: 0,
            cursor_x: 640,
            cursor_y: 400,
        }
    }
}

static PIPELINE: Mutex<PipelineState> = Mutex::new(PipelineState::new());
static FRAME_LIST: Mutex<BlitList> = Mutex::new(BlitList::new());

// -- Public API ----------------------------------------------------------------

/// Initialise pipeline after framebuffer is mapped.
pub fn init(screen_w: u32, screen_h: u32) {
    crate::wm::gpu::init(screen_w, screen_h);
    let mut p = PIPELINE.lock();
    p.screen_w = screen_w;
    p.screen_h = screen_h;
}

/// Register the wallpaper surface (rendered by the ring-3 desktop service).
pub fn set_background_surface(surface_id: u32) {
    PIPELINE.lock().background_surface_id = surface_id;
}

pub fn background_surface_id() -> u32 {
    PIPELINE.lock().background_surface_id
}

/// Update cursor position (called by the input driver on pointer events).
pub fn set_cursor_pos(x: i32, y: i32) {
    let mut p = PIPELINE.lock();
    p.cursor_x = x;
    p.cursor_y = y;
}

/// Set the cursor sprite surface (32x32 BGRA32).
pub fn set_cursor_surface(surface_id: u32) {
    PIPELINE.lock().cursor_surface_id = surface_id;
}

// ── Event-driven API ─────────────────────────────────────────────────────────

/// Compose and flush only the pixels covered by `damage`.
///
/// ## When to call
/// Call this from the desktop event loop whenever `DamageAccumulator::is_dirty()`
/// is true.  Do NOT call on a fixed timer.
///
/// ## virtio-gpu flush
/// Only `damage` (clipped to screen) is transferred to the host via
/// `RESOURCE_FLUSH`.  Unchanged regions of the framebuffer are not touched.
///
/// ## Blit list filtering
/// Surfaces whose screen-space bounding box does not intersect `damage` are
/// excluded from the blit list entirely.  The background layer is always
/// included as the clear base for the damaged region.
pub fn compose_for_damage(damage: crate::wm::damage::DamageRect) {
    if damage.is_empty() {
        return;
    }

    let (screen_w, screen_h, bg_id, cursor_id, cx, cy) = {
        let p = PIPELINE.lock();
        (
            p.screen_w,
            p.screen_h,
            p.background_surface_id,
            p.cursor_surface_id,
            p.cursor_x,
            p.cursor_y,
        )
    };

    let mut list = FRAME_LIST.lock();
    list.clear();

    // Background — always included as the base clear for the damage region.
    if bg_id != 0 && surface_exists(bg_id) {
        let (w, h) = surface_dimensions(bg_id).unwrap_or((screen_w as u16, screen_h as u16));
        list.push(BlitCmd {
            src_surface_id: bg_id,
            src_w: w,
            src_h: h,
            layer: LAYER_BACKGROUND,
            ..BlitCmd::DEFAULT
        });
    } else {
        list.push(BlitCmd {
            src_w: screen_w as u16,
            src_h: screen_h as u16,
            layer: LAYER_BACKGROUND,
            fill_color: 0xFF1A1A2E,
            ..BlitCmd::DEFAULT
        });
    }

    // Windows — only include surfaces that intersect the damage rect.
    let expose_on = EXPOSE_MODE.load(Ordering::Relaxed);
    let mut ids = [0u32; MAX_GPU_SURFACES];
    let n = surface_id_snapshot(&mut ids);

    for &sid in &ids[..n] {
        if sid == bg_id {
            continue;
        }
        if !surface_exists(sid) {
            continue;
        }
        let (w, h) = match surface_dimensions(sid) {
            Some(d) => d,
            None => continue,
        };

        let (dst_x, dst_y, scale_fp, opacity) = if expose_on {
            let tile = expose_tile_for(sid);
            let sfp = ((tile.target_scale as u32).saturating_mul(1024) / 1000) as u16;
            (tile.target_x, tile.target_y, sfp, 220u8)
        } else {
            match surface_transform(sid) {
                Some((px, py, s1000, o1000)) => {
                    let sfp = (s1000.saturating_mul(1024) / 1000) as u16;
                    let op = (o1000.saturating_mul(255) / 1000).min(255) as u8;
                    (px, py, sfp, op)
                }
                None => (0, 0, 1024u16, 255u8),
            }
        };

        // Compute screen-space bounds for this surface.
        let sw = ((w as u64).saturating_mul(scale_fp.max(1) as u64) / 1024) as u32;
        let sh = ((h as u64).saturating_mul(scale_fp.max(1) as u64) / 1024) as u32;
        let surf_rect = crate::wm::damage::DamageRect::new(dst_x, dst_y, sw, sh);

        // Shadow rect (offset by the drop-shadow displacement).
        let shadow_rect = crate::wm::damage::DamageRect::new(dst_x + 4, dst_y + 6, sw, sh);

        let touches = surf_rect.intersects(damage) || shadow_rect.intersects(damage);
        if !touches {
            continue;
        }

        // Shadow
        if opacity >= 32 {
            list.push(BlitCmd {
                src_surface_id: sid,
                src_w: w,
                src_h: h,
                dst_x: dst_x + 4,
                dst_y: dst_y + 6,
                scale_fp,
                opacity: opacity >> 2,
                layer: LAYER_SHADOW,
                fill_color: 0,
            });
        }
        // Window
        list.push(BlitCmd {
            src_surface_id: sid,
            src_w: w,
            src_h: h,
            dst_x,
            dst_y,
            scale_fp,
            opacity,
            layer: LAYER_WINDOW,
            fill_color: 0,
        });
    }

    // Cursor — include only if its sprite overlaps the damage region.
    if cursor_id != 0 && surface_exists(cursor_id) {
        let cursor_rect = crate::wm::damage::DamageRect::new(cx, cy, 32, 32);
        if cursor_rect.intersects(damage) {
            list.push(BlitCmd {
                src_surface_id: cursor_id,
                src_w: 32,
                src_h: 32,
                dst_x: cx,
                dst_y: cy,
                layer: LAYER_CURSOR,
                ..BlitCmd::DEFAULT
            });
        }
    } else {
        // Software fallback: draw a small white arrow cursor.
        let cursor_rect = crate::wm::damage::DamageRect::new(cx, cy, 12, 12);
        if cursor_rect.intersects(damage) {
            list.push(BlitCmd {
                src_surface_id: 0,
                src_w: 12,
                src_h: 12,
                dst_x: cx,
                dst_y: cy,
                layer: LAYER_CURSOR,
                fill_color: 0xFFFFFFFF,
                ..BlitCmd::DEFAULT
            });
        }
    }

    list.sort_by_layer();

    if list.as_slice().len() <= 2 && damage.w >= screen_w && damage.h >= screen_h {
        let log_idx = DAMAGE_MINIMAL_SCENE_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
        if log_idx < 64 {
            crate::arch::serial::write_bytes(b"[gpu-compositor] minimal scene: bg=");
            crate::arch::serial::write_u64_dec_inline(bg_id as u64);
            crate::arch::serial::write_bytes(b" cursor=");
            crate::arch::serial::write_u64_dec_inline(cursor_id as u64);
            crate::arch::serial::write_bytes(b" active_surfaces=");
            crate::arch::serial::write_u64_dec(n as u64);
        }
    }

    // Execute blit list, then flush exactly the damage rect to virtio-gpu.
    execute_for_damage(&list, damage, screen_w, screen_h);
}

/// Advance spring physics for surfaces that are currently animating.
///
/// Returns `true` if at least one spring is still running after this tick,
/// meaning the caller should post scene damage and loop again without halting.
/// Returns `false` when all springs have settled — the caller may then halt.
///
/// ## Timer model
/// This function is the *only* place spring physics advances.  It must be
/// called at ~120 Hz, but ONLY when `any_animating()` is true.  The desktop
/// event loop uses a tick-count gate to enforce the 120 Hz cadence without
/// imposing a global frame timer on the entire system.
pub fn tick_springs_if_active() -> bool {
    if !crate::wm::gpu::any_animating() {
        return false;
    }
    tick_animations();
    crate::wm::gpu::any_animating()
}

fn execute_for_damage(
    list: &BlitList,
    damage: crate::wm::damage::DamageRect,
    screen_w: u32,
    screen_h: u32,
) {
    if !crate::drivers::gpu::virtio_gpu::is_present() {
        GPU_COMPUTE_AVAILABLE.store(false, Ordering::Release);
        return;
    }
    GPU_COMPUTE_AVAILABLE.store(true, Ordering::Release);
    crate::mm::page_table::with_kernel_address_space(|| {
        if SCANOUT_SMOKE_TEST_PENDING.swap(false, Ordering::AcqRel) {
            crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                0, 0, screen_w, screen_h, 0xFF00FF00,
            );
            crate::drivers::gpu::virtio_gpu::flush_rect(0, 0, screen_w, screen_h);
            crate::arch::serial::write_line(b"[gpu-compositor] scanout smoke flush full-screen\n");
        }

        execute_gpu_scanout(list);

        // Flush exactly the damage rect (clipped to screen bounds).
        let clipped = damage.clip(screen_w, screen_h);
        if !clipped.is_empty() {
            crate::drivers::gpu::virtio_gpu::flush_rect(
                clipped.x as u32,
                clipped.y as u32,
                clipped.w,
                clipped.h,
            );

            let n = DAMAGE_FLUSH_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
            if n < 128 {
                crate::arch::serial::write_bytes(b"[gpu-compositor] damage flush rect=");
                crate::arch::serial::write_u64_dec_inline(clipped.x as u64);
                crate::arch::serial::write_bytes(b",");
                crate::arch::serial::write_u64_dec_inline(clipped.y as u64);
                crate::arch::serial::write_bytes(b" ");
                crate::arch::serial::write_u64_dec_inline(clipped.w as u64);
                crate::arch::serial::write_bytes(b"x");
                crate::arch::serial::write_u64_dec_inline(clipped.h as u64);
                crate::arch::serial::write_bytes(b" cmds=");
                crate::arch::serial::write_u64_dec(list.as_slice().len() as u64);
            }
        }
    });
}

/// Per-vsync entry point � call from LAPIC timer / vsync interrupt at 60 Hz.
/// Legacy full-frame entry point.
///
/// Retained as a thin wrapper that simply requests a full-screen damage
/// recompose so any leftover caller goes through the unified
/// `compose_for_damage` pipeline. The previous implementation built a
/// separate scene + flush sequence that raced with the damage-driven path
/// and overwrote app surface pixels.
pub fn frame_tick() {
    // Drain any stale present-queue entries so the damage path doesn't
    // re-blit pixels that have already been composed.
    while present_queue_pop().is_some() {}
    tick_animations();
    let (screen_w, screen_h) = {
        let p = PIPELINE.lock();
        (p.screen_w, p.screen_h)
    };
    if screen_w == 0 || screen_h == 0 {
        return;
    }
    compose_for_damage(crate::wm::damage::DamageRect::new(0, 0, screen_w, screen_h));
}

#[allow(dead_code)]
fn execute_command_list(list: &BlitList) {
    if !crate::drivers::gpu::virtio_gpu::is_present() {
        GPU_COMPUTE_AVAILABLE.store(false, Ordering::Release);
        return;
    }

    GPU_COMPUTE_AVAILABLE.store(true, Ordering::Release);
    execute_gpu_scanout(list);

    let mut any = false;
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;

    for cmd in list.as_slice() {
        let (x, y, w, h) = cmd_bounds(cmd);
        if w <= 0 || h <= 0 {
            continue;
        }
        any = true;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x.saturating_add(w));
        max_y = max_y.max(y.saturating_add(h));
    }

    if !any {
        return;
    }

    let (screen_w, screen_h) = {
        let p = PIPELINE.lock();
        (p.screen_w as i32, p.screen_h as i32)
    };

    let x0 = min_x.clamp(0, screen_w.max(0));
    let y0 = min_y.clamp(0, screen_h.max(0));
    let x1 = max_x.clamp(0, screen_w.max(0));
    let y1 = max_y.clamp(0, screen_h.max(0));
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    crate::drivers::gpu::virtio_gpu::flush_rect(
        x0 as u32,
        y0 as u32,
        (x1 - x0) as u32,
        (y1 - y0) as u32,
    );
}

fn execute_gpu_scanout(list: &BlitList) {
    for cmd in list.as_slice() {
        match cmd.layer {
            LAYER_BACKGROUND if cmd.src_surface_id == 0 => {
                crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                    0,
                    0,
                    cmd.src_w as u32,
                    cmd.src_h as u32,
                    cmd.fill_color,
                );
            }
            LAYER_BACKGROUND => {
                // Use zero-alloc direct-page blit; forces A=0xFF for QEMU compat.
                crate::drivers::gpu::virtio_gpu::blit_surface_scene(
                    cmd.src_surface_id,
                    cmd.src_w as u32,
                    cmd.src_h as u32,
                    cmd.dst_x,
                    cmd.dst_y,
                    cmd.scale_fp,
                    cmd.opacity,
                );
            }
            LAYER_SHADOW => {
                if cmd.opacity >= 16 {
                    crate::drivers::gpu::virtio_gpu::blit_surface_scene(
                        cmd.src_surface_id,
                        cmd.src_w as u32,
                        cmd.src_h as u32,
                        cmd.dst_x,
                        cmd.dst_y,
                        cmd.scale_fp,
                        cmd.opacity,
                    );
                }
            }
            _ if cmd.src_surface_id == 0 => {
                crate::drivers::gpu::virtio_gpu::fill_rect_scanout(
                    cmd.dst_x,
                    cmd.dst_y,
                    cmd.src_w as u32,
                    cmd.src_h as u32,
                    cmd.fill_color,
                );
            }
            _ => {
                // Window / overlay layers: zero-alloc, forces A=0xFF.
                crate::drivers::gpu::virtio_gpu::blit_surface_scene(
                    cmd.src_surface_id,
                    cmd.src_w as u32,
                    cmd.src_h as u32,
                    cmd.dst_x,
                    cmd.dst_y,
                    cmd.scale_fp,
                    cmd.opacity,
                );
            }
        }
    }
}

#[allow(dead_code)]
fn cmd_bounds(cmd: &BlitCmd) -> (i32, i32, i32, i32) {
    let w = if cmd.src_surface_id == 0 {
        cmd.src_w as i32
    } else {
        ((cmd.src_w as i64).saturating_mul(cmd.scale_fp.max(1) as i64) / 1024) as i32
    };
    let h = if cmd.src_surface_id == 0 {
        cmd.src_h as i32
    } else {
        ((cmd.src_h as i64).saturating_mul(cmd.scale_fp.max(1) as i64) / 1024) as i32
    };
    (cmd.dst_x, cmd.dst_y, w.max(0), h.max(0))
}

fn blit_surface(cmd: &BlitCmd) {
    const PIXELS_PER_FRAME: usize = 4096 / core::mem::size_of::<u32>();

    let mut frames = [0u64; MAX_SURFACE_FRAMES];
    let frame_count = surface_frames(cmd.src_surface_id, &mut frames);
    if frame_count == 0 {
        return;
    }

    let w = cmd.src_w as usize;
    let h = cmd.src_h as usize;
    if w == 0 || h == 0 {
        return;
    }

    let total_pixels = w.saturating_mul(h);
    let max_pixels = frame_count.saturating_mul(PIXELS_PER_FRAME);
    let copy_pixels = total_pixels.min(max_pixels);
    if copy_pixels == 0 {
        return;
    }

    let mut src = Vec::new();
    if src.try_reserve_exact(total_pixels).is_err() {
        return;
    }
    src.resize(total_pixels, 0);

    let mut pixel_offset = 0usize;
    for frame_phys in frames.iter().copied().take(frame_count) {
        if !crate::mm::page_table::ensure_identity_mapped_2m(frame_phys) {
            return;
        }
        let frame_pixels =
            unsafe { core::slice::from_raw_parts(frame_phys as *const u32, PIXELS_PER_FRAME) };
        let remaining = copy_pixels.saturating_sub(pixel_offset);
        if remaining == 0 {
            break;
        }
        let chunk = remaining.min(PIXELS_PER_FRAME);
        src[pixel_offset..pixel_offset + chunk].copy_from_slice(&frame_pixels[..chunk]);
        pixel_offset += chunk;
    }

    crate::drivers::gpu::virtio_gpu::blit_pixels_scanout(
        &src,
        w,
        h,
        cmd.dst_x,
        cmd.dst_y,
        cmd.scale_fp,
        cmd.opacity,
    );
}
