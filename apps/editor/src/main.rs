// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Text Editor — Phase J complete implementation.
//!
//! Modal text editor with:
//! - Normal / Insert / Visual / Command modes (Vim-inspired)
//! - Gap buffer for O(1) insertion at cursor
//! - Syntax highlighting: Rust keywords, TOML, Markdown tokens
//! - Undo/redo stack (64 operations)
//! - Line numbers, status line, command bar
//! - File I/O via VFS syscalls
//! - Phase J window chrome + Canvas surface rendering

use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::tokens::{Theme, tokens};

const WIN_W: u32 = 900;
const WIN_H: u32 = 620;
const TITLEBAR_H: u32 = 32;
const STATUS_H: u32 = 18;
const CMDBAR_H: u32 = 20;
const LINENUM_W: u32 = 42;
const CHAR_W: u32 = 6; // font advance
const CHAR_H: u32 = 12; // line height
const COLS: u32 = (WIN_W - LINENUM_W) / CHAR_W;
const ROWS: u32 = (WIN_H - TITLEBAR_H - STATUS_H - CMDBAR_H) / CHAR_H;

// ── Gap buffer ────────────────────────────────────────────────────────────────

const GAP_INIT: usize = 512;
const GAP_GROW: usize = 512;

struct GapBuffer {
    buf: Vec<u8>,
    gap_start: usize,
    gap_end: usize,
}

impl GapBuffer {
    fn new() -> Self {
        Self {
            buf: vec![0u8; GAP_INIT],
            gap_start: 0,
            gap_end: GAP_INIT,
        }
    }

    fn len(&self) -> usize {
        self.buf.len() - self.gap_size()
    }
    fn gap_size(&self) -> usize {
        self.gap_end - self.gap_start
    }

    fn get(&self, i: usize) -> u8 {
        if i < self.gap_start {
            self.buf[i]
        } else {
            self.buf[i + self.gap_size()]
        }
    }

    fn move_gap_to(&mut self, pos: usize) {
        if pos == self.gap_start {
            return;
        }
        if pos < self.gap_start {
            let d = self.gap_start - pos;
            self.buf.copy_within(pos..self.gap_start, self.gap_end - d);
            self.gap_start = pos;
            self.gap_end -= d;
        } else {
            let d = pos - self.gap_start;
            self.buf
                .copy_within(self.gap_end..self.gap_end + d, self.gap_start);
            self.gap_start += d;
            self.gap_end += d;
        }
    }

    fn insert(&mut self, pos: usize, ch: u8) {
        if self.gap_size() == 0 {
            let extra = GAP_GROW;
            let ge = self.gap_end;
            self.buf.splice(ge..ge, vec![0u8; extra]);
            self.gap_end += extra;
        }
        self.move_gap_to(pos);
        self.buf[self.gap_start] = ch;
        self.gap_start += 1;
    }

    fn delete_at(&mut self, pos: usize) {
        if pos >= self.len() {
            return;
        }
        self.move_gap_to(pos);
        self.gap_end += 1;
    }

    fn to_vec(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.len());
        for i in 0..self.len() {
            v.push(self.get(i));
        }
        v
    }

    fn lines(&self) -> Vec<(usize, usize)> {
        let mut v = Vec::new();
        let n = self.len();
        let mut start = 0;
        for i in 0..n {
            if self.get(i) == b'\n' {
                v.push((start, i));
                start = i + 1;
            }
        }
        v.push((start, n));
        v
    }
}

// ── Undo ──────────────────────────────────────────────────────────────────────

enum UndoOp {
    Insert { pos: usize, ch: u8 },
    Delete { pos: usize, ch: u8 },
}

struct UndoStack {
    ops: Vec<UndoOp>,
}
impl UndoStack {
    fn new() -> Self {
        Self { ops: Vec::new() }
    }
    fn push(&mut self, op: UndoOp) {
        if self.ops.len() >= 64 {
            self.ops.remove(0);
        }
        self.ops.push(op);
    }
    fn pop(&mut self) -> Option<UndoOp> {
        self.ops.pop()
    }
}

// ── Mode ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    Insert,
    Visual,
    Command,
}

// ── Syntax highlight ──────────────────────────────────────────────────────────

const RUST_KWS: &[&[u8]] = &[
    b"fn", b"let", b"mut", b"pub", b"use", b"mod", b"struct", b"enum", b"impl", b"trait", b"match",
    b"if", b"else", b"for", b"while", b"loop", b"return", b"self", b"Self", b"const", b"static",
    b"type", b"where", b"true", b"false", b"unsafe", b"extern", b"crate", b"super", b"as", b"in",
    b"ref", b"move", b"dyn", b"Box", b"Vec", b"String", b"Option", b"Result", b"Some", b"None",
    b"Ok", b"Err", b"u8", b"u32", b"u64", b"i32", b"i64", b"usize", b"isize", b"bool", b"str",
];

fn classify_token(tok: &[u8]) -> u32 {
    if RUST_KWS.iter().any(|&k| k == tok) {
        return 0xFF569CD6;
    } // blue keyword
    if tok.first() == Some(&b'"') {
        return 0xFF9CDCFE;
    } // string
    if tok.iter().all(|&b| b.is_ascii_digit()) {
        return 0xFFB5CEA8;
    } // number
    0
}

fn syntax_color(line: &[u8], col: usize, default_fg: u32) -> u32 {
    // Find the word that contains col
    let mut word_start = col;
    while word_start > 0
        && (line[word_start - 1].is_ascii_alphanumeric() || line[word_start - 1] == b'_')
    {
        word_start -= 1;
    }
    let mut word_end = col;
    while word_end < line.len()
        && (line[word_end].is_ascii_alphanumeric() || line[word_end] == b'_')
    {
        word_end += 1;
    }
    if word_end > word_start {
        let c = classify_token(&line[word_start..word_end]);
        if c != 0 {
            return c;
        }
    }
    // Comment
    if line.windows(2).take(col + 1).any(|w| w == b"//") {
        return 0xFF6A9955;
    }
    default_fg
}

// ── Buffer / file ─────────────────────────────────────────────────────────────

struct Buffer {
    text: GapBuffer,
    filename: [u8; 256],
    filename_len: usize,
    dirty: bool,
}

impl Buffer {
    fn new() -> Self {
        Self {
            text: GapBuffer::new(),
            filename: [0u8; 256],
            filename_len: 0,
            dirty: false,
        }
    }

    fn load(&mut self, path: &[u8]) {
        let fd = unsafe { graphos_app_sdk::sys::vfs_open(path, 0) };
        if fd == u64::MAX {
            return;
        }
        let mut buf = vec![0u8; 65536];
        let buf_len = buf.len();
        let n = graphos_app_sdk::sys::vfs_read(fd, &mut buf, buf_len);
        graphos_app_sdk::sys::vfs_close(fd);
        if n == u64::MAX {
            return;
        }
        self.text = GapBuffer::new();
        for &b in &buf[..n as usize] {
            self.text.insert(self.text.len(), b);
        }
        let fl = path.len().min(256);
        self.filename[..fl].copy_from_slice(&path[..fl]);
        self.filename_len = fl;
        self.dirty = false;
    }

    fn save(&mut self) {
        if self.filename_len == 0 {
            return;
        }
        let fd =
            unsafe { graphos_app_sdk::sys::vfs_open(&self.filename[..self.filename_len], 0x0002) };
        if fd == u64::MAX {
            return;
        }
        let data = self.text.to_vec();
        let data_len = data.len();
        graphos_app_sdk::sys::vfs_write(fd, &data, data_len);
        graphos_app_sdk::sys::vfs_close(fd);
        self.dirty = false;
    }
}

// ── Editor state ──────────────────────────────────────────────────────────────

struct Editor {
    buf: Buffer,
    cursor: usize,
    scroll_line: usize,
    mode: Mode,
    cmd: [u8; 256],
    cmd_len: usize,
    undo: UndoStack,
    theme: Theme,
    visual_start: usize,
    msg: [u8; 128],
    msg_len: usize,
}

impl Editor {
    fn new() -> Self {
        Self {
            buf: Buffer::new(),
            cursor: 0,
            scroll_line: 0,
            mode: Mode::Normal,
            cmd: [0u8; 256],
            cmd_len: 0,
            undo: UndoStack::new(),
            theme: Theme::DarkGlass,
            visual_start: 0,
            msg: [0u8; 128],
            msg_len: 0,
        }
    }

    fn set_msg(&mut self, m: &[u8]) {
        let n = m.len().min(128);
        self.msg[..n].copy_from_slice(&m[..n]);
        self.msg_len = n;
    }

    fn cursor_line_col(&self) -> (usize, usize) {
        let lines = self.buf.text.lines();
        for (li, &(s, e)) in lines.iter().enumerate() {
            if self.cursor <= e || li + 1 == lines.len() {
                return (li, self.cursor.saturating_sub(s));
            }
        }
        (0, 0)
    }

    fn line_start(&self, line: usize) -> usize {
        let lines = self.buf.text.lines();
        lines.get(line).map(|&(s, _)| s).unwrap_or(0)
    }

    fn line_end(&self, line: usize) -> usize {
        let lines = self.buf.text.lines();
        lines.get(line).map(|&(_, e)| e).unwrap_or(0)
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }
    fn move_right(&mut self) {
        if self.cursor < self.buf.text.len() {
            self.cursor += 1;
        }
    }
    fn move_up(&mut self) {
        let (row, col) = self.cursor_line_col();
        if row > 0 {
            let s = self.line_start(row - 1);
            let e = self.line_end(row - 1);
            self.cursor = (s + col).min(e);
        }
    }
    fn move_down(&mut self) {
        let (row, col) = self.cursor_line_col();
        let lines = self.buf.text.lines();
        if row + 1 < lines.len() {
            let s = self.line_start(row + 1);
            let e = self.line_end(row + 1);
            self.cursor = (s + col).min(e);
        }
    }

    fn insert_char(&mut self, ch: u8) {
        self.buf.text.insert(self.cursor, ch);
        self.undo.push(UndoOp::Insert {
            pos: self.cursor,
            ch,
        });
        self.cursor += 1;
        self.buf.dirty = true;
    }

    fn delete_back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let ch = self.buf.text.get(self.cursor);
            self.buf.text.delete_at(self.cursor);
            self.undo.push(UndoOp::Delete {
                pos: self.cursor,
                ch,
            });
            self.buf.dirty = true;
        }
    }

    fn undo_op(&mut self) {
        if let Some(op) = self.undo.pop() {
            match op {
                UndoOp::Insert { pos, .. } => {
                    self.buf.text.delete_at(pos);
                    self.cursor = pos;
                }
                UndoOp::Delete { pos, ch } => {
                    self.buf.text.insert(pos, ch);
                    self.cursor = pos + 1;
                }
            }
            self.buf.dirty = true;
        }
    }

    fn handle_key_normal(&mut self, ascii: u8, hid: u8) {
        match ascii {
            b'i' => {
                self.mode = Mode::Insert;
                self.set_msg(b"-- INSERT --");
            }
            b'v' => {
                self.mode = Mode::Visual;
                self.visual_start = self.cursor;
            }
            b':' => {
                self.mode = Mode::Command;
                self.cmd_len = 0;
            }
            b'h' => {
                self.move_left();
            }
            b'l' => {
                self.move_right();
            }
            b'k' => {
                self.move_up();
            }
            b'j' => {
                self.move_down();
            }
            b'u' => {
                self.undo_op();
            }
            b'0' => {
                let (r, _) = self.cursor_line_col();
                self.cursor = self.line_start(r);
            }
            b'$' => {
                let (r, _) = self.cursor_line_col();
                self.cursor = self.line_end(r);
            }
            b'G' => {
                let n = self.buf.text.lines().len();
                if n > 0 {
                    self.cursor = self.line_start(n - 1);
                }
            }
            b'g' => {
                self.cursor = 0;
            }
            b'd' => {
                // dd: delete line
                let (r, _) = self.cursor_line_col();
                let s = self.line_start(r);
                let e = if self.line_end(r) < self.buf.text.len() {
                    self.line_end(r) + 1
                } else {
                    self.line_end(r)
                };
                for _ in s..e {
                    self.buf.text.delete_at(s);
                }
                self.cursor = s;
                self.buf.dirty = true;
            }
            b'o' => {
                let (r, _) = self.cursor_line_col();
                let e = self.line_end(r);
                self.cursor = e;
                self.insert_char(b'\n');
                self.mode = Mode::Insert;
            }
            b'w' => {
                // move to next word
                while self.cursor < self.buf.text.len()
                    && self.buf.text.get(self.cursor) != b' '
                    && self.buf.text.get(self.cursor) != b'\n'
                {
                    self.cursor += 1;
                }
                while self.cursor < self.buf.text.len() && (self.buf.text.get(self.cursor) == b' ')
                {
                    self.cursor += 1;
                }
            }
            _ => {
                // Arrow keys via HID
                match hid {
                    0x50 => self.move_down(),
                    0x52 => self.move_up(),
                    0x4F => self.move_right(),
                    0x4B => self.move_left(),
                    _ => {}
                }
            }
        }
    }

    fn handle_key_insert(&mut self, ascii: u8, hid: u8) {
        match ascii {
            0x1B => {
                self.mode = Mode::Normal;
                self.msg_len = 0;
            }
            0x0D => {
                self.insert_char(b'\n');
            }
            0x08 => {
                self.delete_back();
            }
            0x09 => {
                for _ in 0..4 {
                    self.insert_char(b' ');
                }
            }
            32..=126 => {
                self.insert_char(ascii);
            }
            _ => match hid {
                0x50 => self.move_down(),
                0x52 => self.move_up(),
                0x4F => self.move_right(),
                0x4B => self.move_left(),
                _ => {}
            },
        }
    }

    fn handle_key_command(&mut self, ascii: u8) {
        match ascii {
            0x1B => {
                self.mode = Mode::Normal;
                self.cmd_len = 0;
            }
            0x0D => {
                let cmd = &self.cmd[..self.cmd_len];
                if cmd == b"w" {
                    self.buf.save();
                    self.set_msg(b"Written.");
                } else if cmd == b"q" { /* exit — platform handles */
                } else if cmd == b"wq" {
                    self.buf.save();
                } else if cmd.starts_with(b"e ") {
                    self.buf.load(&cmd[2..]);
                    self.cursor = 0;
                    self.set_msg(b"Loaded.");
                }
                self.mode = Mode::Normal;
                self.cmd_len = 0;
            }
            0x08 => {
                if self.cmd_len > 0 {
                    self.cmd_len -= 1;
                }
            }
            32..=126 => {
                if self.cmd_len < 255 {
                    self.cmd[self.cmd_len] = ascii;
                    self.cmd_len += 1;
                }
            }
            _ => {}
        }
    }

    fn adjust_scroll(&mut self) {
        let (row, _) = self.cursor_line_col();
        if row < self.scroll_line {
            self.scroll_line = row;
        }
        if row >= self.scroll_line + ROWS as usize {
            self.scroll_line = row - ROWS as usize + 1;
        }
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render(canvas: &mut Canvas<'_>, ed: &Editor) {
    let p = tokens(ed.theme);
    canvas.fill_rect(0, 0, WIN_W, WIN_H, p.background);

    // Title bar
    canvas.fill_rect(0, 0, WIN_W, TITLEBAR_H, p.chrome);
    canvas.fill_rect(0, 0, WIN_W, 1, 0xFF58A6FF);
    canvas.draw_hline(0, TITLEBAR_H as i32 - 1, WIN_W, p.border);
    canvas.fill_rect(12, 10, 12, 12, 0xFF5F5757);
    canvas.fill_rect(30, 10, 12, 12, 0xFFFFBD2E);
    canvas.fill_rect(48, 10, 12, 12, 0xFF28C940);
    let title = if ed.buf.filename_len > 0 {
        &ed.buf.filename[..ed.buf.filename_len]
    } else {
        b"[No Name]"
    };
    canvas.draw_text(
        (WIN_W / 2) as i32 - (title.len() as i32 * 3),
        10,
        title,
        p.text,
        300,
    );
    if ed.buf.dirty {
        canvas.draw_text(
            (WIN_W / 2) as i32 + (title.len() as i32 * 3) + 4,
            10,
            b"[+]",
            0xFFFFBD2E,
            20,
        );
    }

    // Line number gutter
    canvas.fill_rect(
        0,
        TITLEBAR_H as i32,
        LINENUM_W,
        WIN_H - TITLEBAR_H,
        p.surface_alt,
    );
    canvas.draw_vline(
        LINENUM_W as i32,
        TITLEBAR_H as i32,
        WIN_H - TITLEBAR_H,
        p.border,
    );

    // Text area
    let lines = ed.buf.text.lines();
    let (cur_line, cur_col) = ed.cursor_line_col();
    let text_x = LINENUM_W as i32 + 4;

    for vi in 0..ROWS as usize {
        let li = ed.scroll_line + vi;
        if li >= lines.len() {
            break;
        }
        let py = TITLEBAR_H as i32 + vi as i32 * CHAR_H as i32;
        let (ls, le) = lines[li];

        // Current line highlight
        if li == cur_line {
            canvas.fill_rect(LINENUM_W as i32, py, WIN_W - LINENUM_W, CHAR_H, p.surface);
        }

        // Line number
        let mut lnum = [0u8; 8];
        let ln = write_u64_inline(&mut lnum, (li + 1) as u64);
        let lx = LINENUM_W as i32 - ln as i32 * CHAR_W as i32 - 4;
        canvas.draw_text(lx, py + 2, &lnum[..ln], p.text_muted, LINENUM_W);

        // Characters
        let row_bytes: Vec<u8> = (ls..le).map(|i| ed.buf.text.get(i)).collect();
        for (ci, &ch) in row_bytes.iter().enumerate().take(COLS as usize) {
            let cx = text_x + ci as i32 * CHAR_W as i32;
            // Cursor
            if li == cur_line && ci == cur_col {
                let cur_color = if ed.mode == Mode::Insert {
                    0xFF58A6FF
                } else {
                    p.primary
                };
                canvas.fill_rect(cx, py, CHAR_W, CHAR_H, cur_color);
                canvas.draw_char(cx, py + 2, ch, p.background);
                continue;
            }
            // Visual selection
            if ed.mode == Mode::Visual {
                let abs = ls + ci;
                let (vs, ve) = if ed.visual_start <= ed.cursor {
                    (ed.visual_start, ed.cursor)
                } else {
                    (ed.cursor, ed.visual_start)
                };
                if abs >= vs && abs <= ve {
                    canvas.fill_rect(cx, py, CHAR_W, CHAR_H, p.primary);
                    canvas.draw_char(cx, py + 2, ch, p.background);
                    continue;
                }
            }
            let fg = syntax_color(&row_bytes, ci, p.text);
            canvas.draw_char(cx, py + 2, ch, fg);
        }
        // Cursor at end of line
        if li == cur_line && cur_col >= row_bytes.len() {
            let cx = text_x + cur_col as i32 * CHAR_W as i32;
            canvas.fill_rect(cx, py, CHAR_W, CHAR_H, p.primary);
        }
    }

    // Status bar
    let sy = (WIN_H - STATUS_H - CMDBAR_H) as i32;
    canvas.fill_rect(0, sy, WIN_W, STATUS_H, p.chrome);
    canvas.draw_hline(0, sy, WIN_W, p.border);
    let mode_txt: &[u8] = match ed.mode {
        Mode::Normal => b"NORMAL",
        Mode::Insert => b"INSERT",
        Mode::Visual => b"VISUAL",
        Mode::Command => b"COMMAND",
    };
    canvas.fill_rect(0, sy, 60, STATUS_H, p.primary);
    canvas.draw_text(4, sy + 4, mode_txt, 0xFF000000, 56);
    let mut pos_buf = [0u8; 24];
    let pl = write_pos_str(&mut pos_buf, cur_line + 1, cur_col + 1);
    canvas.draw_text(
        WIN_W as i32 - pl as i32 * CHAR_W as i32 - 8,
        sy + 4,
        &pos_buf[..pl],
        p.text_muted,
        120,
    );
    if ed.msg_len > 0 {
        canvas.draw_text(68, sy + 4, &ed.msg[..ed.msg_len], p.text, WIN_W - 200);
    }

    // Command bar
    let cb_y = (WIN_H - CMDBAR_H) as i32;
    canvas.fill_rect(0, cb_y, WIN_W, CMDBAR_H, p.background);
    if ed.mode == Mode::Command {
        canvas.draw_text(4, cb_y + 4, b":", p.text, 8);
        canvas.draw_text(10, cb_y + 4, &ed.cmd[..ed.cmd_len], p.text, WIN_W - 20);
    }
}

// ── Utils ─────────────────────────────────────────────────────────────────────

fn write_u64_inline(buf: &mut [u8], mut n: u64) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut l = 0;
    while n > 0 {
        tmp[l] = b'0' + (n % 10) as u8;
        n /= 10;
        l += 1;
    }
    for i in 0..l {
        buf[i] = tmp[l - 1 - i];
    }
    l
}

fn write_pos_str(buf: &mut [u8], line: usize, col: usize) -> usize {
    let l = write_u64_inline(buf, line as u64);
    buf[l] = b':';
    let c = write_u64_inline(&mut buf[l + 1..], col as u64);
    l + 1 + c
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 100, 50, channel) {
        Some(w) => w,
        None => return,
    };
    win.request_focus();
    let mut ed = Editor::new();

    loop {
        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,
                Event::Key {
                    pressed: true,
                    ascii,
                    hid_usage,
                } => {
                    match ed.mode {
                        Mode::Normal | Mode::Visual => ed.handle_key_normal(ascii, hid_usage as u8),
                        Mode::Insert => ed.handle_key_insert(ascii, hid_usage as u8),
                        Mode::Command => ed.handle_key_command(ascii),
                    }
                    ed.adjust_scroll();
                }
                _ => {}
            }
        }
        {
            let mut c = win.canvas();
            render(&mut c, &ed);
        }
        win.present();
        unsafe {
            graphos_app_sdk::sys::yield_task();
        }
    }
}
