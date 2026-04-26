#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#![no_main]

extern crate alloc;

#[path = "../runtime.rs"]
mod runtime;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};
use graphos_compositor::{CompositorState, SurfaceKind, ThemeTone};

// ── Compositor heap ───────────────────────────────────────────────────────────
//
// A simple lock-free bump allocator backed by a static BSS region.
// Deallocation is a no-op — the compositor runs for the OS session lifetime and
// its allocation pattern is dominated by one-time scene graph setup.
// Replace with a slab/free-list allocator in Phase 2 if dealloc pressure grows.

const HEAP_SIZE: usize = 96 * 1024 * 1024; // 96 MiB BSS; zero-cost in binary size

struct BumpAllocator {
    heap:   UnsafeCell<[u8; HEAP_SIZE]>,
    offset: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.heap.get() as usize;
        let align = layout.align();
        let size  = layout.size();
        loop {
            let cur     = self.offset.load(Ordering::Relaxed);
            let aligned = (base + cur + align - 1) & !(align - 1);
            let offset  = aligned - base;
            let next    = offset + size;
            if next > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.offset
                .compare_exchange(cur, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator — dealloc is intentionally a no-op.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap:   UnsafeCell::new([0u8; HEAP_SIZE]),
    offset: AtomicUsize::new(0),
};

const DEFAULT_WIDTH: u16 = 1280;
const DEFAULT_HEIGHT: u16 = 800;
const IPC_REPLY_CAP: usize = 240;
const TAG_DATA: u8 = 0x00;
const TAG_SHUTDOWN: u8 = 0x04;
const TAG_FRAME_TICK: u8 = 0x65;
const TAG_KEY: u8 = 0x60;
const TAG_POINTER: u8 = 0x61;
const CONTROL_DRAIN_BUDGET: usize = 4;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

fn write_line_joined(prefix: &[u8], middle: &[u8], suffix: &[u8]) {
    let mut line = [0u8; 160];
    let mut len = 0usize;
    for part in [prefix, middle, suffix] {
        if len + part.len() + 1 > line.len() {
            runtime::write_line(b"[compositor] log line truncated\n");
            return;
        }
        line[len..len + part.len()].copy_from_slice(part);
        len += part.len();
    }
    line[len] = b'\n';
    runtime::write_line(&line[..=len]);
}

fn append_u32(out: &mut [u8], len: &mut usize, mut value: u32) {
    if value == 0 {
        if *len < out.len() {
            out[*len] = b'0';
            *len += 1;
        }
        return;
    }

    let mut digits = [0u8; 10];
    let mut dlen = 0usize;
    while value > 0 {
        digits[dlen] = b'0' + (value % 10) as u8;
        value /= 10;
        dlen += 1;
    }
    while dlen > 0 {
        dlen -= 1;
        if *len < out.len() {
            out[*len] = digits[dlen];
            *len += 1;
        }
    }
}

fn append_hex_u64(out: &mut [u8], len: &mut usize, value: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let prefix = b"0x";
    if *len + prefix.len() > out.len() {
        return;
    }
    out[*len..*len + prefix.len()].copy_from_slice(prefix);
    *len += prefix.len();

    let mut started = false;
    for shift in (0..16).rev() {
        let nibble = ((value >> (shift * 4)) & 0xF) as usize;
        if nibble != 0 || started || shift == 0 {
            if *len >= out.len() {
                return;
            }
            out[*len] = HEX[nibble];
            *len += 1;
            started = true;
        }
    }
}

fn log_surface_mapping(surface_id: u32, vaddr: u64, pixel_count: usize) {
    let mut line = [0u8; 128];
    let mut len = 0usize;
    let prefix = b"[compositor] desktop surface id=";
    if len + prefix.len() > line.len() {
        runtime::write_line(b"[compositor] desktop surface log truncated\n");
        return;
    }
    line[len..len + prefix.len()].copy_from_slice(prefix);
    len += prefix.len();
    append_u32(&mut line, &mut len, surface_id);

    let middle = b" vaddr=";
    if len + middle.len() > line.len() {
        runtime::write_line(b"[compositor] desktop surface log truncated\n");
        return;
    }
    line[len..len + middle.len()].copy_from_slice(middle);
    len += middle.len();
    append_hex_u64(&mut line, &mut len, vaddr);

    let suffix = b" pixels=";
    if len + suffix.len() > line.len() {
        runtime::write_line(b"[compositor] desktop surface log truncated\n");
        return;
    }
    line[len..len + suffix.len()].copy_from_slice(suffix);
    len += suffix.len();
    append_u32(&mut line, &mut len, pixel_count as u32);

    if len < line.len() {
        line[len] = b'\n';
        runtime::write_line(&line[..=len]);
    }
}

fn trim_payload(payload: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < payload.len() && matches!(payload[start], b' ' | b'\t' | b'\r' | b'\n') {
        start += 1;
    }

    let mut end = payload.len();
    while end > start && matches!(payload[end - 1], b' ' | b'\t' | b'\r' | b'\n' | 0) {
        end -= 1;
    }

    &payload[start..end]
}

fn payload_is_printable_ascii(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return true;
    }
    for &b in payload {
        if !(b == b' ' || b == b'\t' || b.is_ascii_graphic()) {
            return false;
        }
    }
    true
}

fn log_ignored_command(payload: &[u8]) {
    if payload_is_printable_ascii(payload) {
        write_line_joined(b"[compositor] ignored command ", payload, b"");
    } else {
        runtime::write_line(b"[compositor] ignored binary command payload\n");
    }
}

fn parse_mark_dirty(payload: &[u8]) -> Option<u16> {
    let suffix = payload.strip_prefix(b"mark-dirty:")?;
    if suffix.is_empty() {
        return None;
    }

    let mut value = 0u16;
    for &byte in suffix {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?;
        value = value.checked_add((byte - b'0') as u16)?;
    }
    Some(value)
}

fn parse_theme(payload: &[u8]) -> Option<ThemeTone> {
    let suffix = payload.strip_prefix(b"theme:")?;
    match suffix {
        b"dark" => Some(ThemeTone::Dark),
        b"light" => Some(ThemeTone::Light),
        b"high-contrast" => Some(ThemeTone::HighContrast),
        _ => None,
    }
}

fn recv_meta_from_raw(raw: u64) -> Option<runtime::RecvMeta> {
    if raw == 0 || raw == u64::MAX {
        return None;
    }

    Some(runtime::RecvMeta {
        payload_len: (raw & 0xFFFF) as usize,
        tag: ((raw >> 16) & 0xFF) as u8,
        reply_endpoint: (raw >> 24) as u32,
    })
}

fn decode_frame_tick(tag: u8, payload: &[u8]) -> Option<u64> {
    if tag != TAG_FRAME_TICK || payload.len() < 8 {
        return None;
    }
    Some(u64::from_le_bytes([
        payload[0],
        payload[1],
        payload[2],
        payload[3],
        payload[4],
        payload[5],
        payload[6],
        payload[7],
    ]))
}

fn animation_frame(now_ms: u64) -> u32 {
    (now_ms / 16) as u32
}

fn snapshot_scene(state: &CompositorState, endpoint: u32) {
    if endpoint == 0 {
        return;
    }

    let mut snapshot = [0u8; 2048];
    let len = state.snapshot_bytes(&mut snapshot);
    let limit = len.min(snapshot.len()).min(IPC_REPLY_CAP);
    let _ = runtime::channel_send(
        endpoint,
        &snapshot[..limit],
        runtime::TAG_SERVICE_STATUS,
    );
}

fn summary_scene(state: &CompositorState, endpoint: u32) {
    if endpoint == 0 {
        return;
    }

    let mut summary = [0u8; IPC_REPLY_CAP];
    let len = state.summary_bytes(&mut summary);
    let limit = len.min(summary.len());
    let _ = runtime::channel_send(
        endpoint,
        &summary[..limit],
        runtime::TAG_SERVICE_STATUS,
    );
}

fn ack(endpoint: u32, payload: &[u8]) {
    if endpoint == 0 {
        return;
    }
    let _ = runtime::channel_send(endpoint, payload, runtime::TAG_SERVICE_STATUS);
}

fn announce_scene_ready(state: &CompositorState) {
    let telemetry = state.telemetry();
    let _ = runtime::bootstrap_status(b"compositor-scene-ready");
    let mut line = [0u8; 96];
    let mut len = 0usize;
    let prefix = b"[compositor] graph desktop ready surfaces=";
    if len + prefix.len() > line.len() {
        runtime::write_line(b"[compositor] scene summary truncated\n");
        return;
    }
    line[len..len + prefix.len()].copy_from_slice(prefix);
    len += prefix.len();
    append_u16(&mut line, &mut len, telemetry.surfaces);
    if len + 8 <= line.len() {
        line[len..len + 8].copy_from_slice(b" charts=");
        len += 8;
    }
    append_u16(&mut line, &mut len, telemetry.charts);
    if len < line.len() {
        line[len] = b'\n';
        runtime::write_line(&line[..=len]);
    }
}

fn append_u16(out: &mut [u8], len: &mut usize, mut value: u16) {
    if value == 0 {
        if *len < out.len() {
            out[*len] = b'0';
            *len += 1;
        }
        return;
    }

    let mut digits = [0u8; 5];
    let mut dlen = 0usize;
    while value > 0 {
        digits[dlen] = b'0' + (value % 10) as u8;
        value /= 10;
        dlen += 1;
    }
    while dlen > 0 {
        dlen -= 1;
        if *len < out.len() {
            out[*len] = digits[dlen];
            *len += 1;
        }
    }
}

fn fallback_palette(tone: ThemeTone) -> (u32, u32) {
    match tone {
        ThemeTone::Light => (0xFFFBF6EE, 0xFFE4D6C6),
        ThemeTone::HighContrast => (0xFF000000, 0xFF00E7FF),
        ThemeTone::Dark => (0xFF081421, 0xFF1A5069),
    }
}

fn render_bootstrap_frame(pixels: &mut [u32], state: &CompositorState, frame: u32) {
    let width = DEFAULT_WIDTH as usize;
    let height = DEFAULT_HEIGHT as usize;
    let pixel_count = width.saturating_mul(height);
    if pixels.len() < pixel_count || width == 0 || height == 0 {
        return;
    }

    let (sky_bottom, sky_top) = fallback_palette(state.tone);
    pixels[..pixel_count].fill(sky_bottom);

    let band_top = height / 5;
    let band_bottom = (height / 5).saturating_add((height / 7).max(1));
    for y in band_top..band_bottom.min(height) {
        let row = y * width;
        pixels[row..row + width].fill(sky_top);
    }

    let beat = ((frame / 2) as usize) % width.max(1);
    let strip_y = (height.saturating_sub(24)).min(height.saturating_sub(1));
    let strip_row = strip_y * width;
    let strip_end = (strip_row + width).min(pixel_count);
    pixels[strip_row..strip_end].fill(sky_top);
    let marker_start = strip_row + beat.saturating_sub(8);
    let marker_end = (strip_row + (beat + 8).min(width)).min(pixel_count);
    if marker_start < marker_end {
        pixels[marker_start..marker_end].fill(sky_bottom);
    }
}

fn render_bootstrap_probe(pixels: &mut [u32]) {
    let width = DEFAULT_WIDTH as usize;
    let height = DEFAULT_HEIGHT as usize;
    if width == 0 || height == 0 || pixels.len() < width.saturating_mul(height) {
        return;
    }

    // Cheap first-paint probe: touch only a thin set of rows so the screen is
    // visibly alive even if frame-ticks are delayed.
    for y in 0..24usize.min(height) {
        let row = y * width;
        pixels[row..row + width].fill(0xFF123047);
    }

    let band_y = height.saturating_sub(36);
    if band_y < height {
        let row = band_y * width;
        pixels[row..row + width].fill(0xFF2A6E84);
    }

    let cx = width / 2;
    let cy = height / 2;
    for dy in 0..18usize {
        let y = cy.saturating_add(dy).min(height.saturating_sub(1));
        let row = y * width;
        let x0 = cx.saturating_sub(120);
        let x1 = (cx + 120).min(width.saturating_sub(1));
        pixels[row + x0..=row + x1].fill(0xFFE6F0F7);
    }
}

fn fill_rect(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
) {
    if width == 0 || height == 0 || w == 0 || h == 0 {
        return;
    }

    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = (x.saturating_add(w as i32)).max(0).min(width as i32) as usize;
    let y1 = (y.saturating_add(h as i32)).max(0).min(height as i32) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    for py in y0..y1 {
        let row = py * width;
        pixels[row + x0..row + x1].fill(color);
    }
}

fn stroke_rect(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    thickness: u32,
    color: u32,
) {
    if thickness == 0 || w == 0 || h == 0 {
        return;
    }

    fill_rect(pixels, width, height, x, y, w, thickness.min(h), color);
    let bottom_h = thickness.min(h);
    fill_rect(
        pixels,
        width,
        height,
        x,
        y.saturating_add(h as i32).saturating_sub(bottom_h as i32),
        w,
        bottom_h,
        color,
    );
    fill_rect(pixels, width, height, x, y, thickness.min(w), h, color);
    let right_w = thickness.min(w);
    fill_rect(
        pixels,
        width,
        height,
        x.saturating_add(w as i32).saturating_sub(right_w as i32),
        y,
        right_w,
        h,
        color,
    );
}

fn lerp_channel(a: u32, b: u32, t: u32) -> u32 {
    ((a * (255 - t)) + (b * t)) / 255
}

fn lerp_color(a: u32, b: u32, t: u32) -> u32 {
    let aa = (a >> 24) & 0xFF;
    let ar = (a >> 16) & 0xFF;
    let ag = (a >> 8) & 0xFF;
    let ab = a & 0xFF;
    let ba = (b >> 24) & 0xFF;
    let br = (b >> 16) & 0xFF;
    let bg = (b >> 8) & 0xFF;
    let bb = b & 0xFF;

    (lerp_channel(aa, ba, t) << 24)
        | (lerp_channel(ar, br, t) << 16)
        | (lerp_channel(ag, bg, t) << 8)
        | lerp_channel(ab, bb, t)
}

fn rect_contains(x: i32, y: i32, rx: u16, ry: u16, rw: u16, rh: u16) -> bool {
    x >= rx as i32
        && y >= ry as i32
        && x < rx.saturating_add(rw) as i32
        && y < ry.saturating_add(rh) as i32
}

fn render_panel_payload(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    kind: SurfaceKind,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    accent: u32,
    frame: u32,
) {
    let inner_x = x.saturating_add(16);
    let inner_y = y.saturating_add(28);
    let inner_w = w.saturating_sub(32);
    let inner_h = h.saturating_sub(44);
    if inner_w == 0 || inner_h == 0 {
        return;
    }

    match kind {
        SurfaceKind::Navigation => {
            let row_h = (inner_h / 7).max(18);
            for idx in 0..5u32 {
                let row_y = inner_y.saturating_add((idx * row_h) as i32);
                let selected = (frame / 30 + idx) % 5 == 0;
                let row_color = if selected {
                    lerp_color(0xFF0E1823, accent, 72)
                } else {
                    0xFF101A28
                };
                fill_rect(pixels, width, height, inner_x, row_y, inner_w, row_h.saturating_sub(6), row_color);
                fill_rect(
                    pixels,
                    width,
                    height,
                    inner_x,
                    row_y,
                    6,
                    row_h.saturating_sub(6),
                    if selected { accent } else { 0xFF243245 },
                );
            }
        }
        SurfaceKind::Topbar => {
            let chip_w = (inner_w / 4).max(64);
            for idx in 0..3u32 {
                let chip_x = inner_x.saturating_add((idx * (chip_w + 12)) as i32);
                let pulse = ((frame + idx * 11) % 90) as i32;
                fill_rect(pixels, width, height, chip_x, inner_y, chip_w, 24, 0xFF101A28);
                fill_rect(
                    pixels,
                    width,
                    height,
                    chip_x + 8,
                    inner_y + 8,
                    pulse.max(12) as u32,
                    8,
                    accent,
                );
            }
        }
        SurfaceKind::Workspace => {
            let lane_h = (inner_h / 4).max(18);
            for idx in 0..3u32 {
                let lane_y = inner_y.saturating_add((idx * (lane_h + 12)) as i32);
                fill_rect(pixels, width, height, inner_x, lane_y, inner_w, lane_h, 0xFF0F1926);
                let progress = ((frame + idx * 17) % 100) as u32;
                let bar_w = inner_w.saturating_mul(progress.max(18)) / 100;
                fill_rect(pixels, width, height, inner_x, lane_y, bar_w, lane_h, accent);
            }
        }
        SurfaceKind::Chart => {
            let bar_count = 6u32;
            let gap = 10u32;
            let bar_w = inner_w.saturating_sub(gap * (bar_count - 1)).max(bar_count) / bar_count;
            for idx in 0..bar_count {
                let phase = (frame + idx * 13) % 100;
                let bar_h = 18 + inner_h.saturating_mul(phase.max(18)) / 100;
                let bar_x = inner_x.saturating_add((idx * (bar_w + gap)) as i32);
                let bar_y = inner_y.saturating_add(inner_h as i32).saturating_sub(bar_h as i32);
                fill_rect(pixels, width, height, bar_x, bar_y, bar_w, bar_h, accent);
            }
        }
        SurfaceKind::Console => {
            let row_h = 14u32;
            for idx in 0..8u32 {
                let row_y = inner_y.saturating_add((idx * row_h) as i32);
                let row_w = inner_w.saturating_sub(((frame + idx * 19) % 80) as u32);
                fill_rect(pixels, width, height, inner_x, row_y, row_w.max(32), 6, 0xFF6BD7B3);
            }
            let caret_x = inner_x.saturating_add(((frame * 6) % inner_w.max(1)) as i32);
            fill_rect(pixels, width, height, caret_x, inner_y + (7 * row_h as i32), 8, 10, accent);
        }
        SurfaceKind::Inspector => {
            let meter_w = (inner_w / 5).max(20);
            for idx in 0..4u32 {
                let meter_x = inner_x.saturating_add((idx * (meter_w + 10)) as i32);
                fill_rect(pixels, width, height, meter_x, inner_y, meter_w, inner_h, 0xFF0F1824);
                let level = inner_h.saturating_mul(((frame + idx * 21) % 100).max(24)) / 100;
                fill_rect(
                    pixels,
                    width,
                    height,
                    meter_x,
                    inner_y.saturating_add(inner_h as i32).saturating_sub(level as i32),
                    meter_w,
                    level,
                    accent,
                );
            }
        }
        SurfaceKind::StatusBar => {
            let beat = ((frame * 9) % inner_w.max(1)) as i32;
            fill_rect(pixels, width, height, inner_x, inner_y + 2, inner_w, 8, 0xFF0F1926);
            fill_rect(pixels, width, height, inner_x + beat, inner_y, 24, 12, accent);
        }
        SurfaceKind::Empty => {}
    }
}

fn render_desktop_frame(
    pixels: &mut [u32],
    state: &CompositorState,
    frame: u32,
    pointer_x: i32,
    pointer_y: i32,
    _pointer_buttons: u8,
) {
    render_bootstrap_frame(pixels, state, frame);
    if frame == 1 {
        runtime::write_line(b"[compositor] first frame stage=bootstrap-base\n");
    }

    let width = state.width as usize;
    let height = state.height as usize;
    if width == 0 || height == 0 {
        return;
    }

    let theme = state.theme;
    let bg_top = 0xFF00_0000 | theme.background;
    let bg_bottom = 0xFF00_0000 | theme.surface_muted;
    for y in 0..height {
        let t = ((y as u32) * 255 / height.max(1) as u32).min(255);
        let row = y * width;
        pixels[row..row + width].fill(lerp_color(bg_top, bg_bottom, t));
    }
    if frame == 1 {
        runtime::write_line(b"[compositor] first frame stage=background-gradient\n");
    }

    let horizon_y = height.saturating_sub(42);
    fill_rect(
        pixels,
        width,
        height,
        0,
        horizon_y as i32,
        width as u32,
        42,
        0xFF0B1420,
    );
    if frame == 1 {
        runtime::write_line(b"[compositor] first frame stage=horizon-band\n");
    }

    for (surface_index, surface) in state.surfaces.iter().enumerate() {
        if !surface.visible {
            continue;
        }
        if frame == 1 {
            let mut line = [0u8; 64];
            let mut len = 0usize;
            const PREFIX: &[u8] = b"[compositor] first frame surface=";
            line[len..len + PREFIX.len()].copy_from_slice(PREFIX);
            len += PREFIX.len();
            append_u32(&mut line, &mut len, surface_index as u32);
            line[len] = b'\n';
            runtime::write_line(&line[..=len]);
        }

        let bounds = surface.bounds;
        let x = bounds.x as i32;
        let y = bounds.y as i32;
        let w = bounds.w as u32;
        let h = bounds.h as u32;
        let accent = 0xFF00_0000 | surface.accent;
        let hovered = rect_contains(pointer_x, pointer_y, bounds.x, bounds.y, bounds.w, bounds.h);
        let panel_fill = if surface.focused {
            0xFF00_0000 | theme.surface_muted
        } else {
            0xFF00_0000 | theme.surface
        };
        let border = if surface.focused || hovered {
            accent
        } else {
            0xFF00_0000 | theme.border
        };

        fill_rect(pixels, width, height, x + 4, y + 6, w, h, 0xFF050A10);
        fill_rect(pixels, width, height, x, y, w, h, panel_fill);
        fill_rect(pixels, width, height, x, y, w, 18, lerp_color(panel_fill, accent, 52));
        stroke_rect(
            pixels,
            width,
            height,
            x,
            y,
            w,
            h,
            if surface.focused { 3 } else { 2 },
            border,
        );

        render_panel_payload(pixels, width, height, surface.kind, x, y, w, h, accent, frame);

        if hovered {
            stroke_rect(
                pixels,
                width,
                height,
                x + 6,
                y + 6,
                w.saturating_sub(12),
                h.saturating_sub(12),
                1,
                0xFFE8F3FF,
            );
        }
    }
    if frame == 1 {
        runtime::write_line(b"[compositor] first frame stage=surfaces-done\n");
    }
}

fn draw_pointer_cursor(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    buttons: u8,
) {
    if width == 0 || height == 0 {
        return;
    }

    let shadow = 0xFF07101A;
    let outline = 0xFF08182A;
    let fill = if buttons & 0x01 != 0 {
        0xFF9BE7FF
    } else {
        0xFFF7FBFF
    };

    let mut plot = |px: i32, py: i32, color: u32| {
        if px < 0 || py < 0 {
            return;
        }
        let px = px as usize;
        let py = py as usize;
        if px >= width || py >= height {
            return;
        }
        pixels[py * width + px] = color;
    };

    for dy in 0..18i32 {
        let span = if dy < 12 { dy / 2 + 1 } else { 7 - ((dy - 12) / 2) };
        if span <= 0 {
            continue;
        }
        for dx in 0..span {
            plot(x + dx + 1, y + dy + 1, shadow);
            let is_edge = dx == 0 || dx == span - 1 || dy == 0 || dy == 17;
            plot(x + dx, y + dy, if is_edge { outline } else { fill });
        }
    }

    for stem in 0..7i32 {
        plot(x + 5 + 1, y + 12 + stem + 1, shadow);
        plot(x + 5, y + 12 + stem, outline);
        plot(x + 6 + 1, y + 12 + stem + 1, shadow);
        plot(x + 6, y + 12 + stem, fill);
    }
}

fn launch_target_for_surface(kind: SurfaceKind) -> Option<&'static [u8]> {
    match kind {
        SurfaceKind::Navigation => Some(b"cube"),
        SurfaceKind::Topbar => Some(b"settings"),
        SurfaceKind::Workspace => Some(b"files"),
        SurfaceKind::Chart => Some(b"cube"),
        SurfaceKind::Console => Some(b"terminal"),
        SurfaceKind::Inspector => Some(b"ai-console"),
        SurfaceKind::StatusBar | SurfaceKind::Empty => None,
    }
}

fn handle_shell_key(state: &mut CompositorState, ascii: u8) -> bool {
    match ascii {
        b'\t' => state.focus_next().is_some(),
        b'g' | b'i' => {
            state.toggle_inspector();
            true
        }
        b'l' => runtime::spawn_named_checked(b"cube"),
        b'f' => runtime::spawn_named_checked(b"files"),
        b's' => runtime::spawn_named_checked(b"settings"),
        b't' => runtime::spawn_named_checked(b"terminal"),
        b'a' => runtime::spawn_named_checked(b"ai-console"),
        b'3' => runtime::spawn_named_checked(b"cube"),
        _ => false,
    }
}

fn handle_shell_pointer(
    state: &mut CompositorState,
    x: i32,
    y: i32,
    buttons: u8,
    previous_buttons: u8,
) -> bool {
    let left_pressed = (buttons & 0x01) != 0;
    let left_was_pressed = (previous_buttons & 0x01) != 0;
    if !left_pressed || left_was_pressed {
        return false;
    }

    let Some(surface_id) = state.surface_at(x, y) else {
        return false;
    };

    let mut changed = state.focus_surface(surface_id);
    if let Some(kind) = state.surface_kind(surface_id)
        && let Some(target) = launch_target_for_surface(kind)
    {
        let _ = runtime::spawn_named_checked(target);
        changed = true;
    }
    changed
}

struct CompositorLoopState {
    frame_counter: u32,
    frame_tick_counter: u32,
    last_present_ms: u64,
    redraw_stall_reported: bool,
    frame_tick_stall_reported: bool,
    wakeups_without_tick: u32,
    pointer_x: i32,
    pointer_y: i32,
    pointer_buttons: u8,
    pointer_event_logged: bool,
}

fn handle_inbox_message(
    state: &mut CompositorState,
    loop_state: &mut CompositorLoopState,
    desktop_surface_id: u32,
    meta: runtime::RecvMeta,
    inbox: &[u8],
) -> Option<u64> {
    let payload_len = meta.payload_len;
    let tag = meta.tag;
    let reply_endpoint = meta.reply_endpoint;
    if payload_len > inbox.len() {
        runtime::write_line(b"[compositor] dropped oversized payload\n");
        ack(reply_endpoint, b"invalid-payload");
        return None;
    }

    let payload = &inbox[..payload_len];
    if let Some(now_ms) = decode_frame_tick(tag, payload) {
        loop_state.frame_tick_counter = loop_state.frame_tick_counter.wrapping_add(1);
        loop_state.wakeups_without_tick = 0;
        loop_state.frame_tick_stall_reported = false;
        if loop_state.last_present_ms != 0
            && now_ms.saturating_sub(loop_state.last_present_ms) > 2000
            && !loop_state.redraw_stall_reported
        {
            runtime::write_line(b"[compositor] stall: surface-present cadence paused for >2000ms\n");
            loop_state.redraw_stall_reported = true;
        }
        if loop_state.frame_tick_counter == 1 {
            runtime::write_line(b"[compositor] first frame-tick received\n");
        }
        if loop_state.frame_tick_counter <= 16 || loop_state.frame_tick_counter % 120 == 0 {
            let mut line = [0u8; 112];
            let mut len = 0usize;
            const PREFIX: &[u8] = b"[compositor] frame_count=";
            line[len..len + PREFIX.len()].copy_from_slice(PREFIX);
            len += PREFIX.len();
            append_u32(&mut line, &mut len, loop_state.frame_counter);

            const MID: &[u8] = b" frame_tick_count=";
            line[len..len + MID.len()].copy_from_slice(MID);
            len += MID.len();
            append_u32(&mut line, &mut len, loop_state.frame_tick_counter);

            if len < line.len() {
                line[len] = b'\n';
                runtime::write_line(&line[..=len]);
            }
        }
        return Some(now_ms);
    }

    if tag == TAG_KEY && payload_len >= 3 {
        let _ = payload[0] != 0 && handle_shell_key(state, payload[2]);
        return None;
    }

    if tag == TAG_POINTER && payload_len >= 5 {
        let x = i16::from_le_bytes([payload[0], payload[1]]) as i32;
        let y = i16::from_le_bytes([payload[2], payload[3]]) as i32;
        let buttons = payload[4];
        loop_state.pointer_x = x;
        loop_state.pointer_y = y;
        if !loop_state.pointer_event_logged {
            runtime::write_line(b"[compositor] first pointer event received\n");
            loop_state.pointer_event_logged = true;
        }
        let _ = handle_shell_pointer(
            state,
            x,
            y,
            buttons,
            loop_state.pointer_buttons,
        );
        loop_state.pointer_buttons = buttons;
        return None;
    }

    if tag != TAG_DATA && tag != TAG_SHUTDOWN {
        return None;
    }

    let payload = trim_payload(payload);

    if payload == b"shutdown" {
        let _ = runtime::bootstrap_status(b"service-stop:compositor");
        runtime::write_line(b"[compositor] shutdown\n");
        let _ = runtime::surface_destroy(desktop_surface_id);
        runtime::exit(0);
    }

    if payload == b"flush" {
        if runtime::surface_pending() && runtime::surface_flush() {
            let _ = runtime::bootstrap_status(b"compositor-surface-flush");
        }
    } else if payload == b"snapshot" {
        snapshot_scene(state, reply_endpoint);
    } else if payload == b"scene-summary" {
        summary_scene(state, reply_endpoint);
    } else if payload == b"focus-next" {
        let _ = state.focus_next();
        snapshot_scene(state, reply_endpoint);
    } else if payload == b"toggle-inspector" {
        let visible = state.toggle_inspector();
        let _ = runtime::bootstrap_status(if visible {
            b"compositor-inspector-visible" as &[u8]
        } else {
            b"compositor-inspector-hidden" as &[u8]
        });
        snapshot_scene(state, reply_endpoint);
    } else if payload == b"reset-scene" {
        state.seed_graph_desktop();
        let _ = runtime::bootstrap_status(b"compositor-scene-reset");
        snapshot_scene(state, reply_endpoint);
    } else if let Some(theme) = parse_theme(payload) {
        state.tone = theme;
        state.seed_graph_desktop();
        let _ = runtime::bootstrap_status(b"compositor-theme-updated");
        snapshot_scene(state, reply_endpoint);
    } else if let Some(surface_id) = parse_mark_dirty(payload) {
        if state.mark_dirty(surface_id) {
            snapshot_scene(state, reply_endpoint);
        } else {
            ack(reply_endpoint, b"missing-surface");
        }
    } else {
        log_ignored_command(payload);
        ack(reply_endpoint, b"unknown-command");
    }

    None
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let compositor_channel = runtime::service_inbox_or_die(b"compositor");
    runtime::claim_inbox(compositor_channel);
    runtime::write_line(b"[compositor] graph-shell compositor online\n");
    let _ = runtime::bootstrap_status(b"compositor-online");
    let _ = runtime::bootstrap_status(b"service-bound:compositor");

    let mut state = CompositorState::new(DEFAULT_WIDTH, DEFAULT_HEIGHT, ThemeTone::Dark);
    let Some((desktop_surface_id, desktop_vaddr)) =
        runtime::surface_create(DEFAULT_WIDTH, DEFAULT_HEIGHT)
    else {
        runtime::write_line(b"[compositor] failed to allocate desktop surface\n");
        runtime::exit(0xE1);
    };

    let pixel_count = (DEFAULT_WIDTH as usize).saturating_mul(DEFAULT_HEIGHT as usize);
    log_surface_mapping(desktop_surface_id, desktop_vaddr, pixel_count);
    runtime::write_line(b"[compositor] preparing desktop pixel slice\n");
    let pixels = unsafe { core::slice::from_raw_parts_mut(desktop_vaddr as *mut u32, pixel_count) };
    runtime::write_line(b"[compositor] desktop pixel slice ready\n");
    // Keep first-paint path minimal: claim with a cheap bootstrap probe, then
    // switch permanently onto the shared-surface presenter so display ownership,
    // composition, and commit all flow through one path.
    render_bootstrap_probe(pixels);
    runtime::write_line(b"[compositor] bootstrap frame seeded (light probe)\n");
    runtime::write_line(b"[compositor] claiming display for bootstrap frame\n");
    let mut attempts = 0u32;
    loop {
        if runtime::compositor_claim_display(desktop_surface_id) {
            break;
        }
        attempts = attempts.wrapping_add(1);
        if (attempts & 0x1F) == 0 {
            runtime::write_line(b"[compositor] claim pending, yielding\n");
        }
        runtime::yield_now();
    }
    runtime::write_line(b"[compositor] display claim succeeded\n");
    runtime::heartbeat();

    let mut pointer_x = (DEFAULT_WIDTH / 2) as i32;
    let mut pointer_y = (DEFAULT_HEIGHT / 2) as i32;
    let mut pointer_buttons = 0u8;
    runtime::write_line(b"[compositor] preparing first desktop frame\n");
    render_desktop_frame(pixels, &state, 1, pointer_x, pointer_y, pointer_buttons);
    draw_pointer_cursor(
        pixels,
        DEFAULT_WIDTH as usize,
        DEFAULT_HEIGHT as usize,
        pointer_x,
        pointer_y,
        pointer_buttons,
    );
    runtime::write_line(b"[compositor] first desktop frame rasterized\n");
    runtime::write_line(b"[compositor] unified surface-present pipeline active\n");

    runtime::write_line(b"[compositor] committing bootstrap frame\n");
    let _ = runtime::surface_commit(desktop_surface_id);
    runtime::write_line(b"[compositor] bootstrap frame committed\n");
    runtime::write_line(b"[compositor] first present ok\n");
    runtime::write_line(b"[compositor] frame_count=1\n");
    let _ = runtime::bootstrap_status(b"compositor-display-claimed");
    let _ = runtime::bootstrap_status(b"compositor-first-present");
    let Some(event_channel) = runtime::channel_create(64) else {
        runtime::write_line(b"[compositor] failed to allocate event channel\n");
        let _ = runtime::surface_destroy(desktop_surface_id);
        runtime::exit(0xE6);
    };
    if !runtime::input_register_window(0, 0, DEFAULT_WIDTH, DEFAULT_HEIGHT, event_channel) {
        runtime::write_line(b"[compositor] failed to register compositor event window\n");
        let _ = runtime::surface_destroy(desktop_surface_id);
        runtime::exit(0xE7);
    }
    runtime::input_set_focus(event_channel);
    runtime::write_line(b"[compositor] dedicated event channel armed\n");
    runtime::write_line(b"[compositor] subscribing to frame-tick\n");
    if !runtime::frame_tick_subscribe(event_channel) {
        runtime::write_line(b"[compositor] frame-tick subscription failed\n");
        let _ = runtime::surface_destroy(desktop_surface_id);
        runtime::exit(0xE5);
    }
    runtime::write_line(b"[compositor] frame-tick subscribed\n");
    let _ = runtime::bootstrap_status(b"compositor-frameclock-subscribed");
    announce_scene_ready(&state);
    let _ = runtime::bootstrap_status(b"service-ready:compositor");
    runtime::announce_service_ready(b"compositor");

    let mut inbox = [0u8; 256];
    let mut loop_state = CompositorLoopState {
        frame_counter: 1,
        frame_tick_counter: 0,
        last_present_ms: 0,
        redraw_stall_reported: false,
        frame_tick_stall_reported: false,
        wakeups_without_tick: 0,
        pointer_x,
        pointer_y,
        pointer_buttons,
        pointer_event_logged: false,
    };
    loop {
        let mut frame_tick_ms = None;

        for _ in 0..CONTROL_DRAIN_BUDGET {
            let Some(meta) = runtime::try_recv(compositor_channel, &mut inbox) else {
                break;
            };
            if let Some(now_ms) =
                handle_inbox_message(&mut state, &mut loop_state, desktop_surface_id, meta, &inbox)
            {
                frame_tick_ms = Some(now_ms);
            }
        }

        let Some(meta) = recv_meta_from_raw(runtime::channel_recv(event_channel, &mut inbox)) else {
            loop_state.wakeups_without_tick = loop_state.wakeups_without_tick.wrapping_add(1);
            if loop_state.wakeups_without_tick >= 2048 && !loop_state.frame_tick_stall_reported {
                runtime::write_line(
                    b"[compositor] stall: waiting for frame-tick events (wakeups without IPC only)\n",
                );
                loop_state.frame_tick_stall_reported = true;
            }
            runtime::yield_now();
            continue;
        };

        if let Some(now_ms) = handle_inbox_message(
            &mut state,
            &mut loop_state,
            desktop_surface_id,
            meta,
            &inbox,
        ) {
            frame_tick_ms = Some(now_ms);
        }

        while let Some(meta) = runtime::try_recv(event_channel, &mut inbox) {
            if let Some(now_ms) = handle_inbox_message(
                &mut state,
                &mut loop_state,
                desktop_surface_id,
                meta,
                &inbox,
            ) {
                frame_tick_ms = Some(now_ms);
            }
        }

        if let Some(now_ms) = frame_tick_ms {
            let frame = animation_frame(now_ms).max(loop_state.frame_counter);
            render_desktop_frame(
                pixels,
                &state,
                frame,
                loop_state.pointer_x,
                loop_state.pointer_y,
                loop_state.pointer_buttons,
            );
            draw_pointer_cursor(
                pixels,
                DEFAULT_WIDTH as usize,
                DEFAULT_HEIGHT as usize,
                loop_state.pointer_x,
                loop_state.pointer_y,
                loop_state.pointer_buttons,
            );
            let _ = runtime::surface_commit(desktop_surface_id);
            loop_state.frame_counter = loop_state.frame_counter.wrapping_add(1);
            loop_state.last_present_ms = now_ms;
            loop_state.redraw_stall_reported = false;

            if loop_state.frame_counter <= 16 || loop_state.frame_counter % 120 == 0 {
                let mut line = [0u8; 96];
                let mut len = 0usize;
                const PREFIX: &[u8] = b"[compositor] frame_count=";
                line[len..len + PREFIX.len()].copy_from_slice(PREFIX);
                len += PREFIX.len();
                append_u32(&mut line, &mut len, loop_state.frame_counter);
                if len < line.len() {
                    line[len] = b'\n';
                    runtime::write_line(&line[..=len]);
                }
            }
        }

        for _ in 0..CONTROL_DRAIN_BUDGET {
            let Some(meta) = runtime::try_recv(compositor_channel, &mut inbox) else {
                break;
            };
            let _ = handle_inbox_message(
                &mut state,
                &mut loop_state,
                desktop_surface_id,
                meta,
                &inbox,
            );
        }
    }
}
