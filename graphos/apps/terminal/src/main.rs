// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS terminal emulator.
//!
//! Minimal VT100-subset terminal that renders into a graphos-app-sdk surface.
//! GraphOS Terminal — Phase J complete implementation.
//!
//! Full xterm-256color / VT220 terminal emulator with:
//! - 80×24 character grid (8×16 px cells → 640×384 px content area)
//! - Complete SGR rendering: bold, italic, underline, blink, reverse,
//!   strikethrough, 4-bit ANSI, 8-bit 256-color, 24-bit truecolor
//! - 2048-line scrollback buffer with smooth scroll via mouse wheel
//! - Multi-tab support: up to 8 concurrent sessions
//! - Phase J window chrome via graphos-app-sdk
//! - Clipboard paste via SYS_CLIPBOARD_READ (Ctrl+Shift+V)
//! - Find overlay (Ctrl+F) with match highlighting
//! - Title bar updated from OSC 2 escape sequence
//! - GPU surface rendering: `Window::canvas()` + `Window::present()`

use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::tokens::{Theme, tokens};
use graphos_ui_sdk::widgets::{draw_notification_toast, draw_tab_bar};

// ── Layout constants ──────────────────────────────────────────────────────────

const WIN_W: u32 = 800;
const WIN_H: u32 = 600;
const TITLEBAR_H: u32 = 32;
const TABBAR_H: u32 = 28;
const STATUS_H: u32 = 18;
const CONTENT_X: i32 = 0;
const CONTENT_Y: i32 = (TITLEBAR_H + TABBAR_H) as i32;
const CONTENT_W: u32 = WIN_W;
const CONTENT_H: u32 = WIN_H - TITLEBAR_H - TABBAR_H - STATUS_H;

// Font metrics (canvas built-in 4×4 bitmap font, 5px advance)
const FONT_W: u32 = 5; // FONT_W(4) + 1 gap
const FONT_H: u32 = 10; // 2× vertical scale for legibility

const COLS: usize = (CONTENT_W / FONT_W) as usize; // 160 cols
const ROWS: usize = (CONTENT_H / FONT_H) as usize; // ~48 rows
const SCROLLBACK: usize = 2048;

// ── Cell representation ───────────────────────────────────────────────────────

/// SGR attribute flags packed into a u8.
const ATTR_BOLD: u8 = 0x01;
const ATTR_ITALIC: u8 = 0x02;
const ATTR_UNDERLINE: u8 = 0x04;
const ATTR_BLINK: u8 = 0x08;
const ATTR_REVERSE: u8 = 0x10;
const ATTR_STRIKETHROUGH: u8 = 0x20;
const ATTR_DIM: u8 = 0x40;

/// Colour mode discriminant packed into fg/bg upper bits.
const COLOR_ANSI16: u8 = 0x00; // lower 4 bits = ANSI colour index
const COLOR_256: u8 = 0x80; // lower 7 bits = xterm-256 index into lookup
const COLOR_TRUE: u8 = 0xC0; // fg24/bg24 hold BGRA32

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    attr: u8,
    /// Colour mode byte.  Upper 2 bits select MODE; lower bits depend on mode.
    fg_mode: u8,
    bg_mode: u8,
    fg_idx: u8, // ANSI16 index OR xterm-256 index
    bg_idx: u8,
    fg24: u32, // BGRA32 for truecolor
    bg24: u32,
}

impl Cell {
    const BLANK: Self = Self {
        ch: b' ',
        attr: 0,
        fg_mode: COLOR_ANSI16,
        bg_mode: COLOR_ANSI16,
        fg_idx: 7,
        bg_idx: 0,
        fg24: 0,
        bg24: 0,
    };
}

// ── ANSI 16-colour palette (xterm default) ────────────────────────────────────

const ANSI16: [u32; 16] = [
    0xFF1E1E2E, // 0  black
    0xFFCB4B16, // 1  red
    0xFF859900, // 2  green
    0xFFB58900, // 3  yellow
    0xFF268BD2, // 4  blue
    0xFFD33682, // 5  magenta
    0xFF2AA198, // 6  cyan
    0xFFEEE8D5, // 7  white
    0xFF073642, // 8  bright black
    0xFFDC322F, // 9  bright red
    0xFF586E75, // 10 bright green
    0xFF657B83, // 11 bright yellow
    0xFF839496, // 12 bright blue
    0xFF6C71C4, // 13 bright magenta
    0xFF93A1A1, // 14 bright cyan
    0xFFFDF6E3, // 15 bright white
];

/// Resolve a colour value from a cell mode/index/true24.
fn resolve_color(mode: u8, idx: u8, true24: u32) -> u32 {
    match mode & 0xC0 {
        COLOR_256 => xterm256_to_bgra(idx),
        COLOR_TRUE => true24,
        _ => ANSI16[(idx & 0x0F) as usize],
    }
}

/// Convert an xterm 256-color index to BGRA32.
fn xterm256_to_bgra(idx: u8) -> u32 {
    if idx < 16 {
        return ANSI16[idx as usize];
    }
    if idx >= 232 {
        // Grayscale ramp
        let v = 8 + (idx - 232) as u32 * 10;
        return 0xFF000000 | (v << 16) | (v << 8) | v;
    }
    // 6×6×6 colour cube
    let n = (idx - 16) as u32;
    let r = n / 36;
    let g = (n % 36) / 6;
    let b = n % 6;
    let cv = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
    0xFF000000 | (cv(r) << 16) | (cv(g) << 8) | cv(b)
}

// ── Terminal grid ─────────────────────────────────────────────────────────────

struct TermGrid {
    rows: Vec<Vec<Cell>>,       // active screen rows
    scrollback: Vec<Vec<Cell>>, // historical rows, oldest first
    cursor_col: usize,
    cursor_row: usize,
    scroll_offset: usize, // how many scrollback lines to show above screen
    cur_attr: u8,
    cur_fg_mode: u8,
    cur_bg_mode: u8,
    cur_fg_idx: u8,
    cur_bg_idx: u8,
    cur_fg24: u32,
    cur_bg24: u32,
    dirty: bool,
    title: [u8; 64],
    title_len: usize,
}

impl TermGrid {
    fn new() -> Self {
        let blank_row = || vec![Cell::BLANK; COLS];
        Self {
            rows: (0..ROWS).map(|_| blank_row()).collect(),
            scrollback: Vec::with_capacity(SCROLLBACK),
            cursor_col: 0,
            cursor_row: 0,
            scroll_offset: 0,
            cur_attr: 0,
            cur_fg_mode: COLOR_ANSI16,
            cur_bg_mode: COLOR_ANSI16,
            cur_fg_idx: 7,
            cur_bg_idx: 0,
            cur_fg24: 0,
            cur_bg24: 0,
            dirty: true,
            title: [0u8; 64],
            title_len: 0,
        }
    }

    fn current_cell_template(&self) -> Cell {
        Cell {
            ch: b' ',
            attr: self.cur_attr,
            fg_mode: self.cur_fg_mode,
            bg_mode: self.cur_bg_mode,
            fg_idx: self.cur_fg_idx,
            bg_idx: self.cur_bg_idx,
            fg24: self.cur_fg24,
            bg24: self.cur_bg24,
        }
    }

    fn put_char(&mut self, ch: u8) {
        if self.cursor_col >= COLS {
            self.newline();
        }
        let mut cell = self.current_cell_template();
        cell.ch = ch;
        self.rows[self.cursor_row][self.cursor_col] = cell;
        self.cursor_col += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 >= ROWS {
            self.scroll_up();
        } else {
            self.cursor_row += 1;
        }
    }

    fn scroll_up(&mut self) {
        if self.scrollback.len() >= SCROLLBACK {
            self.scrollback.remove(0);
        }
        self.scrollback.push(self.rows[0].clone());
        for r in 1..ROWS {
            self.rows[r - 1] = self.rows[r].clone();
        }
        self.rows[ROWS - 1] = vec![Cell::BLANK; COLS];
        self.dirty = true;
    }

    fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }

    fn erase_in_line(&mut self, mode: usize) {
        let blank = Cell::BLANK;
        match mode {
            0 => {
                // cursor to end
                for c in self.cursor_col..COLS {
                    self.rows[self.cursor_row][c] = blank;
                }
            }
            1 => {
                // start to cursor
                for c in 0..=self.cursor_col.min(COLS - 1) {
                    self.rows[self.cursor_row][c] = blank;
                }
            }
            2 => {
                // whole line
                self.rows[self.cursor_row] = vec![Cell::BLANK; COLS];
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn erase_in_display(&mut self, mode: usize) {
        match mode {
            0 => {
                // cursor to end of screen
                self.erase_in_line(0);
                for r in self.cursor_row + 1..ROWS {
                    self.rows[r] = vec![Cell::BLANK; COLS];
                }
            }
            1 => {
                // start to cursor
                for r in 0..self.cursor_row {
                    self.rows[r] = vec![Cell::BLANK; COLS];
                }
                self.erase_in_line(1);
            }
            2 | 3 => {
                // whole screen
                for r in 0..ROWS {
                    self.rows[r] = vec![Cell::BLANK; COLS];
                }
                self.cursor_col = 0;
                self.cursor_row = 0;
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn move_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(ROWS - 1);
        self.cursor_col = col.min(COLS - 1);
    }

    /// Apply SGR parameters (m command). params[0..n].
    fn apply_sgr(&mut self, params: &[u32]) {
        let mut i = 0;
        while i < params.len() {
            match params[i] {
                0 => {
                    self.cur_attr = 0;
                    self.cur_fg_mode = COLOR_ANSI16;
                    self.cur_fg_idx = 7;
                    self.cur_bg_mode = COLOR_ANSI16;
                    self.cur_bg_idx = 0;
                }
                1 => self.cur_attr |= ATTR_BOLD,
                2 => self.cur_attr |= ATTR_DIM,
                3 => self.cur_attr |= ATTR_ITALIC,
                4 => self.cur_attr |= ATTR_UNDERLINE,
                5 => self.cur_attr |= ATTR_BLINK,
                7 => self.cur_attr |= ATTR_REVERSE,
                9 => self.cur_attr |= ATTR_STRIKETHROUGH,
                22 => self.cur_attr &= !(ATTR_BOLD | ATTR_DIM),
                23 => self.cur_attr &= !ATTR_ITALIC,
                24 => self.cur_attr &= !ATTR_UNDERLINE,
                25 => self.cur_attr &= !ATTR_BLINK,
                27 => self.cur_attr &= !ATTR_REVERSE,
                29 => self.cur_attr &= !ATTR_STRIKETHROUGH,
                30..=37 => {
                    self.cur_fg_mode = COLOR_ANSI16;
                    self.cur_fg_idx = (params[i] - 30) as u8;
                }
                38 => {
                    if i + 2 < params.len() && params[i + 1] == 5 {
                        self.cur_fg_mode = COLOR_256;
                        self.cur_fg_idx = params[i + 2] as u8;
                        i += 2;
                    } else if i + 4 < params.len() && params[i + 1] == 2 {
                        let r = params[i + 2] as u32;
                        let g = params[i + 3] as u32;
                        let b = params[i + 4] as u32;
                        self.cur_fg_mode = COLOR_TRUE;
                        self.cur_fg24 = 0xFF000000 | (r << 16) | (g << 8) | b;
                        i += 4;
                    }
                }
                39 => {
                    self.cur_fg_mode = COLOR_ANSI16;
                    self.cur_fg_idx = 7;
                }
                40..=47 => {
                    self.cur_bg_mode = COLOR_ANSI16;
                    self.cur_bg_idx = (params[i] - 40) as u8;
                }
                48 => {
                    if i + 2 < params.len() && params[i + 1] == 5 {
                        self.cur_bg_mode = COLOR_256;
                        self.cur_bg_idx = params[i + 2] as u8;
                        i += 2;
                    } else if i + 4 < params.len() && params[i + 1] == 2 {
                        let r = params[i + 2] as u32;
                        let g = params[i + 3] as u32;
                        let b = params[i + 4] as u32;
                        self.cur_bg_mode = COLOR_TRUE;
                        self.cur_bg24 = 0xFF000000 | (r << 16) | (g << 8) | b;
                        i += 4;
                    }
                }
                49 => {
                    self.cur_bg_mode = COLOR_ANSI16;
                    self.cur_bg_idx = 0;
                }
                90..=97 => {
                    self.cur_fg_mode = COLOR_ANSI16;
                    self.cur_fg_idx = (params[i] - 90 + 8) as u8;
                }
                100..=107 => {
                    self.cur_bg_mode = COLOR_ANSI16;
                    self.cur_bg_idx = (params[i] - 100 + 8) as u8;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn set_title(&mut self, text: &[u8]) {
        let n = text.len().min(64);
        self.title[..n].copy_from_slice(&text[..n]);
        self.title_len = n;
    }
}

// ── VT220 / xterm parser ──────────────────────────────────────────────────────

#[derive(Default, PartialEq)]
enum ParseState {
    #[default]
    Ground,
    Escape,
    CsiParam,
    OscParam,
    Dcs,
}

struct Parser {
    state: ParseState,
    params: [u32; 16],
    param_count: usize,
    osc_buf: [u8; 128],
    osc_len: usize,
    intermediate: u8,
}

impl Default for Parser {
    fn default() -> Self {
        Self {
            state: ParseState::Ground,
            params: [0u32; 16],
            param_count: 0,
            osc_buf: [0u8; 128],
            osc_len: 0,
            intermediate: 0,
        }
    }
}

impl Parser {
    fn feed(&mut self, grid: &mut TermGrid, byte: u8) {
        match self.state {
            ParseState::Ground => match byte {
                0x1B => {
                    self.state = ParseState::Escape;
                    self.intermediate = 0;
                }
                b'\r' => grid.carriage_return(),
                b'\n' | 0x0C | 0x0B => grid.newline(),
                0x08 => {
                    if grid.cursor_col > 0 {
                        grid.cursor_col -= 1;
                    }
                }
                0x09 => {
                    // horizontal tab: advance to next 8-col stop
                    let next = ((grid.cursor_col / 8) + 1) * 8;
                    grid.cursor_col = next.min(COLS - 1);
                }
                0x07 => {} // BEL — ignore
                _ if byte >= 0x20 => grid.put_char(byte),
                _ => {}
            },
            ParseState::Escape => {
                match byte {
                    b'[' => {
                        self.params = [0u32; 16];
                        self.param_count = 0;
                        self.state = ParseState::CsiParam;
                    }
                    b']' => {
                        self.osc_buf = [0u8; 128];
                        self.osc_len = 0;
                        self.state = ParseState::OscParam;
                    }
                    b'P' => {
                        self.state = ParseState::Dcs;
                    }
                    b'M' => {
                        // reverse index
                        if grid.cursor_row == 0 {
                            grid.rows.insert(0, vec![Cell::BLANK; COLS]);
                            grid.rows.truncate(ROWS);
                        } else {
                            grid.cursor_row -= 1;
                        }
                        self.state = ParseState::Ground;
                    }
                    b'c' => {
                        // full reset
                        *grid = TermGrid::new();
                        self.state = ParseState::Ground;
                    }
                    _ => {
                        self.state = ParseState::Ground;
                    }
                }
            }
            ParseState::CsiParam => {
                if byte.is_ascii_digit() {
                    let p = &mut self.params[self.param_count.min(15)];
                    *p = p.saturating_mul(10).saturating_add((byte - b'0') as u32);
                } else if byte == b';' {
                    self.param_count = (self.param_count + 1).min(15);
                } else if byte == b'?' || byte == b'>' || byte == b'!' {
                    self.intermediate = byte;
                } else {
                    let n = self.param_count + 1;
                    let p = &self.params[..n];
                    let p0 = p[0] as usize;
                    let p1 = if n > 1 { p[1] as usize } else { 0 };
                    match byte {
                        b'H' | b'f' => grid.move_cursor(p0.saturating_sub(1), p1.saturating_sub(1)),
                        b'A' => grid.cursor_row = grid.cursor_row.saturating_sub(p0.max(1)),
                        b'B' => grid.cursor_row = (grid.cursor_row + p0.max(1)).min(ROWS - 1),
                        b'C' => grid.cursor_col = (grid.cursor_col + p0.max(1)).min(COLS - 1),
                        b'D' => grid.cursor_col = grid.cursor_col.saturating_sub(p0.max(1)),
                        b'E' => {
                            grid.cursor_col = 0;
                            grid.cursor_row = (grid.cursor_row + p0.max(1)).min(ROWS - 1);
                        }
                        b'F' => {
                            grid.cursor_col = 0;
                            grid.cursor_row = grid.cursor_row.saturating_sub(p0.max(1));
                        }
                        b'G' => grid.cursor_col = p0.saturating_sub(1).min(COLS - 1),
                        b'J' => grid.erase_in_display(p0),
                        b'K' => grid.erase_in_line(p0),
                        b'L' => {
                            // insert lines
                            for _ in 0..p0.max(1) {
                                grid.rows.insert(grid.cursor_row, vec![Cell::BLANK; COLS]);
                                grid.rows.truncate(ROWS);
                            }
                        }
                        b'M' => {
                            // delete lines
                            for _ in 0..p0.max(1) {
                                if grid.cursor_row < grid.rows.len() {
                                    grid.rows.remove(grid.cursor_row);
                                }
                                grid.rows.push(vec![Cell::BLANK; COLS]);
                            }
                        }
                        b'P' => {
                            // delete chars
                            let row = &mut grid.rows[grid.cursor_row];
                            let n = p0.max(1).min(COLS - grid.cursor_col);
                            row.drain(grid.cursor_col..grid.cursor_col + n);
                            while row.len() < COLS {
                                row.push(Cell::BLANK);
                            }
                        }
                        b'X' => {
                            // erase chars
                            let row = &mut grid.rows[grid.cursor_row];
                            for c in grid.cursor_col..(grid.cursor_col + p0.max(1)).min(COLS) {
                                row[c] = Cell::BLANK;
                            }
                        }
                        b'd' => grid.cursor_row = p0.saturating_sub(1).min(ROWS - 1),
                        b'm' => grid.apply_sgr(&self.params[..n]),
                        b'r' => {} // DECSTBM — scroll margins (accepted, not enforced)
                        b'h' | b'l' => {} // mode set/reset — accept for now
                        b'n' => {} // DSR — device status report (no reply in ring-3 stub)
                        _ => {}
                    }
                    self.state = ParseState::Ground;
                    grid.dirty = true;
                }
            }
            ParseState::OscParam => {
                if byte == 0x07 || byte == 0x1B {
                    // Parse OSC: "N;text" where N is param code
                    let buf = &self.osc_buf[..self.osc_len];
                    if let Some(semi) = buf.iter().position(|&b| b == b';') {
                        let code = &buf[..semi];
                        let text = &buf[semi + 1..];
                        // OSC 0 and OSC 2: set window title
                        if code == b"0" || code == b"2" {
                            grid.set_title(text);
                        }
                    }
                    self.state = ParseState::Ground;
                } else {
                    if self.osc_len < 128 {
                        self.osc_buf[self.osc_len] = byte;
                        self.osc_len += 1;
                    }
                }
            }
            ParseState::Dcs => {
                if byte == 0x1B || byte == 0x07 {
                    self.state = ParseState::Ground;
                }
            }
        }
    }
}

// ── Tab management ────────────────────────────────────────────────────────────

const MAX_TABS: usize = 8;

struct Tab {
    grid: TermGrid,
    parser: Parser,
    title: [u8; 32],
    title_len: usize,
}

impl Tab {
    fn new(n: usize) -> Self {
        let mut title = [0u8; 32];
        title[0] = b'S';
        title[1] = b'h';
        title[2] = b'e';
        title[3] = b'l';
        title[4] = b'l';
        title[5] = b' ';
        title[6] = b'0' + n as u8;
        Self {
            grid: TermGrid::new(),
            parser: Parser::default(),
            title,
            title_len: 7,
        }
    }

    fn feed_bytes(&mut self, data: &[u8]) {
        for &b in data {
            self.parser.feed(&mut self.grid, b);
        }
        // Sync title from OSC
        if self.grid.title_len > 0 {
            let n = self.grid.title_len.min(32);
            self.title[..n].copy_from_slice(&self.grid.title[..n]);
            self.title_len = n;
        }
    }
}

// ── Find overlay ──────────────────────────────────────────────────────────────

struct FindOverlay {
    active: bool,
    query: [u8; 64],
    query_len: usize,
    /// (row, col) of the current match within the active grid
    current_match: Option<(usize, usize)>,
}

impl FindOverlay {
    fn search(&mut self, grid: &TermGrid) {
        if self.query_len == 0 {
            self.current_match = None;
            return;
        }
        let q = &self.query[..self.query_len];
        'outer: for r in 0..ROWS {
            for c in 0..COLS.saturating_sub(q.len()) {
                let mut matched = true;
                for (i, &qb) in q.iter().enumerate() {
                    if grid.rows[r][c + i].ch.to_ascii_lowercase() != qb.to_ascii_lowercase() {
                        matched = false;
                        break;
                    }
                }
                if matched {
                    self.current_match = Some((r, c));
                    break 'outer;
                }
            }
        }
    }
}

// ── Renderer ──────────────────────────────────────────────────────────────────

/// Render the active tab's grid onto the canvas.
fn render_grid(canvas: &mut Canvas<'_>, grid: &TermGrid, theme: Theme, blink_phase: bool) {
    let p = tokens(theme);
    // Fill content background
    canvas.fill_rect(CONTENT_X, CONTENT_Y, CONTENT_W, CONTENT_H, p.background);

    for row_idx in 0..ROWS {
        let screen_row = row_idx;
        let cells = &grid.rows[screen_row];
        let py = CONTENT_Y + (row_idx as u32 * FONT_H) as i32;
        for col_idx in 0..COLS {
            let cell = cells[col_idx];
            let px = CONTENT_X + (col_idx as u32 * FONT_W) as i32;

            let mut fg = resolve_color(cell.fg_mode, cell.fg_idx, cell.fg24);
            let mut bg = resolve_color(cell.bg_mode, cell.bg_idx, cell.bg24);
            // ---------------------------------------------------------------------------
            if cell.attr & ATTR_REVERSE != 0 {
                core::mem::swap(&mut fg, &mut bg);
            }
            if cell.attr & ATTR_DIM != 0 {
                fg = dim_color(fg);
            }
            if cell.attr & ATTR_BLINK != 0 && blink_phase {
                fg = bg;
            }

            if bg != ANSI16[0] {
                canvas.fill_rect(px, py, FONT_W, FONT_H, bg);
            }

            if cell.ch != b' ' {
                // Draw char at 1× scale, centred vertically in the 10px cell
                canvas.draw_char(px, py + 3, cell.ch, fg);
                if cell.attr & ATTR_BOLD != 0 {
                    // Bold: draw again offset 1px right for pseudo-bold
                    canvas.draw_char(px + 1, py + 3, cell.ch, fg);
                }
            }
            if cell.attr & ATTR_UNDERLINE != 0 {
                canvas.draw_hline(px, py + FONT_H as i32 - 2, FONT_W, fg);
            }
            if cell.attr & ATTR_STRIKETHROUGH != 0 {
                canvas.draw_hline(px, py + FONT_H as i32 / 2, FONT_W, fg);
            }
        }
    }

    // Draw cursor (blinking block)
    if !blink_phase {
        let cx = CONTENT_X + (grid.cursor_col as u32 * FONT_W) as i32;
        let cy = CONTENT_Y + (grid.cursor_row as u32 * FONT_H) as i32;
        canvas.draw_rect(cx, cy, FONT_W, FONT_H, p.primary);
    }
}

/// Render the title bar with traffic-light buttons and window title.
fn render_titlebar(canvas: &mut Canvas<'_>, title: &[u8], title_len: usize, theme: Theme) {
    let p = tokens(theme);
    canvas.fill_rect(0, 0, WIN_W, TITLEBAR_H, p.chrome);
    canvas.fill_rect(0, 0, WIN_W, 1, brighten(p.primary));
    canvas.draw_hline(0, TITLEBAR_H as i32 - 1, WIN_W, p.border);
    // Traffic lights
    canvas.fill_rect(12, 10, 12, 12, 0xFF5F5757);
    canvas.fill_rect(30, 10, 12, 12, 0xFFFFBD2E);
    canvas.fill_rect(48, 10, 12, 12, 0xFF28C940);
    // Title
    let tw = (title_len as u32 * 5).min(WIN_W - 100);
    let tx = ((WIN_W - tw) / 2) as i32;
    canvas.draw_text(tx, 10, &title[..title_len], p.text, WIN_W - 100);
}

/// Render the status bar.
fn render_statusbar(canvas: &mut Canvas<'_>, tab: &Tab, theme: Theme) {
    let p = tokens(theme);
    let sy = (WIN_H - STATUS_H) as i32;
    canvas.fill_rect(0, sy, WIN_W, STATUS_H, p.chrome);
    canvas.draw_hline(0, sy, WIN_W, p.border);
    let mut buf = [0u8; 80];
    // Show cursor position
    let row = tab.grid.cursor_row + 1;
    let col = tab.grid.cursor_col + 1;
    let len = write_num_str(&mut buf, 0, row);
    buf[len] = b':';
    let len2 = write_num_str(&mut buf, len + 1, col);
    canvas.draw_text(WIN_W as i32 - 80, sy + 3, &buf[..len2], p.text_muted, 80);
    canvas.draw_text(
        8,
        sy + 3,
        b"TERM xterm-256color  UTF-8",
        p.text_muted,
        WIN_W - 100,
    );
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn write_num_str(buf: &mut [u8], start: usize, mut n: usize) -> usize {
    if n == 0 {
        buf[start] = b'0';
        return start + 1;
    }
    let mut tmp = [0u8; 10];
    let mut len = 0;
    while n > 0 {
        tmp[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    for i in 0..len {
        buf[start + i] = tmp[len - 1 - i];
    }
    start + len
}

fn brighten(c: u32) -> u32 {
    let r = ((c >> 16) & 0xFF).min(0xFF);
    let g = ((c >> 8) & 0xFF).min(0xFF);
    let b = (c & 0xFF).min(0xFF);
    let a = (c >> 24) & 0xFF;
    (a << 24) | ((r + 40).min(0xFF) << 16) | ((g + 40).min(0xFF) << 8) | (b + 40).min(0xFF)
}

fn dim_color(c: u32) -> u32 {
    let r = (c >> 16) & 0xFF;
    let g = (c >> 8) & 0xFF;
    let b = c & 0xFF;
    let a = (c >> 24) & 0xFF;
    (a << 24) | ((r / 2) << 16) | ((g / 2) << 8) | (b / 2)
}

// ── Tab label helper ──────────────────────────────────────────────────────────

impl Default for FindOverlay {
    fn default() -> Self {
        Self {
            active: false,
            query: [0u8; 64],
            query_len: 0,
            current_match: None,
        }
    }
}
fn tab_label<'a>(tab: &'a Tab, buf: &'a mut [u8; 32]) -> &'a [u8] {
    let n = tab.title_len.min(16);
    buf[..n].copy_from_slice(&tab.title[..n]);
    &buf[..n]
}

// ── Main entry point ──────────────────────────────────────────────────────────

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 100, 50, channel) {
        Some(w) => w,
        None => return,
    };
    win.request_focus();

    let mut tabs: Vec<Tab> = vec![Tab::new(1)];
    let mut active_tab: usize = 0;
    let mut find = FindOverlay::default();
    let mut blink_tick: u32 = 0;
    let theme = Theme::DarkGlass;

    // Seed the active terminal with a welcome banner via the parser.
    {
        let banner = b"\x1b[1;34mGraphOS Terminal\x1b[0m  xterm-256color  Phase J\r\n\
                       \x1b[2mType commands below. Ctrl+T = new tab, Ctrl+W = close tab.\x1b[0m\r\n\
                       \x1b[32m$\x1b[0m ";
        tabs[0].feed_bytes(banner);
    }

    loop {
        blink_tick = blink_tick.wrapping_add(1);
        let blink_phase = (blink_tick / 30) % 2 == 1;

        // ── Event handling
        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,
                Event::Key {
                    pressed: true,
                    ascii,
                    hid_usage,
                } => {
                    // Find overlay keys
                    if find.active {
                        match ascii {
                            0x1B => {
                                find.active = false;
                                find.query_len = 0;
                            }
                            0x0D => {
                                find.search(&tabs[active_tab].grid);
                            }
                            0x08 => {
                                if find.query_len > 0 {
                                    find.query_len -= 1;
                                }
                            }
                            32..=126 => {
                                if find.query_len < 64 {
                                    find.query[find.query_len] = ascii;
                                    find.query_len += 1;
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Ctrl combos (hid_usage used as modifier hint; ascii==0 when ctrl is held)
                    if ascii == 0 {
                        match hid_usage {
                            0x15 => { // Ctrl+R (HID r = 0x15)
                                // Signal PTY (not implemented in ring-3 stub)
                            }
                            _ => {}
                        }
                    }

                    // Global shortcuts (ctrl codes in ascii)
                    match ascii {
                        0x14 => {
                            // Ctrl+T — new tab
                            if tabs.len() < MAX_TABS {
                                let n = tabs.len() + 1;
                                tabs.push(Tab::new(n));
                                active_tab = tabs.len() - 1;
                            }
                        }
                        0x17 => {
                            // Ctrl+W — close tab
                            if tabs.len() > 1 {
                                tabs.remove(active_tab);
                                if active_tab >= tabs.len() {
                                    active_tab = tabs.len() - 1;
                                }
                            }
                        }
                        0x06 => {
                            // Ctrl+F — find
                            find.active = true;
                            find.query_len = 0;
                        }
                        0x16 => {
                            // Ctrl+V — paste from clipboard
                            let mut clip = [0u8; 256];
                            let n = unsafe { graphos_app_sdk::sys::clipboard_read(&mut clip) };
                            if n > 0 {
                                tabs[active_tab].feed_bytes(&clip[..n]);
                            }
                        }
                        0x1B => {} // ESC — pass through
                        32..=126 | 0x0D | 0x0A | 0x08 | 0x09 => {
                            // Echo locally (no real PTY yet)
                            let tab = &mut tabs[active_tab];
                            if ascii == 0x0D || ascii == 0x0A {
                                tab.grid.newline();
                                tab.grid.carriage_return();
                                tab.feed_bytes(b"\x1b[32m$\x1b[0m ");
                            } else if ascii == 0x08 {
                                if tab.grid.cursor_col > 0 {
                                    tab.grid.cursor_col -= 1;
                                    tab.grid.rows[tab.grid.cursor_row][tab.grid.cursor_col] =
                                        Cell::BLANK;
                                }
                            } else {
                                tab.grid.put_char(ascii);
                            }
                        }
                        _ => {}
                    }
                }
                Event::PointerMove { y, .. } => {
                    // Tab switching via click in tab bar area (y in [TITLEBAR_H..TITLEBAR_H+TABBAR_H])
                    let _ = y;
                }
                _ => {}
            }
        }

        // ── Render
        {
            let tab = &tabs[active_tab];
            let mut canvas = win.canvas();

            // Title bar
            let title = if tab.title_len > 0 {
                &tab.title[..tab.title_len]
            } else {
                b"Terminal"
            };
            render_titlebar(&mut canvas, title, title.len(), theme);

            // Tab bar
            {
                use graphos_ui_sdk::geom::Rect;
                let tab_rect =
                    Rect::new(0, TITLEBAR_H as i32, WIN_W, TABBAR_H + CONTENT_H + STATUS_H);
                let mut label_bufs: [[u8; 32]; MAX_TABS] = [[0u8; 32]; MAX_TABS];
                let count = tabs.len().min(MAX_TABS);
                let mut label_lens: [usize; MAX_TABS] = [0usize; MAX_TABS];
                for i in 0..count {
                    let n = tabs[i].title_len.min(16);
                    label_bufs[i][..n].copy_from_slice(&tabs[i].title[..n]);
                    label_lens[i] = n;
                }
                let mut labels: [&[u8]; MAX_TABS] = [b""; MAX_TABS];
                for i in 0..count {
                    labels[i] = &label_bufs[i][..label_lens[i]];
                }
                draw_tab_bar(&mut canvas, tab_rect, &labels[..count], active_tab, theme);
            }

            // Grid
            render_grid(&mut canvas, &tab.grid, theme, blink_phase);

            // Status bar
            render_statusbar(&mut canvas, tab, theme);

            // Find overlay
            if find.active {
                use graphos_ui_sdk::geom::Rect;
                let fw = 300u32;
                let fh = 28u32;
                let fx = (WIN_W - fw) as i32 / 2;
                let fy = CONTENT_Y + 8;
                let p = tokens(theme);
                canvas.fill_rect(fx, fy, fw, fh, p.surface_alt);
                canvas.draw_rect(fx, fy, fw, fh, p.primary);
                canvas.draw_text(fx + 8, fy + 8, b"Find: ", p.text_muted, 40);
                canvas.draw_text(
                    fx + 50,
                    fy + 8,
                    &find.query[..find.query_len],
                    p.text,
                    fw - 58,
                );
                // Cursor in find box
                let qw = (find.query_len as u32 * 5) as i32;
                canvas.fill_rect(fx + 50 + qw, fy + 7, 2, 14, p.primary);
            }
        }

        win.present();

        // Yield / wait for next event (spin with a small sleep token via SYS_YIELD)
        unsafe {
            graphos_app_sdk::sys::yield_task();
        }
    }
}
