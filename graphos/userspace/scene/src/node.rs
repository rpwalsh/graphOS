// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Scene nodes — the objects in the 3D scene graph.

use crate::aabb::Aabb;
use crate::material::MaterialId;
use crate::mesh::MeshId;
use crate::transform::Transform3D;

// ── Node identity ─────────────────────────────────────────────────────────────

/// Opaque node handle.  Index into `SceneGraph::nodes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const INVALID: Self = Self(u32::MAX);
    pub fn is_valid(self) -> bool {
        self.0 != u32::MAX
    }
}

// ── Node kinds ────────────────────────────────────────────────────────────────

/// What kind of object this node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Empty / group node (just a transform parent).
    Empty,

    /// 2D desktop surface (application render target composited onto geometry).
    Surface2D,

    /// Window — a movable, resizable spatial surface.
    Window,

    /// Panel / launcher — anchored UI layer in the spatial shell.
    Panel,

    /// Opaque or alpha 3D mesh with a material.
    Mesh3D,

    /// Graph-native view (node graph, analytics, AI, etc.).
    GraphView,

    /// Camera node (for multi-camera scenes).
    Camera,

    /// Point, directional or spot light.
    Light,

    /// Immersive background (weather, AI, analytics).
    Immersive,
}

// ── Node ──────────────────────────────────────────────────────────────────────

/// A node in the 3D scene graph.
pub struct SceneNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub name: [u8; 48],
    pub transform: Transform3D,
    pub parent: NodeId,

    // Hierarchy links (indices into SceneGraph::nodes).
    pub first_child: NodeId,
    pub next_sibling: NodeId,

    // Rendering
    pub mesh: Option<MeshId>,
    pub material: Option<MaterialId>,
    pub local_aabb: Aabb,

    // Visibility / interaction
    pub visible: bool,
    pub cast_shadow: bool,
    pub recv_shadow: bool,
    pub interactive: bool, // participates in hit testing

    // For Surface2D / Window: the compositor surface resource ID.
    pub surface_resource: u32,

    // Layer depth (2D compositor fallback).
    pub z_order: i32,
}

impl SceneNode {
    pub fn new(id: NodeId, kind: NodeKind) -> Self {
        Self {
            id,
            kind,
            name: [0u8; 48],
            transform: Transform3D::default(),
            parent: NodeId::INVALID,
            first_child: NodeId::INVALID,
            next_sibling: NodeId::INVALID,
            mesh: None,
            material: None,
            local_aabb: Aabb::UNIT,
            visible: true,
            cast_shadow: true,
            recv_shadow: true,
            interactive: true,
            surface_resource: 0,
            z_order: 0,
        }
    }

    pub fn set_name(&mut self, s: &str) {
        let b = s.as_bytes();
        let len = b.len().min(47);
        self.name[..len].copy_from_slice(&b[..len]);
        self.name[len] = 0;
    }
}
