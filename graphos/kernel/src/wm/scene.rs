// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Retained-mode scene graph for the GraphOS GPU compositor.
//!
//! ## Architecture
//!
//! Every visual element on screen is a `SceneNode`.  The compositor renders
//! the full scene graph each time it is dirty, rather than processing an
//! ad-hoc blit-list assembled from IPC messages.
//!
//! ```text
//! Ring-3 compositor service
//!   └─ scene_insert / scene_remove / scene_update_transform syscalls
//!        ↓
//! SceneGraph  (kernel, this module)
//!   ├─ NodeKind::Surface   → ring-3 pixel buffer → GPU texture
//!   ├─ NodeKind::Panel     → fill / border, no external pixels
//!   └─ NodeKind::Group     → transform aggregator
//!        ↓
//! SceneCompositor  (scene_compositor.rs)
//!   └─ GpuBackend  (gpu_backend.rs)
//!        ├─ Virtio2dBackend  (active — optimised CPU blit)
//!        └─ NativeGpuBackend (planned — GraphOS-native textured quads)
//! ```
//!
//! ## Design constraints
//! - Fixed-size static pools: no heap allocation in the graph itself.
//! - All coordinates are i32 screen pixels; scale/opacity are integer FP.
//! - The scene graph is a flat pool sorted by `z_order` on demand.
//! - Animation springs are per-node and advance only when unsettled.

#![allow(dead_code)]

use crate::wm::damage::DamageRect;
use spin::Mutex;

// ── Limits ────────────────────────────────────────────────────────────────────

pub const MAX_SCENE_NODES: usize = 128;
pub const MAX_ANIMATIONS: usize = 64;

// ── NodeId ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const INVALID: Self = Self(u32::MAX);

    #[inline]
    pub fn is_valid(self) -> bool {
        self.0 != u32::MAX
    }
}

// ── Transform ────────────────────────────────────────────────────────────────
//
// Integer fixed-point: 1024 = 1.0 for scale; tx/ty are direct pixels.

#[derive(Clone, Copy, Debug)]
pub struct Transform {
    /// X translation in screen pixels.
    pub tx: i32,
    /// Y translation in screen pixels.
    pub ty: i32,
    /// X scale in FP units (1024 = 1.0).
    pub sx: i32,
    /// Y scale in FP units (1024 = 1.0).
    pub sy: i32,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        tx: 0,
        ty: 0,
        sx: 1024,
        sy: 1024,
    };

    pub const fn translate(tx: i32, ty: i32) -> Self {
        Self {
            tx,
            ty,
            sx: 1024,
            sy: 1024,
        }
    }

    /// Compose parent × child.
    pub fn compose(p: &Self, c: &Self) -> Self {
        Self {
            tx: p.tx + c.tx * p.sx / 1024,
            ty: p.ty + c.ty * p.sy / 1024,
            sx: p.sx * c.sx / 1024,
            sy: p.sy * c.sy / 1024,
        }
    }

    /// Screen-space bounding box of a surface with the given pixel dimensions.
    pub fn bounds(self, src_w: u32, src_h: u32) -> DamageRect {
        let w = (src_w as i64 * self.sx as i64 / 1024).max(0) as u32;
        let h = (src_h as i64 * self.sy as i64 / 1024).max(0) as u32;
        DamageRect::new(self.tx, self.ty, w, h)
    }
}

// ── Material ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub opacity: u8, // 0 = transparent, 255 = opaque
    pub blend_mode: BlendMode,
    pub corner_radius: u8, // px (0 = sharp)
    pub shadow: ShadowDef,
    pub blur_radius: u8, // Kawase approximate Gaussian radius (0 = none)
    pub tint: u32,       // ARGB overlay tint applied after texture sample
}

impl Material {
    pub const DEFAULT: Self = Self {
        opacity: 255,
        blend_mode: BlendMode::Alpha,
        corner_radius: 0,
        shadow: ShadowDef::NONE,
        blur_radius: 0,
        tint: 0,
    };

    pub const fn with_opacity(mut self, op: u8) -> Self {
        self.opacity = op;
        self
    }
    pub const fn with_tint(mut self, t: u32) -> Self {
        self.tint = t;
        self
    }
    pub const fn with_shadow(mut self, s: ShadowDef) -> Self {
        self.shadow = s;
        self
    }
    pub const fn with_blur(mut self, r: u8) -> Self {
        self.blur_radius = r;
        self
    }
    pub const fn with_radius(mut self, r: u8) -> Self {
        self.corner_radius = r;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    /// Standard alpha compositing: src_α·src + (1−src_α)·dst
    Alpha,
    /// Additive: src + dst (clamp 255)
    Additive,
    /// Multiply: src·dst/255
    Multiply,
    /// Screen: 255 − (255−src)·(255−dst)/255
    Screen,
    /// Opaque overwrite: no blending
    Opaque,
}

#[derive(Clone, Copy, Debug)]
pub struct ShadowDef {
    pub offset_x: i8,
    pub offset_y: i8,
    pub blur: u8,
    pub color: u32, // ARGB
}

impl ShadowDef {
    pub const NONE: Self = Self {
        offset_x: 0,
        offset_y: 0,
        blur: 0,
        color: 0,
    };

    pub const fn drop_shadow(dx: i8, dy: i8, blur: u8, color: u32) -> Self {
        Self {
            offset_x: dx,
            offset_y: dy,
            blur,
            color,
        }
    }

    #[inline]
    pub fn is_active(self) -> bool {
        self.blur > 0 || self.offset_x != 0 || self.offset_y != 0
    }
}

// ── Fill ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum FillDef {
    /// Solid ARGB color.
    Solid(u32),
    /// Linear gradient: `from` color at top, `to` color at bottom.
    LinearV { from: u32, to: u32 },
    /// Linear gradient: `from` color at left, `to` color at right.
    LinearH { from: u32, to: u32 },
}

impl FillDef {
    /// Sample the fill at normalised position (nx, ny) ∈ [0, 1].
    ///
    /// nx_fp / ny_fp are in FP units (0–1024).
    pub fn sample(self, _nx_fp: u32, ny_fp: u32) -> u32 {
        match self {
            FillDef::Solid(c) => c,
            FillDef::LinearV { from, to } => blend_argb(from, to, ny_fp as u8),
            FillDef::LinearH { from, to } => blend_argb(from, to, _nx_fp as u8),
        }
    }
}

fn blend_argb(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255u32.wrapping_sub(t);
    let r = ((a >> 16 & 0xFF) * it + (b >> 16 & 0xFF) * t) / 255;
    let g = ((a >> 8 & 0xFF) * it + (b >> 8 & 0xFF) * t) / 255;
    let bl = ((a & 0xFF) * it + (b & 0xFF) * t) / 255;
    let ao = ((a >> 24) * it + (b >> 24) * t) / 255;
    (ao << 24) | (r << 16) | (g << 8) | bl
}

// ── Border ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct BorderDef {
    pub color: u32,
    pub width: u8,
    pub radius: u8,
}

// ── NodeKind ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum NodeKind {
    /// A ring-3 pixel surface (becomes a GPU texture in the native backend).
    /// `surface_id` refers to the kernel surface table entry.
    Surface {
        surface_id: u32,
        /// Width/height of the surface in pixels (cached from surface_table).
        src_w: u32,
        src_h: u32,
    },

    /// A purely compositor-side filled panel (no ring-3 pixel source).
    Panel {
        fill: FillDef,
        border: Option<BorderDef>,
        /// Width × height of the panel in pixels.
        w: u32,
        h: u32,
    },

    /// A compositor-owned render target (used for effect passes).
    RenderTarget { rt_id: u32, w: u32, h: u32 },

    /// Transform/clip aggregate for child nodes (no direct rendering).
    Group,
}

impl NodeKind {
    /// Pixel dimensions of this node (width, height) in local space.
    pub fn local_size(&self) -> (u32, u32) {
        match self {
            NodeKind::Surface { src_w, src_h, .. } => (*src_w, *src_h),
            NodeKind::Panel { w, h, .. } => (*w, *h),
            NodeKind::RenderTarget { w, h, .. } => (*w, *h),
            NodeKind::Group => (0, 0),
        }
    }
}

// ── SceneNode ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct SceneNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub transform: Transform,
    pub material: Material,
    /// Z-order: lower = behind, higher = in front.
    pub z_order: i16,
    pub visible: bool,
    /// True when this node's content has changed since the last composite.
    pub dirty: bool,
    /// Optional scissor clip rect (screen space).
    pub clip: Option<DamageRect>,
}

impl SceneNode {
    fn new(id: NodeId, kind: NodeKind, z: i16) -> Self {
        Self {
            id,
            kind,
            transform: Transform::IDENTITY,
            material: Material::DEFAULT,
            z_order: z,
            visible: true,
            dirty: true,
            clip: None,
        }
    }

    /// Compute screen-space bounding rect (including shadow offset).
    pub fn screen_bounds(&self) -> DamageRect {
        let (w, h) = self.kind.local_size();
        let base = self.transform.bounds(w, h);
        if self.material.shadow.is_active() {
            let s = self.material.shadow;
            let sx = s.offset_x as i32;
            let sy = s.offset_y as i32;
            let extra = s.blur as i32;
            DamageRect::new(
                base.x.min(base.x + sx - extra),
                base.y.min(base.y + sy - extra),
                base.w + (sx.abs() + extra) as u32 * 2,
                base.h + (sy.abs() + extra) as u32 * 2,
            )
        } else {
            base
        }
    }
}

// ── SceneGraph ────────────────────────────────────────────────────────────────

pub struct SceneGraph {
    nodes: [Option<SceneNode>; MAX_SCENE_NODES],
    count: usize,
    next_id: u32,
    /// Sorted node indices (back-to-front by z_order).
    sorted: [usize; MAX_SCENE_NODES],
    sorted_len: usize,
    sorted_dirty: bool,
    /// Union of all dirty node bounds since last clear.
    pub damage: DamageRect,
    /// Full scene is dirty (e.g. after a node was added/removed).
    scene_dirty: bool,
}

impl SceneGraph {
    pub const fn new() -> Self {
        const NONE: Option<SceneNode> = None;
        Self {
            nodes: [NONE; MAX_SCENE_NODES],
            count: 0,
            next_id: 1,
            sorted: [0usize; MAX_SCENE_NODES],
            sorted_len: 0,
            sorted_dirty: true,
            damage: DamageRect::EMPTY,
            scene_dirty: true,
        }
    }

    // ── Insertion ─────────────────────────────────────────────────────────────

    /// Add a surface node backed by ring-3 surface `surface_id`.
    pub fn insert_surface(
        &mut self,
        surface_id: u32,
        w: u32,
        h: u32,
        x: i32,
        y: i32,
        z: i16,
    ) -> NodeId {
        let id = self.alloc_id();
        let mut node = SceneNode::new(
            id,
            NodeKind::Surface {
                surface_id,
                src_w: w,
                src_h: h,
            },
            z,
        );
        node.transform = Transform::translate(x, y);
        self.insert_node(node)
    }

    /// Add a filled panel node.
    pub fn insert_panel(
        &mut self,
        fill: FillDef,
        w: u32,
        h: u32,
        x: i32,
        y: i32,
        z: i16,
    ) -> NodeId {
        let id = self.alloc_id();
        let mut node = SceneNode::new(
            id,
            NodeKind::Panel {
                fill,
                border: None,
                w,
                h,
            },
            z,
        );
        node.transform = Transform::translate(x, y);
        self.insert_node(node)
    }

    /// Add a background panel spanning the full screen.
    pub fn insert_background(&mut self, fill: FillDef, screen_w: u32, screen_h: u32) -> NodeId {
        self.insert_panel(fill, screen_w, screen_h, 0, 0, i16::MIN)
    }

    fn insert_node(&mut self, node: SceneNode) -> NodeId {
        if self.count >= MAX_SCENE_NODES {
            return NodeId::INVALID;
        }
        let id = node.id;
        let slot = self.free_slot();
        self.nodes[slot] = Some(node);
        self.count += 1;
        self.sorted_dirty = true;
        self.scene_dirty = true;
        id
    }

    // ── Removal ───────────────────────────────────────────────────────────────

    pub fn remove(&mut self, id: NodeId) {
        for i in 0..MAX_SCENE_NODES {
            if self.nodes[i].as_ref().map(|n| n.id) == Some(id) {
                if let Some(ref n) = self.nodes[i] {
                    self.damage = self.damage.union(n.screen_bounds());
                }
                self.nodes[i] = None;
                self.count = self.count.saturating_sub(1);
                self.sorted_dirty = true;
                self.scene_dirty = true;
                return;
            }
        }
    }

    // ── Mutation ──────────────────────────────────────────────────────────────

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut SceneNode> {
        for i in 0..MAX_SCENE_NODES {
            if self.nodes[i].as_ref().map(|n| n.id) == Some(id) {
                return self.nodes[i].as_mut();
            }
        }
        None
    }

    pub fn get(&self, id: NodeId) -> Option<&SceneNode> {
        for i in 0..MAX_SCENE_NODES {
            if self.nodes[i].as_ref().map(|n| n.id) == Some(id) {
                return self.nodes[i].as_ref();
            }
        }
        None
    }

    /// Find the NodeId of a Surface node by its surface_id. Returns `NodeId::INVALID` if not found.
    pub fn find_by_surface_id(&self, surface_id: u32) -> NodeId {
        for i in 0..MAX_SCENE_NODES {
            if let Some(ref n) = self.nodes[i] {
                if let NodeKind::Surface {
                    surface_id: sid, ..
                } = n.kind
                {
                    if sid == surface_id {
                        return n.id;
                    }
                }
            }
        }
        NodeId::INVALID
    }

    /// Mark a node's content as changed (triggers texture re-upload + redraw).
    pub fn mark_node_dirty(&mut self, id: NodeId) {
        if let Some(n) = self.get_mut(id) {
            let bounds = n.screen_bounds();
            n.dirty = true;
            self.damage = self.damage.union(bounds);
            self.scene_dirty = true;
        }
    }

    /// Mark all surface nodes dirty (e.g. after a global resolution change).
    pub fn mark_all_dirty(&mut self, screen_w: u32, screen_h: u32) {
        for i in 0..MAX_SCENE_NODES {
            if let Some(ref mut n) = self.nodes[i] {
                n.dirty = true;
            }
        }
        self.damage = DamageRect::new(0, 0, screen_w, screen_h);
        self.scene_dirty = true;
    }

    /// Post a damage rect (e.g. from cursor motion) without marking any specific node dirty.
    pub fn post_damage(&mut self, r: DamageRect) {
        self.damage = self.damage.union(r);
        self.scene_dirty = true;
    }

    // ── Query ─────────────────────────────────────────────────────────────────

    pub fn is_dirty(&self) -> bool {
        self.scene_dirty
    }

    /// Take accumulated damage and clear the dirty flag. Returns the rect to re-composite.
    pub fn take_damage(&mut self) -> DamageRect {
        let d = self.damage;
        self.damage = DamageRect::EMPTY;
        self.scene_dirty = false;
        d
    }

    /// Clear per-node dirty flags after a successful composite.
    pub fn clear_node_dirty_flags(&mut self) {
        for i in 0..MAX_SCENE_NODES {
            if let Some(ref mut n) = self.nodes[i] {
                n.dirty = false;
            }
        }
    }

    // ── Sorted iteration ──────────────────────────────────────────────────────

    /// Iterator over visible nodes sorted back-to-front (ascending z_order).
    pub fn sorted_iter(&mut self) -> SortedIter<'_> {
        if self.sorted_dirty {
            self.rebuild_sort();
        }
        SortedIter {
            graph: self,
            pos: 0,
        }
    }

    fn rebuild_sort(&mut self) {
        // Collect (z, slot_index) pairs.
        let mut pairs = [(0i16, 0usize); MAX_SCENE_NODES];
        let mut n = 0;
        for i in 0..MAX_SCENE_NODES {
            if let Some(ref node) = self.nodes[i] {
                pairs[n] = (node.z_order, i);
                n += 1;
            }
        }
        // Insertion sort (small n).
        for i in 1..n {
            let key = pairs[i];
            let mut j = i;
            while j > 0 && pairs[j - 1].0 > key.0 {
                pairs[j] = pairs[j - 1];
                j -= 1;
            }
            pairs[j] = key;
        }
        for i in 0..n {
            self.sorted[i] = pairs[i].1;
        }
        self.sorted_len = n;
        self.sorted_dirty = false;
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn free_slot(&self) -> usize {
        for i in 0..MAX_SCENE_NODES {
            if self.nodes[i].is_none() {
                return i;
            }
        }
        0
    }

    /// Find the topmost visible node whose bounds contain (px, py).
    pub fn hit_test(&mut self, px: i32, py: i32) -> Option<NodeId> {
        if self.sorted_dirty {
            self.rebuild_sort();
        }
        // Iterate front-to-back (reverse).
        for k in (0..self.sorted_len).rev() {
            let slot = self.sorted[k];
            if let Some(ref node) = self.nodes[slot] {
                if !node.visible {
                    continue;
                }
                let b = node.screen_bounds();
                if px >= b.x && px < b.x + b.w as i32 && py >= b.y && py < b.y + b.h as i32 {
                    return Some(node.id);
                }
            }
        }
        None
    }
}

pub struct SortedIter<'a> {
    graph: &'a SceneGraph,
    pos: usize,
}

impl<'a> Iterator for SortedIter<'a> {
    type Item = &'a SceneNode;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos < self.graph.sorted_len {
            let slot = self.graph.sorted[self.pos];
            self.pos += 1;
            if let Some(ref n) = self.graph.nodes[slot] {
                if n.visible {
                    return Some(n);
                }
            }
        }
        None
    }
}

// ── Animation ────────────────────────────────────────────────────────────────
//
// Each animated node gets a `NodeAnimation` with springs for opacity, scale,
// and translation.  Springs advance only when unsettled — no global frame
// timer overhead when the desktop is idle.

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    /// Current value (FP × 1024).
    pub value: i32,
    /// Target value (FP × 1024).
    pub target: i32,
    /// Velocity (FP × 1024 / tick).
    velocity: i32,
    /// Spring stiffness.
    stiffness: i32,
    /// Damping coefficient.
    damping: i32,
    pub settled: bool,
}

impl Spring {
    pub const fn new(value: i32, stiffness: i32, damping: i32) -> Self {
        Self {
            value,
            target: value,
            velocity: 0,
            stiffness,
            damping,
            settled: true,
        }
    }

    pub fn set_target(&mut self, t: i32) {
        if self.target != t {
            self.target = t;
            self.settled = false;
        }
    }

    /// Advance one tick.  Returns `true` if still running.
    pub fn tick(&mut self) -> bool {
        if self.settled {
            return false;
        }
        let err = self.target - self.value;
        let force = err * self.stiffness / 1024 - self.velocity * self.damping / 1024;
        self.velocity += force;
        self.value += self.velocity;
        if (self.value - self.target).abs() < 3 && self.velocity.abs() < 2 {
            self.value = self.target;
            self.velocity = 0;
            self.settled = true;
        }
        !self.settled
    }
}

#[derive(Clone, Copy, Debug)]
pub struct NodeAnimation {
    pub node_id: NodeId,
    pub opacity: Spring, // target range [0..255 × 1024]
    pub scale: Spring,   // target range [0..∞ × 1024] (1024 = 1.0)
    pub tx: Spring,      // pixels × 1024
    pub ty: Spring,      // pixels × 1024
}

impl NodeAnimation {
    pub fn new(id: NodeId) -> Self {
        Self {
            node_id: id,
            opacity: Spring::new(255 * 1024, 160, 22),
            scale: Spring::new(1024 * 1024, 190, 24),
            tx: Spring::new(0, 210, 26),
            ty: Spring::new(0, 210, 26),
        }
    }

    /// Returns `(any_running, opacity_u8, scale_fp_1024, tx_px, ty_px)`.
    pub fn tick(&mut self) -> (bool, u8, i32, i32, i32) {
        let r = self.opacity.tick() | self.scale.tick() | self.tx.tick() | self.ty.tick();
        let op = ((self.opacity.value / 1024).clamp(0, 255)) as u8;
        let sc = self.scale.value / 1024;
        let tx = self.tx.value / 1024;
        let ty = self.ty.value / 1024;
        (r, op, sc, tx, ty)
    }

    pub fn is_settled(&self) -> bool {
        self.opacity.settled && self.scale.settled && self.tx.settled && self.ty.settled
    }
}

pub struct AnimationTimeline {
    anims: [Option<NodeAnimation>; MAX_ANIMATIONS],
}

impl AnimationTimeline {
    pub const fn new() -> Self {
        const NONE: Option<NodeAnimation> = None;
        Self {
            anims: [NONE; MAX_ANIMATIONS],
        }
    }

    pub fn register(&mut self, id: NodeId) {
        if self.slot_for(id).is_some() {
            return;
        }
        for i in 0..MAX_ANIMATIONS {
            if self.anims[i].is_none() {
                self.anims[i] = Some(NodeAnimation::new(id));
                return;
            }
        }
    }

    pub fn remove(&mut self, id: NodeId) {
        if let Some(i) = self.slot_for(id) {
            self.anims[i] = None;
        }
    }

    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut NodeAnimation> {
        let i = self.slot_for(id)?;
        self.anims[i].as_mut()
    }

    /// Tick all springs and apply to scene nodes.  Returns `true` if any spring is still running.
    pub fn tick_all(&mut self, graph: &mut SceneGraph) -> bool {
        let mut any = false;
        for i in 0..MAX_ANIMATIONS {
            if let Some(ref mut anim) = self.anims[i] {
                let (running, op, sc, tx, ty) = anim.tick();
                if running {
                    any = true;
                    let nid = anim.node_id;
                    if let Some(node) = graph.get_mut(nid) {
                        node.material.opacity = op;
                        node.transform.sx = sc;
                        node.transform.sy = sc;
                        node.transform.tx = tx;
                        node.transform.ty = ty;
                        node.dirty = true;
                    }
                }
            }
        }
        if any {
            graph.scene_dirty = true;
        }
        any
    }

    fn slot_for(&self, id: NodeId) -> Option<usize> {
        for i in 0..MAX_ANIMATIONS {
            if self.anims[i].as_ref().map(|a| a.node_id) == Some(id) {
                return Some(i);
            }
        }
        None
    }
}

// ── Global state ─────────────────────────────────────────────────────────────

static SCENE: Mutex<SceneGraph> = Mutex::new(SceneGraph::new());
static TIMELINE: Mutex<AnimationTimeline> = Mutex::new(AnimationTimeline::new());

pub fn with_scene<F, R>(f: F) -> R
where
    F: FnOnce(&mut SceneGraph) -> R,
{
    f(&mut SCENE.lock())
}

pub fn with_timeline<F, R>(f: F) -> R
where
    F: FnOnce(&mut AnimationTimeline) -> R,
{
    f(&mut TIMELINE.lock())
}

/// Tick animations, applying spring results to the scene.
/// Returns `true` if any spring is still running (caller should request another tick).
pub fn tick_animations() -> bool {
    let mut scene = SCENE.lock();
    let mut timeline = TIMELINE.lock();
    timeline.tick_all(&mut scene)
}

/// Quick dirty check without taking the timeline lock.
pub fn scene_is_dirty() -> bool {
    SCENE.lock().is_dirty()
}

/// Take accumulated damage from the scene (clears dirty flag).
pub fn scene_take_damage() -> DamageRect {
    SCENE.lock().take_damage()
}
