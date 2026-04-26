// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Retained render scene graph — nodes, transforms, animation, and z-ordering.
//!
//! ## Scene graph model
//!
//! The compositor maintains a flat, z-sorted list of `RenderNode`s.  Each node
//! has a `NodeKind` (Background, Panel, Window, Overlay, Cursor, etc.), a 3-D
//! transform (x, y, z-depth, scale, rotation), and a `Material`.
//!
//! The scene graph does **not** own GPU resources — those are managed by the
//! `GfxContext` layer.  The scene graph is pure retained state and can be
//! mutated from the compositor event loop.
//!
//! ## Animation
//!
//! Each numeric transform dimension has an optional `Spring` that converges
//! to the target value.  The compositor calls `RenderScene::tick_animations(dt_ms)`
//! every frame to advance springs.  When all springs are settled the scene is
//! quiescent and no redraw is needed.

extern crate alloc;
use alloc::vec::Vec;

use crate::material::Material;
use crate::render_graph::RenderTargetHandle;

// ── Node identifier ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const NONE: Self = Self(0);
}

// ── 3-D transform ─────────────────────────────────────────────────────────────

/// Integer fixed-point 3-D transform for a render node.
///
/// All scale and rotation values use x1024 fixed-point (1024 = 1.0).
/// Z is a depth ordering hint — higher Z = closer to the viewer.
#[derive(Clone, Copy, Debug)]
pub struct Transform3D {
    /// Screen X position of the node's origin (top-left), pixels.
    pub x: i32,
    /// Screen Y position of the node's origin (top-left), pixels.
    pub y: i32,
    /// Depth ordering: nodes with higher Z are drawn on top.
    /// Z is an integer priority; fractional sub-ordering is handled by insertion order.
    pub z: i32,
    /// X scale ×1024 (1024 = 100%, 512 = 50%).
    pub scale_x: i32,
    /// Y scale ×1024 (1024 = 100%, 512 = 50%).
    pub scale_y: i32,
    /// Rotation in millidegrees (1000 = 1°).  Positive = clockwise.
    pub rotation_mdeg: i32,
}

impl Transform3D {
    pub const IDENTITY: Self = Self {
        x: 0,
        y: 0,
        z: 0,
        scale_x: 1024,
        scale_y: 1024,
        rotation_mdeg: 0,
    };

    pub const fn at(x: i32, y: i32) -> Self {
        Self {
            x,
            y,
            ..Self::IDENTITY
        }
    }

    pub const fn at_z(x: i32, y: i32, z: i32) -> Self {
        Self {
            x,
            y,
            z,
            ..Self::IDENTITY
        }
    }

    /// Compute scaled width/height of a node with source dimensions `(src_w, src_h)`.
    pub fn scaled_dims(&self, src_w: u32, src_h: u32) -> (u32, u32) {
        let w = ((src_w as i64 * self.scale_x as i64) / 1024).max(0) as u32;
        let h = ((src_h as i64 * self.scale_y as i64) / 1024).max(0) as u32;
        (w, h)
    }
}

// ── Spring physics ────────────────────────────────────────────────────────────

/// Semi-implicit Euler spring for smooth animation.
///
/// Converges to `target` with configurable stiffness and damping.
/// A spring is "settled" when both velocity and displacement fall below
/// their respective epsilon thresholds.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    /// Current value (×1024 fixed-point).
    pub value: i64,
    /// Target value (×1024 fixed-point).
    pub target: i64,
    /// Current velocity (×1024 units / ms).
    pub velocity: i64,
    /// Stiffness constant k (×1024).
    pub stiffness: i64,
    /// Damping constant c (×1024).  Critically damped ≈ 2 × sqrt(k × mass).
    pub damping: i64,
    /// True when the spring has converged.
    pub settled: bool,
}

const SPRING_EPS_VALUE: i64 = 64; // ~0.06 in x1024
const SPRING_EPS_VEL: i64 = 32; // ~0.03

impl Spring {
    pub const fn new(initial: i64, stiffness: i64, damping: i64) -> Self {
        Self {
            value: initial,
            target: initial,
            velocity: 0,
            stiffness,
            damping,
            settled: true,
        }
    }

    /// Standard UI spring presets.
    pub const SMOOTH: Self = Self::new(0, 300, 40);
    pub const SNAPPY: Self = Self::new(0, 600, 52);
    pub const BOUNCY: Self = Self::new(0, 250, 22);
    pub const INSTANT: Self = Self::new(0, 4096, 200);

    pub fn set_target(&mut self, target: i64) {
        self.target = target;
        self.settled = false;
    }

    pub fn set_immediate(&mut self, value: i64) {
        self.value = value;
        self.target = value;
        self.velocity = 0;
        self.settled = true;
    }

    /// Advance by `dt_ms` milliseconds.  Returns true if still animating.
    pub fn tick(&mut self, dt_ms: u32) -> bool {
        if self.settled {
            return false;
        }
        let dt = dt_ms as i64;
        let disp = self.target - self.value;
        let force = self.stiffness * disp / 1024 - self.damping * self.velocity / 1024;
        self.velocity += force * dt / 1024;
        self.value += self.velocity * dt / 1024;

        if disp.abs() < SPRING_EPS_VALUE && self.velocity.abs() < SPRING_EPS_VEL {
            self.value = self.target;
            self.velocity = 0;
            self.settled = true;
        }
        !self.settled
    }
}

// ── Per-node animation ────────────────────────────────────────────────────────

/// Animated transform state for a single render node.
#[derive(Clone, Debug)]
pub struct NodeAnimation {
    pub x: Spring,
    pub y: Spring,
    pub z: Spring,
    pub scale_x: Spring,
    pub scale_y: Spring,
    pub opacity: Spring,
}

impl NodeAnimation {
    pub fn new() -> Self {
        Self {
            x: Spring::new(0, 300, 40),
            y: Spring::new(0, 300, 40),
            z: Spring::new(0, 600, 52),
            scale_x: Spring::new(1024, 300, 40),
            scale_y: Spring::new(1024, 300, 40),
            opacity: Spring::new(255 * 1024, 400, 50),
        }
    }

    /// Advance all springs by `dt_ms`.  Returns true if any spring is still running.
    pub fn tick(&mut self, dt_ms: u32) -> bool {
        let a = self.x.tick(dt_ms);
        let b = self.y.tick(dt_ms);
        let c = self.z.tick(dt_ms);
        let d = self.scale_x.tick(dt_ms);
        let e = self.scale_y.tick(dt_ms);
        let f = self.opacity.tick(dt_ms);
        a || b || c || d || e || f
    }

    /// Produce a `Transform3D` snapshot from current spring values.
    pub fn current_transform(&self, base: &Transform3D) -> Transform3D {
        Transform3D {
            x: (self.x.value / 1024) as i32 + base.x,
            y: (self.y.value / 1024) as i32 + base.y,
            z: (self.z.value / 1024) as i32 + base.z,
            scale_x: (self.scale_x.value / 1024) as i32,
            scale_y: (self.scale_y.value / 1024) as i32,
            rotation_mdeg: base.rotation_mdeg,
        }
    }

    pub fn current_opacity(&self) -> u8 {
        (self.opacity.value / 1024).clamp(0, 255) as u8
    }
}

// ── Node kind ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// Desktop wallpaper — always drawn first, fills the entire screen.
    Background,
    /// Shell panel (top bar, sidebar, dock).
    Panel { w: u32, h: u32 },
    /// Application window surface.
    Window { surface_id: u32, w: u32, h: u32 },
    /// Full-screen overlay (lockscreen, modal, expose).
    Overlay { w: u32, h: u32 },
    /// Notification toast.
    Toast { w: u32, h: u32 },
    /// Context menu / popover.
    Menu { w: u32, h: u32 },
    /// Hardware cursor sprite.
    Cursor { w: u32, h: u32 },
    /// Off-screen render target (blur source, effects output).
    RenderTarget {
        handle: RenderTargetHandle,
        w: u32,
        h: u32,
    },
    /// Invisible layout / hit-test region only.
    HitRect { w: u32, h: u32 },
}

impl NodeKind {
    pub fn dimensions(&self) -> (u32, u32) {
        match *self {
            Self::Background => (0, 0),
            Self::Panel { w, h } => (w, h),
            Self::Window { w, h, .. } => (w, h),
            Self::Overlay { w, h } => (w, h),
            Self::Toast { w, h } => (w, h),
            Self::Menu { w, h } => (w, h),
            Self::Cursor { w, h } => (w, h),
            Self::RenderTarget { w, h, .. } => (w, h),
            Self::HitRect { w, h } => (w, h),
        }
    }

    pub fn is_surface(&self) -> bool {
        matches!(self, Self::Window { .. })
    }
}

// ── Render node ───────────────────────────────────────────────────────────────

pub struct RenderNode {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Base transform (static layout position).
    pub transform: Transform3D,
    /// Active animation state (springs drive toward `transform` targets).
    pub animation: Option<NodeAnimation>,
    pub material: Material,
    pub visible: bool,
    pub dirty: bool,
    pub clip_to_screen: bool,
}

impl RenderNode {
    pub fn new(id: NodeId, kind: NodeKind, transform: Transform3D, material: Material) -> Self {
        Self {
            id,
            kind,
            transform,
            animation: None,
            material,
            visible: true,
            dirty: true,
            clip_to_screen: true,
        }
    }

    /// Compute the effective (animated) transform for this frame.
    pub fn effective_transform(&self) -> Transform3D {
        match &self.animation {
            Some(anim) => anim.current_transform(&self.transform),
            None => self.transform,
        }
    }

    /// Compute the effective opacity (animated or static).
    pub fn effective_opacity(&self) -> u8 {
        match &self.animation {
            Some(anim) => anim.current_opacity(),
            None => self.material.opacity(),
        }
    }

    /// Enable animation on this node, inheriting transform as initial spring position.
    pub fn enable_animation(&mut self) {
        if self.animation.is_none() {
            let mut anim = NodeAnimation::new();
            anim.x.set_immediate(self.transform.x as i64 * 1024);
            anim.y.set_immediate(self.transform.y as i64 * 1024);
            anim.z.set_immediate(self.transform.z as i64 * 1024);
            anim.scale_x
                .set_immediate(self.transform.scale_x as i64 * 1024);
            anim.scale_y
                .set_immediate(self.transform.scale_y as i64 * 1024);
            anim.opacity
                .set_immediate(self.material.opacity() as i64 * 1024);
            self.animation = Some(anim);
        }
    }

    /// Animate this node to a new position.
    pub fn animate_to(&mut self, x: i32, y: i32) {
        let anim = self.animation.get_or_insert_with(NodeAnimation::new);
        anim.x.set_target(x as i64 * 1024);
        anim.y.set_target(y as i64 * 1024);
    }

    /// Animate this node's scale (for open/close transitions).
    pub fn animate_scale(&mut self, scale: i32) {
        let anim = self.animation.get_or_insert_with(NodeAnimation::new);
        anim.scale_x.set_target(scale as i64 * 1024);
        anim.scale_y.set_target(scale as i64 * 1024);
    }

    /// Animate opacity (for fade-in/fade-out transitions).
    pub fn animate_opacity(&mut self, opacity: u8) {
        let anim = self.animation.get_or_insert_with(NodeAnimation::new);
        anim.opacity.set_target(opacity as i64 * 1024);
    }
}

// ── Render scene ──────────────────────────────────────────────────────────────

/// The retained render scene graph.
///
/// Maintains a flat list of nodes sorted by Z-order.  The compositor calls
/// `tick_animations()` every frame to advance springs, then iterates
/// `sorted_nodes()` to produce the frame's draw list.
pub struct RenderScene {
    nodes: Vec<RenderNode>,
    next_id: u32,
    dirty: bool,
    screen_w: u32,
    screen_h: u32,
}

impl RenderScene {
    pub fn new(screen_w: u32, screen_h: u32) -> Self {
        Self {
            nodes: Vec::new(),
            next_id: 1,
            dirty: true,
            screen_w,
            screen_h,
        }
    }

    fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Insert a new node and return its ID.
    pub fn add(&mut self, kind: NodeKind, transform: Transform3D, material: Material) -> NodeId {
        let id = self.alloc_id();
        self.nodes
            .push(RenderNode::new(id, kind, transform, material));
        self.sort();
        self.dirty = true;
        id
    }

    /// Remove a node by ID.
    pub fn remove(&mut self, id: NodeId) {
        if let Some(pos) = self.nodes.iter().position(|n| n.id == id) {
            self.nodes.remove(pos);
            self.dirty = true;
        }
    }

    /// Get a mutable reference to a node by ID.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut RenderNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Advance all animation springs by `dt_ms`.
    ///
    /// Returns `true` if any spring is still running (caller should schedule
    /// another frame immediately).
    pub fn tick_animations(&mut self, dt_ms: u32) -> bool {
        let mut any_active = false;
        for node in &mut self.nodes {
            if let Some(anim) = &mut node.animation {
                if anim.tick(dt_ms) {
                    any_active = true;
                    node.dirty = true;
                }
            }
        }
        if any_active {
            self.dirty = true;
        }
        any_active
    }

    /// Returns true if the scene needs a composite pass.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the scene dirty flag (call after a successful composite).
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
        for node in &mut self.nodes {
            node.dirty = false;
        }
    }

    /// Mark all nodes dirty (force full redraw).
    pub fn invalidate_all(&mut self) {
        self.dirty = true;
        for node in &mut self.nodes {
            node.dirty = true;
        }
    }

    /// Mark a specific node dirty.
    pub fn invalidate(&mut self, id: NodeId) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
            n.dirty = true;
            self.dirty = true;
        }
    }

    /// Iterate nodes in back-to-front Z order (lowest Z first).
    pub fn sorted_nodes(&self) -> &[RenderNode] {
        &self.nodes
    }

    /// Re-sort nodes by Z-order after mutation.
    fn sort(&mut self) {
        self.nodes.sort_by_key(|n| n.transform.z);
    }

    pub fn screen_dims(&self) -> (u32, u32) {
        (self.screen_w, self.screen_h)
    }

    /// Build the standard GraphOS desktop scene.
    ///
    /// Populates the scene with:
    /// - Background (Z=0)
    /// - Top bar (Z=10, glass)
    /// - Left sidebar (Z=10, glass)
    /// - Cursor placeholder (Z=100)
    ///
    /// Call `add()` to insert window surfaces on top.
    pub fn seed_desktop(
        &mut self,
        style: &crate::material::ShellStyleSheet,
        topbar_h: u32,
        sidebar_w: u32,
    ) {
        let sw = self.screen_w;
        let sh = self.screen_h;

        // Background — full screen, Z=0
        self.add(
            NodeKind::Background,
            Transform3D::at_z(0, 0, 0),
            style.background,
        );

        // Top bar — glass strip across the top, Z=10
        self.add(
            NodeKind::Panel { w: sw, h: topbar_h },
            Transform3D::at_z(0, 0, 10),
            style.topbar,
        );

        // Left sidebar, Z=10
        self.add(
            NodeKind::Panel {
                w: sidebar_w,
                h: sh.saturating_sub(topbar_h),
            },
            Transform3D::at_z(0, topbar_h as i32, 10),
            style.sidebar,
        );

        // Cursor, Z=100
        self.add(
            NodeKind::Cursor { w: 12, h: 20 },
            Transform3D::at_z(sw as i32 / 2, sh as i32 / 2, 100),
            style.cursor,
        );
    }

    /// Insert an application window node.
    ///
    /// `surface_id` — kernel surface ID.  The frame executor will import it as
    /// a GPU texture on the first frame the node appears.
    pub fn add_window(
        &mut self,
        surface_id: u32,
        x: i32,
        y: i32,
        z: i32,
        w: u32,
        h: u32,
        style: &crate::material::ShellStyleSheet,
        focused: bool,
    ) -> NodeId {
        let mat = if focused {
            style.window_active
        } else {
            style.window_idle
        };
        let id = self.add(
            NodeKind::Window { surface_id, w, h },
            Transform3D::at_z(x, y, z),
            mat,
        );
        // Animate in from scale 0.92 to 1.0
        if let Some(node) = self.node_mut(id) {
            node.enable_animation();
            node.animate_scale(942); // start at 92%
            node.animate_opacity(0);
        }
        // Now animate to full size / opacity
        if let Some(node) = self.node_mut(id) {
            node.animate_scale(1024);
            node.animate_opacity(255);
        }
        id
    }

    /// Remove a window node with an exit animation (scale-down + fade-out).
    pub fn remove_window_animated(&mut self, id: NodeId) {
        if let Some(node) = self.node_mut(id) {
            node.animate_scale(900);
            node.animate_opacity(0);
            // TODO: schedule deferred removal after animation settles
            // For now, mark dirty so the fade is rendered.
        }
    }
}
