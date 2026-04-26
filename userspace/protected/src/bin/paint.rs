// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! paint - GraphOS sketch and markup tool.
//!
//! Modern utility surface for quick concept sketches:
//! - persistent color/brush controls
//! - keyboard and pointer shortcuts
//! - save-to-PPM export for demo assets

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::{
    geom::Rect,
    tokens::{tokens, Theme},
    widgets::{draw_button, draw_panel, draw_stat_card, draw_window_frame, ButtonKind},
};

const WIN_W: u32 = 960;
const WIN_H: u32 = 620;
const THEME: Theme = Theme::DarkGlass;
const CANVAS_W: u32 = 512;
const CANVAS_H: u32 = 320;
const SAVE_PATH: &[u8] = b"/tmp/paint.ppm";
const STATUS_CAP: usize = 96;

struct Swatch {
    label: &'static [u8],
    color: u32,
}

const SWATCHES: [Swatch; 8] = [
    Swatch { label: b"Cloud", color: 0xFFFFFFFF },
    Swatch { label: b"Ink", color: 0xFF0D1117 },
    Swatch { label: b"Signal", color: 0xFF58A6FF },
    Swatch { label: b"Mint", color: 0xFF39C5BB },
    Swatch { label: b"Gold", color: 0xFFD29922 },
    Swatch { label: b"Coral", color: 0xFFF78166 },
    Swatch { label: b"Violet", color: 0xFFA371F7 },
    Swatch { label: b"Neon", color: 0xFF3FB950 },
];

const BRUSHES: [u8; 3] = [1, 3, 5];

struct PaintApp {
    sel_colour: usize,
    brush_size: u8,
    canvas_pixels: [u32; (CANVAS_W * CANVAS_H) as usize],
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
    status: [u8; STATUS_CAP],
    status_len: usize,
}

impl PaintApp {
    fn new() -> Self {
        let mut app = Self {
            sel_colour: 2,
            brush_size: 3,
            canvas_pixels: [0xFFFFFFFF; (CANVAS_W * CANVAS_H) as usize],
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
            status: [0u8; STATUS_CAP],
            status_len: 0,
        };
        app.set_status(b"Sketch canvas ready.");
        app
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn clear(&mut self) {
        let mut i = 0usize;
        while i < self.canvas_pixels.len() {
            self.canvas_pixels[i] = 0xFFFFFFFF;
            i += 1;
        }
        self.set_status(b"Canvas cleared.");
    }

    fn fill(&mut self) {
        let fill = SWATCHES[self.sel_colour].color;
        let mut i = 0usize;
        while i < self.canvas_pixels.len() {
            self.canvas_pixels[i] = fill;
            i += 1;
        }
        self.set_status(b"Canvas filled.");
    }

    fn paint(&mut self, px: i32, py: i32) {
        let radius = (self.brush_size as i32) / 2;
        let color = SWATCHES[self.sel_colour].color;
        let mut dy = -radius;
        while dy <= radius {
            let mut dx = -radius;
            while dx <= radius {
                let x = px + dx;
                let y = py + dy;
                if x >= 0 && y >= 0 && x < CANVAS_W as i32 && y < CANVAS_H as i32 {
                    self.canvas_pixels[(y as u32 * CANVAS_W + x as u32) as usize] = color;
                }
                dx += 1;
            }
            dy += 1;
        }
    }

    fn save_ppm(&mut self) {
        let fd = runtime::vfs_create(SAVE_PATH);
        if fd == u64::MAX {
            self.set_status(b"Save failed.");
            return;
        }
        let mut header = [0u8; 32];
        let mut header_len = 0usize;
        header[header_len..header_len + 3].copy_from_slice(b"P6\n");
        header_len += 3;
        header_len += write_u32(CANVAS_W, &mut header[header_len..]);
        header[header_len] = b' ';
        header_len += 1;
        header_len += write_u32(CANVAS_H, &mut header[header_len..]);
        header[header_len..header_len + 5].copy_from_slice(b"\n255\n");
        header_len += 5;
        let _ = runtime::vfs_write(fd, &header[..header_len]);

        let mut row = [0u8; (CANVAS_W * 3) as usize];
        let mut y = 0u32;
        while y < CANVAS_H {
            let mut x = 0u32;
            while x < CANVAS_W {
                let pixel = self.canvas_pixels[(y * CANVAS_W + x) as usize];
                let idx = (x * 3) as usize;
                row[idx] = ((pixel >> 16) & 0xFF) as u8;
                row[idx + 1] = ((pixel >> 8) & 0xFF) as u8;
                row[idx + 2] = (pixel & 0xFF) as u8;
                x += 1;
            }
            let _ = runtime::vfs_write(fd, &row);
            y += 1;
        }
        runtime::vfs_close(fd);
        self.set_status(b"Saved /tmp/paint.ppm.");
    }

    fn handle_pointer(&mut self, x: i16, y: i16, buttons: u8) -> bool {
        self.pointer_x = x;
        self.pointer_y = y;
        let left_down = buttons & 1 != 0;
        let left_prev = self.prev_buttons & 1 != 0;
        let dirty = true;
        if left_down && !left_prev {
            if contains(action_rect(0), x, y) {
                self.save_ppm();
            } else if contains(action_rect(1), x, y) {
                self.clear();
            } else if contains(action_rect(2), x, y) {
                self.fill();
            } else {
                let mut swatch = 0usize;
                while swatch < SWATCHES.len() {
                    if contains(swatch_rect(swatch), x, y) {
                        self.sel_colour = swatch;
                        self.set_status(b"Color selected.");
                        break;
                    }
                    swatch += 1;
                }
                let mut brush = 0usize;
                while brush < BRUSHES.len() {
                    if contains(brush_rect(brush), x, y) {
                        self.brush_size = BRUSHES[brush];
                        self.set_status(b"Brush updated.");
                        break;
                    }
                    brush += 1;
                }
            }
        }
        if left_down {
            let canvas = canvas_rect();
            if contains(canvas, x, y) {
                self.paint(x as i32 - canvas.x, y as i32 - canvas.y);
            }
        }
        self.prev_buttons = buttons;
        dirty
    }
}

fn contains(rect: Rect, x: i16, y: i16) -> bool {
    let x = x as i32;
    let y = y as i32;
    x >= rect.x
        && y >= rect.y
        && x < rect.x + rect.w as i32
        && y < rect.y + rect.h as i32
}

fn canvas_rect() -> Rect {
    Rect::new(26, 162, CANVAS_W, CANVAS_H)
}

fn action_rect(index: usize) -> Rect {
    Rect::new(714, 162 + index as i32 * 38, 214, 30)
}

fn swatch_rect(index: usize) -> Rect {
    let row = index as i32;
    Rect::new(714, 304 + row * 34, 214, 28)
}

fn brush_rect(index: usize) -> Rect {
    Rect::new(714 + index as i32 * 72, 236, 64, 28)
}

fn write_u32(mut value: u32, out: &mut [u8]) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut len = 0usize;
    while value > 0 {
        tmp[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    let mut i = 0usize;
    while i < len {
        out[i] = tmp[len - 1 - i];
        i += 1;
    }
    len
}

fn draw_canvas_pixels(canvas: &mut graphos_app_sdk::canvas::Canvas<'_>, app: &PaintApp) {
    let rect = canvas_rect();
    canvas.fill_rect(rect.x - 1, rect.y - 1, rect.w + 2, rect.h + 2, 0xFF000000);
    let mut y = 0u32;
    while y < CANVAS_H {
        let mut x = 0u32;
        while x < CANVAS_W {
            let pixel = app.canvas_pixels[(y * CANVAS_W + x) as usize];
            let mut run = 1u32;
            while x + run < CANVAS_W
                && app.canvas_pixels[(y * CANVAS_W + x + run) as usize] == pixel
            {
                run += 1;
            }
            canvas.fill_rect(rect.x + x as i32, rect.y + y as i32, run, 1, pixel);
            x += run;
        }
        y += 1;
    }
}

fn draw(win: &mut Window, app: &PaintApp) {
    let palette = tokens(THEME);
    let mut canvas = win.canvas();
    canvas.clear(palette.background);

    draw_window_frame(&mut canvas, Rect::new(0, 0, WIN_W, WIN_H), b"GraphOS Paint", THEME);

    draw_stat_card(
        &mut canvas,
        Rect::new(18, 40, 182, 38),
        b"Canvas",
        b"512 x 320",
        palette.primary,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(208, 40, 182, 38),
        b"Brush",
        brush_name(app.brush_size),
        palette.success,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(398, 40, 182, 38),
        b"Color",
        SWATCHES[app.sel_colour].label,
        palette.warning,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(588, 40, 342, 38),
        b"Status",
        &app.status[..app.status_len],
        palette.text_muted,
        THEME,
    );

    let canvas_panel = draw_panel(&mut canvas, Rect::new(18, 92, 666, 500), b"Canvas", THEME);
    let tools_panel = draw_panel(&mut canvas, Rect::new(700, 92, 242, 500), b"Tools", THEME);

    draw_canvas_pixels(&mut canvas, app);
    canvas.draw_text(canvas_panel.x, canvas_panel.y + canvas_panel.h as i32 - 12, b"Hold left mouse to draw.", palette.text_muted, canvas_panel.w);

    draw_button(
        &mut canvas,
        action_rect(0),
        b"Save PPM",
        ButtonKind::Primary,
        false,
        contains(action_rect(0), app.pointer_x, app.pointer_y),
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        action_rect(1),
        b"Clear",
        ButtonKind::Secondary,
        false,
        contains(action_rect(1), app.pointer_x, app.pointer_y),
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        action_rect(2),
        b"Fill",
        ButtonKind::Secondary,
        false,
        contains(action_rect(2), app.pointer_x, app.pointer_y),
        false,
        THEME,
    );

    canvas.draw_text(tools_panel.x, tools_panel.y + 72, b"Brush", palette.text_muted, tools_panel.w);
    let mut brush = 0usize;
    while brush < BRUSHES.len() {
        let selected = app.brush_size == BRUSHES[brush];
        draw_button(
            &mut canvas,
            brush_rect(brush),
            brush_name(BRUSHES[brush]),
            if selected { ButtonKind::Primary } else { ButtonKind::Secondary },
            false,
            contains(brush_rect(brush), app.pointer_x, app.pointer_y),
            false,
            THEME,
        );
        brush += 1;
    }

    canvas.draw_text(tools_panel.x, tools_panel.y + 126, b"Palette", palette.text_muted, tools_panel.w);
    let mut swatch = 0usize;
    while swatch < SWATCHES.len() {
        let rect = swatch_rect(swatch);
        let selected = swatch == app.sel_colour;
        canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, if selected { palette.surface } else { palette.surface_alt });
        canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, if selected { palette.primary } else { palette.border });
        canvas.fill_rect(rect.x + 8, rect.y + 8, 12, 12, SWATCHES[swatch].color);
        canvas.draw_text(rect.x + 28, rect.y + 7, SWATCHES[swatch].label, palette.text, rect.w - 36);
        swatch += 1;
    }

    canvas.draw_text(tools_panel.x, tools_panel.y + 418, b"Shortcuts", palette.text_muted, tools_panel.w);
    canvas.draw_text(tools_panel.x, tools_panel.y + 436, b"S save  C clear  F fill", palette.text, tools_panel.w);
    canvas.draw_text(tools_panel.x, tools_panel.y + 452, b"[ and ] adjust brush", palette.text, tools_panel.w);

    win.present();
}

fn brush_name(size: u8) -> &'static [u8] {
    match size {
        1 => b"1 px",
        3 => b"3 px",
        5 => b"5 px",
        _ => b"Brush",
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[paint] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut app = PaintApp::new();
    draw(&mut win, &app);
    win.request_focus();

    loop {
        match win.poll_event() {
            Event::PointerMove { x, y, buttons } => {
                if app.handle_pointer(x, y, buttons) {
                    draw(&mut win, &app);
                }
            }
            Event::Key { pressed: true, ascii, .. } => {
                let mut dirty = true;
                match ascii {
                    b's' | b'S' => app.save_ppm(),
                    b'c' | b'C' => app.clear(),
                    b'f' | b'F' => app.fill(),
                    b'[' => {
                        app.brush_size = if app.brush_size <= 1 { 1 } else { app.brush_size - 2 };
                        app.set_status(b"Brush reduced.");
                    }
                    b']' => {
                        app.brush_size = if app.brush_size >= 5 { 5 } else { app.brush_size + 2 };
                        app.set_status(b"Brush increased.");
                    }
                    0x1B => runtime::exit(0),
                    _ => dirty = false,
                }
                if dirty {
                    draw(&mut win, &app);
                }
            }
            Event::None => runtime::yield_now(),
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    runtime::write_line(b"[paint] panic\n");
    runtime::exit(255)
}
