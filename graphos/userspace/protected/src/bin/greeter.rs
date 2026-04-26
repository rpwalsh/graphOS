// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! greeter — GraphOS login screen.
//!
//! Shows a glassmorphic login dialog. On successful admin/admin auth,
//! spawns the full desktop shell (launcher) and exits cleanly.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;
use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;

const WIN_W: u32 = 1280;
const WIN_H: u32 = 800;

// Login panel geometry
const PANEL_W: u32 = 420;
const PANEL_H: u32 = 340;
const PANEL_X: i32 = (WIN_W as i32 - PANEL_W as i32) / 2;
const PANEL_Y: i32 = (WIN_H as i32 - PANEL_H as i32) / 2 - 20;

const PASS_MAX: usize = 64;

// Color palette — high-contrast for maximum visibility
const BG_DARK:      u32 = 0xFF0D1117; // near-black
const PANEL_FILL:   u32 = 0xFF1C2A4A; // deep blue panel
const PANEL_BDR:    u32 = 0xFF4A7FD4; // bright blue border
const ACCENT:       u32 = 0xFF3B82F6; // vivid blue accent bar
const TEXT:         u32 = 0xFFFFFFFF; // pure white text
const TEXT_MUTED:   u32 = 0xFFAEC6EF; // soft blue-white
const FIELD_FILL:   u32 = 0xFF0F1D35; // dark field bg
const FIELD_BDR:    u32 = 0xFF3B82F6; // bright blue field border
const FIELD_ACTIVE: u32 = 0xFF60A5FA; // lighter blue when active
const BTN_FILL:     u32 = 0xFF1D4ED8; // strong blue button
const BTN_BDR:      u32 = 0xFF93C5FD; // light blue button border
const BTN_HOV_FILL: u32 = 0xFF2563EB; // slightly lighter on hover
const ERROR_COL:    u32 = 0xFFFF5252; // red error
const SUCCESS_COL:  u32 = 0xFF4ADE80; // green success

// Derived field positions (mirrors render())
const USER_LABEL_Y: i32 = PANEL_Y + 96;
const USER_FIELD_Y: i32 = USER_LABEL_Y + 20;
const PASS_LABEL_Y: i32 = USER_FIELD_Y + 52;
const PASS_FIELD_Y: i32 = PASS_LABEL_Y + 20;
const BTN_Y:        i32 = PASS_FIELD_Y + 54;
const FIELD_X:      i32 = PANEL_X + 40;
const FIELD_W:      u32 = PANEL_W - 80;

#[derive(Clone, Copy, PartialEq)]
enum Status { None, Error, Ok }

struct State {
    pass:      [u8; PASS_MAX],
    pass_len:  usize,
    status:    Status,
    btn_hover: bool,
}

impl State {
    fn new() -> Self {
        Self { pass: [0u8; PASS_MAX], pass_len: 0, status: Status::None, btn_hover: false }
    }

    fn push(&mut self, c: u8) {
        if self.pass_len < PASS_MAX {
            self.pass[self.pass_len] = c;
            self.pass_len += 1;
            self.status = Status::None;
        }
    }

    fn pop(&mut self) {
        if self.pass_len > 0 {
            self.pass_len -= 1;
            self.pass[self.pass_len] = 0;
            self.status = Status::None;
        }
    }

    fn clear(&mut self) {
        self.pass = [0u8; PASS_MAX];
        self.pass_len = 0;
    }
}

fn render(canvas: &mut Canvas<'_>, state: &State) {
    // ── Background ──────────────────────────────────────────────────────────
    canvas.fill_rect(0, 0, WIN_W, WIN_H, BG_DARK);

    // ── Top bar ─────────────────────────────────────────────────────────────
    canvas.fill_rect(0, 0, WIN_W, 32, 0xFF111827);
    canvas.draw_hline(0, 31, WIN_W, PANEL_BDR);
    canvas.draw_text(16, 12, b"GraphOS", TEXT_MUTED, 80);
    canvas.draw_text((WIN_W - 150) as i32, 12, b"SECURE BOOT: OK", SUCCESS_COL, 150);

    // ── Login panel ─────────────────────────────────────────────────────────
    canvas.fill_rect(PANEL_X, PANEL_Y, PANEL_W, PANEL_H, PANEL_FILL);
    canvas.draw_rect(PANEL_X, PANEL_Y, PANEL_W, PANEL_H, PANEL_BDR);
    // Top accent bar — bright and wide so it's unmissable
    canvas.fill_rect(PANEL_X, PANEL_Y, PANEL_W, 4, ACCENT);

    // Title
    canvas.draw_text(PANEL_X + 40, PANEL_Y + 26, b"GraphOS", TEXT, 110);
    canvas.draw_text(PANEL_X + 40, PANEL_Y + 52, b"Authenticated Access", TEXT_MUTED, 220);
    canvas.draw_hline(PANEL_X + 40, PANEL_Y + 76, PANEL_W - 80, PANEL_BDR);

    // Username field
    canvas.draw_text(FIELD_X, USER_LABEL_Y, b"Username", TEXT_MUTED, 90);
    canvas.fill_rect(FIELD_X, USER_FIELD_Y, FIELD_W, 38, FIELD_FILL);
    canvas.draw_rect(FIELD_X, USER_FIELD_Y, FIELD_W, 38, FIELD_BDR);
    canvas.draw_text(FIELD_X + 14, USER_FIELD_Y + 12, b"admin", TEXT_MUTED, FIELD_W - 20);

    // Password field
    canvas.draw_text(FIELD_X, PASS_LABEL_Y, b"Password", TEXT_MUTED, 90);
    canvas.fill_rect(FIELD_X, PASS_FIELD_Y, FIELD_W, 38, FIELD_FILL);
    canvas.draw_rect(FIELD_X, PASS_FIELD_Y, FIELD_W, 38, FIELD_ACTIVE);
    let dot_buf = [b'*'; PASS_MAX];
    canvas.draw_text(FIELD_X + 14, PASS_FIELD_Y + 12,
                     &dot_buf[..state.pass_len], TEXT, FIELD_W - 20);
    // Cursor blink
    let caret_x = (FIELD_X + 14 + state.pass_len as i32 * 9)
        .min(FIELD_X + FIELD_W as i32 - 10);
    canvas.fill_rect(caret_x, PASS_FIELD_Y + 8, 2, 20, ACCENT);

    // Sign In button
    let btn_fill = if state.btn_hover { BTN_HOV_FILL } else { BTN_FILL };
    canvas.fill_rect(FIELD_X, BTN_Y, FIELD_W, 40, btn_fill);
    canvas.draw_rect(FIELD_X, BTN_Y, FIELD_W, 40, BTN_BDR);
    let label_x = FIELD_X + (FIELD_W as i32 / 2) - 26;
    canvas.draw_text(label_x, BTN_Y + 16, b"Sign In", TEXT, 60);

    // Status message
    let msg_y = BTN_Y + 50;
    match state.status {
        Status::Error => {
            canvas.draw_text(FIELD_X, msg_y, b"Invalid credentials. Try again.",
                             ERROR_COL, FIELD_W);
        }
        Status::Ok => {
            canvas.draw_text(FIELD_X, msg_y, b"Authenticated. Loading desktop...",
                             SUCCESS_COL, FIELD_W);
        }
        Status::None => {}
    }

    // Bottom hint
    canvas.draw_text(PANEL_X + 40, PANEL_Y + PANEL_H as i32 - 16,
                     b"Enter key to sign in", TEXT_MUTED, 200);
}

/// Returns true on successful authentication.
fn try_login(state: &mut State) -> bool {
    if runtime::login(b"admin", &state.pass[..state.pass_len]) {
        state.status = Status::Ok;
        true
    } else {
        state.status = Status::Error;
        state.clear();
        false
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[greeter] starting\n");

    let input_ch = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => {
            runtime::write_line(b"[greeter] channel_create failed\n");
            runtime::exit(1)
        }
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_ch) {
        Some(w) => w,
        None => {
            runtime::write_line(b"[greeter] window open failed\n");
            runtime::exit(2)
        }
    };

    win.request_focus();

    // Fast-path: try admin/admin immediately. On success, skip the login UI.
    {
        let mut auto = State::new();
        for &b in b"admin" { auto.push(b); }
        if try_login(&mut auto) {
            { let mut c = win.canvas(); render(&mut c, &auto); }
            win.present();
            runtime::yield_now();
            let _ = runtime::spawn_named(b"cube");
            runtime::exit(0);
        }
    }

    let mut state = State::new();
    let mut prev_btn_down = false;

    // Button hit rect
    let btn_x1 = FIELD_X + FIELD_W as i32;
    let btn_y1 = BTN_Y + 40;

    // Initial frame
    { let mut c = win.canvas(); render(&mut c, &state); }
    win.present();
    runtime::yield_now();

    loop {
        let mut dirty = false;

        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,

                Event::Key { pressed: true, ascii, .. } => {
                    match ascii {
                        // Enter — attempt login
                        0x0D | 0x0A => {
                            if try_login(&mut state) {
                                { let mut c = win.canvas(); render(&mut c, &state); }
                                win.present();
                                runtime::yield_now();
                                let _ = runtime::spawn_named(b"cube");
                                runtime::exit(0);
                            }
                            dirty = true;
                        }
                        // Backspace / Delete
                        0x08 | 0x7F => { state.pop(); dirty = true; }
                        // Printable ASCII
                        0x20..=0x7E => { state.push(ascii); dirty = true; }
                        _ => {}
                    }
                }

                Event::PointerMove { x, y, buttons } => {
                    let xi = x as i32;
                    let yi = y as i32;
                    let over = xi >= FIELD_X && xi < btn_x1
                             && yi >= BTN_Y && yi < btn_y1;

                    if state.btn_hover != over {
                        state.btn_hover = over;
                        dirty = true;
                    }

                    let now_down = buttons & 1 != 0;
                    if !prev_btn_down && now_down && over {
                        // Fresh left-click on button
                        if try_login(&mut state) {
                            { let mut c = win.canvas(); render(&mut c, &state); }
                            win.present();
                            runtime::yield_now();
                            let _ = runtime::spawn_named(b"cube");
                            runtime::exit(0);
                        }
                        dirty = true;
                    }
                    prev_btn_down = now_down;
                }

                _ => {}
            }
        }

        if dirty {
            { let mut c = win.canvas(); render(&mut c, &state); }
            win.present();
        }
        runtime::yield_now();
    }
}

#[panic_handler]
fn panic(_: &PanicInfo<'_>) -> ! {
    runtime::write_line(b"[greeter] panic\n");
    runtime::exit(255)
}
