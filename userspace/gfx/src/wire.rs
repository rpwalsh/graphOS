// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Wire encoding: serialize a `CommandBuffer` into the flat byte buffer sent
//! via `SYS_GPU_SUBMIT` to the kernel executor.
//!
//! ## Format
//!
//! The wire buffer is a sequence of variable-length records:
//!
//! ```text
//! [u8 opcode]  [payload bytes ...]
//! ```
//!
//! All multi-byte integers are little-endian.  The kernel decoder reads the
//! opcode, then reads the fixed payload size for that opcode.
//!
//! The format is intentionally simple and compact — this is an IPC protocol
//! that crosses the ring-3→ring-0 boundary once per frame, not a network
//! protocol.  Versioning is by opcode; old opcodes are never reused.

extern crate alloc;
use crate::command::{
    BlendMode, CommandBuffer, GpuCmd, GradientDir, PixelFormat, ResourceId, ResourceKind,
};
use crate::types::{
    BlendState, BufferKind, DepthState, IndexFormat, RasterState, Topology, VertexLayout,
};
use alloc::vec::Vec;

// ── Opcodes ───────────────────────────────────────────────────────────────────

#[repr(u8)]
enum Op {
    AllocResource = 0x01,
    FreeResource = 0x02,
    BeginFrame = 0x03,
    EndFrame = 0x04,
    FillRect = 0x10,
    FillGradient = 0x11,
    DrawBorder = 0x12,
    DrawShadow = 0x13,
    BlitResource = 0x20,
    ImportSurface = 0x21,
    BlurRegion = 0x22,
    Composite = 0x23,
    Bloom = 0x30,
    Vignette = 0x31,
    Signal = 0x40,
    Present = 0x50,
    // 3D
    AllocBuffer = 0x60,
    UploadBuffer = 0x61,
    SetRenderTarget = 0x62,
    ClearDepth = 0x63,
    SetViewport = 0x64,
    SetScissor = 0x65,
    SetTransform = 0x66,
    SetBlendState = 0x67,
    SetDepthState = 0x68,
    SetRasterState = 0x69,
    BindTexture = 0x6A,
    SetUniform = 0x6B,
    DrawPrimitives = 0x6C,
    SetShaderHint = 0x6D,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[inline]
fn push_u8(buf: &mut Vec<u8>, v: u8) {
    buf.push(v);
}
#[inline]
fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn push_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn push_i8(buf: &mut Vec<u8>, v: i8) {
    buf.push(v as u8);
}
#[inline]
fn push_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn push_f32(buf: &mut Vec<u8>, v: f32) {
    buf.extend_from_slice(&v.to_bits().to_le_bytes());
}
#[inline]
fn push_f64(buf: &mut Vec<u8>, v: f64) {
    buf.extend_from_slice(&v.to_bits().to_le_bytes());
}
#[inline]
fn push_color(buf: &mut Vec<u8>, c: crate::command::Color) {
    push_u32(buf, c.0);
}
#[inline]
fn push_rid(buf: &mut Vec<u8>, r: ResourceId) {
    push_u32(buf, r.0);
}
#[inline]
fn push_rect(buf: &mut Vec<u8>, r: &crate::command::Rect) {
    push_i32(buf, r.x);
    push_i32(buf, r.y);
    push_u32(buf, r.w);
    push_u32(buf, r.h);
}
#[inline]
fn push_bool_u8(buf: &mut Vec<u8>, v: bool) {
    buf.push(v as u8);
}

// ── Encode ────────────────────────────────────────────────────────────────────

/// Encode all commands in `cmds` into the flat wire buffer `out`.
pub fn encode(cmds: &CommandBuffer, out: &mut Vec<u8>) {
    for cmd in cmds.cmds() {
        encode_one(cmd, out);
    }
}

fn encode_one(cmd: &GpuCmd, out: &mut Vec<u8>) {
    use GpuCmd::*;
    match cmd {
        AllocResource {
            width,
            height,
            format,
            kind,
        } => {
            push_u8(out, Op::AllocResource as u8);
            push_u32(out, *width);
            push_u32(out, *height);
            push_u8(out, *format as u8);
            push_u8(out, *kind as u8);
        }
        FreeResource { id } => {
            push_u8(out, Op::FreeResource as u8);
            push_rid(out, *id);
        }
        BeginFrame { target, clear } => {
            push_u8(out, Op::BeginFrame as u8);
            push_rid(out, *target);
            push_bool_u8(out, clear.is_some());
            push_color(out, clear.unwrap_or(crate::command::Color::TRANSPARENT));
        }
        EndFrame { target } => {
            push_u8(out, Op::EndFrame as u8);
            push_rid(out, *target);
        }
        FillRect {
            target,
            rect,
            color,
            radius,
            blend,
        } => {
            push_u8(out, Op::FillRect as u8);
            push_rid(out, *target);
            push_rect(out, rect);
            push_color(out, *color);
            push_u8(out, *radius);
            push_u8(out, *blend as u8);
        }
        FillGradient {
            target,
            rect,
            color_a,
            color_b,
            dir,
            blend,
        } => {
            push_u8(out, Op::FillGradient as u8);
            push_rid(out, *target);
            push_rect(out, rect);
            push_color(out, *color_a);
            push_color(out, *color_b);
            push_u8(out, *dir as u8);
            push_u8(out, *blend as u8);
        }
        DrawBorder {
            target,
            rect,
            color,
            width,
            radius,
            blend,
        } => {
            push_u8(out, Op::DrawBorder as u8);
            push_rid(out, *target);
            push_rect(out, rect);
            push_color(out, *color);
            push_u8(out, *width);
            push_u8(out, *radius);
            push_u8(out, *blend as u8);
        }
        DrawShadow {
            target,
            rect,
            color,
            offset_x,
            offset_y,
            sigma,
            spread,
        } => {
            push_u8(out, Op::DrawShadow as u8);
            push_rid(out, *target);
            push_rect(out, rect);
            push_color(out, *color);
            push_i8(out, *offset_x);
            push_i8(out, *offset_y);
            push_u8(out, *sigma);
            push_i8(out, *spread);
        }
        BlitResource {
            src,
            src_rect,
            dst,
            dst_rect,
            opacity,
            blend,
        } => {
            push_u8(out, Op::BlitResource as u8);
            push_rid(out, *src);
            push_rect(out, src_rect);
            push_rid(out, *dst);
            push_rect(out, dst_rect);
            push_u8(out, *opacity);
            push_u8(out, *blend as u8);
        }
        ImportSurface { surface_id, dst } => {
            push_u8(out, Op::ImportSurface as u8);
            push_u32(out, *surface_id);
            push_rid(out, *dst);
        }
        BlurRegion {
            target,
            rect,
            sigma,
            passes,
        } => {
            push_u8(out, Op::BlurRegion as u8);
            push_rid(out, *target);
            push_rect(out, rect);
            push_u8(out, *sigma);
            push_u8(out, *passes);
        }
        Composite {
            src,
            dst,
            rect,
            blend,
            opacity,
        } => {
            push_u8(out, Op::Composite as u8);
            push_rid(out, *src);
            push_rid(out, *dst);
            push_rect(out, rect);
            push_u8(out, *blend as u8);
            push_u8(out, *opacity);
        }
        Bloom {
            src,
            dst,
            threshold,
            intensity,
        } => {
            push_u8(out, Op::Bloom as u8);
            push_rid(out, *src);
            push_rid(out, *dst);
            push_u8(out, *threshold);
            push_u8(out, *intensity);
        }
        Vignette {
            target,
            strength,
            color,
        } => {
            push_u8(out, Op::Vignette as u8);
            push_rid(out, *target);
            push_u8(out, *strength);
            push_color(out, *color);
        }
        Signal { fence } => {
            push_u8(out, Op::Signal as u8);
            push_u32(out, fence.0);
        }
        Present { src } => {
            push_u8(out, Op::Present as u8);
            push_rid(out, *src);
        }
        AllocBuffer { kind, size } => {
            push_u8(out, Op::AllocBuffer as u8);
            push_u8(out, *kind as u8);
            push_u32(out, *size);
        }
        UploadBuffer {
            dst,
            offset,
            data_ptr,
            data_len,
        } => {
            push_u8(out, Op::UploadBuffer as u8);
            push_rid(out, *dst);
            push_u32(out, *offset);
            push_u64(out, *data_ptr);
            push_u32(out, *data_len);
        }
        SetRenderTarget { color, depth } => {
            push_u8(out, Op::SetRenderTarget as u8);
            push_rid(out, *color);
            push_rid(out, *depth);
        }
        ClearDepth { depth } => {
            push_u8(out, Op::ClearDepth as u8);
            push_f32(out, *depth);
        }
        SetViewport {
            x,
            y,
            w,
            h,
            min_depth,
            max_depth,
        } => {
            push_u8(out, Op::SetViewport as u8);
            push_f32(out, *x);
            push_f32(out, *y);
            push_f32(out, *w);
            push_f32(out, *h);
            push_f32(out, *min_depth);
            push_f32(out, *max_depth);
        }
        SetScissor { rect } => {
            push_u8(out, Op::SetScissor as u8);
            push_rect(out, rect);
        }
        SetTransform { matrix } => {
            push_u8(out, Op::SetTransform as u8);
            for row in matrix {
                for v in row {
                    push_f32(out, *v);
                }
            }
        }
        SetBlendState { state } => {
            push_u8(out, Op::SetBlendState as u8);
            push_bool_u8(out, state.enabled);
            push_u8(out, state.src_color as u8);
            push_u8(out, state.dst_color as u8);
            push_u8(out, state.color_op as u8);
            push_u8(out, state.src_alpha as u8);
            push_u8(out, state.dst_alpha as u8);
            push_u8(out, state.alpha_op as u8);
            push_u8(out, state.write_mask);
        }
        SetDepthState { state } => {
            push_u8(out, Op::SetDepthState as u8);
            push_bool_u8(out, state.test_enable);
            push_bool_u8(out, state.write_enable);
            push_u8(out, state.compare_op as u8);
        }
        SetRasterState { state } => {
            push_u8(out, Op::SetRasterState as u8);
            push_u8(out, state.cull_mode as u8);
            push_u8(out, state.fill_mode as u8);
            push_bool_u8(out, state.front_ccw);
            push_bool_u8(out, state.depth_clip);
        }
        BindTexture { slot, resource } => {
            push_u8(out, Op::BindTexture as u8);
            push_u8(out, *slot);
            push_rid(out, *resource);
        }
        SetShaderHint { hint } => {
            push_u8(out, Op::SetShaderHint as u8);
            push_u8(out, *hint);
        }
        SetUniform { slot, value } => {
            push_u8(out, Op::SetUniform as u8);
            push_u8(out, *slot);
            for &w in value {
                push_u32(out, w);
            }
        }
        DrawPrimitives {
            vertices,
            indices,
            layout,
            index_fmt,
            topology,
            first,
            count,
            instances,
        } => {
            push_u8(out, Op::DrawPrimitives as u8);
            push_rid(out, *vertices);
            push_rid(out, *indices);
            push_u8(out, *layout as u8);
            push_u8(out, *index_fmt as u8);
            push_u8(out, *topology as u8);
            push_u32(out, *first);
            push_u32(out, *count);
            push_u32(out, *instances);
        }
    }
}
