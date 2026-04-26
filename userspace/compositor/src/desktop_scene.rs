// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! `DesktopScene` — 3D spatial shell built on top of `graphos-scene`.
//!
//! Every application window, panel, and launcher is a `SceneNode` in a
//! `SceneGraph`.  The compositor uses spring-animated TRS transforms so that
//! drag, minimize, maximize, and workspace-switch all feel physically alive.
//!
//! # Architecture
//!
//! ```
//! DesktopScene
//!   ├── root          (NodeKind::Empty)
//!   │   ├── panel     (NodeKind::Panel)   — launcher / dock
//!   │   ├── window_0  (NodeKind::Window)  — app 0
//!   │   ├── window_1  (NodeKind::Window)  — app 1
//!   │   └── ...
//!   └── camera        (NodeKind::Camera)
//! ```
//!
//! # Usage
//! ```no_run
//! let mut scene = DesktopScene::new(1920, 1080);
//! let node = scene.spawn_window(42, 100.0, 100.0);
//! scene.tick(0.016);
//! let graph = scene.build_render_graph(backbuffer, depth, 1920, 1080);
//! ```

extern crate alloc;
use alloc::collections::BTreeMap;

use graphos_gfx::command::ResourceId;
use graphos_scene::{
    animation::{Spring, Spring3},
    camera::Camera,
    graph::SceneGraph,
    math::{Quat, Vec3},
    node::{NodeId, NodeKind},
    pass::RenderGraph,
};

// ── Window state ──────────────────────────────────────────────────────────────

/// Per-window animation state.
struct WindowEntry {
    node: NodeId,
    /// Target world-space position.
    pos_spring: Spring3,
    /// Target scale (1.0 = full-size).
    scale_spring: Spring,
    /// Opacity spring (1.0 = fully visible).
    opacity: Spring,
    /// Whether the window is minimised.
    minimised: bool,
}

impl WindowEntry {
    fn new(node: NodeId, x: f32, y: f32) -> Self {
        let mut pos = Spring3::smooth();
        pos.x.value = x;
        pos.y.value = y;
        pos.x.target = x;
        pos.y.target = y;

        let mut scale = Spring::smooth();
        scale.value = 1.0;
        scale.target = 1.0;

        let mut opacity = Spring::smooth();
        opacity.value = 1.0;
        opacity.target = 1.0;

        Self {
            node,
            pos_spring: pos,
            scale_spring: scale,
            opacity,
            minimised: false,
        }
    }
}

// ── DesktopScene ──────────────────────────────────────────────────────────────

/// The 3-D spatial desktop shell.
pub struct DesktopScene {
    pub graph: SceneGraph,
    pub camera: Camera,
    root: NodeId,
    panel_node: NodeId,
    windows: BTreeMap<u32, WindowEntry>, // surface_id → state
    width: u32,
    height: u32,
}

impl DesktopScene {
    /// Create a new scene for a display of `width × height` pixels.
    pub fn new(width: u32, height: u32) -> Self {
        let mut graph = SceneGraph::new();
        let mut camera = Camera::perspective(60.0_f32, width as f32 / height as f32, 0.1, 1000.0);

        // Position camera so 1 scene-unit ≈ 1 pixel at z=0 for 2D content.
        // half-fov = 30 deg = PI/6
        let z = (height as f32 / 2.0) / libm::tanf(core::f32::consts::PI / 6.0);
        camera.set_position(Vec3::new(width as f32 / 2.0, height as f32 / 2.0, z));
        camera.set_target(Vec3::new(width as f32 / 2.0, height as f32 / 2.0, 0.0));

        let root = graph.add_root(NodeKind::Empty);
        let panel = graph.add_node(NodeKind::Panel, root);
        if let Some(pn) = graph.node_mut(panel) {
            pn.transform.position = Vec3::new(width as f32 / 2.0, height as f32 - 40.0, 0.0);
            pn.transform.scale = Vec3::new(width as f32 * 0.6, 60.0, 1.0);
        }

        Self {
            graph,
            camera,
            root,
            panel_node: panel,
            windows: BTreeMap::new(),
            width,
            height,
        }
    }

    // ── Window lifecycle ──────────────────────────────────────────────────────

    /// Spawn a new window for `surface_id` at position (x, y).
    pub fn spawn_window(&mut self, surface_id: u32, x: f32, y: f32) -> NodeId {
        let node = self.graph.add_node(NodeKind::Window, self.root);
        if let Some(n) = self.graph.node_mut(node) {
            n.surface_resource = surface_id;
            n.interactive = true;
            n.visible = true;
            n.transform.position = Vec3::new(x, y, 0.0);
        }
        let mut entry = WindowEntry::new(node, x, y);
        // Animate in from slightly below with scale 0.
        entry.pos_spring.y.value = y + 40.0;
        entry.scale_spring.value = 0.0;
        entry.opacity.value = 0.0;
        self.windows.insert(surface_id, entry);
        node
    }

    /// Close / remove a window.
    pub fn close_window(&mut self, surface_id: u32) {
        if let Some(entry) = self.windows.remove(&surface_id) {
            // Spring-animate out (scale to 0, opacity to 0).
            // In a real implementation we'd run the animation then remove the
            // node; for Phase 1 we just remove immediately.
            let _ = entry;
        }
    }

    /// Move a window to a new target position.
    pub fn move_window(&mut self, surface_id: u32, x: f32, y: f32) {
        if let Some(entry) = self.windows.get_mut(&surface_id) {
            entry.pos_spring.x.target = x;
            entry.pos_spring.y.target = y;
        }
    }

    /// Minimise / restore a window.
    pub fn minimise_window(&mut self, surface_id: u32, minimise: bool) {
        if let Some(entry) = self.windows.get_mut(&surface_id) {
            entry.minimised = minimise;
            if minimise {
                entry.scale_spring.target = 0.0;
                entry.opacity.target = 0.0;
            } else {
                entry.scale_spring.target = 1.0;
                entry.opacity.target = 1.0;
            }
        }
    }

    // ── Per-frame update ──────────────────────────────────────────────────────

    /// Advance all springs by `dt` seconds and flush transforms into the scene graph.
    pub fn tick(&mut self, dt: f32) {
        for entry in self.windows.values_mut() {
            entry.pos_spring.update(dt);
            entry.scale_spring.update(dt);
            entry.opacity.update(dt);

            // Write back into scene node.
            if let Some(node) = self.graph.node_mut(entry.node) {
                let s = entry.scale_spring.value;
                node.transform.position =
                    Vec3::new(entry.pos_spring.x.value, entry.pos_spring.y.value, 0.0);
                node.transform.scale = Vec3::new(s, s, 1.0);
                node.visible = entry.opacity.value > 0.01;
            }
        }
        self.graph.update_transforms();
    }

    // ── Render graph ──────────────────────────────────────────────────────────

    /// Build the frame render graph after culling.
    pub fn build_render_graph(
        &mut self,
        backbuffer: ResourceId,
        depth_rt: ResourceId,
    ) -> RenderGraph {
        self.graph.cull(&mut self.camera);
        self.graph
            .build_frame_graph(backbuffer, depth_rt, self.width, self.height)
    }

    // ── Hit testing ───────────────────────────────────────────────────────────

    /// Find the topmost interactive node at screen pixel (px, py).
    pub fn hit_test_screen(&mut self, px: f32, py: f32) -> Option<graphos_scene::hit::HitRecord> {
        // Convert pixel coords → NDC.
        let ndc_x = (px / self.width as f32) * 2.0 - 1.0;
        let ndc_y = 1.0 - (py / self.height as f32) * 2.0;
        let ray = self.camera.unproject_ray(ndc_x, ndc_y);
        self.graph.hit_test(ray)
    }
}
