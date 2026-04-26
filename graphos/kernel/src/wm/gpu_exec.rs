// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS-native GPU command executor.
//!
//! Decodes the wire format produced by `graphos-gfx::wire::encode()` and
//! executes it through a persistent in-kernel resource table. The current
//! backend is software-native: resources, buffers, uniforms, and draw packets
//! are all processed by the kernel and only the final present step touches the
//! virtio-gpu scanout path.

#![allow(dead_code)]

extern crate alloc;

use crate::arch::x86_64::serial;
use crate::syscall::validate_user_slice;
use crate::wm::surface_table::{MAX_SURFACE_FRAMES, surface_dimensions, surface_frames};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

static DIRECT_PRESENT_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn direct_present_active() -> bool {
    DIRECT_PRESENT_ACTIVE.load(Ordering::Acquire)
}

// ── Wire op-codes (must match graphos-gfx/src/wire.rs) ───────────────────────

#[allow(non_upper_case_globals)]
mod op {
    pub const AllocResource: u8 = 0x01;
    pub const FreeResource: u8 = 0x02;
    pub const BeginFrame: u8 = 0x03;
    pub const EndFrame: u8 = 0x04;
    pub const FillRect: u8 = 0x10;
    pub const FillGradient: u8 = 0x11;
    pub const DrawBorder: u8 = 0x12;
    pub const DrawShadow: u8 = 0x13;
    pub const BlitResource: u8 = 0x20;
    pub const ImportSurface: u8 = 0x21;
    pub const BlurRegion: u8 = 0x22;
    pub const Composite: u8 = 0x23;
    pub const Bloom: u8 = 0x30;
    pub const Vignette: u8 = 0x31;
    pub const Signal: u8 = 0x40;
    pub const Present: u8 = 0x50;
    pub const AllocBuffer: u8 = 0x60;
    pub const UploadBuffer: u8 = 0x61;
    pub const SetRenderTarget: u8 = 0x62;
    pub const ClearDepth: u8 = 0x63;
    pub const SetViewport: u8 = 0x64;
    pub const SetScissor: u8 = 0x65;
    pub const SetTransform: u8 = 0x66;
    pub const SetBlendState: u8 = 0x67;
    pub const SetDepthState: u8 = 0x68;
    pub const SetRasterState: u8 = 0x69;
    pub const BindTexture: u8 = 0x6A;
    pub const SetUniform: u8 = 0x6B;
    pub const DrawPrimitives: u8 = 0x6C;
    pub const SetShaderHint: u8 = 0x6D;
}

const MAX_GPU_RESOURCES: usize = 512;
const MAX_TEXTURE_SLOTS: usize = 32;
const MAX_UNIFORM_SLOTS: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceClass {
    Image,
    Depth,
    Buffer,
}

struct ResourceSlot {
    id: u32,
    class: ResourceClass,
    format: u8,
    kind: u8,
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    depth: Vec<f32>,
    bytes: Vec<u8>,
    imported_surface: Option<u32>,
    ready: bool,
}

impl ResourceSlot {
    fn image(id: u32, width: u32, height: u32, format: u8, kind: u8) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        let class = if kind == 2 {
            ResourceClass::Depth
        } else {
            ResourceClass::Image
        };
        Self {
            id,
            class,
            format,
            kind,
            width,
            height,
            pixels: if class == ResourceClass::Image {
                vec![0; len]
            } else {
                Vec::new()
            },
            depth: if class == ResourceClass::Depth {
                vec![1.0; len]
            } else {
                Vec::new()
            },
            bytes: Vec::new(),
            imported_surface: None,
            ready: false,
        }
    }

    fn buffer(id: u32, kind: u8, size: usize) -> Self {
        Self {
            id,
            class: ResourceClass::Buffer,
            format: 0,
            kind,
            width: 0,
            height: 0,
            pixels: Vec::new(),
            depth: Vec::new(),
            bytes: vec![0; size],
            imported_surface: None,
            ready: true,
        }
    }
}

#[derive(Clone, Copy)]
struct PipelineState {
    color_target: u32,
    depth_target: u32,
    viewport_x: f32,
    viewport_y: f32,
    viewport_w: f32,
    viewport_h: f32,
    scissor: Option<(i32, i32, u32, u32)>,
    transform: [[f32; 4]; 4],
    textures: [u32; MAX_TEXTURE_SLOTS],
    uniforms: [[u32; 4]; MAX_UNIFORM_SLOTS],
    blend_enabled: bool,
    depth_test: bool,
    depth_write: bool,
    shader_hint: u8,
}

impl PipelineState {
    const fn identity() -> [[f32; 4]; 4] {
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ]
    }

    const fn new() -> Self {
        Self {
            color_target: 0,
            depth_target: 0,
            viewport_x: 0.0,
            viewport_y: 0.0,
            viewport_w: 0.0,
            viewport_h: 0.0,
            scissor: None,
            transform: Self::identity(),
            textures: [0; MAX_TEXTURE_SLOTS],
            uniforms: [[0; 4]; MAX_UNIFORM_SLOTS],
            blend_enabled: true,
            depth_test: false,
            depth_write: false,
            shader_hint: 0,
        }
    }
}

struct ExecState {
    next_id: u32,
    resources: Vec<ResourceSlot>,
    pipe: PipelineState,
}

impl ExecState {
    fn new() -> Self {
        let (screen_w, screen_h) = crate::drivers::gpu::virtio_gpu::resolution();
        let mut resources = Vec::new();
        resources.push(ResourceSlot::image(0, screen_w, screen_h, 0, 1));
        Self {
            next_id: 1,
            resources,
            pipe: {
                let mut pipe = PipelineState::new();
                pipe.color_target = 0;
                pipe.viewport_w = screen_w as f32;
                pipe.viewport_h = screen_h as f32;
                pipe
            },
        }
    }

    fn alloc_image(&mut self, width: u32, height: u32, format: u8, kind: u8) -> Option<u32> {
        if self.resources.len() >= MAX_GPU_RESOURCES {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.resources
            .push(ResourceSlot::image(id, width, height, format, kind));
        Some(id)
    }

    fn alloc_buffer(&mut self, kind: u8, size: usize) -> Option<u32> {
        if self.resources.len() >= MAX_GPU_RESOURCES {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.resources.push(ResourceSlot::buffer(id, kind, size));
        Some(id)
    }

    fn get(&self, id: u32) -> Option<&ResourceSlot> {
        self.resources.iter().find(|r| r.id == id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut ResourceSlot> {
        self.resources.iter_mut().find(|r| r.id == id)
    }

    fn free(&mut self, id: u32) {
        if let Some(index) = self.resources.iter().position(|r| r.id == id) {
            self.resources.swap_remove(index);
        }
    }
}

static EXEC: Mutex<Option<ExecState>> = Mutex::new(None);

fn with_exec_mut<R>(f: impl FnOnce(&mut ExecState) -> R) -> R {
    let mut guard = EXEC.lock();
    if guard.is_none() {
        *guard = Some(ExecState::new());
    }
    f(guard.as_mut().unwrap())
}

pub fn alloc_image_resource(width: u32, height: u32, format: u8, kind: u8) -> Option<u32> {
    with_exec_mut(|exec| exec.alloc_image(width, height, format, kind))
}

pub fn alloc_buffer_resource(kind: u8, size: usize) -> Option<u32> {
    with_exec_mut(|exec| exec.alloc_buffer(kind, size))
}

pub fn free_resource(resource_id: u32) {
    with_exec_mut(|exec| exec.free(resource_id));
}

// ── Wire helpers ──────────────────────────────────────────────────────────────

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Some(v)
    }

    fn read_u16(&mut self) -> Option<u16> {
        if self.remaining() < 2 {
            return None;
        }
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Some(v)
    }

    fn read_u32(&mut self) -> Option<u32> {
        if self.remaining() < 4 {
            return None;
        }
        let b = &self.buf[self.pos..self.pos + 4];
        let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        self.pos += 4;
        Some(v)
    }

    fn read_i32(&mut self) -> Option<i32> {
        self.read_u32().map(|v| v as i32)
    }

    fn read_f32(&mut self) -> Option<f32> {
        self.read_u32().map(f32::from_bits)
    }

    fn read_color(&mut self) -> Option<u32> {
        self.read_u32()
    }

    fn read_rid(&mut self) -> Option<u32> {
        self.read_u32()
    }

    fn read_rect(&mut self) -> Option<(i32, i32, u32, u32)> {
        let x = self.read_i32()?;
        let y = self.read_i32()?;
        let w = self.read_u32()?;
        let h = self.read_u32()?;
        Some((x, y, w, h))
    }
}

// ── Executor ─────────────────────────────────────────────────────────────────

/// Execute a GraphOS-native GPU wire buffer.
///
/// Called from `syscall::sys_gpu_submit()` with a validated user-provided byte
/// slice.  Each decoded command is forwarded to the virtio-gpu 2D backend.
pub fn execute(buf: &[u8]) {
    let mut r = Reader::new(buf);
    let mut cmd_count = 0u32;

    while r.remaining() > 0 {
        let Some(opcode) = r.read_u8() else { break };
        cmd_count += 1;

        match opcode {
            op::BeginFrame => {
                let Some(target) = r.read_rid() else { break };
                let Some(has_clear) = r.read_u8() else { break };
                let Some(clear) = r.read_color() else { break };
                begin_frame(target, has_clear != 0, clear);
            }
            op::EndFrame => {
                let Some(target) = r.read_rid() else { break };
                end_frame(target);
            }
            op::FillRect => {
                let Some(target) = r.read_rid() else { break };
                let Some((x, y, w, h)) = r.read_rect() else {
                    break;
                };
                let Some(color) = r.read_color() else { break };
                let _ = r.read_u8();
                let _ = r.read_u8();
                fill_rect(target, x, y, w, h, color);
            }
            op::BlitResource => {
                let Some(src) = r.read_rid() else { break };
                let Some((sx, sy, sw, sh)) = r.read_rect() else {
                    break;
                };
                let Some(dst) = r.read_rid() else { break };
                let Some((dx, dy, dw, dh)) = r.read_rect() else {
                    break;
                };
                let Some(alpha) = r.read_u8() else { break };
                let _ = r.read_u8();
                blit_resource(src, sx, sy, sw, sh, dst, dx, dy, dw, dh, alpha);
            }
            op::Present => {
                let Some(src) = r.read_rid() else { break };
                present(src);
            }
            op::AllocResource => {
                let Some(w) = r.read_u32() else { break };
                let Some(h) = r.read_u32() else { break };
                let Some(fmt) = r.read_u8() else { break };
                let Some(kind) = r.read_u8() else { break };
                let _ = with_exec_mut(|exec| exec.alloc_image(w, h, fmt, kind));
            }
            op::FreeResource => {
                let Some(rid) = r.read_rid() else { break };
                free_resource(rid);
            }
            op::Signal => {
                // arg: fence_id(u32)
                let Some(fid) = r.read_u32() else { break };
                crate::syscall::gpu_fence_signal(fid);
            }
            op::SetViewport => {
                let Some(x) = r.read_f32() else { break };
                let Some(y) = r.read_f32() else { break };
                let Some(w) = r.read_f32() else { break };
                let Some(h) = r.read_f32() else { break };
                let _ = r.read_f32();
                let _ = r.read_f32();
                set_viewport(x, y, w, h);
            }
            op::ClearDepth => {
                let Some(value) = r.read_f32() else { break };
                clear_depth(value);
            }
            op::SetRenderTarget => {
                let Some(color) = r.read_u32() else { break };
                let Some(depth) = r.read_u32() else { break };
                set_render_target(color, depth);
            }
            op::DrawPrimitives => {
                let Some(vbo) = r.read_u32() else { break };
                let Some(ibo) = r.read_u32() else { break };
                let Some(layout) = r.read_u8() else { break };
                let Some(topology) = r.read_u8() else { break };
                let Some(index_fmt) = r.read_u8() else { break };
                let Some(first) = r.read_u32() else { break };
                let Some(count) = r.read_u32() else { break };
                let Some(instances) = r.read_u32() else { break };
                draw_primitives(
                    vbo, ibo, layout, topology, index_fmt, first, count, instances,
                );
            }
            op::SetBlendState | op::SetDepthState | op::SetRasterState => match opcode {
                op::SetBlendState => {
                    for _ in 0..8 {
                        let _ = r.read_u8();
                    }
                }
                op::SetDepthState => {
                    for _ in 0..3 {
                        let _ = r.read_u8();
                    }
                }
                op::SetRasterState => {
                    for _ in 0..4 {
                        let _ = r.read_u8();
                    }
                }
                _ => {}
            },
            op::SetTransform => {
                let mut matrix = [[0.0f32; 4]; 4];
                for row in &mut matrix {
                    for cell in row {
                        let Some(v) = r.read_f32() else { return };
                        *cell = v;
                    }
                }
                set_transform(matrix);
            }
            op::SetShaderHint => {
                let Some(hint) = r.read_u8() else { break };
                set_shader_hint(hint);
            }
            op::SetUniform => {
                let Some(slot) = r.read_u8() else { break };
                let mut value = [0u32; 4];
                for word in &mut value {
                    let Some(v) = r.read_u32() else { return };
                    *word = v;
                }
                set_uniform(slot, value);
            }
            op::BindTexture => {
                let Some(unit) = r.read_u8() else { break };
                let Some(rid) = r.read_u32() else { break };
                bind_texture(unit, rid);
            }
            op::AllocBuffer => {
                let Some(kind) = r.read_u8() else { break };
                let Some(size) = r.read_u32() else { break };
                let _ = with_exec_mut(|exec| exec.alloc_buffer(kind, size as usize));
            }
            op::UploadBuffer => {
                let Some(dst) = r.read_u32() else { break };
                let Some(offset) = r.read_u32() else { break };
                let lo = r.read_u32().unwrap_or(0) as u64;
                let hi = r.read_u32().unwrap_or(0) as u64;
                let Some(data_len) = r.read_u32() else { break };
                upload_buffer(dst, offset, (hi << 32) | lo, data_len);
            }
            op::BlurRegion => {
                let Some(target) = r.read_u32() else { break };
                let Some((x, y, w, h)) = r.read_rect() else {
                    break;
                };
                let Some(sigma) = r.read_u8() else { break };
                let Some(passes) = r.read_u8() else { break };
                blur_region(target, x, y, w, h, sigma, passes);
            }
            op::FillGradient => {
                let Some(target) = r.read_rid() else { break };
                let Some((x, y, w, h)) = r.read_rect() else {
                    break;
                };
                let Some(ca) = r.read_u32() else { break };
                let Some(cb) = r.read_u32() else { break };
                let Some(dir) = r.read_u8() else { break };
                let _ = r.read_u8();
                fill_gradient(target, x, y, w, h, ca, cb, dir);
            }
            op::DrawBorder => {
                let Some(target) = r.read_u32() else { break };
                let Some((x, y, w, h)) = r.read_rect() else {
                    break;
                };
                let Some(color) = r.read_u32() else { break };
                let Some(width) = r.read_u8() else { break };
                let _ = r.read_u8();
                let _ = r.read_u8();
                draw_border(target, x, y, w, h, color, width);
            }
            op::DrawShadow => {
                let Some(target) = r.read_u32() else { break };
                let Some((x, y, w, h)) = r.read_rect() else {
                    break;
                };
                let Some(color) = r.read_u32() else { break };
                let ox = r.read_u8().unwrap_or(0) as i8;
                let oy = r.read_u8().unwrap_or(0) as i8;
                let sigma = r.read_u8().unwrap_or(0);
                let spread = r.read_u8().unwrap_or(0) as i8;
                draw_shadow(target, x, y, w, h, color, ox, oy, sigma, spread);
            }
            op::ImportSurface => {
                let Some(surface_id) = r.read_u32() else {
                    break;
                };
                let Some(rid) = r.read_u32() else { break };
                import_surface(surface_id, rid);
            }
            op::Composite => {
                let Some(src) = r.read_rid() else { break };
                let Some(dst) = r.read_rid() else { break };
                let Some((x, y, w, h)) = r.read_rect() else {
                    break;
                };
                let _ = r.read_u8();
                let opacity = r.read_u8().unwrap_or(255);
                composite(src, dst, x, y, w, h, opacity);
            }
            op::Bloom => {
                let Some(src) = r.read_rid() else { break };
                let Some(dst) = r.read_rid() else { break };
                let threshold = r.read_u8().unwrap_or(0);
                let intensity = r.read_u8().unwrap_or(0);
                bloom(src, dst, threshold, intensity);
            }
            op::Vignette => {
                let Some(target) = r.read_rid() else { break };
                let strength = r.read_u8().unwrap_or(0);
                let color = r.read_u32().unwrap_or(0);
                vignette(target, strength, color);
            }
            op::SetScissor => {
                let rect = r.read_rect();
                set_scissor(rect);
            }
            _ => {
                serial::write_bytes(b"[gpu_exec] unknown opcode=0x");
                serial::write_hex_inline(opcode as u64);
                serial::write_line(b"");
                break;
            }
        }
    }

    let _ = cmd_count;
}

fn begin_frame(target: u32, has_clear: bool, clear: u32) {
    with_exec_mut(|exec| {
        exec.pipe.color_target = target;
        if exec.pipe.viewport_w == 0.0 || exec.pipe.viewport_h == 0.0 {
            if let Some((width, height)) = exec.get(target).map(|slot| (slot.width, slot.height)) {
                exec.pipe.viewport_w = width as f32;
                exec.pipe.viewport_h = height as f32;
            }
        }
    });
    if has_clear {
        clear_target(target, clear);
    }
}

fn end_frame(target: u32) {
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(target) {
            slot.ready = true;
        }
    });
}

fn set_render_target(color: u32, depth: u32) {
    with_exec_mut(|exec| {
        exec.pipe.color_target = color;
        exec.pipe.depth_target = depth;
    });
}

fn set_viewport(x: f32, y: f32, w: f32, h: f32) {
    with_exec_mut(|exec| {
        exec.pipe.viewport_x = x;
        exec.pipe.viewport_y = y;
        exec.pipe.viewport_w = w;
        exec.pipe.viewport_h = h;
    });
}

fn set_scissor(rect: Option<(i32, i32, u32, u32)>) {
    with_exec_mut(|exec| exec.pipe.scissor = rect);
}

fn set_transform(matrix: [[f32; 4]; 4]) {
    with_exec_mut(|exec| exec.pipe.transform = matrix);
}

fn set_shader_hint(hint: u8) {
    with_exec_mut(|exec| exec.pipe.shader_hint = hint);
}

fn set_uniform(slot: u8, value: [u32; 4]) {
    with_exec_mut(|exec| {
        if let Some(dst) = exec.pipe.uniforms.get_mut(slot as usize) {
            *dst = value;
        }
    });
}

fn bind_texture(slot: u8, rid: u32) {
    with_exec_mut(|exec| {
        if let Some(dst) = exec.pipe.textures.get_mut(slot as usize) {
            *dst = rid;
        }
    });
}

fn clear_target(target: u32, color: u32) {
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(target) {
            for pixel in &mut slot.pixels {
                *pixel = color;
            }
            slot.ready = true;
        }
    });
}

fn clear_depth(value: f32) {
    with_exec_mut(|exec| {
        let depth_id = exec.pipe.depth_target;
        if let Some(slot) = exec.get_mut(depth_id) {
            for d in &mut slot.depth {
                *d = value;
            }
        }
    });
}

fn fill_rect(target: u32, x: i32, y: i32, w: u32, h: u32, color: u32) {
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(target) {
            fill_rect_pixels(slot, x, y, w, h, color);
            slot.ready = true;
        }
    });
}

fn fill_gradient(target: u32, x: i32, y: i32, w: u32, h: u32, ca: u32, cb: u32, dir: u8) {
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(target) {
            fill_gradient_pixels(slot, x, y, w, h, ca, cb, dir);
            slot.ready = true;
        }
    });
}

fn draw_border(target: u32, x: i32, y: i32, w: u32, h: u32, color: u32, width: u8) {
    let bw = width.max(1) as u32;
    fill_rect(target, x, y, w, bw, color);
    fill_rect(target, x, y + h as i32 - bw as i32, w, bw, color);
    fill_rect(target, x, y, bw, h, color);
    fill_rect(target, x + w as i32 - bw as i32, y, bw, h, color);
}

fn draw_shadow(
    target: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
    ox: i8,
    oy: i8,
    sigma: u8,
    spread: i8,
) {
    let expand = spread.max(0) as i32 + sigma as i32;
    let rx = x + ox as i32 - expand;
    let ry = y + oy as i32 - expand;
    let rw = w.saturating_add((expand.max(0) as u32).saturating_mul(2));
    let rh = h.saturating_add((expand.max(0) as u32).saturating_mul(2));
    fill_rect(target, rx, ry, rw, rh, color);
}

fn import_surface(surface_id: u32, rid: u32) {
    with_exec_mut(|exec| {
        if exec.get(rid).is_none() {
            let (w, h) = surface_dimensions(surface_id).unwrap_or((0, 0));
            exec.resources
                .push(ResourceSlot::image(rid, w as u32, h as u32, 0, 0));
        }
        if let Some(slot) = exec.get_mut(rid) {
            import_surface_into_slot(surface_id, slot);
        }
    });
}

fn upload_buffer(dst: u32, offset: u32, data_ptr: u64, data_len: u32) {
    let Some(src) = validate_user_slice(data_ptr, data_len as u64) else {
        return;
    };
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(dst) {
            if slot.class != ResourceClass::Buffer {
                return;
            }
            let start = offset as usize;
            let end = start.saturating_add(src.len());
            if end > slot.bytes.len() {
                slot.bytes.resize(end, 0);
            }
            slot.bytes[start..end].copy_from_slice(src);
        }
    });
}

fn blur_region(target: u32, x: i32, y: i32, w: u32, h: u32, _sigma: u8, passes: u8) {
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(target) {
            for _ in 0..passes.max(1) {
                box_blur_region(slot, x, y, w, h);
            }
            slot.ready = true;
        }
    });
}

fn composite(src: u32, dst: u32, x: i32, y: i32, w: u32, h: u32, opacity: u8) {
    blit_resource(src, x, y, w, h, dst, x, y, w, h, opacity);
}

fn bloom(src: u32, dst: u32, threshold: u8, intensity: u8) {
    with_exec_mut(|exec| {
        let src_pixels = exec.get(src).map(|r| r.pixels.clone()).unwrap_or_default();
        if let Some(dst_slot) = exec.get_mut(dst) {
            let len = src_pixels.len().min(dst_slot.pixels.len());
            for i in 0..len {
                let p = src_pixels[i];
                let lum = (((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF)) / 3;
                if lum >= threshold as u32 {
                    dst_slot.pixels[i] = add_color(dst_slot.pixels[i], scale_color(p, intensity));
                }
            }
            dst_slot.ready = true;
        }
    });
}

fn vignette(target: u32, strength: u8, color: u32) {
    with_exec_mut(|exec| {
        if let Some(slot) = exec.get_mut(target) {
            let width = slot.width.max(1);
            let height = slot.height.max(1);
            for y in 0..height {
                for x in 0..width {
                    let edge_x = x.min(width - 1 - x);
                    let edge_y = y.min(height - 1 - y);
                    let edge = edge_x.min(edge_y).min(255);
                    let fade = 255u32
                        .saturating_sub(edge.saturating_mul(255) / 32)
                        .min(255);
                    let alpha = ((fade * strength as u32) / 255) as u8;
                    let idx = (y as usize)
                        .saturating_mul(width as usize)
                        .saturating_add(x as usize);
                    slot.pixels[idx] = alpha_blend(slot.pixels[idx], color, alpha);
                }
            }
            slot.ready = true;
        }
    });
}

fn present(src: u32) {
    let maybe = with_exec_mut(|exec| exec.get(src).map(|r| (r.width, r.height, r.pixels.clone())));
    let Some((w, h, pixels)) = maybe else { return };
    DIRECT_PRESENT_ACTIVE.store(true, Ordering::Release);
    crate::drivers::gpu::virtio_gpu::blit_pixels_scanout(
        &pixels, w as usize, h as usize, 0, 0, 1024, 255,
    );
    crate::drivers::gpu::virtio_gpu::flush_rect(0, 0, w, h);
}

fn blit_resource(
    src: u32,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    dst: u32,
    dx: i32,
    dy: i32,
    dw: u32,
    dh: u32,
    alpha: u8,
) {
    with_exec_mut(|exec| {
        let Some(src_slot) = exec.get(src) else {
            return;
        };
        let src_pixels = src_slot.pixels.clone();
        let src_w = src_slot.width;
        let src_h = src_slot.height;
        if let Some(dst_slot) = exec.get_mut(dst) {
            blit_pixels_to_slot(
                dst_slot,
                &src_pixels,
                src_w,
                src_h,
                sx,
                sy,
                sw,
                sh,
                dx,
                dy,
                dw,
                dh,
                alpha,
            );
            dst_slot.ready = true;
        }
    });
}

fn draw_primitives(
    vbo: u32,
    ibo: u32,
    layout: u8,
    topology: u8,
    index_fmt: u8,
    first: u32,
    count: u32,
    instances: u32,
) {
    if topology != 0 || instances == 0 {
        return;
    }
    with_exec_mut(|exec| {
        let pipe = exec.pipe;
        let Some(vbo_slot) = exec.get(vbo) else {
            return;
        };
        let vertex_bytes = vbo_slot.bytes.clone();
        let index_bytes = exec.get(ibo).map(|r| r.bytes.clone());
        let tex0 = pipe.textures[0];
        let tex_slot = exec
            .get(tex0)
            .map(|r| (r.width, r.height, r.pixels.clone()));
        let depth_target = pipe.depth_target;
        let color_target = pipe.color_target;
        let depth_snapshot = exec.get(depth_target).map(|r| r.depth.clone());

        if let Some(dst_slot) = exec.get_mut(color_target) {
            let mut depth_work = depth_snapshot.unwrap_or_default();
            rasterize_triangles(
                dst_slot,
                depth_target != 0,
                &mut depth_work,
                tex_slot,
                &vertex_bytes,
                index_bytes.as_deref(),
                layout,
                index_fmt,
                first,
                count,
                pipe,
            );
            dst_slot.ready = true;
            if let Some(depth_slot) = exec.get_mut(depth_target) {
                if depth_slot.depth.len() == depth_work.len() {
                    depth_slot.depth.copy_from_slice(&depth_work);
                }
            }
        }
    });
}

fn import_surface_into_slot(surface_id: u32, slot: &mut ResourceSlot) {
    let (w, h) = surface_dimensions(surface_id).unwrap_or((slot.width as u16, slot.height as u16));
    let mut frames = [0u64; MAX_SURFACE_FRAMES];
    let frame_count = surface_frames(surface_id, &mut frames);
    if frame_count == 0 {
        return;
    }
    slot.width = w as u32;
    slot.height = h as u32;
    let len = slot.width as usize * slot.height as usize;
    slot.pixels.resize(len, 0);
    for pixel_idx in 0..len {
        let byte_off = pixel_idx * 4;
        let page_idx = byte_off / 4096;
        let word_in_pg = (byte_off % 4096) / 4;
        if page_idx >= frame_count {
            break;
        }
        let page_phys = frames[page_idx];
        if page_phys == 0 {
            continue;
        }
        let _ = crate::mm::page_table::ensure_identity_mapped_2m(page_phys);
        slot.pixels[pixel_idx] =
            unsafe { core::ptr::read_volatile((page_phys as *const u32).add(word_in_pg)) };
    }
    slot.imported_surface = Some(surface_id);
    slot.ready = true;
}

fn fill_rect_pixels(slot: &mut ResourceSlot, x: i32, y: i32, w: u32, h: u32, color: u32) {
    if slot.class != ResourceClass::Image || slot.width == 0 || slot.height == 0 {
        return;
    }
    let x0 = x.clamp(0, slot.width as i32);
    let y0 = y.clamp(0, slot.height as i32);
    let x1 = x.saturating_add(w as i32).clamp(0, slot.width as i32);
    let y1 = y.saturating_add(h as i32).clamp(0, slot.height as i32);
    for row in y0..y1 {
        for col in x0..x1 {
            let idx = row as usize * slot.width as usize + col as usize;
            slot.pixels[idx] = color;
        }
    }
}

fn fill_gradient_pixels(
    slot: &mut ResourceSlot,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    ca: u32,
    cb: u32,
    dir: u8,
) {
    let x0 = x.clamp(0, slot.width as i32);
    let y0 = y.clamp(0, slot.height as i32);
    let x1 = x.saturating_add(w as i32).clamp(0, slot.width as i32);
    let y1 = y.saturating_add(h as i32).clamp(0, slot.height as i32);
    let span_w = (x1 - x0).max(1) as u32;
    let span_h = (y1 - y0).max(1) as u32;
    for row in y0..y1 {
        for col in x0..x1 {
            let t = match dir {
                1 => ((col - x0) as u32 * 255 / span_w) as u8,
                _ => ((row - y0) as u32 * 255 / span_h) as u8,
            };
            let idx = row as usize * slot.width as usize + col as usize;
            slot.pixels[idx] = lerp_color(ca, cb, t);
        }
    }
}

fn box_blur_region(slot: &mut ResourceSlot, x: i32, y: i32, w: u32, h: u32) {
    let width = slot.width as i32;
    let height = slot.height as i32;
    let x0 = x.clamp(0, width);
    let y0 = y.clamp(0, height);
    let x1 = x.saturating_add(w as i32).clamp(0, width);
    let y1 = y.saturating_add(h as i32).clamp(0, height);
    let original = slot.pixels.clone();
    for row in y0..y1 {
        for col in x0..x1 {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in (row - 1).max(0)..=(row + 1).min(height - 1) {
                for sx in (col - 1).max(0)..=(col + 1).min(width - 1) {
                    let p = original[sy as usize * slot.width as usize + sx as usize];
                    acc[0] += (p >> 24) & 0xFF;
                    acc[1] += (p >> 16) & 0xFF;
                    acc[2] += (p >> 8) & 0xFF;
                    acc[3] += p & 0xFF;
                    n += 1;
                }
            }
            slot.pixels[row as usize * slot.width as usize + col as usize] =
                ((acc[0] / n) << 24) | ((acc[1] / n) << 16) | ((acc[2] / n) << 8) | (acc[3] / n);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_pixels_to_slot(
    dst_slot: &mut ResourceSlot,
    src_pixels: &[u32],
    src_w: u32,
    src_h: u32,
    sx: i32,
    sy: i32,
    sw: u32,
    sh: u32,
    dx: i32,
    dy: i32,
    dw: u32,
    dh: u32,
    alpha: u8,
) {
    if dw == 0 || dh == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    for ty in 0..dh as i32 {
        let dst_y = dy + ty;
        if dst_y < 0 || dst_y >= dst_slot.height as i32 {
            continue;
        }
        let v = ty as f32 / dh.max(1) as f32;
        let src_y = sy + (v * sh as f32) as i32;
        if src_y < 0 || src_y >= src_h as i32 {
            continue;
        }
        for tx in 0..dw as i32 {
            let dst_x = dx + tx;
            if dst_x < 0 || dst_x >= dst_slot.width as i32 {
                continue;
            }
            let u = tx as f32 / dw.max(1) as f32;
            let src_x = sx + (u * sw as f32) as i32;
            if src_x < 0 || src_x >= src_w as i32 {
                continue;
            }
            let src_px = src_pixels[src_y as usize * src_w as usize + src_x as usize];
            let idx = dst_y as usize * dst_slot.width as usize + dst_x as usize;
            dst_slot.pixels[idx] = alpha_blend(dst_slot.pixels[idx], src_px, alpha);
        }
    }
}

#[derive(Clone, Copy)]
struct VertexIn {
    pos: [f32; 4],
    uv: [f32; 2],
    color: u32,
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangles(
    dst: &mut ResourceSlot,
    use_depth: bool,
    depth: &mut [f32],
    tex0: Option<(u32, u32, Vec<u32>)>,
    vbo: &[u8],
    ibo: Option<&[u8]>,
    layout: u8,
    index_fmt: u8,
    first: u32,
    count: u32,
    pipe: PipelineState,
) {
    let stride = match layout {
        0 => 16,
        1 => 32,
        2 => 16,
        _ => return,
    };
    let vertex_count = vbo.len() / stride;
    let indices: Vec<u32> = if let Some(index_bytes) = ibo {
        decode_indices(index_bytes, index_fmt, first, count)
    } else {
        (first..first.saturating_add(count)).collect()
    };
    for tri in indices.chunks(3) {
        if tri.len() < 3 {
            break;
        }
        let Some(v0) = decode_vertex(vbo, vertex_count, stride, layout, tri[0]) else {
            continue;
        };
        let Some(v1) = decode_vertex(vbo, vertex_count, stride, layout, tri[1]) else {
            continue;
        };
        let Some(v2) = decode_vertex(vbo, vertex_count, stride, layout, tri[2]) else {
            continue;
        };
        rasterize_triangle(dst, use_depth, depth, tex0.as_ref(), pipe, v0, v1, v2);
    }
}

fn decode_indices(bytes: &[u8], fmt: u8, first: u32, count: u32) -> Vec<u32> {
    let mut out = Vec::new();
    match fmt {
        0 => {
            let start = first as usize * 2;
            for i in 0..count as usize {
                let off = start + i * 2;
                if off + 1 >= bytes.len() {
                    break;
                }
                out.push(u16::from_le_bytes([bytes[off], bytes[off + 1]]) as u32);
            }
        }
        _ => {
            let start = first as usize * 4;
            for i in 0..count as usize {
                let off = start + i * 4;
                if off + 3 >= bytes.len() {
                    break;
                }
                out.push(u32::from_le_bytes([
                    bytes[off],
                    bytes[off + 1],
                    bytes[off + 2],
                    bytes[off + 3],
                ]));
            }
        }
    }
    out
}

fn decode_vertex(
    bytes: &[u8],
    vertex_count: usize,
    stride: usize,
    layout: u8,
    index: u32,
) -> Option<VertexIn> {
    let idx = index as usize;
    if idx >= vertex_count {
        return None;
    }
    let off = idx * stride;
    match layout {
        0 => Some(VertexIn {
            pos: [read_f32(bytes, off)?, read_f32(bytes, off + 4)?, 0.0, 1.0],
            uv: [read_f32(bytes, off + 8)?, read_f32(bytes, off + 12)?],
            color: 0xFFFFFFFF,
        }),
        1 => Some(VertexIn {
            pos: [
                read_f32(bytes, off)?,
                read_f32(bytes, off + 4)?,
                read_f32(bytes, off + 8)?,
                1.0,
            ],
            uv: [read_f32(bytes, off + 12)?, read_f32(bytes, off + 16)?],
            color: 0xFFFFFFFF,
        }),
        2 => Some(VertexIn {
            pos: [
                read_f32(bytes, off)?,
                read_f32(bytes, off + 4)?,
                read_f32(bytes, off + 8)?,
                1.0,
            ],
            uv: [0.0, 0.0],
            color: read_u32(bytes, off + 12)?,
        }),
        _ => None,
    }
}

fn rasterize_triangle(
    dst: &mut ResourceSlot,
    use_depth: bool,
    depth: &mut [f32],
    tex0: Option<&(u32, u32, Vec<u32>)>,
    pipe: PipelineState,
    v0: VertexIn,
    v1: VertexIn,
    v2: VertexIn,
) {
    let p0 = transform_vertex(v0.pos, pipe.transform);
    let p1 = transform_vertex(v1.pos, pipe.transform);
    let p2 = transform_vertex(v2.pos, pipe.transform);
    let s0 = ndc_to_screen(dst.width, dst.height, pipe, p0);
    let s1 = ndc_to_screen(dst.width, dst.height, pipe, p1);
    let s2 = ndc_to_screen(dst.width, dst.height, pipe, p2);
    let min_x = floor_to_i32(s0.0.min(s1.0).min(s2.0)).max(0);
    let max_x = ceil_to_i32(s0.0.max(s1.0).max(s2.0)).min(dst.width as i32 - 1);
    let min_y = floor_to_i32(s0.1.min(s1.1).min(s2.1)).max(0);
    let max_y = ceil_to_i32(s0.1.max(s1.1).max(s2.1)).min(dst.height as i32 - 1);
    let area = edge_fn(s0.0, s0.1, s1.0, s1.1, s2.0, s2.1);
    if area == 0.0 {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if let Some((sx, sy, sw, sh)) = pipe.scissor {
                if x < sx || y < sy || x >= sx + sw as i32 || y >= sy + sh as i32 {
                    continue;
                }
            }
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let w0 = edge_fn(s1.0, s1.1, s2.0, s2.1, px, py) / area;
            let w1 = edge_fn(s2.0, s2.1, s0.0, s0.1, px, py) / area;
            let w2 = edge_fn(s0.0, s0.1, s1.0, s1.1, px, py) / area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = s0.2 * w0 + s1.2 * w1 + s2.2 * w2;
            let idx = y as usize * dst.width as usize + x as usize;
            if use_depth && idx < depth.len() {
                if z > depth[idx] {
                    continue;
                }
                depth[idx] = z;
            }
            let mut color = shade_fragment(
                pipe.shader_hint,
                pipe.uniforms[0],
                tex0,
                v0,
                v1,
                v2,
                w0,
                w1,
                w2,
            );
            if pipe.blend_enabled {
                color = alpha_blend(dst.pixels[idx], color, ((color >> 24) & 0xFF) as u8);
            }
            dst.pixels[idx] = color;
        }
    }
}

fn shade_fragment(
    hint: u8,
    uniform0: [u32; 4],
    tex0: Option<&(u32, u32, Vec<u32>)>,
    v0: VertexIn,
    v1: VertexIn,
    v2: VertexIn,
    w0: f32,
    w1: f32,
    w2: f32,
) -> u32 {
    match hint {
        1 | 2 | 6 | 7 => {
            let u = v0.uv[0] * w0 + v1.uv[0] * w1 + v2.uv[0] * w2;
            let v = v0.uv[1] * w0 + v1.uv[1] * w1 + v2.uv[1] * w2;
            let tex = tex0
                .and_then(|(w, h, pixels)| sample_texture(*w, *h, pixels, u, v))
                .unwrap_or(0xFFFFFFFF);
            let tint = color_from_uniform(uniform0).unwrap_or(0xFFFFFFFF);
            modulate_color(tex, tint)
        }
        _ => {
            if v0.color != 0xFFFFFFFF || v1.color != 0xFFFFFFFF || v2.color != 0xFFFFFFFF {
                lerp_color3(v0.color, v1.color, v2.color, w0, w1, w2)
            } else {
                color_from_uniform(uniform0).unwrap_or(0xFFFFFFFF)
            }
        }
    }
}

fn read_f32(bytes: &[u8], off: usize) -> Option<f32> {
    Some(f32::from_bits(read_u32(bytes, off)?))
}

fn read_u32(bytes: &[u8], off: usize) -> Option<u32> {
    let slice = bytes.get(off..off + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn transform_vertex(pos: [f32; 4], m: [[f32; 4]; 4]) -> [f32; 4] {
    [
        m[0][0] * pos[0] + m[0][1] * pos[1] + m[0][2] * pos[2] + m[0][3] * pos[3],
        m[1][0] * pos[0] + m[1][1] * pos[1] + m[1][2] * pos[2] + m[1][3] * pos[3],
        m[2][0] * pos[0] + m[2][1] * pos[1] + m[2][2] * pos[2] + m[2][3] * pos[3],
        m[3][0] * pos[0] + m[3][1] * pos[1] + m[3][2] * pos[2] + m[3][3] * pos[3],
    ]
}

fn ndc_to_screen(width: u32, height: u32, pipe: PipelineState, p: [f32; 4]) -> (f32, f32, f32) {
    let inv_w = if p[3] != 0.0 { 1.0 / p[3] } else { 1.0 };
    let ndc_x = p[0] * inv_w;
    let ndc_y = p[1] * inv_w;
    let ndc_z = p[2] * inv_w;
    let vp_w = if pipe.viewport_w > 0.0 {
        pipe.viewport_w
    } else {
        width as f32
    };
    let vp_h = if pipe.viewport_h > 0.0 {
        pipe.viewport_h
    } else {
        height as f32
    };
    let x = pipe.viewport_x + (ndc_x * 0.5 + 0.5) * vp_w;
    let y = pipe.viewport_y + (1.0 - (ndc_y * 0.5 + 0.5)) * vp_h;
    (x, y, ndc_z)
}

fn edge_fn(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (px - ax) * (by - ay) - (py - ay) * (bx - ax)
}

fn sample_texture(width: u32, height: u32, pixels: &[u32], u: f32, v: f32) -> Option<u32> {
    if width == 0 || height == 0 {
        return None;
    }
    let tx = (u.clamp(0.0, 1.0) * (width - 1) as f32) as usize;
    let ty = (v.clamp(0.0, 1.0) * (height - 1) as f32) as usize;
    pixels.get(ty * width as usize + tx).copied()
}

fn color_from_uniform(words: [u32; 4]) -> Option<u32> {
    if words == [0; 4] {
        return None;
    }
    let r = (f32::from_bits(words[0]).clamp(0.0, 1.0) * 255.0) as u32;
    let g = (f32::from_bits(words[1]).clamp(0.0, 1.0) * 255.0) as u32;
    let b = (f32::from_bits(words[2]).clamp(0.0, 1.0) * 255.0) as u32;
    let a = if words[3] == 0 {
        255
    } else {
        (f32::from_bits(words[3]).clamp(0.0, 1.0) * 255.0) as u32
    };
    Some((a << 24) | (r << 16) | (g << 8) | b)
}

fn modulate_color(a: u32, b: u32) -> u32 {
    let aa = (a >> 24) & 0xFF;
    let ar = (a >> 16) & 0xFF;
    let ag = (a >> 8) & 0xFF;
    let ab = a & 0xFF;
    let ba = (b >> 24) & 0xFF;
    let br = (b >> 16) & 0xFF;
    let bg = (b >> 8) & 0xFF;
    let bb = b & 0xFF;
    (((aa * ba) / 255) << 24)
        | (((ar * br) / 255) << 16)
        | (((ag * bg) / 255) << 8)
        | ((ab * bb) / 255)
}

fn lerp_color(a: u32, b: u32, t: u8) -> u32 {
    let t = t as u32;
    let it = 255 - t;
    let aa = ((a >> 24) & 0xFF) * it + ((b >> 24) & 0xFF) * t;
    let ar = ((a >> 16) & 0xFF) * it + ((b >> 16) & 0xFF) * t;
    let ag = ((a >> 8) & 0xFF) * it + ((b >> 8) & 0xFF) * t;
    let ab = (a & 0xFF) * it + (b & 0xFF) * t;
    ((aa / 255) << 24) | ((ar / 255) << 16) | ((ag / 255) << 8) | (ab / 255)
}

fn floor_to_i32(value: f32) -> i32 {
    let truncated = value as i32;
    if value < truncated as f32 {
        truncated.saturating_sub(1)
    } else {
        truncated
    }
}

fn ceil_to_i32(value: f32) -> i32 {
    let truncated = value as i32;
    if value > truncated as f32 {
        truncated.saturating_add(1)
    } else {
        truncated
    }
}

fn lerp_color3(a: u32, b: u32, c: u32, wa: f32, wb: f32, wc: f32) -> u32 {
    let mix = |shift: u32| -> u32 {
        ((((a >> shift) & 0xFF) as f32 * wa)
            + (((b >> shift) & 0xFF) as f32 * wb)
            + (((c >> shift) & 0xFF) as f32 * wc)) as u32
    };
    (mix(24) << 24) | (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

fn alpha_blend(dst: u32, src: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let ia = 255 - a;
    let blend =
        |shift: u32| -> u32 { (((dst >> shift) & 0xFF) * ia + ((src >> shift) & 0xFF) * a) / 255 };
    (0xFF << 24) | (blend(16) << 16) | (blend(8) << 8) | blend(0)
}

fn add_color(dst: u32, src: u32) -> u32 {
    let add = |shift: u32| -> u32 { (((dst >> shift) & 0xFF) + ((src >> shift) & 0xFF)).min(255) };
    (0xFF << 24) | (add(16) << 16) | (add(8) << 8) | add(0)
}

fn scale_color(color: u32, amount: u8) -> u32 {
    let scale = amount as u32;
    let comp = |shift: u32| -> u32 { (((color >> shift) & 0xFF) * scale / 255).min(255) };
    (comp(24) << 24) | (comp(16) << 16) | (comp(8) << 8) | comp(0)
}
