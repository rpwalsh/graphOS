// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Scene compositor — renders the `SceneGraph` using the active `GpuBackend`.
//!
//! ## Render loop
//!
//! ```text
//! desktop event loop
//!   └─ SceneCompositor::composite(damage)
//!        ├─ 1. Upload dirty surfaces (surface_table pages → GPU texture)
//!        ├─ 2. begin_frame(damage)
//!        ├─ 3. For each visible node (back-to-front, z_order sorted):
//!        │       ├─ draw_shadow  (if material.shadow.is_active())
//!        │       ├─ fill_rect    (NodeKind::Panel)
//!        │       ├─ draw_border  (if border.is_some())
//!        │       └─ blit_surface (NodeKind::Surface)
//!        ├─ 4. draw_cursor
//!        └─ 5. end_frame(damage)  → flush scanout / submit native GPU work
//! ```
//!
//! ## Surface texture cache
//!
//! Each `NodeKind::Surface` corresponds to a ring-3 surface. In a future native
//! GPU path, the surface's physical backing pages will be registered once as a
//! device resource and only dirty nodes will need re-upload or rebind work.
//!
//! In the virtio-2D path there is no explicit texture cache; the backend
//! reads directly from the surface's physical pages on every blit.
//!
//! ## Cursor
//!
//! The cursor position is tracked independently of the scene graph.  Cursor
//! motion posts a 24×24 damage rect around the old and new positions so the
//! compositor can re-composite just that region without a full scene pass.

#![allow(dead_code)]

use crate::wm::damage::DamageRect;
use crate::wm::gpu_backend::{ActiveBackend, GpuBackend};
use crate::wm::scene::{NodeId, NodeKind, tick_animations, with_scene};
use crate::wm::surface_table::{surface_dimensions, surface_exists};
use spin::Mutex;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Cursor sprite size.
const CURSOR_W: u32 = 12;
const CURSOR_H: u32 = 20;

// ── Compositor state ──────────────────────────────────────────────────────────

struct CompositorState {
    backend: ActiveBackend,
    screen_w: u32,
    screen_h: u32,
    cursor_x: i32,
    cursor_y: i32,
}

impl CompositorState {
    fn new(screen_w: u32, screen_h: u32) -> Self {
        Self {
            backend: ActiveBackend::init(screen_w, screen_h),
            screen_w,
            screen_h,
            cursor_x: (screen_w / 2) as i32,
            cursor_y: (screen_h / 2) as i32,
        }
    }
}

static COMPOSITOR: Mutex<Option<CompositorState>> = Mutex::new(None);

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the scene compositor at the given screen resolution.
///
/// Must be called once after the virtio-gpu driver is bound. Selects the best
/// GraphOS-supported backend for the current build.
pub fn init(screen_w: u32, screen_h: u32) {
    *COMPOSITOR.lock() = Some(CompositorState::new(screen_w, screen_h));
    crate::arch::serial::write_line(b"[scene-compositor] initialised\n");
}

/// Update the cursor position and post damage for the cursor region.
pub fn set_cursor(x: i32, y: i32) {
    let mut guard = COMPOSITOR.lock();
    if let Some(ref mut c) = *guard {
        // Post damage for old cursor position.
        let old = DamageRect::new(c.cursor_x, c.cursor_y, CURSOR_W, CURSOR_H);
        with_scene(|scene| scene.post_damage(old));
        c.cursor_x = x;
        c.cursor_y = y;
        // Post damage for new cursor position.
        let new = DamageRect::new(x, y, CURSOR_W, CURSOR_H);
        with_scene(|scene| scene.post_damage(new));
    }
}

/// Returns `true` if the scene is dirty and needs a composite pass.
pub fn needs_composite() -> bool {
    with_scene(|s| s.is_dirty())
}

/// Tick animation springs and return `true` if any spring is still running.
pub fn tick_scene_animations() -> bool {
    tick_animations()
}

/// Composite the current scene graph into the framebuffer.
///
/// Only re-renders if the scene is dirty.  The damage rect is the union of
/// all dirty node bounds since the last composite.
///
/// Returns the damage rect that was flushed (for logging / debug overlay).
pub fn composite() -> DamageRect {
    let damage = with_scene(|s| s.take_damage());
    if damage.is_empty() {
        return damage;
    }

    let mut guard = COMPOSITOR.lock();
    let compositor = match guard.as_mut() {
        Some(c) => c,
        None => return damage,
    };

    let sw = compositor.screen_w;
    let sh = compositor.screen_h;
    let damage_clipped = damage.clip(sw, sh);
    if damage_clipped.is_empty() {
        return damage;
    }

    // Render the scene.
    compositor.backend.begin_frame(damage_clipped, sw, sh);

    with_scene(|scene| {
        let cursor_x = compositor.cursor_x;
        let cursor_y = compositor.cursor_y;

        for node in scene.sorted_iter() {
            if !node.visible {
                continue;
            }

            let mat = &node.material;

            match &node.kind {
                NodeKind::Panel { fill, border, w, h } => {
                    let rect = node.transform.bounds(*w, *h);
                    let clipped = rect.clip(sw, sh);
                    if clipped.is_empty() {
                        continue;
                    }

                    // Shadow behind panel.
                    if mat.shadow.is_active() {
                        compositor.backend.draw_shadow(rect, mat.shadow);
                    }
                    compositor.backend.fill_rect(rect, *fill, mat);
                    if let Some(bd) = border {
                        compositor.backend.draw_border(rect, *bd, mat);
                    }
                }

                NodeKind::Surface {
                    surface_id,
                    src_w,
                    src_h,
                } => {
                    if !surface_exists(*surface_id) {
                        continue;
                    }

                    // Shadow behind surface.
                    if mat.shadow.is_active() {
                        let bounds = node.transform.bounds(*src_w, *src_h);
                        compositor.backend.draw_shadow(bounds, mat.shadow);
                    }
                    compositor.backend.blit_surface(
                        *surface_id,
                        *src_w,
                        *src_h,
                        &node.transform,
                        mat,
                    );
                }

                NodeKind::RenderTarget { .. } => {
                    // Phase 3: composite a render target (blur/effects output).
                }

                NodeKind::Group => {}
            }
        }

        // Cursor last (always on top).
        compositor.backend.draw_cursor(cursor_x, cursor_y);
    });

    compositor.backend.end_frame(damage_clipped);

    // Clear per-node dirty flags now that the composite succeeded.
    with_scene(|s| s.clear_node_dirty_flags());

    damage_clipped
}

// ── Scene builder helpers ─────────────────────────────────────────────────────
//
// Convenience functions called by the desktop init code to populate the
// initial scene graph (background, panels, etc.).

/// Build the standard desktop scene: background + top-bar panel + cursor node.
pub fn build_desktop_scene(screen_w: u32, screen_h: u32) {
    use crate::wm::scene::{FillDef, Material};

    with_scene(|scene| {
        // Background gradient.
        let _bg = scene.insert_background(
            FillDef::LinearV {
                from: 0xFF060A16,
                to: 0xFF101A31,
            },
            screen_w,
            screen_h,
        );

        // Top status bar (28 px high).
        let bar_mat = Material::DEFAULT.with_opacity(230);
        let bar_id = scene.insert_panel(
            FillDef::LinearH {
                from: 0xFF0D1829,
                to: 0xFF0A1020,
            },
            screen_w,
            28,
            0,
            0,
            10,
        );
        if let Some(node) = scene.get_mut(bar_id) {
            node.material = bar_mat;
        }

        // Compositor posts full-screen damage on first frame.
        scene.mark_all_dirty(screen_w, screen_h);
    });
}

/// Register a ring-3 surface as a scene node.
///
/// Called by the kernel desktop when a new surface is committed.
/// Returns the `NodeId` for the new node.
pub fn add_surface_node(surface_id: u32, x: i32, y: i32, z: i16) -> NodeId {
    let (w, h) = surface_dimensions(surface_id).unwrap_or((0, 0));
    if w == 0 || h == 0 {
        return NodeId::INVALID;
    }

    with_scene(|scene| scene.insert_surface(surface_id, w as u32, h as u32, x, y, z))
}

/// Remove the scene node for a surface that has been destroyed.
pub fn remove_surface_node(surface_id: u32) {
    with_scene(|scene| {
        let found = scene.find_by_surface_id(surface_id);
        if found.is_valid() {
            scene.remove(found);
        }
    });
}

/// Mark a surface node dirty (triggers texture re-upload + redraw next frame).
pub fn dirty_surface_node(surface_id: u32) {
    with_scene(|scene| {
        let id = scene.find_by_surface_id(surface_id);
        if id.is_valid() {
            scene.mark_node_dirty(id);
        }
    });
}
