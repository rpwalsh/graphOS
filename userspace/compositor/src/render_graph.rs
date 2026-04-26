// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Frame graph — typed resource handles, pass descriptors, and execution ordering.
//!
//! ## Architecture
//!
//! The frame graph is a **retained, acyclic directed graph** of rendering passes.
//! Each pass declares its read inputs and write outputs as typed resource handles.
//! `FrameGraph::compile()` performs a topological sort, detects resource lifetimes,
//! and produces a `CompiledGraph` whose `execute()` drives a `FrameExecutor`.
//!
//! ## Resource kinds
//!
//! | Handle type         | Description                                     |
//! |---------------------|-------------------------------------------------|
//! | `TextureHandle`     | Read-only GPU texture (surface import, atlas)   |
//! | `RenderTargetHandle`| GPU render target (read/write)                  |
//! | `BufferHandle`      | Linear GPU buffer (vertex, uniform, readback)   |
//!
//! ## Pass kinds
//!
//! | PassKind         | Description                                          |
//! |------------------|------------------------------------------------------|
//! | Geometry         | Draw panels, fills, borders into a render target     |
//! | Blur             | Dual-Kawase iterative blur pass (glassmorphism)      |
//! | Shadow           | Soft drop-shadow into a render target                |
//! | Composite        | Blend scene layers (back-to-front z-ordered)         |
//! | Bloom            | Luminance extract + blur + additive composite        |
//! | ToneMap          | HDR → display LDR (Reinhard or ACES filmic)          |
//! | Present          | Scanout: write render target to virtio-gpu scanout   |

extern crate alloc;
use alloc::vec::Vec;

// ── Resource handles ──────────────────────────────────────────────────────────

pub type ResourceId = u32;

/// Handle to a read-only GPU texture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextureHandle(pub ResourceId);

/// Handle to a GPU render target (read + write).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RenderTargetHandle(pub ResourceId);

/// Handle to a linear GPU buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BufferHandle(pub ResourceId);

// ── Pixel formats ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PixelFormat {
    Bgra8Unorm = 0,
    Rgba16Float = 1,
    R8Unorm = 2,  // single-channel (luma, alpha mask)
    Rg8Unorm = 3, // two-channel (normal XY, velocity)
}

// ── Resource descriptors ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct TextureDesc {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Mip level count.  1 = no mipmaps.
    pub mip_levels: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct RenderTargetDesc {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// True when this render target is also readable as a texture in subsequent passes.
    pub can_sample: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct BufferDesc {
    pub size_bytes: u32,
}

// ── Internal resource registry entry ─────────────────────────────────────────

#[derive(Debug)]
enum ResourceEntry {
    Texture(TextureDesc),
    RenderTarget(RenderTargetDesc),
    Buffer(BufferDesc),
}

// ── Pass input / output ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum PassRead {
    Texture(TextureHandle),
    RenderTarget(RenderTargetHandle),
    Buffer(BufferHandle),
}

#[derive(Clone, Copy, Debug)]
pub enum PassWrite {
    /// Write to a render target.
    RenderTarget(RenderTargetHandle),
    /// Final scanout — no explicit resource; writes to the display.
    Scanout,
}

// ── Pass kinds ────────────────────────────────────────────────────────────────

/// Dual-Kawase blur configuration.
#[derive(Clone, Copy, Debug)]
pub struct BlurConfig {
    /// Blur kernel radius (pixels at full resolution).
    pub radius: u32,
    /// Downsample factor before blurring (1 = no downsample, 2 = half-res, 4 = quarter).
    pub downsample: u32,
    /// Number of dual-Kawase iterations.  More = softer/wider blur.
    pub iterations: u32,
}

/// Bloom/glow configuration.
#[derive(Clone, Copy, Debug)]
pub struct BloomConfig {
    /// Luminance threshold: pixels brighter than this (0–255) are extracted.
    pub threshold: u8,
    /// Additive blend strength of the bloom contribution (0–255).
    pub intensity: u8,
    /// Blur config for the extracted bright pass.
    pub blur: BlurConfig,
}

/// Tone-mapping operator.
#[derive(Clone, Copy, Debug)]
pub enum ToneMapOp {
    /// Simple Reinhard global operator.
    Reinhard,
    /// ACES filmic approximation (Krzysztof Narkowicz).
    AcesFilmic,
    /// Neutral — no tone mapping (pass-through for LDR paths).
    Neutral,
}

/// Post-processing vignette.
#[derive(Clone, Copy, Debug)]
pub struct VignetteConfig {
    /// Strength 0–255 (0 = off, 255 = full black at corners).
    pub strength: u8,
    /// Vignette radius as fraction of half-diagonal (x256; 256 = 1.0).
    pub radius_fp: u8,
    /// Feather softness (x256; larger = softer edge).
    pub feather_fp: u8,
}

/// What the pass computes.
#[derive(Clone, Copy, Debug)]
pub enum PassKind {
    /// Draw solid fills, gradients, borders into the output render target.
    Geometry,
    /// Dual-Kawase iterative blur.
    Blur(BlurConfig),
    /// Soft drop shadow.
    Shadow {
        blur: BlurConfig,
        color: u32,
        offset_x: i16,
        offset_y: i16,
    },
    /// Alpha-composite scene layers (back-to-front, z-sorted).
    Composite,
    /// Luminance extract + blur + additive blend.
    Bloom(BloomConfig),
    /// HDR → LDR tone mapping.
    ToneMap { op: ToneMapOp, exposure_fp: u16 },
    /// Vignette post-process.
    Vignette(VignetteConfig),
    /// Write to the display scanout buffer.
    Present,
}

// ── Pass descriptor ───────────────────────────────────────────────────────────

/// Maximum read inputs per pass.
pub const MAX_PASS_READS: usize = 8;

pub struct PassDescriptor {
    pub name: &'static str,
    pub kind: PassKind,
    pub reads: [Option<PassRead>; MAX_PASS_READS],
    pub writes: PassWrite,
}

impl PassDescriptor {
    pub fn new(name: &'static str, kind: PassKind, writes: PassWrite) -> Self {
        Self {
            name,
            kind,
            reads: [None; MAX_PASS_READS],
            writes,
        }
    }

    pub fn read(mut self, input: PassRead) -> Self {
        for slot in &mut self.reads {
            if slot.is_none() {
                *slot = Some(input);
                break;
            }
        }
        self
    }
}

// ── Frame graph builder ───────────────────────────────────────────────────────

pub struct FrameGraph {
    resources: Vec<ResourceEntry>,
    passes: Vec<PassDescriptor>,
    next_id: ResourceId,
}

impl FrameGraph {
    pub fn new() -> Self {
        Self {
            resources: Vec::new(),
            passes: Vec::new(),
            next_id: 1,
        }
    }

    fn alloc_id(&mut self) -> ResourceId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Declare a new GPU texture resource.
    pub fn create_texture(&mut self, desc: TextureDesc) -> TextureHandle {
        let id = self.alloc_id();
        self.resources.push(ResourceEntry::Texture(desc));
        TextureHandle(id)
    }

    /// Declare a new render target resource.
    pub fn create_render_target(&mut self, desc: RenderTargetDesc) -> RenderTargetHandle {
        let id = self.alloc_id();
        self.resources.push(ResourceEntry::RenderTarget(desc));
        RenderTargetHandle(id)
    }

    /// Declare a new linear GPU buffer.
    pub fn create_buffer(&mut self, desc: BufferDesc) -> BufferHandle {
        let id = self.alloc_id();
        self.resources.push(ResourceEntry::Buffer(desc));
        BufferHandle(id)
    }

    /// Add a pass to the graph.
    pub fn add_pass(&mut self, pass: PassDescriptor) {
        self.passes.push(pass);
    }

    /// Compile the graph into an ordered execution plan.
    ///
    /// Currently performs a simple in-order topological sort (passes are
    /// assumed to be added in dependency order by the builder).  A full
    /// dependency analysis will be added in the next iteration.
    pub fn compile(self) -> CompiledGraph {
        let n = self.passes.len();
        CompiledGraph {
            ordered: (0..n).collect(),
            graph: self,
        }
    }
}

impl Default for FrameGraph {
    fn default() -> Self {
        Self::new()
    }
}

// ── Compiled graph ────────────────────────────────────────────────────────────

/// Result of `FrameGraph::compile()`.
pub struct CompiledGraph {
    ordered: Vec<usize>,
    graph: FrameGraph,
}

impl CompiledGraph {
    /// Execute all passes in dependency order.
    ///
    /// `executor` is the backend implementation — either a CPU blitter or a GPU
    /// command encoder.
    pub fn execute(&self, executor: &mut dyn FrameExecutor) {
        for &idx in &self.ordered {
            let pass = &self.graph.passes[idx];
            executor.execute_pass(pass);
        }
    }

    /// Access the underlying frame graph (e.g. for resource queries).
    pub fn graph(&self) -> &FrameGraph {
        &self.graph
    }
}

// ── Frame executor trait ──────────────────────────────────────────────────────

/// Drives execution of a compiled frame graph.
///
/// Implementations provide either CPU-side software rendering (for the virtio-2D
/// path) or GPU command encoding (for the native hardware path).
pub trait FrameExecutor {
    /// Called once before the first pass in a frame.
    fn begin_frame(&mut self, screen_w: u32, screen_h: u32);

    /// Called for each pass in execution order.
    fn execute_pass(&mut self, pass: &PassDescriptor);

    /// Called after the final pass; flushes output to virtio-gpu.
    fn end_frame(&mut self);
}

// ── Standard desktop frame graph builder ─────────────────────────────────────

/// Build the standard GraphOS desktop frame graph.
///
/// Declares the following passes in order:
/// 1. Wallpaper / background geometry pass
/// 2. Blur pass (background blur source for glass panels)
/// 3. Composite pass (assembles all layers)
/// 4. Shadow pass (per-window drop shadows)
/// 5. Bloom pass (optional; active when any surface has bloom material)
/// 6. Tone-map pass
/// 7. Vignette pass
/// 8. Present pass (scanout)
pub fn build_desktop_frame_graph(screen_w: u32, screen_h: u32) -> CompiledGraph {
    let mut fg = FrameGraph::new();

    // Resource declarations
    let rt_bg = fg.create_render_target(RenderTargetDesc {
        width: screen_w,
        height: screen_h,
        format: PixelFormat::Bgra8Unorm,
        can_sample: true,
    });
    let rt_blur_src = fg.create_render_target(RenderTargetDesc {
        width: screen_w / 2,
        height: screen_h / 2,
        format: PixelFormat::Bgra8Unorm,
        can_sample: true,
    });
    let rt_blur_out = fg.create_render_target(RenderTargetDesc {
        width: screen_w / 2,
        height: screen_h / 2,
        format: PixelFormat::Bgra8Unorm,
        can_sample: true,
    });
    let rt_shadow = fg.create_render_target(RenderTargetDesc {
        width: screen_w,
        height: screen_h,
        format: PixelFormat::Bgra8Unorm,
        can_sample: true,
    });
    let rt_composite = fg.create_render_target(RenderTargetDesc {
        width: screen_w,
        height: screen_h,
        format: PixelFormat::Rgba16Float, // HDR accumulation
        can_sample: true,
    });
    let rt_bloom = fg.create_render_target(RenderTargetDesc {
        width: screen_w / 4,
        height: screen_h / 4,
        format: PixelFormat::Rgba16Float,
        can_sample: true,
    });
    let rt_tonemap = fg.create_render_target(RenderTargetDesc {
        width: screen_w,
        height: screen_h,
        format: PixelFormat::Bgra8Unorm,
        can_sample: true,
    });

    // 1. Wallpaper / background geometry
    fg.add_pass(PassDescriptor::new(
        "background-geometry",
        PassKind::Geometry,
        PassWrite::RenderTarget(rt_bg),
    ));

    // 2. Background downsample for blur source
    fg.add_pass(
        PassDescriptor::new(
            "blur-downsample",
            PassKind::Geometry,
            PassWrite::RenderTarget(rt_blur_src),
        )
        .read(PassRead::RenderTarget(rt_bg)),
    );

    // 3. Dual-Kawase blur (for glassmorphism panels)
    fg.add_pass(
        PassDescriptor::new(
            "background-blur",
            PassKind::Blur(BlurConfig {
                radius: 24,
                downsample: 2,
                iterations: 5,
            }),
            PassWrite::RenderTarget(rt_blur_out),
        )
        .read(PassRead::RenderTarget(rt_blur_src)),
    );

    // 4. Shadow pass (all window shadows, back-to-front)
    fg.add_pass(
        PassDescriptor::new(
            "shadow",
            PassKind::Shadow {
                blur: BlurConfig {
                    radius: 16,
                    downsample: 2,
                    iterations: 3,
                },
                color: 0xB0000000,
                offset_x: 0,
                offset_y: 8,
            },
            PassWrite::RenderTarget(rt_shadow),
        )
        .read(PassRead::RenderTarget(rt_bg)),
    );

    // 5. Composite (all layers: bg + blurred bg + surfaces + shadows + overlays)
    fg.add_pass(
        PassDescriptor::new(
            "composite",
            PassKind::Composite,
            PassWrite::RenderTarget(rt_composite),
        )
        .read(PassRead::RenderTarget(rt_bg))
        .read(PassRead::RenderTarget(rt_blur_out))
        .read(PassRead::RenderTarget(rt_shadow)),
    );

    // 6. Bloom
    fg.add_pass(
        PassDescriptor::new(
            "bloom",
            PassKind::Bloom(BloomConfig {
                threshold: 220,
                intensity: 60,
                blur: BlurConfig {
                    radius: 8,
                    downsample: 4,
                    iterations: 4,
                },
            }),
            PassWrite::RenderTarget(rt_bloom),
        )
        .read(PassRead::RenderTarget(rt_composite)),
    );

    // 7. Tone-map (HDR → LDR, incorporates bloom)
    fg.add_pass(
        PassDescriptor::new(
            "tonemap",
            PassKind::ToneMap {
                op: ToneMapOp::AcesFilmic,
                exposure_fp: 256,
            },
            PassWrite::RenderTarget(rt_tonemap),
        )
        .read(PassRead::RenderTarget(rt_composite))
        .read(PassRead::RenderTarget(rt_bloom)),
    );

    // 8. Vignette
    fg.add_pass(
        PassDescriptor::new(
            "vignette",
            PassKind::Vignette(VignetteConfig {
                strength: 40,
                radius_fp: 180,
                feather_fp: 80,
            }),
            PassWrite::RenderTarget(rt_tonemap),
        )
        .read(PassRead::RenderTarget(rt_tonemap)),
    );

    // 9. Present to scanout
    fg.add_pass(
        PassDescriptor::new("present", PassKind::Present, PassWrite::Scanout)
            .read(PassRead::RenderTarget(rt_tonemap)),
    );

    fg.compile()
}
