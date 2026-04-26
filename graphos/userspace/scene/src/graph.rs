// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! `SceneGraph` — the flat-array scene node tree with frustum culling.

extern crate alloc;
use alloc::vec::Vec;

use crate::aabb::Aabb;
use crate::camera::Camera;
use crate::hit::{ray_aabb, HitRecord, Ray};
use crate::material::{Material, MaterialId};
use crate::math::Mat4;
use crate::mesh::{Mesh, MeshId};
use crate::node::{NodeId, NodeKind, SceneNode};
use crate::pass::{PassKind, RenderGraph, RenderPass};

// ── SceneGraph ────────────────────────────────────────────────────────────────

/// The master scene graph.
///
/// All nodes live in `nodes[]`.  The root has no parent (`NodeId::INVALID`).
/// Parent/child relationships are maintained as a singly-linked child list.
pub struct SceneGraph {
    pub nodes: Vec<SceneNode>,
    pub materials: Vec<Material>,
    pub meshes: Vec<Mesh>,
    next_node_id: u32,
    next_mat_id: u32,
    next_mesh_id: u32,

    // Scratch: indices of visible nodes per frame (rebuilt each frame).
    visible_scratch: Vec<usize>,
    // World matrices (parallel to nodes[]).
    world_matrices: Vec<Mat4>,
    world_aabbs: Vec<Aabb>,
}

impl SceneGraph {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            materials: Vec::new(),
            meshes: Vec::new(),
            next_node_id: 1,
            next_mat_id: 1,
            next_mesh_id: 1,
            visible_scratch: Vec::new(),
            world_matrices: Vec::new(),
            world_aabbs: Vec::new(),
        }
    }

    // ── Node creation ─────────────────────────────────────────────────────────

    pub fn add_node(&mut self, kind: NodeKind, parent: NodeId) -> NodeId {
        let id = NodeId(self.next_node_id);
        self.next_node_id += 1;
        let mut node = SceneNode::new(id, kind);
        node.parent = parent;
        // Link as first child of parent.
        if parent.is_valid() {
            let parent_idx = self.node_index(parent);
            if let Some(idx) = parent_idx {
                node.next_sibling = self.nodes[idx].first_child;
                self.nodes[idx].first_child = id;
            }
        }
        self.nodes.push(node);
        self.world_matrices.push(Mat4::IDENTITY);
        self.world_aabbs.push(Aabb::UNIT);
        id
    }

    pub fn add_root(&mut self, kind: NodeKind) -> NodeId {
        self.add_node(kind, NodeId::INVALID)
    }

    // ── Node access ───────────────────────────────────────────────────────────

    pub fn node(&self, id: NodeId) -> Option<&SceneNode> {
        self.node_index(id).map(|i| &self.nodes[i])
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut SceneNode> {
        self.node_index(id).map(|i| &mut self.nodes[i])
    }

    fn node_index(&self, id: NodeId) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    // ── Material / mesh registration ──────────────────────────────────────────

    pub fn add_material(&mut self, mat: Material) -> MaterialId {
        let id = mat.id;
        self.materials.push(mat);
        id
    }

    pub fn add_mesh(&mut self, mesh: Mesh) -> MeshId {
        let id = mesh.id;
        self.meshes.push(mesh);
        id
    }

    pub fn next_mat_id(&mut self) -> MaterialId {
        let id = MaterialId(self.next_mat_id);
        self.next_mat_id += 1;
        id
    }

    pub fn next_mesh_id(&mut self) -> MeshId {
        let id = MeshId(self.next_mesh_id);
        self.next_mesh_id += 1;
        id
    }

    // ── Transform propagation ─────────────────────────────────────────────────

    /// Recompute all world matrices bottom-up.
    ///
    /// Must be called once per frame before culling or rendering.
    pub fn update_transforms(&mut self) {
        let n = self.nodes.len();
        self.world_matrices.resize(n, Mat4::IDENTITY);
        self.world_aabbs.resize(n, Aabb::UNIT);

        // Process in insertion order (parents always inserted before children).
        for i in 0..n {
            let parent_id = self.nodes[i].parent;
            let parent_world = if parent_id.is_valid() {
                self.node_index(parent_id)
                    .map(|pi| self.world_matrices[pi])
                    .unwrap_or(Mat4::IDENTITY)
            } else {
                Mat4::IDENTITY
            };
            let local = self.nodes[i].transform.local_matrix();
            self.world_matrices[i] = parent_world * local;
            self.world_aabbs[i] = self.nodes[i].local_aabb.transform(self.world_matrices[i]);
        }
    }

    // ── Frustum culling ───────────────────────────────────────────────────────

    /// Populate `visible_scratch` with indices of visible renderable nodes.
    pub fn cull(&mut self, camera: &mut Camera) {
        self.visible_scratch.clear();
        let frustum = camera.frustum();
        for (i, node) in self.nodes.iter().enumerate() {
            if !node.visible {
                continue;
            }
            if matches!(
                node.kind,
                NodeKind::Empty | NodeKind::Camera | NodeKind::Light
            ) {
                continue;
            }
            if frustum.test_aabb(self.world_aabbs[i]) {
                self.visible_scratch.push(i);
            }
        }
    }

    // ── Render graph construction ─────────────────────────────────────────────

    /// Build a `RenderGraph` from the culled visible set.
    ///
    /// Assigns each node to the correct pass based on its material.
    pub fn build_frame_graph(
        &self,
        backbuffer: graphos_gfx::command::ResourceId,
        depth_rt: graphos_gfx::command::ResourceId,
        w: u32,
        h: u32,
    ) -> RenderGraph {
        let mut graph = RenderGraph::standard(backbuffer, depth_rt, w, h);

        for &idx in &self.visible_scratch {
            let node = &self.nodes[idx];
            let mat_kind = node
                .material
                .and_then(|mid| self.materials.iter().find(|m| m.id == mid))
                .map(|m| m.kind)
                .unwrap_or(crate::material::MaterialKind::Unlit);

            use crate::material::MaterialKind as MK;
            let pass_kind = match mat_kind {
                MK::Pbr | MK::Unlit => {
                    // Transparent if material has alpha blend.
                    let alpha = node
                        .material
                        .and_then(|mid| self.materials.iter().find(|m| m.id == mid))
                        .map(|m| m.alpha_blend)
                        .unwrap_or(false);
                    if alpha {
                        PassKind::Transparent
                    } else {
                        PassKind::Opaque
                    }
                }
                MK::Glass => PassKind::Glass,
                MK::Panel => PassKind::Glass,
                MK::Ui => PassKind::Ui,
                MK::PostProcess => PassKind::PostProcess,
            };

            if let Some(pass) = graph.pass_mut(pass_kind) {
                pass.node_indices.push(idx);
            }
        }

        // Sort transparent back-to-front by world Z (camera space depth).
        // (simplified: sort by node z_order and AABB centre Z)
        if let Some(pass) = graph.pass_mut(PassKind::Transparent) {
            pass.node_indices.sort_by(|&a, &b| {
                let za = self.world_aabbs[a].centre().z;
                let zb = self.world_aabbs[b].centre().z;
                zb.partial_cmp(&za).unwrap_or(core::cmp::Ordering::Equal)
            });
        }

        graph
    }

    // ── Hit testing ───────────────────────────────────────────────────────────

    /// Find the closest interactive node intersected by `ray`.
    pub fn hit_test(&self, ray: Ray) -> Option<HitRecord> {
        let mut closest: Option<HitRecord> = None;

        for (i, node) in self.nodes.iter().enumerate() {
            if !node.visible || !node.interactive {
                continue;
            }
            let world = self.world_matrices[i];
            let aabb = self.world_aabbs[i];

            // First, broad-phase AABB test.
            let t = match ray_aabb(ray, aabb) {
                Some((t, _)) if t >= 0.0 => t,
                _ => continue,
            };

            // For Surface2D / Window / Panel: quad intersection.
            let hit_t = if matches!(
                node.kind,
                NodeKind::Surface2D | NodeKind::Window | NodeKind::Panel | NodeKind::GraphView
            ) {
                crate::hit::ray_quad(ray, world).unwrap_or(t)
            } else {
                t
            };

            if closest.map_or(true, |c| hit_t < c.t) {
                let pos = ray.at(hit_t);
                let normal = (pos - aabb.centre()).normalise();
                closest = Some(HitRecord {
                    node: node.id,
                    t: hit_t,
                    position: pos,
                    normal,
                    uv: [0.0; 2],
                });
            }
        }

        closest
    }

    // ── World matrix access ───────────────────────────────────────────────────

    pub fn world_matrix(&self, id: NodeId) -> Mat4 {
        self.node_index(id)
            .map(|i| self.world_matrices[i])
            .unwrap_or(Mat4::IDENTITY)
    }

    pub fn world_aabb(&self, id: NodeId) -> Aabb {
        self.node_index(id)
            .map(|i| self.world_aabbs[i])
            .unwrap_or(Aabb::UNIT)
    }
}

impl Default for SceneGraph {
    fn default() -> Self {
        Self::new()
    }
}
