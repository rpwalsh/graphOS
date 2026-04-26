// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! `graphos-scene` — 3D scene engine for the GraphOS spatial compositor.
//!
//! ## Architecture
//!
//! ```text
//! SceneGraph ──► Camera ──► Culler ──► RenderGraph ──► GlContext ──► kernel
//!     │                                    │
//!     ▼                                    ▼
//! SceneNode[]                         RenderPass[]
//!   transform                           geometry
//!   material                            post-process
//!   mesh/surface                        present
//! ```
//!
//! - `SceneGraph` owns all nodes in a flat array with parent-child links.
//! - `Transform3D` stores translation/rotation/scale and caches the world matrix.
//! - `Camera` computes view/projection matrices and owns a `Frustum` for culling.
//! - `RenderGraph` encodes the per-frame render pass sequence.
//! - `Animation` drives transform/material values with springs and keyframes.
//! - `HitTest` performs ray-scene intersection for pointer and spatial input.

#![no_std]
extern crate alloc;

pub mod aabb;
pub mod animation;
pub mod camera;
pub mod graph;
pub mod hit;
pub mod material;
pub mod math;
pub mod mesh;
pub mod node;
pub mod pass;
pub mod transform;

pub use aabb::Aabb;
pub use animation::{AnimTarget, Keyframe, Spring, Timeline};
pub use camera::{Camera, Frustum, Projection};
pub use graph::SceneGraph;
pub use hit::{HitRecord, Ray};
pub use material::{GlassMaterial, Material, MaterialKind};
pub use math::{Mat4, Quat, Vec2, Vec3, Vec4};
pub use mesh::{Mesh, Submesh};
pub use node::{NodeId, NodeKind, SceneNode};
pub use pass::{PassKind, RenderGraph, RenderPass};
pub use transform::Transform3D;
