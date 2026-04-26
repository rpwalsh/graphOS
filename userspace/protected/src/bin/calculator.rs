// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! calculator - GraphOS desktop calculator.
//!
//! Modern toolkit-based calculator with:
//! - 64-bit integer arithmetic
//! - pointer and keyboard input
//! - rolling expression history

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
    widgets::{
        draw_button, draw_list_row, draw_panel, draw_stat_card, draw_window_frame, ButtonKind,
    },
};

const WIN_W: u32 = 720;
const WIN_H: u32 = 560;
const THEME: Theme = Theme::DarkGlass;
const HISTORY_MAX: usize = 8;
const DISPLAY_CAP: usize = 32;
const STATUS_CAP: usize = 80;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy)]
enum KeyKind {
    Digit(u8),
    DoubleZero,
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Clear,
    Sign,
    Percent,
    Delete,
}

struct KeyDef {
    label: &'static [u8],
    kind: KeyKind,
}

const KEYS: [KeyDef; 20] = [
    KeyDef { label: b"C", kind: KeyKind::Clear },
    KeyDef { label: b"DEL", kind: KeyKind::Delete },
    KeyDef { label: b"+/-", kind: KeyKind::Sign },
    KeyDef { label: b"/", kind: KeyKind::Div },
    KeyDef { label: b"7", kind: KeyKind::Digit(7) },
    KeyDef { label: b"8", kind: KeyKind::Digit(8) },
    KeyDef { label: b"9", kind: KeyKind::Digit(9) },
    KeyDef { label: b"*", kind: KeyKind::Mul },
    KeyDef { label: b"4", kind: KeyKind::Digit(4) },
    KeyDef { label: b"5", kind: KeyKind::Digit(5) },
    KeyDef { label: b"6", kind: KeyKind::Digit(6) },
    KeyDef { label: b"-", kind: KeyKind::Sub },
    KeyDef { label: b"1", kind: KeyKind::Digit(1) },
    KeyDef { label: b"2", kind: KeyKind::Digit(2) },
    KeyDef { label: b"3", kind: KeyKind::Digit(3) },
    KeyDef { label: b"+", kind: KeyKind::Add },
    KeyDef { label: b"0", kind: KeyKind::Digit(0) },
    KeyDef { label: b"00", kind: KeyKind::DoubleZero },
    KeyDef { label: b"%", kind: KeyKind::Percent },
    KeyDef { label: b"=", kind: KeyKind::Eq },
];

struct Calculator {
    acc: i64,
    cur: i64,
    entering: bool,
    op: Option<Op>,
    display: [u8; DISPLAY_CAP],
    display_len: usize,
    history: [[u8; 48]; HISTORY_MAX],
    history_lens: [usize; HISTORY_MAX],
    history_count: usize,
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
    status: [u8; STATUS_CAP],
    status_len: usize,
}

impl Calculator {
    fn new() -> Self {
        let mut app = Self {
            acc: 0,
            cur: 0,
            entering: false,
            op: None,
            display: [0u8; DISPLAY_CAP],
            display_len: 0,
            history: [[0u8; 48]; HISTORY_MAX],
            history_lens: [0usize; HISTORY_MAX],
            history_count: 0,
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
            status: [0u8; STATUS_CAP],
            status_len: 0,
        };
        app.set_status(b"Integer calculator ready.");
        app.refresh_display(0);
        app
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn refresh_display(&mut self, value: i64) {
        self.display_len = format_i64(value, &mut self.display);
    }

    fn press(&mut self, key: KeyKind) {
        match key {
            KeyKind::Digit(d) => self.push_digit(d),
            KeyKind::DoubleZero => {
                self.push_digit(0);
                self.push_digit(0);
            }
            KeyKind::Clear => {
                self.acc = 0;
                self.cur = 0;
                self.entering = false;
                self.op = None;
                self.refresh_display(0);
                self.set_status(b"Calculator cleared.");
            }
            KeyKind::Delete => {
                if self.entering {
                    self.cur /= 10;
                    self.refresh_display(self.cur);
                    self.set_status(b"Digit removed.");
                }
            }
            KeyKind::Sign => {
                self.cur = self.cur.wrapping_neg();
                self.entering = true;
                self.refresh_display(self.cur);
                self.set_status(b"Sign toggled.");
            }
            KeyKind::Percent => {
                self.cur /= 100;
                self.entering = true;
                self.refresh_display(self.cur);
                self.set_status(b"Percent applied.");
            }
            KeyKind::Eq => self.equals(),
            KeyKind::Add => self.stage_op(Op::Add),
            KeyKind::Sub => self.stage_op(Op::Sub),
            KeyKind::Mul => self.stage_op(Op::Mul),
            KeyKind::Div => self.stage_op(Op::Div),
        }
    }

    fn push_digit(&mut self, digit: u8) {
        if !self.entering {
            self.cur = 0;
            self.entering = true;
        }
        self.cur = self.cur.saturating_mul(10).saturating_add(digit as i64);
        self.refresh_display(self.cur);
        self.set_status(b"Input updated.");
    }

    fn stage_op(&mut self, next: Op) {
        if self.entering || self.op.is_none() {
            if let Some(prev) = self.op {
                self.acc = apply_op(self.acc, self.cur, prev);
            } else {
                self.acc = self.cur;
            }
        }
        self.op = Some(next);
        self.entering = false;
        self.refresh_display(self.acc);
        self.set_status(op_status(next));
    }

    fn equals(&mut self) {
        let Some(op) = self.op.take() else {
            self.set_status(b"No pending operation.");
            return;
        };
        if op == Op::Div && self.cur == 0 {
            self.set_status(b"Divide by zero prevented.");
            return;
        }
        let lhs = self.acc;
        let rhs = self.cur;
        let result = apply_op(lhs, rhs, op);
        self.push_history(lhs, op, rhs, result);
        self.acc = result;
        self.cur = result;
        self.entering = false;
        self.refresh_display(result);
        self.set_status(b"Expression resolved.");
    }

    fn push_history(&mut self, lhs: i64, op: Op, rhs: i64, result: i64) {
        if self.history_count < HISTORY_MAX {
            let slot = self.history_count;
            self.history_lens[slot] = format_history(lhs, op, rhs, result, &mut self.history[slot]);
            self.history_count += 1;
        } else {
            let mut i = 1usize;
            while i < HISTORY_MAX {
                self.history[i - 1] = self.history[i];
                self.history_lens[i - 1] = self.history_lens[i];
                i += 1;
            }
            self.history_lens[HISTORY_MAX - 1] =
                format_history(lhs, op, rhs, result, &mut self.history[HISTORY_MAX - 1]);
        }
    }

    fn pending_label(&self) -> &'static [u8] {
        match self.op {
            Some(Op::Add) => b"Add",
            Some(Op::Sub) => b"Subtract",
            Some(Op::Mul) => b"Multiply",
            Some(Op::Div) => b"Divide",
            None => b"Ready",
        }
    }

    fn handle_pointer(&mut self, x: i16, y: i16, buttons: u8) -> bool {
        self.pointer_x = x;
        self.pointer_y = y;
        let left_down = buttons & 1 != 0;
        let left_prev = self.prev_buttons & 1 != 0;
        let mut dirty = true;
        if left_down && !left_prev {
            let mut idx = 0usize;
            while idx < KEYS.len() {
                if contains(key_rect(idx), x, y) {
                    self.press(KEYS[idx].kind);
                    break;
                }
                idx += 1;
            }
        } else if !left_down && !left_prev {
            dirty = true;
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

fn key_rect(index: usize) -> Rect {
    let origin_x = 18i32;
    let origin_y = 186i32;
    let cell_w = 96u32;
    let cell_h = 58u32;
    let col = (index % 4) as i32;
    let row = (index / 4) as i32;
    Rect::new(origin_x + col * 100, origin_y + row * 62, cell_w, cell_h)
}

fn apply_op(lhs: i64, rhs: i64, op: Op) -> i64 {
    match op {
        Op::Add => lhs.saturating_add(rhs),
        Op::Sub => lhs.saturating_sub(rhs),
        Op::Mul => lhs.saturating_mul(rhs),
        Op::Div => {
            if rhs == 0 {
                lhs
            } else {
                lhs / rhs
            }
        }
    }
}

fn op_status(op: Op) -> &'static [u8] {
    match op {
        Op::Add => b"Queued addition.",
        Op::Sub => b"Queued subtraction.",
        Op::Mul => b"Queued multiplication.",
        Op::Div => b"Queued division.",
    }
}

fn format_i64(mut value: i64, out: &mut [u8]) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let negative = value < 0;
    if negative {
        out[0] = b'-';
        value = value.wrapping_neg();
    }
    let mut tmp = [0u8; 24];
    let mut len = 0usize;
    let mut num = value as u64;
    while num > 0 {
        tmp[len] = b'0' + (num % 10) as u8;
        num /= 10;
        len += 1;
    }
    let mut cursor = if negative { 1 } else { 0 };
    let mut i = 0usize;
    while i < len {
        out[cursor + i] = tmp[len - 1 - i];
        i += 1;
    }
    cursor + len
}

fn format_history(lhs: i64, op: Op, rhs: i64, result: i64, out: &mut [u8; 48]) -> usize {
    let mut cursor = 0usize;
    cursor += format_i64(lhs, &mut out[cursor..]);
    out[cursor] = b' ';
    cursor += 1;
    out[cursor] = match op {
        Op::Add => b'+',
        Op::Sub => b'-',
        Op::Mul => b'*',
        Op::Div => b'/',
    };
    cursor += 1;
    out[cursor] = b' ';
    cursor += 1;
    cursor += format_i64(rhs, &mut out[cursor..]);
    out[cursor] = b' ';
    out[cursor + 1] = b'=';
    out[cursor + 2] = b' ';
    cursor += 3;
    cursor += format_i64(result, &mut out[cursor..]);
    cursor
}

fn draw(win: &mut Window, app: &Calculator) {
    let palette = tokens(THEME);
    let mut canvas = win.canvas();
    canvas.clear(palette.background);

    draw_window_frame(&mut canvas, Rect::new(0, 0, WIN_W, WIN_H), b"GraphOS Calculator", THEME);

    draw_stat_card(
        &mut canvas,
        Rect::new(14, 40, 168, 38),
        b"Mode",
        b"64-bit integer",
        palette.primary,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(190, 40, 168, 38),
        b"Pending",
        app.pending_label(),
        palette.success,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(366, 40, 168, 38),
        b"History",
        if app.history_count == 0 { b"Empty" } else { b"Rolling" },
        palette.warning,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(542, 40, 164, 38),
        b"Status",
        &app.status[..app.status_len],
        palette.text_muted,
        THEME,
    );

    let display_panel = draw_panel(&mut canvas, Rect::new(14, 92, 392, 78), b"Display", THEME);
    canvas.fill_rect(display_panel.x, display_panel.y + 6, display_panel.w, display_panel.h - 6, palette.chrome);
    canvas.draw_rect(display_panel.x, display_panel.y + 6, display_panel.w, display_panel.h - 6, palette.border);
    let tw = graphos_app_sdk::canvas::Canvas::text_width(&app.display[..app.display_len]);
    let tx = display_panel.x + display_panel.w as i32 - tw as i32 - 12;
    canvas.draw_text(tx, display_panel.y + 32, &app.display[..app.display_len], palette.text, display_panel.w - 24);

    let history_panel = draw_panel(&mut canvas, Rect::new(422, 92, 284, 438), b"History", THEME);
    let mut row = 0usize;
    let start = app.history_count.saturating_sub(10);
    let mut idx = start;
    while idx < app.history_count {
        let y = history_panel.y + row as i32 * 30;
        draw_list_row(
            &mut canvas,
            Rect::new(history_panel.x, y, history_panel.w, 28),
            &app.history[idx][..app.history_lens[idx]],
            if idx + 1 == app.history_count { b"latest" } else { b"" },
            palette.primary,
            idx + 1 == app.history_count,
            false,
            THEME,
        );
        row += 1;
        idx += 1;
    }
    canvas.draw_text(history_panel.x, history_panel.y + 332, b"Keyboard", palette.text_muted, history_panel.w);
    canvas.draw_text(history_panel.x, history_panel.y + 350, b"Digits, + - * /, Enter", palette.text, history_panel.w);
    canvas.draw_text(history_panel.x, history_panel.y + 366, b"Backspace deletes, C clears", palette.text, history_panel.w);

    let mut i = 0usize;
    while i < KEYS.len() {
        let hovered = contains(key_rect(i), app.pointer_x, app.pointer_y);
        let kind = match KEYS[i].kind {
            KeyKind::Eq => ButtonKind::Primary,
            KeyKind::Add | KeyKind::Sub | KeyKind::Mul | KeyKind::Div => ButtonKind::Secondary,
            KeyKind::Clear => ButtonKind::Danger,
            _ => ButtonKind::Ghost,
        };
        draw_button(
            &mut canvas,
            key_rect(i),
            KEYS[i].label,
            kind,
            false,
            hovered,
            false,
            THEME,
        );
        i += 1;
    }

    win.present();
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[calculator] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut app = Calculator::new();
    draw(&mut win, &app);
    win.request_focus();

    loop {
        match win.poll_event() {
            Event::Key { pressed: true, ascii, .. } => {
                let mut dirty = true;
                match ascii {
                    b'0'..=b'9' => app.press(KeyKind::Digit(ascii - b'0')),
                    b'+' => app.press(KeyKind::Add),
                    b'-' => app.press(KeyKind::Sub),
                    b'*' => app.press(KeyKind::Mul),
                    b'/' => app.press(KeyKind::Div),
                    b'%' => app.press(KeyKind::Percent),
                    b'=' | b'\r' | b'\n' => app.press(KeyKind::Eq),
                    0x08 | 0x7F => app.press(KeyKind::Delete),
                    b'c' | b'C' => app.press(KeyKind::Clear),
                    0x1B => runtime::exit(0),
                    _ => dirty = false,
                }
                if dirty {
                    draw(&mut win, &app);
                }
            }
            Event::PointerMove { x, y, buttons } => {
                if app.handle_pointer(x, y, buttons) {
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
    runtime::write_line(b"[calculator] panic\n");
    runtime::exit(255)
}
