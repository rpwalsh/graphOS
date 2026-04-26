// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GPU command protocol — the typed contract between the userspace compositor
//! and the kernel GPU executor.
//!
//! ## Design
//!
//! `GpuCmd` is an enum of every high-level rendering operation. It is
//! serialised into a `#[repr(C)]` wire format (see `wire.rs`) and submitted
//! to the kernel via `SYS_GPU_SUBMIT`. The kernel executor translates each
//! command to GraphOS-supported hardware operations:
//!
//! | Stage | Backend                | Mechanism                                  |
//! |-------|------------------------|--------------------------------------------|
//! | 1     | virtio-gpu scanout     | Pixel operations on framebuffer + FLUSH    |
//! | 2     | native GraphOS GPU     | GraphOS command packets via SYS_GPU_SUBMIT |
//!
//! Userspace is insulated from both. The protocol is stable across the
//! scanout-only to native-GPU transition; no compositor code changes are
//! required.
//!
//! ## Resource model
//!
//! Every GPU-visible object (texture, render target, depth buffer) has a
//! `ResourceId` — an opaque `u32` assigned by the kernel on `AllocResource`.
//! Resources are freed with `FreeResource`.  The compositor owns resource
//! lifetimes through the RAII wrappers in `resource.rs`.

extern crate alloc;
use alloc::vec::Vec;

use crate::types::{
    BlendState, BufferKind, CullMode, DepthState, FillMode, IndexFormat, RasterState, Topology,
    VertexLayout,
};

// ── Resource handle ───────────────────────────────────────────────────────────

/// Opaque handle to a kernel GPU resource.
///
/// Assigned by the kernel on `GpuCmd::AllocResource` and echoed back in the
/// response.  Valid until `GpuCmd::FreeResource` is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceId(pub u32);

impl ResourceId {
    pub const INVALID: Self = Self(0);
    #[inline]
    pub fn is_valid(self) -> bool {
        self.0 != 0
    }
}

// ── Color ─────────────────────────────────────────────────────────────────────

/// ARGB8 color (A in high byte, B in low byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Self = Self(0x00000000);
    pub const BLACK: Self = Self(0xFF000000);
    pub const WHITE: Self = Self(0xFFFFFFFF);

    #[inline]
    pub fn argb(a: u8, r: u8, g: u8, b: u8) -> Self {
        Self(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
    }

    #[inline]
    pub fn with_alpha(self, a: u8) -> Self {
        Self((self.0 & 0x00FF_FFFF) | ((a as u32) << 24))
    }

    #[inline]
    pub fn alpha(self) -> u8 {
        (self.0 >> 24) as u8
    }
    #[inline]
    pub fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }
    #[inline]
    pub fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }
    #[inline]
    pub fn b(self) -> u8 {
        self.0 as u8
    }
}

// ── Rect ──────────────────────────────────────────────────────────────────────

/// Axis-aligned rectangle in screen pixels.  (0, 0) is top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const ZERO: Self = Self {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    #[inline]
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
    #[inline]
    pub fn screen(w: u32, h: u32) -> Self {
        Self { x: 0, y: 0, w, h }
    }
    #[inline]
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn intersect(self, o: Self) -> Option<Self> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w as i32).min(o.x + o.w as i32);
        let y1 = (self.y + self.h as i32).min(o.y + o.h as i32);
        if x1 > x0 && y1 > y0 {
            Some(Self::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
        } else {
            None
        }
    }

    pub fn union(self, o: Self) -> Self {
        if self.is_empty() {
            return o;
        }
        if o.is_empty() {
            return self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w as i32).max(o.x + o.w as i32);
        let y1 = (self.y + self.h as i32).max(o.y + o.h as i32);
        Self::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }

    pub fn expand(self, px: i32) -> Self {
        Self::new(
            self.x - px,
            self.y - px,
            (self.w as i32 + px * 2).max(0) as u32,
            (self.h as i32 + px * 2).max(0) as u32,
        )
    }
}

// ── Enums ─────────────────────────────────────────────────────────────────────

/// Gradient direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GradientDir {
    TopToBottom = 0,
    LeftToRight = 1,
    Diagonal = 2,
    Radial = 3,
}

/// Porter-Duff / compositing blend mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BlendMode {
    SrcOver = 0, // default — pre-multiplied alpha src-over
    Add = 1,     // additive (glow, bloom)
    Multiply = 2,
    Screen = 3,
}

/// GPU resource pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PixelFormat {
    Bgra8Unorm = 0,
    Rgba8Unorm = 1,
    A8Unorm = 2,
    Rgba16Float = 3,
}

/// GPU resource kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResourceKind {
    Texture2D = 0,    // read-only once uploaded
    RenderTarget = 1, // writable + samplable
    DepthStencil = 2,
}

// ── GpuCmd ────────────────────────────────────────────────────────────────────

/// Typed GPU command — the unit of work from the compositor to the kernel.
///
/// Commands are submitted as a batch via `SYS_GPU_SUBMIT`.  The kernel executor
/// processes them in order.  All resource handles (`ResourceId`) must be valid
/// at submission time; the kernel validates this before executing any command.
///
/// No pixels, no raw pointers, no hardware-specific encoding in this type.
/// All of that is the kernel executor's concern.
#[derive(Debug, Clone)]
pub enum GpuCmd {
    // ── Resource lifecycle ────────────────────────────────────────────────────
    /// Allocate a GPU resource.
    ///
    /// The kernel assigns the `ResourceId` and writes it back to the response
    /// slot.  The compositor receives it via `GpuContext::alloc_resource()`.
    AllocResource {
        width: u32,
        height: u32,
        format: PixelFormat,
        kind: ResourceKind,
    },

    /// Release a GPU resource.  Must not be in-flight.
    FreeResource { id: ResourceId },

    // ── Frame control ─────────────────────────────────────────────────────────
    /// Begin compositing a frame into `target`.
    BeginFrame {
        target: ResourceId,
        /// Optional clear color.  `None` = preserve previous content.
        clear: Option<Color>,
    },

    /// End compositing.  Marks the resource as ready for `BlitResource`/`Present`.
    EndFrame { target: ResourceId },

    // ── Draw operations ───────────────────────────────────────────────────────
    /// Fill a rectangle with a solid color.  `radius` is corner radius in px.
    FillRect {
        target: ResourceId,
        rect: Rect,
        color: Color,
        radius: u8,
        blend: BlendMode,
    },

    /// Fill a rectangle with a two-stop linear/radial gradient.
    FillGradient {
        target: ResourceId,
        rect: Rect,
        color_a: Color,
        color_b: Color,
        dir: GradientDir,
        blend: BlendMode,
    },

    /// Draw a rectangular border (inset by `width/2`).
    DrawBorder {
        target: ResourceId,
        rect: Rect,
        color: Color,
        width: u8,
        radius: u8,
        blend: BlendMode,
    },

    /// Draw a blurred drop shadow behind `rect`.
    DrawShadow {
        target: ResourceId,
        rect: Rect,
        color: Color,
        offset_x: i8,
        offset_y: i8,
        /// Approximate Gaussian sigma in pixels.
        sigma: u8,
        spread: i8,
    },

    // ── Resource transfer ─────────────────────────────────────────────────────
    /// Blit from `src_rect` in `src` into `dst_rect` in `dst`.
    ///
    /// Handles scaling, opacity, and blend mode.  Neither resource must be
    /// currently executing.
    BlitResource {
        src: ResourceId,
        src_rect: Rect,
        dst: ResourceId,
        dst_rect: Rect,
        opacity: u8,
        blend: BlendMode,
    },

    /// Zero-copy import of a ring-3 app surface backing store.
    ///
    /// The kernel maps the physical pages of `surface_id`'s pixel buffer into
    /// the GPU resource `dst` without any CPU copy.  After this command, `dst`
    /// reflects the current surface content and can be used as a `BlitResource`
    /// source.
    ImportSurface { surface_id: u32, dst: ResourceId },

    /// Apply a Gaussian blur to `rect` within `target`, writing in-place.
    BlurRegion {
        target: ResourceId,
        rect: Rect,
        /// Gaussian sigma in pixels.
        sigma: u8,
        /// Number of box-blur passes (≥ 3 approximates Gaussian).
        passes: u8,
    },

    /// Alpha-composite `src` over `dst`.
    Composite {
        src: ResourceId,
        dst: ResourceId,
        rect: Rect,
        blend: BlendMode,
        opacity: u8,
    },

    // ── Post-processing ───────────────────────────────────────────────────────
    /// Bloom: extract luminance above `threshold`, blur, additively blend onto `dst`.
    Bloom {
        src: ResourceId,
        dst: ResourceId,
        threshold: u8,
        intensity: u8,
    },

    /// Vignette: darken edges of `target` with `color` at `strength` (0–255).
    Vignette {
        target: ResourceId,
        strength: u8,
        color: Color,
    },

    // ── Buffer management ──────────────────────────────────────────────────────
    /// Allocate a GPU buffer (vertex, index, uniform, or storage).
    ///
    /// The returned `ResourceId` can be used in `UploadBuffer` and
    /// `DrawPrimitives`. Freed with `FreeResource`.
    AllocBuffer {
        kind: BufferKind,
        /// Size in bytes.
        size: u32,
    },

    /// Upload bytes from a user-space pointer into a GPU buffer.
    ///
    /// The kernel copies `data_len` bytes from `data_ptr` (user VA) into
    /// `dst` at byte offset `offset`.  `dst` must be a buffer resource.
    UploadBuffer {
        dst: ResourceId,
        offset: u32,
        /// User-space virtual address of source data.
        data_ptr: u64,
        data_len: u32,
    },

    // ── Render target ─────────────────────────────────────────────────────────
    /// Set the active color render target and optional depth buffer.
    ///
    /// All subsequent draw calls write into `color`.  Pass
    /// `ResourceId::INVALID` for `depth` when depth testing is not needed.
    SetRenderTarget {
        color: ResourceId,
        depth: ResourceId,
    },

    /// Clear the depth buffer to `depth` (typically 1.0 for far plane).
    ClearDepth { depth: f32 },

    // ── Pipeline state ────────────────────────────────────────────────────────
    /// Set the viewport transform for subsequent draw calls.
    SetViewport {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        min_depth: f32,
        max_depth: f32,
    },

    /// Set the scissor rectangle.  Fragments outside are discarded.
    SetScissor { rect: Rect },

    /// Set the MVP transform matrix for the next geometry draw.
    ///
    /// Stored as row-major `[[f32; 4]; 4]`.  The kernel executor multiplies
    /// each vertex position by this matrix before rasterisation.
    SetTransform { matrix: [[f32; 4]; 4] },

    /// Set per-attachment blend state.
    SetBlendState { state: BlendState },

    /// Set depth test / write configuration.
    SetDepthState { state: DepthState },

    /// Set rasterizer configuration.
    SetRasterState { state: RasterState },

    // ── Texture binding ───────────────────────────────────────────────────────
    /// Bind a GPU image/texture to a texture slot (0–7).
    BindTexture { slot: u8, resource: ResourceId },

    /// Select the built-in shader mode for subsequent draws.
    SetShaderHint { hint: u8 },

    /// Set a 16-byte uniform value (can encode floats, vec4, mat-row).
    SetUniform { slot: u8, value: [u32; 4] },

    // ── Geometry draw ─────────────────────────────────────────────────────────
    /// Rasterize primitives from vertex/index buffers.
    ///
    /// Phase 1: the kernel executor software-rasterises into the active
    /// `RenderTarget`.  Phase 2: translated to hardware draw commands.
    DrawPrimitives {
        vertices: ResourceId,
        /// `ResourceId::INVALID` → draw without index buffer.
        indices: ResourceId,
        layout: VertexLayout,
        index_fmt: IndexFormat,
        topology: Topology,
        first: u32,
        count: u32,
        instances: u32,
    },

    // ── Synchronization ───────────────────────────────────────────────────────
    /// Insert a fence signal into the command stream.
    ///
    /// When the kernel executor reaches this command, it signals the fence
    /// so that CPU code waiting on `Fence::wait()` can proceed.
    Signal { fence: crate::sync::FenceId },

    // ── Present ───────────────────────────────────────────────────────────────
    /// Submit `src` to the display.
    ///
    /// Triggers `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH`
    /// in the current scanout path, or a native display flip once the hardware
    /// backend exists.
    Present { src: ResourceId },
}

// ── CommandBuffer ─────────────────────────────────────────────────────────────

/// An ordered list of `GpuCmd`s submitted atomically to the kernel.
///
/// Build with the fluent API; submit via `GpuContext::submit()`.
pub struct CommandBuffer {
    cmds: Vec<GpuCmd>,
}

impl CommandBuffer {
    pub fn new() -> Self {
        Self {
            cmds: Vec::with_capacity(64),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cmds: Vec::with_capacity(cap),
        }
    }

    #[inline]
    pub fn push(&mut self, cmd: GpuCmd) {
        self.cmds.push(cmd);
    }
    #[inline]
    pub fn clear(&mut self) {
        self.cmds.clear();
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.cmds.len()
    }
    #[inline]
    pub fn cmds(&self) -> &[GpuCmd] {
        &self.cmds
    }

    // ── Fluent builders ───────────────────────────────────────────────────────

    pub fn begin_frame(&mut self, target: ResourceId, clear: Option<Color>) -> &mut Self {
        self.push(GpuCmd::BeginFrame { target, clear });
        self
    }

    pub fn end_frame(&mut self, target: ResourceId) -> &mut Self {
        self.push(GpuCmd::EndFrame { target });
        self
    }

    pub fn fill_rect(
        &mut self,
        target: ResourceId,
        rect: Rect,
        color: Color,
        radius: u8,
    ) -> &mut Self {
        self.push(GpuCmd::FillRect {
            target,
            rect,
            color,
            radius,
            blend: BlendMode::SrcOver,
        });
        self
    }

    pub fn fill_gradient(
        &mut self,
        target: ResourceId,
        rect: Rect,
        a: Color,
        b: Color,
        dir: GradientDir,
    ) -> &mut Self {
        self.push(GpuCmd::FillGradient {
            target,
            rect,
            color_a: a,
            color_b: b,
            dir,
            blend: BlendMode::SrcOver,
        });
        self
    }

    pub fn border(
        &mut self,
        target: ResourceId,
        rect: Rect,
        color: Color,
        w: u8,
        r: u8,
    ) -> &mut Self {
        self.push(GpuCmd::DrawBorder {
            target,
            rect,
            color,
            width: w,
            radius: r,
            blend: BlendMode::SrcOver,
        });
        self
    }

    pub fn shadow(
        &mut self,
        target: ResourceId,
        rect: Rect,
        color: Color,
        ox: i8,
        oy: i8,
        sigma: u8,
        spread: i8,
    ) -> &mut Self {
        self.push(GpuCmd::DrawShadow {
            target,
            rect,
            color,
            offset_x: ox,
            offset_y: oy,
            sigma,
            spread,
        });
        self
    }

    pub fn blit(
        &mut self,
        src: ResourceId,
        src_rect: Rect,
        dst: ResourceId,
        dst_rect: Rect,
        opacity: u8,
    ) -> &mut Self {
        self.push(GpuCmd::BlitResource {
            src,
            src_rect,
            dst,
            dst_rect,
            opacity,
            blend: BlendMode::SrcOver,
        });
        self
    }

    pub fn import_surface(&mut self, surface_id: u32, dst: ResourceId) -> &mut Self {
        self.push(GpuCmd::ImportSurface { surface_id, dst });
        self
    }

    pub fn blur(&mut self, target: ResourceId, rect: Rect, sigma: u8) -> &mut Self {
        self.push(GpuCmd::BlurRegion {
            target,
            rect,
            sigma,
            passes: 3,
        });
        self
    }

    pub fn composite(
        &mut self,
        src: ResourceId,
        dst: ResourceId,
        rect: Rect,
        opacity: u8,
    ) -> &mut Self {
        self.push(GpuCmd::Composite {
            src,
            dst,
            rect,
            blend: BlendMode::SrcOver,
            opacity,
        });
        self
    }

    pub fn bloom(
        &mut self,
        src: ResourceId,
        dst: ResourceId,
        threshold: u8,
        intensity: u8,
    ) -> &mut Self {
        self.push(GpuCmd::Bloom {
            src,
            dst,
            threshold,
            intensity,
        });
        self
    }

    pub fn vignette(&mut self, target: ResourceId, strength: u8) -> &mut Self {
        self.push(GpuCmd::Vignette {
            target,
            strength,
            color: Color::BLACK,
        });
        self
    }

    pub fn present(&mut self, src: ResourceId) -> &mut Self {
        self.push(GpuCmd::Present { src });
        self
    }

    // ── 3D / pipeline builders ────────────────────────────────────────────────

    pub fn set_render_target(&mut self, color: ResourceId, depth: ResourceId) -> &mut Self {
        self.push(GpuCmd::SetRenderTarget { color, depth });
        self
    }

    pub fn set_viewport(&mut self, x: f32, y: f32, w: f32, h: f32) -> &mut Self {
        self.push(GpuCmd::SetViewport {
            x,
            y,
            w,
            h,
            min_depth: 0.0,
            max_depth: 1.0,
        });
        self
    }

    pub fn set_scissor(&mut self, rect: Rect) -> &mut Self {
        self.push(GpuCmd::SetScissor { rect });
        self
    }

    pub fn set_transform(&mut self, m: [[f32; 4]; 4]) -> &mut Self {
        self.push(GpuCmd::SetTransform { matrix: m });
        self
    }

    pub fn bind_texture(&mut self, slot: u8, resource: ResourceId) -> &mut Self {
        self.push(GpuCmd::BindTexture { slot, resource });
        self
    }

    pub fn set_shader_hint(&mut self, hint: u8) -> &mut Self {
        self.push(GpuCmd::SetShaderHint { hint });
        self
    }

    pub fn set_uniform(&mut self, slot: u8, value: [u32; 4]) -> &mut Self {
        self.push(GpuCmd::SetUniform { slot, value });
        self
    }

    pub fn set_blend(&mut self, state: BlendState) -> &mut Self {
        self.push(GpuCmd::SetBlendState { state });
        self
    }

    pub fn set_depth(&mut self, state: DepthState) -> &mut Self {
        self.push(GpuCmd::SetDepthState { state });
        self
    }

    pub fn set_raster_state(&mut self, state: RasterState) -> &mut Self {
        self.push(GpuCmd::SetRasterState { state });
        self
    }

    pub fn set_blend_state(&mut self, state: BlendState) -> &mut Self {
        self.set_blend(state)
    }

    pub fn set_depth_state(&mut self, state: DepthState) -> &mut Self {
        self.set_depth(state)
    }

    pub fn clear_depth(&mut self, value: f32) -> &mut Self {
        self.push(GpuCmd::ClearDepth { depth: value });
        self
    }

    pub fn free_resource(&mut self, id: ResourceId) -> &mut Self {
        self.push(GpuCmd::FreeResource { id });
        self
    }

    pub fn draw_primitives(
        &mut self,
        vertices: ResourceId,
        indices: ResourceId,
        layout: VertexLayout,
        index_fmt: IndexFormat,
        topology: Topology,
        first: u32,
        count: u32,
        instances: u32,
    ) -> &mut Self {
        self.push(GpuCmd::DrawPrimitives {
            vertices,
            indices,
            layout,
            index_fmt,
            topology,
            first,
            count,
            instances,
        });
        self
    }

    pub fn upload_buffer(&mut self, dst: ResourceId, data: &[u8]) -> &mut Self {
        self.push(GpuCmd::UploadBuffer {
            dst,
            offset: 0,
            data_ptr: data.as_ptr() as u64,
            data_len: data.len() as u32,
        });
        self
    }

    pub fn upload_buffer_raw(
        &mut self,
        dst: ResourceId,
        offset: u32,
        data_ptr: u64,
        data_len: u32,
    ) -> &mut Self {
        self.push(GpuCmd::UploadBuffer {
            dst,
            offset,
            data_ptr,
            data_len,
        });
        self
    }

    pub fn alloc_buffer(&mut self, kind: BufferKind, size: u32) -> &mut Self {
        self.push(GpuCmd::AllocBuffer { kind, size });
        self
    }
}

impl Default for CommandBuffer {
    fn default() -> Self {
        Self::new()
    }
}
