// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Spinning cube demo rendered through the GraphOS GPU command path.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

#[path = "../runtime.rs"]
mod runtime;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::mem::size_of;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};
use graphos_app_sdk::event;
use graphos_app_sdk::sys as app_sys;
use graphos_app_sdk::window::Window;
use graphos_gfx::types::{BufferKind, DepthState, IndexFormat, RasterState, Topology, VertexLayout};
use graphos_gfx::{Color, CommandBuffer, GpuContext, GpuDevice, PixelFormat, ResourceId, ResourceKind};
use graphos_gl::math::{Mat4, Vec3};

const HEAP_SIZE: usize = 8 * 1024 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    offset: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.heap.get() as usize;
        let align = layout.align();
        let size = layout.size();
        loop {
            let current = self.offset.load(Ordering::Relaxed);
            let aligned = (base + current + align - 1) & !(align - 1);
            let offset = aligned - base;
            let next = offset + size;
            if next > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .offset
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: UnsafeCell::new([0u8; HEAP_SIZE]),
    offset: AtomicUsize::new(0),
};

#[repr(C)]
#[derive(Clone, Copy)]
struct VertexPos3Color {
    x: f32,
    y: f32,
    z: f32,
    color: u32,
}

const CUBE_VERTS: [VertexPos3Color; 8] = [
    VertexPos3Color { x: -0.8, y: -0.8, z: -0.8, color: 0xFFFF_6A6A },
    VertexPos3Color { x: 0.8, y: -0.8, z: -0.8, color: 0xFFFF_D36A },
    VertexPos3Color { x: 0.8, y: 0.8, z: -0.8, color: 0xFFEC_FF6A },
    VertexPos3Color { x: -0.8, y: 0.8, z: -0.8, color: 0xFF83_FF6A },
    VertexPos3Color { x: -0.8, y: -0.8, z: 0.8, color: 0xFF6A_FFD9 },
    VertexPos3Color { x: 0.8, y: -0.8, z: 0.8, color: 0xFF6A_C8FF },
    VertexPos3Color { x: 0.8, y: 0.8, z: 0.8, color: 0xFF8E_6AFF },
    VertexPos3Color { x: -0.8, y: 0.8, z: 0.8, color: 0xFFE9_6AFF },
];

const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3,
    1, 5, 6, 1, 6, 2,
    5, 4, 7, 5, 7, 6,
    4, 0, 3, 4, 3, 7,
    3, 2, 6, 3, 6, 7,
    4, 5, 1, 4, 1, 0,
];

fn as_bytes<T>(slice: &[T]) -> &[u8] {
    let len = size_of::<T>().saturating_mul(slice.len());
    unsafe { core::slice::from_raw_parts(slice.as_ptr() as *const u8, len) }
}

fn mat4_to_row_major(m: Mat4) -> [[f32; 4]; 4] {
    [
        [m.cols[0].x, m.cols[1].x, m.cols[2].x, m.cols[3].x],
        [m.cols[0].y, m.cols[1].y, m.cols[2].y, m.cols[3].y],
        [m.cols[0].z, m.cols[1].z, m.cols[2].z, m.cols[3].z],
        [m.cols[0].w, m.cols[1].w, m.cols[2].w, m.cols[3].w],
    ]
}

fn run_software_cube_fallback() -> ! {
    runtime::write_line(b"[cube] falling back to software animation window\n");

    let input_channel = runtime::channel_create(64).unwrap_or(0);
    let Some(mut win) = Window::open(960, 600, 120, 80, input_channel) else {
        runtime::write_line(b"[cube] software fallback window open failed\n");
        runtime::exit(1)
    };
    win.request_focus();

    let mut frame = 0u32;
    loop {
        let t = frame as i32;
        let x = 120 + ((t / 2) % 220);
        let y = 90 + ((t / 3) % 160);

        {
            let mut canvas = win.canvas();
            canvas.clear(0xFF0C1624);

            // Layered quads approximate a rotating cube silhouette in pure 2D.
            canvas.fill_rect(x, y, 220, 220, 0xFF2A5A8A);
            canvas.fill_rect(x + 34, y - 26, 220, 220, 0xFF3F7FC2);
            canvas.fill_rect(x + 68, y - 52, 220, 220, 0xFF88C5FF);

            canvas.fill_rect(20, 20, 360, 22, 0xFF17324A);
            canvas.fill_rect(22, 22, (frame % 336) + 1, 18, 0xFF6AD0FF);
        }

        if !win.present() {
            runtime::write_line(b"[cube] software fallback present failed\n");
        }

        frame = frame.wrapping_add(1);
        app_sys::sleep_ticks(8);
    }
}

fn run_cube() -> ! {
    runtime::write_line(b"[cube] starting\n");
    runtime::write_line(b"[cube] draw mode: gpu submit\n");

    let Some(device) = GpuDevice::open() else {
        runtime::write_line(b"[cube] gpu open failed\n");
        runtime::exit(1)
    };

    let caps = device.caps();
    if caps.present_2d {
        runtime::write_line(b"[cube] gpu caps: present_2d=1\n");
    } else {
        runtime::write_line(b"[cube] gpu caps: present_2d=0\n");
    }

    let Some(swapchain) = device.create_swapchain(PixelFormat::Bgra8Unorm) else {
        runtime::write_line(b"[cube] swapchain creation failed\n");
        runtime::exit(1)
    };

    let render_w = core::cmp::min(swapchain.width, 960);
    let render_h = core::cmp::min(swapchain.height, 600);
    {
        let mut line = [0u8; 96];
        let mut n = 0usize;
        const PREFIX: &[u8] = b"[cube] render_target=";
        line[n..n + PREFIX.len()].copy_from_slice(PREFIX);
        n += PREFIX.len();
        append_u32_dec(&mut line, &mut n, render_w);
        line[n] = b'x';
        n += 1;
        append_u32_dec(&mut line, &mut n, render_h);
        line[n] = b'\n';
        n += 1;
        runtime::write_line(&line[..n]);
    }

    let color_rt = device
        .alloc_resource(render_w, render_h, PixelFormat::Bgra8Unorm, ResourceKind::RenderTarget)
        .unwrap_or(ResourceId::INVALID);
    if !color_rt.is_valid() {
        runtime::write_line(b"[cube] color target alloc failed\n");
        runtime::exit(1)
    }

    let depth = device
        .alloc_resource(
            render_w,
            render_h,
            PixelFormat::Rgba8Unorm,
            ResourceKind::DepthStencil,
        )
        .unwrap_or(ResourceId::INVALID);

    let Some(vbo) = device.alloc_buffer(BufferKind::Vertex, (size_of::<VertexPos3Color>() * CUBE_VERTS.len()) as u32) else {
        runtime::write_line(b"[cube] vertex buffer alloc failed\n");
        runtime::exit(1)
    };
    let Some(ibo) = device.alloc_buffer(BufferKind::Index, (size_of::<u16>() * CUBE_INDICES.len()) as u32) else {
        runtime::write_line(b"[cube] index buffer alloc failed\n");
        runtime::exit(1)
    };

    let mut init = CommandBuffer::new();
    init.upload_buffer(vbo, as_bytes(&CUBE_VERTS));
    init.upload_buffer(ibo, as_bytes(&CUBE_INDICES));
    if !device.submit(&init) {
        runtime::write_line(b"[cube] initial gpu upload failed\n");
        run_software_cube_fallback()
    }

    let aspect = render_w as f32 / render_h as f32;
    let proj = Mat4::perspective(core::f32::consts::FRAC_PI_4, aspect, 0.1, 100.0);
    let view = Mat4::look_at(Vec3::new(0.0, 1.3, 4.0), Vec3::ZERO, Vec3::Y);

    let input_channel = runtime::channel_create(64).unwrap_or(0);
    if input_channel == 0 {
        runtime::write_line(b"[cube] input channel create failed\n");
    } else if app_sys::input_register_window(
        0,
        0,
        swapchain.width as u16,
        swapchain.height as u16,
        input_channel,
    ) {
        app_sys::input_set_focus(input_channel);
        runtime::write_line(b"[cube] pointer input enabled (drag with left mouse)\n");
    } else {
        runtime::write_line(b"[cube] input register window failed\n");
    }

    let mut recv_buf = [0u8; 64];
    let mut drag_active = false;
    let mut last_ptr_x: i16 = 0;
    let mut last_ptr_y: i16 = 0;
    let mut orbit_x = 0.0f32;
    let mut orbit_y = 0.0f32;

    let mut angle = 0.0f32;
    let mut frame_count = 0u32;
    let mut submit_ok = 0u32;
    let mut submit_fail = 0u32;
    let mut first_submit_logged = false;
    let mut last_submit_ok_frame = 0u32;
    let mut submit_stall_reported = false;

    runtime::write_line(b"[cube] entering render loop\n");

    loop {
        // Keep input work bounded so render submit cadence stays stable.
        for _ in 0..8 {
            if input_channel == 0 {
                break;
            }
            let Some(meta) = runtime::try_recv(input_channel, &mut recv_buf) else {
                break;
            };
            if meta.tag != event::TAG_POINTER || meta.payload_len < 5 {
                continue;
            }

            let x = i16::from_le_bytes([recv_buf[0], recv_buf[1]]);
            let y = i16::from_le_bytes([recv_buf[2], recv_buf[3]]);
            let buttons = recv_buf[4];
            let left_down = (buttons & 0x01) != 0;

            if left_down {
                if drag_active {
                    let dx = (x - last_ptr_x) as f32;
                    let dy = (y - last_ptr_y) as f32;
                    orbit_y += dx * 0.012;
                    orbit_x += dy * 0.010;
                }
                last_ptr_x = x;
                last_ptr_y = y;
                drag_active = true;
            } else {
                drag_active = false;
            }
        }

        let model = Mat4::rotation_y(angle + orbit_y) * Mat4::rotation_x(angle * 0.55 + orbit_x);
        let mvp = proj * view * model;

        let mut ctx = GpuContext::new(&device);
        ctx.cmd()
            .begin_frame(color_rt, Some(Color(0xFF1A_1A2E)))
            .set_render_target(color_rt, depth)
            .set_viewport(0.0, 0.0, render_w as f32, render_h as f32)
            .set_raster_state(RasterState::NO_CULL)
            .set_transform(mat4_to_row_major(mvp));

        if depth.is_valid() {
            ctx.cmd().set_depth_state(DepthState::READ_WRITE).clear_depth(1.0);
        } else {
            ctx.cmd().set_depth_state(DepthState::DISABLED);
        }

        ctx.cmd()
            .draw_primitives(
                vbo,
                ibo,
                VertexLayout::Pos3Color,
                IndexFormat::U16,
                Topology::Triangles,
                0,
                CUBE_INDICES.len() as u32,
                1,
            )
            .end_frame(color_rt)
            .present(color_rt);

        if ctx.submit() {
            submit_ok = submit_ok.wrapping_add(1);
            last_submit_ok_frame = frame_count;
            submit_stall_reported = false;
            if !first_submit_logged {
                runtime::write_line(b"[cube] first gpu submit ok\n");
                first_submit_logged = true;
            }
        } else {
            submit_fail = submit_fail.wrapping_add(1);
            if submit_fail <= 8 || submit_fail % 60 == 0 {
                runtime::write_line(b"[cube] gpu submit failed\n");
            }
        }

        frame_count = frame_count.wrapping_add(1);
        if frame_count <= 16 || frame_count % 60 == 0 {
            let mut stats = [0u8; 96];
            let mut len = 0usize;

            const PREFIX_OK: &[u8] = b"[cube] submit_ok=";
            stats[len..len + PREFIX_OK.len()].copy_from_slice(PREFIX_OK);
            len += PREFIX_OK.len();
            append_u32_dec(&mut stats, &mut len, submit_ok);

            const MID_FAIL: &[u8] = b" fail=";
            stats[len..len + MID_FAIL.len()].copy_from_slice(MID_FAIL);
            len += MID_FAIL.len();
            append_u32_dec(&mut stats, &mut len, submit_fail);

            const MID_FRAME: &[u8] = b" frame=";
            stats[len..len + MID_FRAME.len()].copy_from_slice(MID_FRAME);
            len += MID_FRAME.len();
            append_u32_dec(&mut stats, &mut len, frame_count);

            stats[len] = b'\n';
            len += 1;
            runtime::write_line(&stats[..len]);
        }

        if frame_count.wrapping_sub(last_submit_ok_frame) >= 180 && !submit_stall_reported {
            runtime::write_line(b"[cube] stall: no successful gpu submit in 180 frames\n");
            submit_stall_reported = true;
        }

        angle += 0.05;
        if angle > core::f32::consts::TAU {
            angle -= core::f32::consts::TAU;
        }
        app_sys::sleep_ticks(16);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    run_cube()
}

struct LineBuf {
    buf: [u8; 128],
    len: usize,
}

impl Write for LineBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let b = s.as_bytes();
        let n = b.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut w = LineBuf {
        buf: [0u8; 128],
        len: 0,
    };
    let _ = write!(w, "[cube] panic: {}\n", info);
    runtime::write_line(&w.buf[..w.len]);
    runtime::exit(255)
}

fn append_u32_dec(out: &mut [u8], len: &mut usize, mut value: u32) {
    let mut tmp = [0u8; 10];
    let mut n = 0usize;
    if value == 0 {
        if *len < out.len() {
            out[*len] = b'0';
            *len += 1;
        }
        return;
    }
    while value != 0 {
        tmp[n] = b'0' + (value % 10) as u8;
        value /= 10;
        n += 1;
    }
    while n != 0 {
        n -= 1;
        if *len < out.len() {
            out[*len] = tmp[n];
            *len += 1;
        }
    }
}
