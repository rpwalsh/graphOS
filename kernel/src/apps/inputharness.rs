// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::gfx::canvas::Canvas;
use crate::gfx::surface::Surface;
use crate::input::diagnostics::{
    self, InputHealth, KEY_HISTORY_LINES, POINTER_TRAIL_POINTS, Snapshot,
};
use crate::input::event::InputEvent;

const BG: u32 = 0x000c1117;
const PANEL_BG: u32 = 0x00101820;
const PANEL_EDGE: u32 = 0x00334a5e;
const TEXT: u32 = 0x00e6eff7;
const MUTED: u32 = 0x0092a8ba;
const GOOD: u32 = 0x0077d7a2;
const WARN: u32 = 0x00f0c36d;
const BAD: u32 = 0x00ef8c85;
const CLEAR_BUTTON: (i32, i32, u32, u32) = (432, 14, 108, 22);

pub struct InputHarnessApp;

impl InputHarnessApp {
    pub fn new() -> Self {
        Self
    }

    pub fn handle_event(&mut self, event: InputEvent) {
        if let InputEvent::Text(byte) = event
            && matches!(byte, b'c' | b'C' | b'r' | b'R')
        {
            diagnostics::clear_history();
        }
    }

    pub fn handle_click(&mut self, x: i32, y: i32) -> bool {
        let (bx, by, bw, bh) = CLEAR_BUTTON;
        if x >= bx
            && y >= by
            && x < bx.saturating_add(bw as i32)
            && y < by.saturating_add(bh as i32)
        {
            diagnostics::clear_history();
            return true;
        }
        false
    }

    pub fn render(&self, surface: &mut Surface) {
        let snapshot = diagnostics::snapshot();
        let width = surface.width();
        let mut canvas = Canvas::new(surface);
        canvas.clear(BG);

        canvas.fill_rect(0, 0, width, 46, 0x00111a22);
        let health_color = match snapshot.health {
            InputHealth::Stable => GOOD,
            InputHealth::Watching => WARN,
            InputHealth::Unstable => BAD,
        };
        canvas.fill_rect(16, 14, 104, 16, health_color);
        canvas.draw_text(
            24,
            18,
            health_bytes(snapshot.health),
            0x0009120f,
            health_color,
        );
        canvas.draw_text(136, 12, b"input harness", TEXT, 0x00111a22);
        canvas.draw_text(
            136,
            24,
            b"pointer, display, keyboard, graph",
            MUTED,
            0x00111a22,
        );
        draw_button(&mut canvas, CLEAR_BUTTON, b"clear c/r");

        draw_panel(&mut canvas, 16, 58, 240, 90, b"pointer path");
        draw_panel(&mut canvas, 272, 58, 272, 90, b"graph state");
        draw_panel(&mut canvas, 16, 160, 320, 176, b"motion surface");
        draw_panel(&mut canvas, 352, 160, 192, 176, b"keyboard capture");

        let mut line = LineBuf::new();
        line.push_bytes(b"backend ");
        line.push_bytes(&snapshot.backend[..snapshot.backend_len]);
        draw_line(&mut canvas, 28, 82, &line, TEXT);

        let mut line = LineBuf::new();
        line.push_bytes(b"events in ");
        line.push_u64(snapshot.enqueued_events);
        line.push_bytes(b" out ");
        line.push_u64(snapshot.delivered_events);
        draw_line(&mut canvas, 28, 96, &line, TEXT);

        let mut line = LineBuf::new();
        line.push_bytes(b"queue ");
        line.push_u64(snapshot.queue_depth as u64);
        line.push_bytes(b" max ");
        line.push_u64(snapshot.max_queue_depth as u64);
        draw_line(
            &mut canvas,
            28,
            110,
            &line,
            status_color(snapshot.queue_depth <= 2),
        );

        let mut line = LineBuf::new();
        line.push_bytes(b"jump ");
        line.push_u64(snapshot.last_jump as u64);
        line.push_bytes(b" max ");
        line.push_u64(snapshot.max_jump as u64);
        line.push_bytes(b" severe ");
        line.push_u64(snapshot.severe_jump_count);
        draw_line(
            &mut canvas,
            28,
            124,
            &line,
            if snapshot.severe_jump_count == 0 {
                GOOD
            } else {
                BAD
            },
        );

        let mut line = LineBuf::new();
        line.push_bytes(b"pointer node ");
        line.push_u64(snapshot.pointer_device_node);
        draw_line(&mut canvas, 284, 82, &line, TEXT);

        let mut line = LineBuf::new();
        line.push_bytes(b"keyboard node ");
        line.push_u64(snapshot.keyboard_device_node);
        draw_line(&mut canvas, 284, 96, &line, TEXT);

        let mut line = LineBuf::new();
        line.push_bytes(b"anomaly node ");
        line.push_u64(snapshot.latest_anomaly_node);
        draw_line(
            &mut canvas,
            284,
            110,
            &line,
            if snapshot.latest_anomaly_node == 0 {
                MUTED
            } else {
                WARN
            },
        );

        let mut line = LineBuf::new();
        line.push_bytes(b"graph anomalies ");
        line.push_u64(snapshot.graph_anomaly_count);
        draw_line(
            &mut canvas,
            284,
            124,
            &line,
            if snapshot.graph_anomaly_count == 0 {
                GOOD
            } else {
                BAD
            },
        );

        render_motion_surface(&mut canvas, 28, 184, 296, 140, &snapshot);
        render_keyboard_capture(&mut canvas, 364, 184, &snapshot);
    }
}

fn draw_panel(canvas: &mut Canvas<'_, Surface>, x: i32, y: i32, w: u32, h: u32, title: &[u8]) {
    canvas.fill_rect(x, y, w, h, PANEL_BG);
    canvas.stroke_rect(x, y, w, h, PANEL_EDGE);
    canvas.fill_rect(x, y, w, 1, 0x00496e8a);
    canvas.draw_text(x + 10, y + 10, title, TEXT, PANEL_BG);
}

fn draw_button(canvas: &mut Canvas<'_, Surface>, rect: (i32, i32, u32, u32), label: &[u8]) {
    let (x, y, w, h) = rect;
    canvas.fill_rect(x, y, w, h, 0x0018222e);
    canvas.stroke_rect(x, y, w, h, 0x006389a8);
    canvas.draw_text(x + 14, y + 7, label, TEXT, 0x0018222e);
}

fn draw_line(canvas: &mut Canvas<'_, Surface>, x: i32, y: i32, line: &LineBuf, color: u32) {
    canvas.draw_text(x, y, &line.buf[..line.len], color, PANEL_BG);
}

fn render_motion_surface(
    canvas: &mut Canvas<'_, Surface>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    snapshot: &Snapshot,
) {
    canvas.fill_rect(x, y, w, h, 0x000d141b);
    canvas.stroke_rect(x, y, w, h, 0x00324c62);
    canvas.draw_text(
        x + 10,
        y + 8,
        b"raw trail cyan  rendered amber",
        MUTED,
        0x000d141b,
    );

    let content_x = x + 10;
    let content_y = y + 24;
    let content_w = w.saturating_sub(20);
    let content_h = h.saturating_sub(34);
    canvas.fill_rect(content_x, content_y, content_w, content_h, 0x00081115);
    canvas.stroke_rect(content_x, content_y, content_w, content_h, 0x00213649);

    if !snapshot.online || snapshot.display_width == 0 || snapshot.display_height == 0 {
        canvas.draw_text(
            content_x + 12,
            content_y + 12,
            b"pointer offline",
            BAD,
            0x00081115,
        );
        return;
    }

    let visible = snapshot.trail_count.min(POINTER_TRAIL_POINTS);
    let first = snapshot.trail_count.saturating_sub(visible);
    for row in 0..visible {
        let logical = first + row;
        let slot = if snapshot.trail_count < POINTER_TRAIL_POINTS {
            logical
        } else {
            (snapshot.trail_head + logical) % POINTER_TRAIL_POINTS
        };
        let point = snapshot.trail[slot];
        let px = scaled_axis(point.x, snapshot.display_width, content_w).saturating_add(content_x);
        let py = scaled_axis(point.y, snapshot.display_height, content_h).saturating_add(content_y);
        let color = if point.jump >= 64 { BAD } else { 0x0067d4e4 };
        canvas.fill_rect(px - 1, py - 1, 3, 3, color);
    }

    if snapshot.have_abs_sample {
        let raw_x = scaled_axis(snapshot.last_abs_x, snapshot.display_width, content_w)
            .saturating_add(content_x);
        let raw_y = scaled_axis(snapshot.last_abs_y, snapshot.display_height, content_h)
            .saturating_add(content_y);
        draw_crosshair(canvas, raw_x, raw_y, 0x0067d4e4, 0x00081115);
    }

    if snapshot.have_rendered_cursor {
        let rendered_x = scaled_axis(snapshot.rendered_x, snapshot.display_width, content_w)
            .saturating_add(content_x);
        let rendered_y = scaled_axis(snapshot.rendered_y, snapshot.display_height, content_h)
            .saturating_add(content_y);
        draw_crosshair(canvas, rendered_x, rendered_y, 0x00f6b26b, 0x00081115);
    }

    let mut line = LineBuf::new();
    line.push_bytes(b"raw ");
    line.push_i32(snapshot.last_abs_x);
    line.push_bytes(b",");
    line.push_i32(snapshot.last_abs_y);
    line.push_bytes(b" render ");
    line.push_i32(snapshot.rendered_x);
    line.push_bytes(b",");
    line.push_i32(snapshot.rendered_y);
    canvas.draw_text(
        x + 12,
        y + h as i32 - 24,
        &line.buf[..line.len],
        TEXT,
        PANEL_BG,
    );

    let mut line = LineBuf::new();
    line.push_bytes(b"lag ");
    line.push_i32(snapshot.render_lag_x);
    line.push_bytes(b",");
    line.push_i32(snapshot.render_lag_y);
    line.push_bytes(b" ticks ");
    line.push_u64(snapshot.last_input_tick);
    canvas.draw_text(
        x + 12,
        y + h as i32 - 12,
        &line.buf[..line.len],
        MUTED,
        PANEL_BG,
    );
}

fn render_keyboard_capture(canvas: &mut Canvas<'_, Surface>, x: i32, y: i32, snapshot: &Snapshot) {
    let mut header = LineBuf::new();
    header.push_bytes(b"events ");
    header.push_u64(snapshot.keyboard_events);
    canvas.draw_text(x, y, &header.buf[..header.len], TEXT, PANEL_BG);

    let visible = snapshot.key_count.min(KEY_HISTORY_LINES);
    let first = snapshot.key_count.saturating_sub(visible);
    let mut row_y = y + 18;
    for row in 0..visible {
        let logical = first + row;
        let slot = if snapshot.key_count < KEY_HISTORY_LINES {
            logical
        } else {
            (snapshot.key_head + logical) % KEY_HISTORY_LINES
        };
        canvas.draw_text(
            x,
            row_y,
            &snapshot.key_lines[slot][..snapshot.key_lens[slot] as usize],
            0x00d7e5f2,
            PANEL_BG,
        );
        row_y += 10;
    }
}

fn draw_crosshair(canvas: &mut Canvas<'_, Surface>, x: i32, y: i32, color: u32, bg: u32) {
    canvas.fill_rect(x - 5, y, 11, 1, color);
    canvas.fill_rect(x, y - 5, 1, 11, color);
    canvas.fill_rect(x - 1, y - 1, 3, 3, bg);
    canvas.fill_rect(x, y, 1, 1, color);
}

fn scaled_axis(value: i32, max_value: u32, size: u32) -> i32 {
    if max_value <= 1 || size <= 1 {
        return 0;
    }
    let clamped = value.clamp(0, max_value.saturating_sub(1) as i32) as u64;
    ((clamped * size.saturating_sub(1) as u64) / max_value.saturating_sub(1) as u64) as i32
}

fn status_color(ok: bool) -> u32 {
    if ok { GOOD } else { WARN }
}

fn health_bytes(health: InputHealth) -> &'static [u8] {
    match health {
        InputHealth::Stable => b"stable",
        InputHealth::Watching => b"watching",
        InputHealth::Unstable => b"unstable",
    }
}

struct LineBuf {
    buf: [u8; 96],
    len: usize,
}

impl LineBuf {
    const fn new() -> Self {
        Self {
            buf: [0; 96],
            len: 0,
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if self.len >= self.buf.len() {
            return;
        }
        let count = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
    }

    fn push_u64(&mut self, mut value: u64) {
        if value == 0 {
            self.push_bytes(b"0");
            return;
        }
        let mut digits = [0u8; 20];
        let mut len = 0usize;
        while value > 0 {
            digits[len] = b'0' + (value % 10) as u8;
            value /= 10;
            len += 1;
        }
        while len > 0 {
            len -= 1;
            self.push_bytes(&digits[len..len + 1]);
        }
    }

    fn push_i32(&mut self, value: i32) {
        if value < 0 {
            self.push_bytes(b"-");
            self.push_u64(value.unsigned_abs() as u64);
        } else {
            self.push_u64(value as u64);
        }
    }
}
