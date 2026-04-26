// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Render graph — frame pass sequence.

extern crate alloc;
use alloc::vec::Vec;
use graphos_gfx::command::ResourceId;

// ── Pass kinds ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    /// Depth pre-pass — writes depth only, no color.
    DepthPrepass,
    /// Opaque geometry.
    Opaque,
    /// Transparent / alpha-blended geometry.
    Transparent,
    /// Frosted glass / blur-backed surfaces.
    Glass,
    /// UI / HUD overlay.
    Ui,
    /// Post-process effects (bloom, tone-map, vignette).
    PostProcess,
    /// Final composite and present.
    Present,
}

// ── Render pass descriptor ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RenderPass {
    pub kind: PassKind,
    pub name: [u8; 32],
    /// Color render target.  `INVALID` = backbuffer.
    pub color: ResourceId,
    /// Depth render target.  `INVALID` = no depth.
    pub depth: ResourceId,
    pub clear_color: bool,
    pub clear_depth: bool,
    pub clear_stencil: bool,
    /// Nodes to render in this pass (by index into SceneGraph::nodes).
    pub node_indices: Vec<usize>,
}

impl RenderPass {
    pub fn new(kind: PassKind) -> Self {
        Self {
            kind,
            name: [0u8; 32],
            color: ResourceId::INVALID,
            depth: ResourceId::INVALID,
            clear_color: false,
            clear_depth: false,
            clear_stencil: false,
            node_indices: Vec::new(),
        }
    }

    pub fn set_name(&mut self, s: &str) {
        let b = s.as_bytes();
        let len = b.len().min(31);
        self.name[..len].copy_from_slice(&b[..len]);
    }

    pub fn with_color(mut self, rt: ResourceId, clear: bool) -> Self {
        self.color = rt;
        self.clear_color = clear;
        self
    }
    pub fn with_depth(mut self, rt: ResourceId, clear: bool) -> Self {
        self.depth = rt;
        self.clear_depth = clear;
        self
    }
}

// ── RenderGraph ───────────────────────────────────────────────────────────────

/// Ordered sequence of render passes for one frame.
///
/// Built by `SceneGraph::build_frame_graph()` after frustum culling.
pub struct RenderGraph {
    pub passes: Vec<RenderPass>,
    pub backbuffer: ResourceId,
    pub width: u32,
    pub height: u32,
}

impl RenderGraph {
    pub fn new(backbuffer: ResourceId, width: u32, height: u32) -> Self {
        Self {
            passes: Vec::new(),
            backbuffer,
            width,
            height,
        }
    }

    /// Standard 3D + UI + present graph.
    pub fn standard(backbuffer: ResourceId, depth: ResourceId, w: u32, h: u32) -> Self {
        let mut g = Self::new(backbuffer, w, h);

        let mut depth_pre = RenderPass::new(PassKind::DepthPrepass);
        depth_pre.set_name("depth-prepass");
        depth_pre.color = ResourceId::INVALID;
        depth_pre.depth = depth;
        depth_pre.clear_depth = true;

        let mut opaque = RenderPass::new(PassKind::Opaque);
        opaque.set_name("opaque");
        opaque.color = backbuffer;
        opaque.depth = depth;
        opaque.clear_color = true;

        let mut glass = RenderPass::new(PassKind::Glass);
        glass.set_name("glass");
        glass.color = backbuffer;
        glass.depth = depth;

        let mut transparent = RenderPass::new(PassKind::Transparent);
        transparent.set_name("transparent");
        transparent.color = backbuffer;
        transparent.depth = depth;

        let mut ui = RenderPass::new(PassKind::Ui);
        ui.set_name("ui");
        ui.color = backbuffer;

        let mut post = RenderPass::new(PassKind::PostProcess);
        post.set_name("post");
        post.color = backbuffer;

        let mut present = RenderPass::new(PassKind::Present);
        present.set_name("present");
        present.color = backbuffer;

        g.passes
            .extend_from_slice(&[depth_pre, opaque, glass, transparent, ui, post, present]);
        g
    }

    pub fn pass_mut(&mut self, kind: PassKind) -> Option<&mut RenderPass> {
        self.passes.iter_mut().find(|p| p.kind == kind)
    }
}
