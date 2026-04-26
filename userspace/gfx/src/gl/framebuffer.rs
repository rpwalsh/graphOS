// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Framebuffer Objects — FBO + renderbuffer attachment management.

extern crate alloc;
use crate::command::ResourceId;
use alloc::vec::Vec;

// ── Attachment points ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AttachPoint {
    Color0 = 0,
    Color1 = 1,
    Color2 = 2,
    Color3 = 3,
    Depth = 4,
    Stencil = 5,
    DepthStencil = 6,
}

impl AttachPoint {
    pub fn is_color(self) -> bool {
        (self as u8) < 4
    }
    pub fn is_depth(self) -> bool {
        matches!(self, Self::Depth | Self::DepthStencil)
    }
}

// ── Attachment descriptor ─────────────────────────────────────────────────────

/// Describes a single attachment bound to an FBO.
#[derive(Debug, Clone, Copy)]
pub enum AttachmentSource {
    /// Attach a texture at the given mip level and array layer.
    Texture {
        resource: ResourceId,
        level: u8,
        layer: u32,
    },
    /// Attach a renderbuffer.
    Renderbuf { resource: ResourceId },
}

#[derive(Debug, Clone, Copy)]
pub struct Attachment {
    pub point: AttachPoint,
    pub source: AttachmentSource,
}

// ── Framebuffer status ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FboStatus {
    Complete = 0,
    Undefined = 1,
    IncompleteAttachment = 2,
    MissingAttachment = 3,
    UnsupportedFormat = 4,
    MultisampleMismatch = 5,
}

// ── GlFbo ─────────────────────────────────────────────────────────────────────

/// A Framebuffer Object.
///
/// Wraps one or more `ResourceId` color/depth attachments.
/// The kernel executor uses the bound attachments as render targets.
pub struct GlFbo {
    pub(crate) name: u32,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Flat kernel resource IDs for fast submission.
    pub(crate) color_rt: [ResourceId; 4],
    pub(crate) depth_rt: ResourceId,
}

impl GlFbo {
    pub(crate) fn new(name: u32) -> Self {
        Self {
            name,
            attachments: Vec::new(),
            width: 0,
            height: 0,
            color_rt: [ResourceId::INVALID; 4],
            depth_rt: ResourceId::INVALID,
        }
    }

    pub fn name(&self) -> u32 {
        self.name
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn color_rt(&self, slot: usize) -> ResourceId {
        self.color_rt[slot]
    }
    pub fn depth_rt(&self) -> ResourceId {
        self.depth_rt
    }

    /// Attach a texture or renderbuffer.
    pub fn attach(&mut self, att: Attachment, res_w: u32, res_h: u32) {
        let resource = match att.source {
            AttachmentSource::Texture { resource, .. } => resource,
            AttachmentSource::Renderbuf { resource } => resource,
        };
        match att.point {
            AttachPoint::Color0
            | AttachPoint::Color1
            | AttachPoint::Color2
            | AttachPoint::Color3 => {
                let slot = att.point as usize;
                self.color_rt[slot] = resource;
            }
            AttachPoint::Depth | AttachPoint::DepthStencil | AttachPoint::Stencil => {
                self.depth_rt = resource;
            }
        }
        self.attachments.push(att);
        self.width = self.width.max(res_w);
        self.height = self.height.max(res_h);
    }

    /// Validate the FBO — mirrors `glCheckFramebufferStatus`.
    pub fn status(&self) -> FboStatus {
        if self.color_rt[0].is_valid() || self.depth_rt.is_valid() {
            FboStatus::Complete
        } else {
            FboStatus::MissingAttachment
        }
    }
}
