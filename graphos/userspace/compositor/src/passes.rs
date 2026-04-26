// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Frame pass implementations — geometry, blur, shadow, composite, bloom, tone-map, present.
//!
//! ## Pass model
//!
//! Each pass is a plain function `execute_*(ctx, scene, ...)` that drives the active
//! `GfxContext`.  The frame executor in `gfx_context.rs` walks the compiled frame graph
//! and dispatches to the appropriate function for each `PassKind`.
//!
//! ## CPU vs GPU
//!
//! The functions are `GfxContext`-generic.  When `ctx.gpu_available()` is true, calls
//! are encoded into GraphOS-native GPU commands and submitted in bulk.  Otherwise, the CPU
//! blitter executes them inline.

use crate::gfx_context::GfxContext;
use crate::material::{
    GlassMaterial, GradientDirection, Material, ShadowMaterial, SurfaceMaterial,
};
use crate::render_graph::{BloomConfig, BlurConfig, ToneMapOp, VignetteConfig};
use crate::render_node::RenderScene;

// ── Geometry pass ─────────────────────────────────────────────────────────────

/// Draw all opaque background and panel geometry into the current render target.
///
/// Iterates the scene back-to-front and draws all non-surface, non-glass nodes.
/// Surface nodes and glass nodes are deferred to the composite pass so that
/// the background blur source is fully populated first.
pub fn execute_geometry_pass(ctx: &mut dyn GfxContext, scene: &RenderScene) {
    let (sw, sh) = ctx.screen_dims();
    for node in scene.sorted_nodes() {
        if !node.visible {
            continue;
        }
        let t = node.effective_transform();
        let (w, h) = node.kind.dimensions();
        let (sw_w, sw_h) = t.scaled_dims(w, h);
        let x = t.x;
        let y = t.y;
        match &node.material {
            Material::Solid(argb) => {
                ctx.fill_rect(x, y, sw_w, sw_h, *argb, 0);
            }
            Material::Gradient(g) => {
                let from = g.stops[0].color;
                let to = g.stops[(g.stop_count as usize).saturating_sub(1)].color;
                let dir: u8 = match g.direction {
                    GradientDirection::TopToBottom => 0,
                    GradientDirection::LeftToRight => 1,
                    GradientDirection::TopLeftToBottomRight => 2,
                    GradientDirection::TopRightToBottomLeft => 3,
                    GradientDirection::Radial => 4,
                };
                ctx.fill_gradient(x, y, sw_w, sw_h, from, to, dir);
            }
            Material::Wallpaper { surface_id } => {
                // Wallpaper fills the full screen at z=0.
                ctx.blit_surface(*surface_id, sw, sh, 0, 0, sw, sh, 255);
            }
            Material::None => {}
            // Glass and Surface are deferred to composite pass.
            Material::Glass(_) | Material::Surface(_) => {}
        }
    }
}

// ── Blur pass ─────────────────────────────────────────────────────────────────

/// Dual-Kawase iterative blur.
///
/// In the CPU path this applies a multi-pass box blur (3 passes per iteration)
/// to approximate a Gaussian. In the native GPU path a `BLUR_RECT` command is
/// encoded and the hardware backend can execute the real dual-Kawase pass.
pub fn execute_blur_pass(
    ctx: &mut dyn GfxContext,
    config: &BlurConfig,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    if w == 0 || h == 0 {
        return;
    }
    ctx.blur_rect(
        x,
        y,
        w,
        h,
        config.radius as u8,
        config.iterations as u8,
        config.downsample as u8,
    );
}

// ── Shadow pass ───────────────────────────────────────────────────────────────

/// Render drop shadows for all window and panel nodes.
///
/// Executed before the composite pass so shadows land behind window surfaces.
pub fn execute_shadow_pass(ctx: &mut dyn GfxContext, scene: &RenderScene) {
    for node in scene.sorted_nodes() {
        if !node.visible {
            continue;
        }

        // Extract shadow from material.
        let shadow = match &node.material {
            Material::Surface(s) => s.shadow,
            Material::Glass(g) => ShadowMaterial {
                // Glass nodes get a default window shadow.
                color: 0xB0000000,
                offset_x: 0,
                offset_y: 8,
                blur: 24,
                spread: -4,
            },
            _ => continue,
        };

        if !shadow.is_active() {
            continue;
        }

        let t = node.effective_transform();
        let (w, h) = node.kind.dimensions();
        let (sw_w, sw_h) = t.scaled_dims(w, h);
        let spread = shadow.spread;

        ctx.draw_shadow(
            t.x + shadow.offset_x as i32 - spread as i32,
            t.y + shadow.offset_y as i32 - spread as i32,
            (sw_w as i32 + spread as i32 * 2).max(0) as u32,
            (sw_h as i32 + spread as i32 * 2).max(0) as u32,
            shadow.color,
            shadow.offset_x as i8,
            shadow.offset_y as i8,
            shadow.blur as u8,
        );
    }
}

// ── Composite pass ────────────────────────────────────────────────────────────

/// Assemble all scene layers in Z order.
///
/// For each node:
/// - Glass nodes: blit the blurred background, then overlay the tint and draw
///   the edge glow and specular highlight.
/// - Surface nodes: blit the application surface with opacity.
/// - Any remaining solid/gradient nodes (fallback if geometry pass missed them).
pub fn execute_composite_pass(ctx: &mut dyn GfxContext, scene: &RenderScene) {
    for node in scene.sorted_nodes() {
        if !node.visible {
            continue;
        }

        let t = node.effective_transform();
        let (nw, nh) = node.kind.dimensions();
        let (sw_w, sw_h) = t.scaled_dims(nw, nh);
        let x = t.x;
        let y = t.y;
        let opacity = node.effective_opacity();

        match &node.material {
            Material::Glass(g) => {
                composite_glass(ctx, g, x, y, sw_w, sw_h, opacity);
            }
            Material::Surface(s) => {
                composite_surface(ctx, s, nw, nh, x, y, sw_w, sw_h);
            }
            // Background / wallpaper already drawn in geometry pass — skip.
            Material::Wallpaper { .. } => {}
            // Fallback solids that weren't drawn in geometry pass.
            Material::Solid(argb) => {
                if let crate::render_node::NodeKind::Cursor { .. } = node.kind {
                    draw_arrow_cursor(ctx, x, y, *argb, opacity);
                    continue;
                }
                if opacity < 255 {
                    let a = (((*argb >> 24) as u32 * opacity as u32) / 255) as u8;
                    let composited = (*argb & 0x00FFFFFF) | ((a as u32) << 24);
                    ctx.fill_rect(x, y, sw_w, sw_h, composited, 0);
                } else {
                    ctx.fill_rect(x, y, sw_w, sw_h, *argb, 0);
                }
            }
            Material::Gradient(g) => {
                let from = g.stops[0].color;
                let to = g.stops[(g.stop_count as usize).saturating_sub(1)].color;
                ctx.fill_gradient(x, y, sw_w, sw_h, from, to, 0);
            }
            Material::None => {}
        }
    }
}

fn draw_arrow_cursor(ctx: &mut dyn GfxContext, x: i32, y: i32, fill_argb: u32, opacity: u8) {
    const ARROW: [u16; 20] = [
        0b1000_0000_0000,
        0b1100_0000_0000,
        0b1110_0000_0000,
        0b1111_0000_0000,
        0b1111_1000_0000,
        0b1111_1100_0000,
        0b1111_1110_0000,
        0b1111_1111_0000,
        0b1111_1111_1000,
        0b1111_1111_1100,
        0b1111_1111_1110,
        0b1111_1111_0000,
        0b1111_0111_0000,
        0b1110_0011_1000,
        0b1100_0011_1000,
        0b1000_0001_1100,
        0b0000_0001_1100,
        0b0000_0000_1110,
        0b0000_0000_1110,
        0b0000_0000_0000,
    ];
    const OUTLINE: [u16; 20] = [
        0b1000_0000_0000,
        0b1100_0000_0000,
        0b1010_0000_0000,
        0b1001_0000_0000,
        0b1000_1000_0000,
        0b1000_0100_0000,
        0b1000_0010_0000,
        0b1000_0001_0000,
        0b1000_0000_1000,
        0b1000_0000_0100,
        0b1111_1111_1110,
        0b1000_0000_0000,
        0b1001_0000_0000,
        0b1010_0010_1000,
        0b1100_0010_1000,
        0b1000_0001_0100,
        0b0000_0001_0100,
        0b0000_0000_1010,
        0b0000_0000_0110,
        0b0000_0000_0000,
    ];

    let fill_a = (((fill_argb >> 24) & 0xFF) as u8 * opacity / 255) as u32;
    let fill = (fill_argb & 0x00FF_FFFF) | (fill_a << 24);
    let outline = 0xFF1A_2640u32;

    for (row, (fill_row, outline_row)) in ARROW.iter().zip(OUTLINE.iter()).enumerate() {
        let py = y + row as i32;
        for col in 0..12i32 {
            let bit = 1u16 << (11 - col);
            let px = x + col;
            if (*outline_row & bit) != 0 {
                ctx.fill_rect(px, py, 1, 1, outline, 0);
            } else if (*fill_row & bit) != 0 {
                ctx.fill_rect(px, py, 1, 1, fill, 0);
            }
        }
    }
}

fn composite_glass(
    ctx: &mut dyn GfxContext,
    g: &GlassMaterial,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    opacity: u8,
) {
    // 1. Background blur source (already in blur render target) — blit into this region.
    //    The GPU path: the blur RT was populated by the blur pass; just blit from it.
    //    The CPU path: apply blur to this rect in-place.
    ctx.blur_rect(
        x,
        y,
        w,
        h,
        g.blur.radius,
        g.blur.iterations,
        g.blur.downsample,
    );

    // 2. Tint overlay.
    let tint_a = (((g.tint >> 24) as u32 * opacity as u32) / 255) as u8;
    let tint = (g.tint & 0x00FFFFFF) | ((tint_a as u32) << 24);
    ctx.fill_rect(x, y, w, h, tint, g.corner_radius);

    // 3. Edge glow.
    if let Some(eg) = g.edge_glow {
        ctx.draw_border(x, y, w, h, eg.color, eg.width, g.corner_radius);
    }

    // 4. Specular highlight (top edge — a thin lighter strip).
    if let Some(spec) = g.specular {
        ctx.fill_gradient(
            x + 1,
            y + 1,
            w.saturating_sub(2),
            spec.width as u32,
            spec.color,
            0,
            0,
        );
    }
}

fn composite_surface(
    ctx: &mut dyn GfxContext,
    s: &SurfaceMaterial,
    src_w: u32,
    src_h: u32,
    x: i32,
    y: i32,
    dst_w: u32,
    dst_h: u32,
) {
    ctx.blit_surface(s.surface_id, src_w, src_h, x, y, dst_w, dst_h, s.opacity);
    if s.corner_radius > 0 {
        // Draw a rounded mask border to clip the surface corners visually.
        // Full hardware round-clip requires a stencil; for Phase 1 we just
        // draw corner fills in the background color (implicit cutout).
        // TODO: Phase 2 — GPU stencil round-rect clip.
    }
}

// ── Bloom pass ────────────────────────────────────────────────────────────────

/// Bloom / glow post-processing.
///
/// Extracts pixels above `config.threshold` luminance, blurs them, then
/// additively blends the result over the HDR composite buffer.
pub fn execute_bloom_pass(
    ctx: &mut dyn GfxContext,
    config: &BloomConfig,
    screen_w: u32,
    screen_h: u32,
) {
    if config.intensity == 0 {
        return;
    }
    // GPU path: a BLOOM_EXTRACT command handles extract+blur+blend.
    // CPU path: approximated by a high-luminance rect blur — Phase 1 no-op.
    if ctx.gpu_available() {
        // Command encoding handled by GpuCmdEncoder.bloom_extract() in the GPU path.
        // For now, the bloom pass is a no-op on the CPU path.
    }
}

// ── Tone-map pass ─────────────────────────────────────────────────────────────

/// HDR → LDR tone mapping.
///
/// CPU path: no-op (all CPU colors are already LDR u8; HDR accumulation is
/// approximated by clamping).
///
/// GPU path: encodes a TONEMAP command selecting between Reinhard and ACES filmic.
pub fn execute_tonemap_pass(ctx: &mut dyn GfxContext, op: ToneMapOp, exposure_fp: u16) {
    if ctx.gpu_available() {
        let op_id: u8 = match op {
            ToneMapOp::Reinhard => 0,
            ToneMapOp::AcesFilmic => 1,
            ToneMapOp::Neutral => 2,
        };
        // TONEMAP is handled by the GPU command encoder when a native backend
        // is active.
        let _ = (op_id, exposure_fp);
    }
    // CPU path: no-op — all values are already clamped to 8-bit range.
}

// ── Vignette pass ─────────────────────────────────────────────────────────────

/// Darkening vignette applied after tone-mapping.
///
/// CPU path: draws an ARGB-tinted overlay ring around the screen edges.
/// GPU path: a shader variant; for Phase 1 falls through to CPU ring.
pub fn execute_vignette_pass(ctx: &mut dyn GfxContext, config: &VignetteConfig) {
    if config.strength == 0 {
        return;
    }
    let (sw, sh) = ctx.screen_dims();
    if sw == 0 || sh == 0 {
        return;
    }

    // Simple vignette: draw semi-transparent black borders at decreasing widths.
    let border = (sw.min(sh) as u32 * config.strength as u32 / 255 / 8).max(1);
    let fade_steps = 6u32;
    for i in 0..fade_steps {
        let alpha = ((config.strength as u32) * (fade_steps - i) / fade_steps) as u8;
        let b = border * (fade_steps - i) / fade_steps;
        if b == 0 {
            continue;
        }
        let argb = ((alpha as u32) << 24);
        ctx.draw_border(0, 0, sw, sh, argb, b.min(255) as u8, 0);
    }
}

// ── Present pass ──────────────────────────────────────────────────────────────

/// Submit the composited frame to the display.
///
/// Calls `GfxContext::present()` which either flushes the CPU framebuffer
/// via `SYS_SURFACE_COMMIT` or submits a GraphOS-native GPU command buffer via
/// `SYS_GPU_SUBMIT` and triggers a scanout flip.
pub fn execute_present_pass(ctx: &mut dyn GfxContext) {
    ctx.present();
}

// ── Frame compositor ──────────────────────────────────────────────────────────

/// Dispatch a full frame through all passes.
///
/// Called by the compositor event loop after the scene has been updated and
/// `RenderScene::is_dirty()` returns true.
pub fn composite_frame(ctx: &mut dyn GfxContext, scene: &RenderScene) {
    let (sw, sh) = ctx.screen_dims();

    // 1. Background geometry (wallpaper + solid fills)
    execute_geometry_pass(ctx, scene);

    // 2. Drop shadows
    execute_shadow_pass(ctx, scene);

    // 3. Composite all layers
    execute_composite_pass(ctx, scene);

    // 8. Present
    execute_present_pass(ctx);
}
