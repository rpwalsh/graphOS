// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Desktop scene generator that exercises compositor, UI cards, text, and graph overlays.

use crate::compositor::{
    DesktopCompositor, OffscreenWindowRenderer, WindowSurface, WindowSurfaceTarget,
};
use crate::gl::Context;
use crate::math::{Vec2, Vec4};
use crate::pipeline::Target;
use crate::text::{
    GlyphAtlas, TextAlign, TextStyle, append_text, append_text_aligned, append_text_lines,
};
use crate::texture::Texture;
use crate::ui::{Rect, UiBatch, UiRenderer};
use alloc::vec::Vec;

pub struct DemoWindowScene {
    pub texture: u32,
    pub rect: Rect,
    pub batch: UiBatch,
}

pub struct DesktopDemoScene {
    pub compositor: DesktopCompositor,
    pub overlay: UiBatch,
    pub atlas: GlyphAtlas,
    pub windows: Vec<DemoWindowScene>,
}

/// Render a complete desktop demo into `target`.
///
/// This exercises the real offscreen path:
/// 1) each window is rendered into its own texture-backed framebuffer surface
/// 2) surfaces are composited back onto the final screen target with alpha
pub fn render_desktop_demo(ctx: &mut Context, target: &mut Target<'_>, atlas_texture: u32) -> bool {
    let screen_w = target.width;
    let screen_h = target.height;
    if screen_w == 0 || screen_h == 0 {
        return false;
    }

    let descriptors = [
        (screen_w as f32 * 0.44, screen_h as f32 * 0.42),
        (screen_w as f32 * 0.49, screen_h as f32 * 0.40),
        (screen_w as f32 * 0.42, screen_h as f32 * 0.42),
        (screen_w as f32 * 0.42, screen_h as f32 * 0.40),
    ];

    let mut surfaces: Vec<WindowSurfaceTarget> = Vec::with_capacity(descriptors.len());
    for &(w, h) in &descriptors {
        let sw = w.max(1.0) as u32;
        let sh = h.max(1.0) as u32;
        let Some(surface) = WindowSurfaceTarget::create(ctx, sw, sh) else {
            for s in surfaces.drain(..) {
                s.destroy(ctx);
            }
            return false;
        };
        surfaces.push(surface);
    }

    let mut texture_ids = [0u32; 4];
    for (i, s) in surfaces.iter().enumerate() {
        texture_ids[i] = s.color_texture;
    }

    let scene = build_demo_scene(
        screen_w as f32,
        screen_h as f32,
        &texture_ids,
        atlas_texture,
    );

    let mut offscreen = OffscreenWindowRenderer::new();
    for (window, surface) in scene.windows.iter().zip(surfaces.iter()) {
        let snapshots = ctx.texture_snapshot_table();
        let mut views: Vec<Option<Texture<'_>>> = Vec::with_capacity(snapshots.len());
        for item in &snapshots {
            views.push(item.as_ref().map(|s| s.as_texture()));
        }
        if !offscreen.render_batch(ctx, surface, &window.batch, &views) {
            for s in surfaces.drain(..) {
                s.destroy(ctx);
            }
            return false;
        }
    }

    let mut final_batch = UiBatch::with_capacity(16384, 24576);
    scene
        .compositor
        .composite_into(&mut final_batch, screen_w as f32, screen_h as f32, 0.0);
    final_batch.append(&scene.overlay);

    let final_snapshots = ctx.texture_snapshot_table();
    let mut final_views: Vec<Option<Texture<'_>>> = Vec::with_capacity(final_snapshots.len());
    for item in &final_snapshots {
        final_views.push(item.as_ref().map(|s| s.as_texture()));
    }
    let renderer = UiRenderer::new(screen_w as f32, screen_h as f32, &final_views);
    renderer.render(target, &final_batch);

    for s in surfaces {
        s.destroy(ctx);
    }
    true
}

/// Build a desktop-like scene description with overlapping windows and widgets.
///
/// Texture names are expected to be offscreen window surfaces already rendered
/// into texture objects by the caller.
pub fn build_demo_scene(
    screen_w: f32,
    screen_h: f32,
    window_textures: &[u32],
    atlas_texture: u32,
) -> DesktopDemoScene {
    let mut compositor = DesktopCompositor::new();
    let mut overlay = UiBatch::with_capacity(8192, 12288);
    let atlas = GlyphAtlas::ascii_8x8();
    let mut windows = Vec::new();

    let fallback = 0;
    let t0 = *window_textures.first().unwrap_or(&fallback);
    let t1 = *window_textures.get(1).unwrap_or(&fallback);
    let t2 = *window_textures.get(2).unwrap_or(&fallback);
    let t3 = *window_textures.get(3).unwrap_or(&fallback);

    let terminal_rect = Rect::new(40.0, 36.0, screen_w * 0.44, screen_h * 0.42);
    let ai_rect = Rect::new(screen_w * 0.46, 44.0, screen_w * 0.49, screen_h * 0.40);
    let store_rect = Rect::new(56.0, screen_h * 0.50, screen_w * 0.42, screen_h * 0.42);
    let graph_rect = Rect::new(
        screen_w * 0.52,
        screen_h * 0.50,
        screen_w * 0.42,
        screen_h * 0.40,
    );

    windows.push(DemoWindowScene {
        texture: t0,
        rect: terminal_rect,
        batch: build_terminal_window(&atlas, atlas_texture, terminal_rect.w, terminal_rect.h),
    });
    windows.push(DemoWindowScene {
        texture: t1,
        rect: ai_rect,
        batch: build_ai_window(&atlas, atlas_texture, ai_rect.w, ai_rect.h),
    });
    windows.push(DemoWindowScene {
        texture: t2,
        rect: store_rect,
        batch: build_store_window(&atlas, atlas_texture, store_rect.w, store_rect.h),
    });
    windows.push(DemoWindowScene {
        texture: t3,
        rect: graph_rect,
        batch: build_graph_window(&atlas, atlas_texture, graph_rect.w, graph_rect.h),
    });

    compositor.add_window(WindowSurface {
        texture: t0,
        rect: terminal_rect,
        z: 1,
        opacity: 0.96,
        clip: None,
    });
    compositor.add_window(WindowSurface {
        texture: t1,
        rect: ai_rect,
        z: 2,
        opacity: 0.95,
        clip: None,
    });
    compositor.add_window(WindowSurface {
        texture: t2,
        rect: store_rect,
        z: 3,
        opacity: 0.98,
        clip: None,
    });
    compositor.add_window(WindowSurface {
        texture: t3,
        rect: graph_rect,
        z: 4,
        opacity: 0.94,
        clip: None,
    });

    overlay.add_rounded_rect(
        Rect::new(18.0, screen_h - 72.0, screen_w - 36.0, 54.0),
        0.90,
        Vec4::new(0.03, 0.04, 0.08, 0.72),
        18.0,
        8,
    );
    overlay.add_rounded_border(
        Rect::new(18.0, screen_h - 72.0, screen_w - 36.0, 54.0),
        0.901,
        Vec4::new(0.18, 0.25, 0.42, 0.92),
        18.0,
        1.0,
        8,
    );

    let p = [
        Vec2::new(screen_w * 0.14, screen_h * 0.18),
        Vec2::new(screen_w * 0.28, screen_h * 0.11),
        Vec2::new(screen_w * 0.42, screen_h * 0.22),
        Vec2::new(screen_w * 0.74, screen_h * 0.18),
        Vec2::new(screen_w * 0.86, screen_h * 0.30),
    ];
    for i in 0..p.len() {
        for j in i + 1..p.len() {
            if (i + j) % 2 == 0 {
                overlay.add_graph_edge(
                    p[i],
                    p[j],
                    0.951,
                    1.4,
                    Vec4::new(0.33, 0.67, 0.96, 0.56),
                    Some(Vec4::new(0.12, 0.34, 0.70, 0.18)),
                );
            }
        }
    }
    for point in &p {
        overlay.add_graph_node(
            point.x,
            point.y,
            8.0,
            0.952,
            Vec4::new(0.65, 0.87, 1.0, 0.95),
        );
        overlay.add_graph_node(
            point.x,
            point.y,
            16.0,
            0.9515,
            Vec4::new(0.23, 0.53, 0.92, 0.15),
        );
    }

    let style = TextStyle {
        color: Vec4::new(0.93, 0.96, 1.0, 1.0),
        scale: 1.5,
        line_height: 10.0,
        letter_spacing: 0.0,
    };
    append_text_aligned(
        &mut overlay,
        &atlas,
        atlas_texture,
        "GraphOS Desktop Surface",
        Rect::new(32.0, 18.0, screen_w - 64.0, 22.0),
        0.96,
        style,
        TextAlign::Center,
        None,
    );
    append_text(
        &mut overlay,
        &atlas,
        atlas_texture,
        "Realtime graph/orchestrator view   secure shell   app store   AI substrate",
        32.0,
        screen_h - 55.0,
        0.96,
        TextStyle {
            scale: 1.0,
            ..style
        },
        None,
    );

    DesktopDemoScene {
        compositor,
        overlay,
        atlas,
        windows,
    }
}

fn build_terminal_window(atlas: &GlyphAtlas, atlas_texture: u32, w: f32, h: f32) -> UiBatch {
    let mut batch = UiBatch::with_capacity(2048, 3072);
    let bounds = Rect::new(0.0, 0.0, w, h);
    batch.add_terminal_panel(
        bounds,
        0.05,
        Vec4::new(0.02, 0.04, 0.07, 0.98),
        Vec4::new(0.18, 0.31, 0.46, 0.96),
        Vec4::new(0.06, 0.10, 0.16, 1.0),
    );
    let text_bounds = bounds.inset(18.0, 34.0, 18.0, 18.0);
    let style = TextStyle {
        color: Vec4::new(0.70, 0.95, 0.78, 1.0),
        scale: 1.0,
        line_height: 11.0,
        letter_spacing: 0.0,
    };
    append_text_lines(
        &mut batch,
        atlas,
        atlas_texture,
        &[
            "graphos@localhost:~$ launch shell3d --desktop",
            "[ok] orch.timer             16 ms cadence",
            "[ok] compositor            4 surfaces blended",
            "[ok] graph view            58 nodes / 142 edges",
            "[ok] ai substrate          3 agents warm",
            "[run] package sync         delta 14 bundles",
        ],
        text_bounds,
        0.06,
        style,
        Some(text_bounds),
    );
    batch
}

fn build_ai_window(atlas: &GlyphAtlas, atlas_texture: u32, w: f32, h: f32) -> UiBatch {
    let mut batch = UiBatch::with_capacity(2048, 3072);
    let bounds = Rect::new(0.0, 0.0, w, h);
    batch.add_rounded_rect(bounds, 0.05, Vec4::new(0.05, 0.07, 0.11, 0.98), 14.0, 8);
    batch.add_rounded_border(
        bounds,
        0.051,
        Vec4::new(0.22, 0.35, 0.50, 0.94),
        14.0,
        1.0,
        8,
    );
    let header = Rect::new(0.0, 0.0, w, 28.0);
    batch.add_gradient_rect(
        header,
        0.052,
        Vec4::new(0.09, 0.13, 0.21, 1.0),
        Vec4::new(0.12, 0.18, 0.28, 1.0),
        Vec4::new(0.06, 0.10, 0.17, 1.0),
        Vec4::new(0.08, 0.12, 0.20, 1.0),
    );
    let style = TextStyle {
        color: Vec4::new(0.93, 0.96, 1.0, 1.0),
        scale: 1.0,
        line_height: 10.0,
        letter_spacing: 0.0,
    };
    append_text(
        &mut batch,
        atlas,
        atlas_texture,
        "AI Dashboard",
        18.0,
        10.0,
        0.06,
        style,
        None,
    );
    let cards = [
        Rect::new(20.0, 46.0, w * 0.28, 72.0),
        Rect::new(24.0 + w * 0.30, 46.0, w * 0.28, 72.0),
        Rect::new(28.0 + w * 0.60, 46.0, w * 0.28, 72.0),
    ];
    for (i, card) in cards.iter().enumerate() {
        batch.add_package_card(
            *card,
            0.055,
            Vec4::new(0.08, 0.11, 0.18, 0.94),
            Vec4::new(0.21, 0.31, 0.48, 0.92),
            Vec4::new(0.35, 0.67, 0.96, 0.92),
            None,
        );
        let y = card.y + card.h - 24.0;
        let label = match i {
            0 => "Latency 14 ms",
            1 => "Index 0.82",
            _ => "Agents 03",
        };
        append_text(
            &mut batch,
            atlas,
            atlas_texture,
            label,
            card.x + 16.0,
            y,
            0.056,
            style,
            Some(*card),
        );
    }
    append_text_lines(
        &mut batch,
        atlas,
        atlas_texture,
        &[
            "Substrate: stable",
            "Planner: 2 tasks in-flight",
            "Vector cache: 18.2 MiB hot",
            "Policy drift: nominal",
        ],
        Rect::new(22.0, 142.0, w - 44.0, h - 160.0),
        0.056,
        TextStyle {
            color: Vec4::new(0.78, 0.85, 0.95, 1.0),
            ..style
        },
        None,
    );
    batch
}

fn build_store_window(atlas: &GlyphAtlas, atlas_texture: u32, w: f32, h: f32) -> UiBatch {
    let mut batch = UiBatch::with_capacity(2048, 3072);
    let bounds = Rect::new(0.0, 0.0, w, h);
    batch.add_rounded_rect(bounds, 0.05, Vec4::new(0.05, 0.07, 0.10, 0.98), 16.0, 8);
    batch.add_rounded_border(
        bounds,
        0.051,
        Vec4::new(0.18, 0.27, 0.41, 0.94),
        16.0,
        1.0,
        8,
    );
    append_text(
        &mut batch,
        atlas,
        atlas_texture,
        "GraphOS Store",
        18.0,
        12.0,
        0.06,
        TextStyle::default(),
        None,
    );
    let cards = [
        Rect::new(18.0, 42.0, w - 36.0, 92.0),
        Rect::new(18.0, 148.0, w - 36.0, 92.0),
        Rect::new(
            18.0,
            254.0_f32.min(h - 110.0),
            w - 36.0,
            92.0_f32.min(h - 272.0),
        ),
    ];
    for (i, card) in cards.iter().enumerate() {
        batch.add_package_card(
            *card,
            0.055,
            Vec4::new(0.08, 0.10, 0.16, 0.95),
            Vec4::new(0.19, 0.28, 0.43, 0.92),
            Vec4::new(0.28, 0.74, 0.96, 0.88),
            None,
        );
        let label = match i {
            0 => "Neural Navigator 2.1",
            1 => "Secure Fleet Console",
            _ => "Spatial Files Preview",
        };
        append_text(
            &mut batch,
            atlas,
            atlas_texture,
            label,
            card.x + 18.0,
            card.y + card.h - 26.0,
            0.056,
            TextStyle::default(),
            Some(*card),
        );
    }
    batch
}

fn build_graph_window(atlas: &GlyphAtlas, atlas_texture: u32, w: f32, h: f32) -> UiBatch {
    let mut batch = UiBatch::with_capacity(2048, 3072);
    let bounds = Rect::new(0.0, 0.0, w, h);
    batch.add_rounded_rect(bounds, 0.05, Vec4::new(0.04, 0.06, 0.11, 0.97), 16.0, 8);
    batch.add_rounded_border(
        bounds,
        0.051,
        Vec4::new(0.18, 0.30, 0.50, 0.92),
        16.0,
        1.0,
        8,
    );
    append_text(
        &mut batch,
        atlas,
        atlas_texture,
        "Graph Topology",
        18.0,
        12.0,
        0.06,
        TextStyle::default(),
        None,
    );
    let pts = [
        Vec2::new(64.0, 88.0),
        Vec2::new(w * 0.42, 104.0),
        Vec2::new(w * 0.72, 86.0),
        Vec2::new(w * 0.82, h * 0.54),
        Vec2::new(w * 0.32, h * 0.68),
    ];
    for i in 0..pts.len() {
        for j in i + 1..pts.len() {
            if (i + j) % 2 == 0 {
                batch.add_graph_edge(
                    pts[i],
                    pts[j],
                    0.055,
                    1.6,
                    Vec4::new(0.52, 0.86, 1.0, 0.82),
                    Some(Vec4::new(0.18, 0.48, 0.88, 0.18)),
                );
            }
        }
    }
    for point in pts {
        batch.add_graph_node(
            point.x,
            point.y,
            8.0,
            0.056,
            Vec4::new(0.78, 0.94, 1.0, 1.0),
        );
    }
    batch
}
