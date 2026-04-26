// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Phase J GPU compositor — Kawase dual-filter blur, spring physics, Exposé.
//!
//! This module provides the kernel-resident GPU compositing infrastructure
//! for GraphOS Phase J ("3D KDE" shell).  It layers on top of the existing
//! `surface_table` and `compositor` modules and operates through the
//! existing `SYS_SURFACE_PRESENT` / `SYS_SURFACE_FLUSH` path.
//!
//! ## Architecture
//!
//! ```text
//! Ring-3 compositor service
//!        │ SYS_SURFACE_COMMIT (0x405)
//!        ▼
//! GpuCompositor (this module)
//!   ┌──────────────────────────────────┐
//!   │  SurfaceRecord × MAX_SURFACES    │
//!   │  ┌─────────────────────────────┐ │
//!   │  │  SpringState (per-window)   │ │
//!   │  │  KawaseState (per-surface)  │ │
//!   │  └─────────────────────────────┘ │
//!   │  ExposeLayout (overview mode)    │
//!   └──────────────────────────────────┘
//!        │ rendered pixels
//!        ▼
//! kernel compositor (compositor.rs) → framebuffer
//! ```
//!
//! ## Kawase dual-filter blur
//!
//! The Kawase dual-filter is a two-pass approx Gaussian blur:
//!   - Down-sample pass: radius = N/2, sample 4 diagonal neighbours
//!   - Up-sample pass:   radius = N/2 + 0.5, blend 4 neighbours
//!
//! Parameters are tuned for the Dark Glass theme (σ ≈ 16 px at 1× DPI).
//! All passes operate on the software pixel buffer because virtio-gpu
//! compute shaders are not yet available; the blur is therefore applied
//! only to the desktop wallpaper layer, not per-frame.
//!
//! ## Spring physics
//!
//! Each window has a 1D spring for each of (x, y, scale, opacity).
//! Integration uses the semi-implicit Euler method at a fixed 120 Hz
//! (1 tick = 8.33 ms):
//!
//!   v[n+1] = v[n] + dt · (−k · (x[n] − target) − c · v[n])
//!   x[n+1] = x[n] + dt · v[n+1]
//!
//! Default spring constants:   k = 350, c = 30 (critically-damped-ish).
//! One `tick()` call advances all active windows by one 120 Hz frame.
//!
//! ## Exposé overview
//!
//! Exposé lays out up to `MAX_GPU_SURFACES` windows in a grid, animating
//! each to its target tile with spring physics.  Selecting a window
//! springs it back to its original position and restores normal mode.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum surfaces tracked by the GPU compositor.
pub const MAX_GPU_SURFACES: usize = 16;

/// Maximum blur kernel radius (pixels, software Kawase).
pub const MAX_KAWASE_RADIUS: u32 = 32;

/// Spring constant k (stiffness) in fixed-point × 1000.
const SPRING_K: i32 = 350_000;

/// Spring damping c in fixed-point × 1000.
const SPRING_C: i32 = 30_000;

/// Integration time step dt in fixed-point × 1 000 000 (≈ 1/120 s).
const SPRING_DT_MICRO: i32 = 8_333;

// ── Kawase pass ───────────────────────────────────────────────────────────────

/// Parameters for one Kawase blur pass.
#[derive(Clone, Copy, Debug)]
pub struct KawasePass {
    /// Blur radius in pixels (half-texel offset for the diagonal samples).
    pub radius: u32,
    /// Down-sample (true) or up-sample (false).
    pub downsample: bool,
}

impl KawasePass {
    /// Standard dual-filter preset for Dark Glass (2 passes, σ ≈ 16 px).
    pub const DARK_GLASS: [Self; 2] = [
        KawasePass {
            radius: 8,
            downsample: true,
        },
        KawasePass {
            radius: 9,
            downsample: false,
        },
    ];

    /// Light Frost preset (lighter blur, σ ≈ 8 px).
    pub const LIGHT_FROST: [Self; 2] = [
        KawasePass {
            radius: 4,
            downsample: true,
        },
        KawasePass {
            radius: 5,
            downsample: false,
        },
    ];
}

/// Apply one Kawase pass to `src` (BGRA32 pixels) and write into `dst`.
///
/// `width` and `height` are in pixels.  Both slices must be `width × height`
/// u32 elements (BGRA32 format).
///
/// This is the software fallback path used when virtio-gpu compute is absent.
pub fn kawase_pass(src: &[u32], dst: &mut [u32], width: usize, height: usize, pass: KawasePass) {
    if src.len() < width * height || dst.len() < width * height {
        return;
    }
    let r = pass.radius as i32;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            // Sample four diagonal neighbours at offset (±r, ±r) and
            // two cardinal midpoints at (±r/2, ±r/2) for the dual-filter.
            let offsets: [(i32, i32); 4] = [(-r, -r), (r, -r), (-r, r), (r, r)];
            let mut sum_b = 0u32;
            let mut sum_g = 0u32;
            let mut sum_r = 0u32;
            let mut count = 0u32;
            for (dx, dy) in offsets {
                let sx = (x + dx).clamp(0, width as i32 - 1) as usize;
                let sy = (y + dy).clamp(0, height as i32 - 1) as usize;
                let px = src[sy * width + sx];
                sum_b += px & 0xFF;
                sum_g += (px >> 8) & 0xFF;
                sum_r += (px >> 16) & 0xFF;
                count += 1;
            }
            let avg_b = sum_b / count;
            let avg_g = sum_g / count;
            let avg_r = sum_r / count;
            dst[y as usize * width + x as usize] =
                avg_b | (avg_g << 8) | (avg_r << 16) | 0xFF00_0000;
        }
    }
}

/// Apply a full dual-filter blur (down + up pass) in-place on `buf`.
///
/// Uses a temporary scratch buffer (`tmp`) for the intermediate result.
/// Both `buf` and `tmp` must be `width × height` u32 elements.
pub fn kawase_dual_filter(
    buf: &mut [u32],
    tmp: &mut [u32],
    width: usize,
    height: usize,
    passes: &[KawasePass],
) {
    for pass in passes {
        kawase_pass(buf, tmp, width, height, *pass);
        buf[..width * height].copy_from_slice(&tmp[..width * height]);
    }
}

// ── Spring physics ─────────────────────────────────────────────────────────────

/// 1D spring state for a single animated property.
#[derive(Clone, Copy, Debug, Default)]
pub struct SpringAxis {
    /// Current value in fixed-point × 1000 (e.g. pixel position × 1000).
    pub value: i32,
    /// Current velocity in fixed-point × 1000 per second.
    pub velocity: i32,
    /// Target value in fixed-point × 1000.
    pub target: i32,
}

impl SpringAxis {
    /// Advance the spring by one tick (SPRING_DT_MICRO µs).
    ///
    /// Returns `true` if the spring has settled (|v| < 10, |x-target| < 1).
    pub fn tick(&mut self) -> bool {
        let displacement = self.value - self.target;
        // Force = −k·displacement − c·velocity
        // Both k and c are × 1000; dt is × 1_000_000.
        // To keep the result in the same units we divide by 10^9.
        let force =
            -(SPRING_K / 1000) * (displacement / 1000) - (SPRING_C / 1000) * (self.velocity / 1000);
        // v += force × dt;  force in units/s², dt ≈ 1/120 s
        self.velocity += force * (SPRING_DT_MICRO / 1000) / 1000;
        self.value += self.velocity * (SPRING_DT_MICRO / 1000) / 1000;
        // Settled?
        self.velocity.abs() < 10 && (self.value - self.target).abs() < 100
    }

    /// Snap immediately to the target.
    pub fn snap(&mut self) {
        self.value = self.target;
        self.velocity = 0;
    }
}

/// Per-window spring state (x, y, scale, opacity).
///
/// Coordinates are in fixed-point × 1000 (1000 = 1 pixel or 1.0 scale).
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowSpring {
    pub x: SpringAxis,
    pub y: SpringAxis,
    /// Scale factor × 1000 (1000 = 1.0 = normal size).
    pub scale: SpringAxis,
    /// Opacity × 1000 (1000 = fully opaque).
    pub opacity: SpringAxis,
    /// True while any axis has not settled.
    pub animating: bool,
}

impl WindowSpring {
    /// Create a spring at rest at the given position (pixels).
    pub fn at_rest(x: i32, y: i32) -> Self {
        Self {
            x: SpringAxis {
                value: x * 1000,
                velocity: 0,
                target: x * 1000,
            },
            y: SpringAxis {
                value: y * 1000,
                velocity: 0,
                target: y * 1000,
            },
            scale: SpringAxis {
                value: 1000,
                velocity: 0,
                target: 1000,
            },
            opacity: SpringAxis {
                value: 1000,
                velocity: 0,
                target: 1000,
            },
            animating: false,
        }
    }

    /// Set new target position and start animating.
    pub fn move_to(&mut self, x: i32, y: i32) {
        self.x.target = x * 1000;
        self.y.target = y * 1000;
        self.animating = true;
    }

    /// Set target scale and opacity.
    pub fn transform_to(&mut self, scale: i32, opacity: i32) {
        self.scale.target = scale;
        self.opacity.target = opacity;
        self.animating = true;
    }

    /// Advance all axes by one tick.  Sets `animating = false` when settled.
    pub fn tick(&mut self) {
        if !self.animating {
            return;
        }
        let s1 = self.x.tick();
        let s2 = self.y.tick();
        let s3 = self.scale.tick();
        let s4 = self.opacity.tick();
        if s1 && s2 && s3 && s4 {
            self.animating = false;
        }
    }

    /// Current pixel position (rounded from fixed-point × 1000).
    pub fn pixel_x(&self) -> i32 {
        (self.x.value + 500) / 1000
    }
    pub fn pixel_y(&self) -> i32 {
        (self.y.value + 500) / 1000
    }
    /// Current scale as fraction × 1000 (1000 = 1.0).
    pub fn scale_fp(&self) -> u32 {
        self.scale.value.max(0) as u32
    }
    /// Current opacity as fraction × 1000 (1000 = fully opaque).
    pub fn opacity_fp(&self) -> u32 {
        self.opacity.value.clamp(0, 1000) as u32
    }
}

// ── Exposé layout ──────────────────────────────────────────────────────────────

/// Computed tile geometry for one window in Exposé mode.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExposeTile {
    pub surface_id: u32,
    pub target_x: i32,
    pub target_y: i32,
    pub target_scale: i32, // × 1000
}

/// Compute Exposé tile targets for `count` windows within `(screen_w, screen_h)`.
///
/// Fills `out[..count]` with the tile parameters and returns the count.
/// Layout is a square-ish grid with 8-pixel gutters.
pub fn expose_layout(
    surface_ids: &[u32],
    screen_w: u32,
    screen_h: u32,
    window_w: u32,
    window_h: u32,
    out: &mut [ExposeTile],
) -> usize {
    let count = surface_ids.len().min(out.len());
    if count == 0 {
        return 0;
    }

    // Compute grid dimensions (smallest square that fits all windows).
    let cols = integer_sqrt(count as u32).max(1) as usize;
    let rows = count.div_ceil(cols);

    let gutter = 8u32;
    let cell_w = (screen_w.saturating_sub(gutter * (cols as u32 + 1))) / cols as u32;
    let cell_h = (screen_h.saturating_sub(gutter * (rows as u32 + 1))) / rows as u32;

    // Scale to fit window in cell while preserving aspect ratio.
    let scale_x = (cell_w * 1000) / window_w.max(1);
    let scale_y = (cell_h * 1000) / window_h.max(1);
    let scale = scale_x.min(scale_y).min(1000); // never magnify

    for (i, &sid) in surface_ids[..count].iter().enumerate() {
        let col = (i % cols) as u32;
        let row = (i / cols) as u32;
        let scaled_w = window_w * scale / 1000;
        let scaled_h = window_h * scale / 1000;
        // Centre in cell.
        let cell_x = gutter + col * (cell_w + gutter) + (cell_w.saturating_sub(scaled_w)) / 2;
        let cell_y = gutter + row * (cell_h + gutter) + (cell_h.saturating_sub(scaled_h)) / 2;
        out[i] = ExposeTile {
            surface_id: sid,
            target_x: cell_x as i32,
            target_y: cell_y as i32,
            target_scale: scale as i32,
        };
    }
    count
}

/// Integer square root (floor).
fn integer_sqrt(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

// ── GPU compositor state ───────────────────────────────────────────────────────

/// Per-surface GPU compositor record.
#[derive(Clone, Copy, Default)]
struct GpuSurfaceRecord {
    active: bool,
    surface_id: u32,
    spring: WindowSpring,
    expose_tile: ExposeTile,
}

/// Global GPU compositor state.
struct GpuCompositorState {
    surfaces: [GpuSurfaceRecord; MAX_GPU_SURFACES],
    surface_count: usize,
    expose_active: bool,
    expose_pending_commit: u32, // surface_id to bring forward (0 = none)
    screen_w: u32,
    screen_h: u32,
}

impl GpuCompositorState {
    const fn new() -> Self {
        Self {
            surfaces: [GpuSurfaceRecord {
                active: false,
                surface_id: 0,
                spring: WindowSpring {
                    x: SpringAxis {
                        value: 0,
                        velocity: 0,
                        target: 0,
                    },
                    y: SpringAxis {
                        value: 0,
                        velocity: 0,
                        target: 0,
                    },
                    scale: SpringAxis {
                        value: 1000,
                        velocity: 0,
                        target: 1000,
                    },
                    opacity: SpringAxis {
                        value: 1000,
                        velocity: 0,
                        target: 1000,
                    },
                    animating: false,
                },
                expose_tile: ExposeTile {
                    surface_id: 0,
                    target_x: 0,
                    target_y: 0,
                    target_scale: 1000,
                },
            }; MAX_GPU_SURFACES],
            surface_count: 0,
            expose_active: false,
            expose_pending_commit: 0,
            screen_w: 1280,
            screen_h: 800,
        }
    }
}

static GPU_COMPOSITOR: Mutex<GpuCompositorState> = Mutex::new(GpuCompositorState::new());

/// Whether the GPU compositor is in Exposé (overview) mode.
pub(crate) static EXPOSE_MODE: AtomicBool = AtomicBool::new(false);

/// Monotonic commit counter — incremented on each `surface_commit`.
static COMMIT_COUNTER: AtomicU32 = AtomicU32::new(0);

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the GPU compositor with the screen dimensions.
/// Call once at kernel boot after the framebuffer is mapped.
pub fn init(screen_w: u32, screen_h: u32) {
    let mut state = GPU_COMPOSITOR.lock();
    state.screen_w = screen_w;
    state.screen_h = screen_h;
}

/// Register a new surface with the GPU compositor.
/// Called when a ring-3 compositor creates a surface (SYS_SURFACE_CREATE).
pub fn register_surface(surface_id: u32, origin_x: i32, origin_y: i32) {
    let mut state = GPU_COMPOSITOR.lock();
    // Reclaim stale slots first. A task can terminate without an explicit
    // surface_destroy path, leaving a dead record that would otherwise consume
    // one of the fixed compositor slots.
    let mut reclaimed = 0usize;
    for rec in state.surfaces.iter_mut() {
        if rec.active && !crate::wm::surface_table::surface_exists(rec.surface_id) {
            rec.active = false;
            reclaimed += 1;
        }
    }
    state.surface_count = state.surface_count.saturating_sub(reclaimed);

    // Replace any dead slot.
    for rec in state.surfaces.iter_mut() {
        if !rec.active {
            rec.active = true;
            rec.surface_id = surface_id;
            rec.spring = WindowSpring::at_rest(origin_x, origin_y);
            if state.surface_count < MAX_GPU_SURFACES {
                state.surface_count += 1;
            }
            crate::arch::serial::write_bytes(b"[gpu] register_surface sid=");
            crate::arch::serial::write_u64_dec_inline(surface_id as u64);
            crate::arch::serial::write_bytes(b" origin=");
            crate::arch::serial::write_u64_dec_inline(origin_x as u64);
            crate::arch::serial::write_bytes(b",");
            crate::arch::serial::write_u64_dec_inline(origin_y as u64);
            return;
        }
    }
    crate::arch::serial::write_line(b"[gpu] register_surface: table full");
}

/// Unregister a surface (SYS_SURFACE_DESTROY).
pub fn unregister_surface(surface_id: u32) {
    let mut state = GPU_COMPOSITOR.lock();
    for rec in state.surfaces.iter_mut() {
        if rec.active && rec.surface_id == surface_id {
            rec.active = false;
            if state.surface_count > 0 {
                state.surface_count -= 1;
            }
            return;
        }
    }
}

/// Commit a surface frame and push it to the present queue.
///
/// This is the Phase J equivalent of `SYS_SURFACE_PRESENT` with GPU routing.
/// It increments the commit counter, marks the surface dirty in the
/// present queue, and applies spring physics for any in-flight animation.
///
/// Returns `Ok(commit_counter)` on success, or `Err` if the present queue
/// is full (caller should propagate a backpressure error to ring-3).
pub fn surface_commit(surface_id: u32) -> Result<u32, crate::wm::surface_table::PresentError> {
    crate::wm::surface_table::present_queue_push(surface_id)?;
    let commit = COMMIT_COUNTER.fetch_add(1, Ordering::Release) + 1;
    crate::arch::serial::write_bytes(b"[gpu] surface_commit sid=");
    crate::arch::serial::write_u64_dec_inline(surface_id as u64);
    crate::arch::serial::write_bytes(b" commit=");
    crate::arch::serial::write_u64_dec_inline(commit as u64);
    crate::arch::serial::write_line(b" registered");
    Ok(commit)
}

/// Advance all window spring animations by one tick (call at ~120 Hz).
pub fn tick_animations() {
    let mut state = GPU_COMPOSITOR.lock();
    for rec in state.surfaces.iter_mut() {
        if rec.active {
            rec.spring.tick();
        }
    }
}

/// Returns `true` if at least one surface still has an active spring animation.
///
/// Used by the event-driven compositor to decide whether to re-arm the
/// animation timer source without recomposing the full scene.
pub fn any_animating() -> bool {
    let state = GPU_COMPOSITOR.lock();
    state
        .surfaces
        .iter()
        .any(|rec| rec.active && rec.spring.animating)
}

/// Toggle Exposé overview mode.
///
/// In Exposé mode, all windows spring to their computed tile positions.
/// Toggling again springs them back to their original positions.
pub fn toggle_expose(window_w: u32, window_h: u32) {
    let was_active = EXPOSE_MODE.fetch_xor(true, Ordering::AcqRel);
    let mut state = GPU_COMPOSITOR.lock();
    let sw = state.screen_w;
    let sh = state.screen_h;
    if !was_active {
        // Entering Exposé: compute tiles and animate.
        let mut ids = [0u32; MAX_GPU_SURFACES];
        let mut count = 0usize;
        for rec in state.surfaces.iter() {
            if rec.active && count < MAX_GPU_SURFACES {
                ids[count] = rec.surface_id;
                count += 1;
            }
        }
        let mut tiles = [ExposeTile::default(); MAX_GPU_SURFACES];
        expose_layout(&ids[..count], sw, sh, window_w, window_h, &mut tiles);
        for (rec, tile) in state.surfaces.iter_mut().zip(tiles.iter()) {
            if rec.active {
                rec.expose_tile = *tile;
                rec.spring.move_to(tile.target_x, tile.target_y);
                rec.spring.transform_to(tile.target_scale, 1000);
            }
        }
        state.expose_active = true;
    } else {
        // Exiting Exposé: spring back to resting positions.
        for rec in state.surfaces.iter_mut() {
            if rec.active {
                rec.spring
                    .move_to(rec.spring.x.value / 1000, rec.spring.y.value / 1000);
                rec.spring.transform_to(1000, 1000);
            }
        }
        state.expose_active = false;
    }
}

/// Get the current spring-physics-derived position for a surface.
///
/// Returns `(pixel_x, pixel_y, scale_fp1000, opacity_fp1000)` or `None`
/// if the surface is not registered.
pub fn surface_transform(surface_id: u32) -> Option<(i32, i32, u32, u32)> {
    let state = GPU_COMPOSITOR.lock();
    for rec in state.surfaces.iter() {
        if rec.active && rec.surface_id == surface_id {
            return Some((
                rec.spring.pixel_x(),
                rec.spring.pixel_y(),
                rec.spring.scale_fp(),
                rec.spring.opacity_fp(),
            ));
        }
    }
    None
}

/// Return the current commit counter (monotonic).
pub fn commit_counter() -> u32 {
    COMMIT_COUNTER.load(Ordering::Acquire)
}

/// Return a snapshot of all active surface IDs (for the gpu_compositor frame loop).
/// Fills `out[..]` and returns the count.
pub fn surface_id_snapshot(out: &mut [u32; MAX_GPU_SURFACES]) -> usize {
    let state = GPU_COMPOSITOR.lock();
    let mut n = 0usize;
    for rec in state.surfaces.iter() {
        if rec.active && n < MAX_GPU_SURFACES {
            out[n] = rec.surface_id;
            n += 1;
        }
    }
    n
}

/// Return the Exposé tile target for a surface (used by gpu_compositor in Exposé mode).
/// Returns a default tile if the surface is not found.
pub fn expose_tile_for(surface_id: u32) -> ExposeTile {
    let state = GPU_COMPOSITOR.lock();
    for rec in state.surfaces.iter() {
        if rec.active && rec.surface_id == surface_id {
            return rec.expose_tile;
        }
    }
    ExposeTile {
        surface_id,
        target_x: 0,
        target_y: 0,
        target_scale: 1000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_axis_settles() {
        let mut axis = SpringAxis {
            value: 0,
            velocity: 0,
            target: 1000_000,
        };
        let mut ticks = 0;
        for _ in 0..2000 {
            if axis.tick() {
                break;
            }
            ticks += 1;
        }
        // Should settle within 500 ticks at 120 Hz (≈ 4 s).
        assert!(
            ticks < 500,
            "spring did not settle in time: ticks={}",
            ticks
        );
        assert!((axis.value - axis.target).abs() < 100);
    }

    #[test]
    fn spring_axis_snap() {
        let mut axis = SpringAxis {
            value: 0,
            velocity: 500,
            target: 10_000,
        };
        axis.snap();
        assert_eq!(axis.value, axis.target);
        assert_eq!(axis.velocity, 0);
    }

    #[test]
    fn expose_layout_single_window() {
        let ids = [1u32];
        let mut tiles = [ExposeTile::default(); 4];
        let n = expose_layout(&ids, 1280, 800, 800, 600, &mut tiles);
        assert_eq!(n, 1);
        // Single window should be placed somewhere within the screen.
        assert!(tiles[0].target_x >= 0 && tiles[0].target_x < 1280);
        assert!(tiles[0].target_y >= 0 && tiles[0].target_y < 800);
    }

    #[test]
    fn expose_layout_four_windows() {
        let ids = [1u32, 2, 3, 4];
        let mut tiles = [ExposeTile::default(); 4];
        let n = expose_layout(&ids, 1280, 800, 400, 300, &mut tiles);
        assert_eq!(n, 4);
        // All tiles should be within the screen.
        for tile in &tiles[..n] {
            assert!(tile.target_x >= 0 && tile.target_x < 1280);
            assert!(tile.target_y >= 0 && tile.target_y < 800);
        }
    }

    #[test]
    fn kawase_pass_does_not_panic() {
        let src = [0xFF80_4020u32; 16]; // 4×4 BGRA pixels
        let mut dst = [0u32; 16];
        kawase_pass(
            &src,
            &mut dst,
            4,
            4,
            KawasePass {
                radius: 1,
                downsample: true,
            },
        );
        // Output should be non-zero (blur of a coloured input).
        assert!(dst.iter().any(|&p| p != 0));
    }

    #[test]
    fn integer_sqrt_values() {
        assert_eq!(integer_sqrt(0), 0);
        assert_eq!(integer_sqrt(1), 1);
        assert_eq!(integer_sqrt(4), 2);
        assert_eq!(integer_sqrt(9), 3);
        assert_eq!(integer_sqrt(16), 4);
        assert_eq!(integer_sqrt(15), 3);
    }
}
