// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ssh — GraphOS SSH client stub.
//!
//! Provides a minimal SSH client UI.  The networking stack (TCP/IP) is not
//! yet implemented; this app displays connection state and a status message
//! until the stack becomes available.  The socket layer (SYS_SOCKET through
//! SYS_CONNECT) is already wired, so this app probes readiness and updates
//! its display accordingly.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

const WIN_W: u32 = 480;
const WIN_H: u32 = 300;

const TITLE_H: u32 = 20;
const MARGIN: u32 = 12;
const FIELD_H: u32 = 18;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

const BG: u32 = 0xFF_0A_12_1A;
const TITLE_BG: u32 = 0xFF_06_0C_12;
const FIELD_BG: u32 = 0xFF_10_1C_28;
const BORDER: u32 = 0xFF_20_40_60;
const TITLE_TEXT: u32 = 0xFF_50_B0_E0;
const LABEL_TEXT: u32 = 0xFF_60_A0_C0;
const VALUE_TEXT: u32 = 0xFF_C0_D8_F0;
const STATUS_WARN: u32 = 0xFF_F0_C0_40;
const BTN_BG: u32 = 0xFF_18_34_50;
const BTN_HOVER: u32 = 0xFF_28_50_78;
const BTN_TEXT: u32 = 0xFF_90_D0_FF;

// ---------------------------------------------------------------------------
// Input field
// ---------------------------------------------------------------------------

struct Field {
    buf: [u8; 64],
    len: usize,
}

impl Field {
    fn new() -> Self { Field { buf: [0u8; 64], len: 0 } }
    fn text(&self) -> &[u8] { &self.buf[..self.len] }
    fn push(&mut self, ch: u8) { if self.len < 63 { self.buf[self.len] = ch; self.len += 1; } }
    fn pop(&mut self) { if self.len > 0 { self.len -= 1; } }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

enum ConnState { Idle, Probing, NoStack }

struct AppState {
    host: Field,
    port: Field,
    focus_host: bool,
    btn_hover: bool,
    conn: ConnState,
    msg: [u8; 64],
    msg_len: usize,
}

impl AppState {
    fn new() -> Self {
        let mut s = AppState {
            host: Field::new(),
            port: Field::new(),
            focus_host: true,
            btn_hover: false,
            conn: ConnState::Idle,
            msg: [0u8; 64],
            msg_len: 0,
        };
        // Defaults
        b"22".iter().for_each(|&b| s.port.push(b));
        s.set_msg(b"Enter host and press Connect");
        s
    }

    fn set_msg(&mut self, m: &[u8]) {
        let n = m.len().min(self.msg.len());
        self.msg[..n].copy_from_slice(&m[..n]);
        self.msg_len = n;
    }

    fn try_connect(&mut self) {
        self.conn = ConnState::Probing;
        self.set_msg(b"Probing socket layer...");

        // Try to open a socket — if the kernel returns error, the stack is absent.
        match runtime::socket_open() {
            Some(sock) => {
                runtime::socket_close(&sock);
                // Stack present — would attempt TCP connect here.
                // For now report that full TCP/IP is not yet wired.
                self.conn = ConnState::NoStack;
                self.set_msg(b"Socket layer present; TCP/IP not yet implemented");
            }
            None => {
                self.conn = ConnState::NoStack;
                self.set_msg(b"Network stack not available");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Button geometry
// ---------------------------------------------------------------------------

const BTN_X: u32 = MARGIN;
const BTN_Y: u32 = TITLE_H + MARGIN + (FIELD_H + 8) * 2 + 16;
const BTN_W: u32 = 100;
const BTN_H: u32 = 20;

fn hit_btn(x: i32, y: i32) -> bool {
    x >= BTN_X as i32 && x < (BTN_X + BTN_W) as i32
        && y >= BTN_Y as i32 && y < (BTN_Y + BTN_H) as i32
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn draw(win: &mut Window, s: &AppState) {
    let mut c = win.canvas();
    c.clear(BG);

    c.fill_rect(0, 0, WIN_W, TITLE_H, TITLE_BG);
    c.draw_text(8, 5, b"GraphOS SSH Client", TITLE_TEXT, WIN_W - 16);

    let fy = TITLE_H + MARGIN;

    // Host field
    c.draw_text(MARGIN as i32, fy as i32, b"Host:", LABEL_TEXT, 40);
    let hx = MARGIN + 44;
    c.fill_rect(hx as i32, fy as i32, WIN_W - hx - MARGIN, FIELD_H, FIELD_BG);
    c.draw_rect(hx as i32, fy as i32, WIN_W - hx - MARGIN, FIELD_H,
        if s.focus_host { BTN_TEXT } else { BORDER });
    c.draw_text((hx + 4) as i32, (fy + 4) as i32, s.host.text(), VALUE_TEXT, WIN_W - hx - MARGIN - 8);

    // Port field
    let py2 = fy + FIELD_H + 8;
    c.draw_text(MARGIN as i32, py2 as i32, b"Port:", LABEL_TEXT, 40);
    let px2 = MARGIN + 44;
    let pw = 60u32;
    c.fill_rect(px2 as i32, py2 as i32, pw, FIELD_H, FIELD_BG);
    c.draw_rect(px2 as i32, py2 as i32, pw, FIELD_H,
        if !s.focus_host { BTN_TEXT } else { BORDER });
    c.draw_text((px2 + 4) as i32, (py2 + 4) as i32, s.port.text(), VALUE_TEXT, pw - 8);

    // Connect button
    let btn_col = if s.btn_hover { BTN_HOVER } else { BTN_BG };
    c.fill_rect(BTN_X as i32, BTN_Y as i32, BTN_W, BTN_H, btn_col);
    c.draw_rect(BTN_X as i32, BTN_Y as i32, BTN_W, BTN_H, BORDER);
    let tw = graphos_app_sdk::canvas::Canvas::text_width(b"Connect");
    c.draw_text((BTN_X + (BTN_W - tw) / 2) as i32, (BTN_Y + 5) as i32, b"Connect", BTN_TEXT, BTN_W);

    // Status message
    let status_y = BTN_Y + BTN_H + 12;
    let msg_col = match s.conn {
        ConnState::Idle    => LABEL_TEXT,
        ConnState::Probing => STATUS_WARN,
        ConnState::NoStack => STATUS_WARN,
    };
    c.draw_text(MARGIN as i32, status_y as i32, &s.msg[..s.msg_len], msg_col, WIN_W - MARGIN * 2);

    win.present();
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[ssh] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut app = AppState::new();
    draw(&mut win, &app);
    win.request_focus();

    let mut prev_buttons: u8 = 0;

    loop {
        match win.poll_event() {
            Event::PointerMove { x, y, buttons } => {
                let new_hover = hit_btn(x as i32, y as i32);
                let mut dirty = new_hover != app.btn_hover;
                app.btn_hover = new_hover;
                let btn1 = buttons & 1 != 0;
                let btn1_prev = prev_buttons & 1 != 0;
                if btn1 && !btn1_prev && new_hover {
                    app.try_connect();
                    dirty = true;
                }
                prev_buttons = buttons;
                if dirty { draw(&mut win, &app); }
            }
            Event::Key { pressed: true, ascii, .. } => {
                match ascii {
                    0x09 => { // Tab — switch focus
                        app.focus_host = !app.focus_host;
                    }
                    0x08 => { // Backspace
                        if app.focus_host { app.host.pop(); } else { app.port.pop(); }
                    }
                    0x0D => { // Enter — connect
                        app.try_connect();
                    }
                    0x20..=0x7E => {
                        if app.focus_host { app.host.push(ascii); } else { app.port.push(ascii); }
                    }
                    _ => {}
                }
                draw(&mut win, &app);
            }
            Event::None => runtime::yield_now(),
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    runtime::write_line(b"[ssh] panic\n");
    runtime::exit(255)
}
