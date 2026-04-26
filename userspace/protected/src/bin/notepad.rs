// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! notepad - GraphOS quick capture notes surface.
//!
//! Lightweight companion to the full editor:
//! - autosaved scratch note at `/tmp/notepad.txt`
//! - modern GraphOS shell chrome and stats
//! - keyboard-first editing with a simple side rail

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;
#[path = "../workspace_context.rs"]
mod workspace_context;

use core::panic::PanicInfo;
use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::{
    geom::Rect,
    tokens::{Theme, tokens},
    widgets::{
        ButtonKind, draw_button, draw_panel, draw_scroll_track, draw_sidebar_item, draw_stat_card,
        draw_window_frame,
    },
};

const WIN_W: u32 = 920;
const WIN_H: u32 = 560;
const THEME: Theme = Theme::DarkGlass;

const HEADER_H: u32 = 32;
const FOOTER_H: u32 = 26;
const SIDEBAR_W: u32 = 188;
const RAIL_W: u32 = 224;
const LINE_H: u32 = 12;
const GUTTER_W: u32 = 36;
const BUF_CAP: usize = 6144;
const STATUS_CAP: usize = 96;
const SAVE_PATH: &[u8] = b"/tmp/notepad.txt";

struct NotesApp {
    buf: [u8; BUF_CAP],
    len: usize,
    cursor: usize,
    scroll: usize,
    modified: bool,
    status: [u8; STATUS_CAP],
    status_len: usize,
    now_ms: u64,
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
    context_scope: [u8; workspace_context::PATH_CAP],
    context_scope_len: usize,
    context_focus: [u8; workspace_context::PATH_CAP],
    context_focus_len: usize,
    context_source: [u8; workspace_context::SOURCE_CAP],
    context_source_len: usize,
    last_context_refresh_ms: u64,
}

impl NotesApp {
    fn new() -> Self {
        let mut app = Self {
            buf: [0u8; BUF_CAP],
            len: 0,
            cursor: 0,
            scroll: 0,
            modified: false,
            status: [0u8; STATUS_CAP],
            status_len: 0,
            now_ms: 0,
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
            context_scope: [0u8; workspace_context::PATH_CAP],
            context_scope_len: 0,
            context_focus: [0u8; workspace_context::PATH_CAP],
            context_focus_len: 0,
            context_source: [0u8; workspace_context::SOURCE_CAP],
            context_source_len: 0,
            last_context_refresh_ms: 0,
        };
        app.refresh_context();
        app.restore();
        if app.len == 0 {
            app.set_status(b"Quick capture pad ready.");
        }
        app
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn restore(&mut self) {
        let fd = runtime::vfs_open(SAVE_PATH);
        if fd == u64::MAX {
            return;
        }
        let read = runtime::vfs_read(fd, &mut self.buf) as usize;
        runtime::vfs_close(fd);
        if read > 0 {
            self.len = read.min(BUF_CAP);
            self.cursor = self.len;
            self.scroll = 0;
            self.modified = false;
            self.set_status(b"Restored quick note.");
        }
    }

    fn workspace_scope(&self) -> &[u8] {
        if self.context_scope_len > 0 {
            &self.context_scope[..self.context_scope_len]
        } else {
            b"/graph"
        }
    }

    fn context_focus(&self) -> &[u8] {
        if self.context_focus_len > 0 {
            &self.context_focus[..self.context_focus_len]
        } else {
            self.workspace_scope()
        }
    }

    fn context_source(&self) -> &[u8] {
        if self.context_source_len > 0 {
            &self.context_source[..self.context_source_len]
        } else {
            b"graph"
        }
    }

    fn refresh_context(&mut self) -> bool {
        let Some(ctx) = workspace_context::read() else {
            return false;
        };
        let mut changed = false;
        changed |= replace_bytes(
            &mut self.context_scope,
            &mut self.context_scope_len,
            ctx.scope(),
        );
        changed |= replace_bytes(
            &mut self.context_focus,
            &mut self.context_focus_len,
            ctx.focus(),
        );
        changed |= replace_bytes(
            &mut self.context_source,
            &mut self.context_source_len,
            ctx.source(),
        );
        changed
    }

    fn save(&mut self) {
        let fd = runtime::vfs_create(SAVE_PATH);
        if fd == u64::MAX {
            self.set_status(b"Save failed.");
            return;
        }
        let written = runtime::vfs_write(fd, &self.buf[..self.len]) as usize;
        runtime::vfs_close(fd);
        if written == self.len {
            self.modified = false;
            self.set_status(b"Saved quick note.");
        } else {
            self.set_status(b"Partial save.");
        }
    }

    fn clear(&mut self) {
        self.buf = [0u8; BUF_CAP];
        self.len = 0;
        self.cursor = 0;
        self.scroll = 0;
        self.modified = false;
        self.set_status(b"Scratch note cleared.");
    }

    fn launch_editor(&mut self) {
        if runtime::spawn_named_checked(b"editor") {
            self.set_status(b"Editor launched.");
        } else {
            self.set_status(b"Editor launch failed.");
        }
    }

    fn launch_files(&mut self) {
        if runtime::spawn_named_checked(b"files") {
            self.set_status(b"Files launched.");
        } else {
            self.set_status(b"Files launch failed.");
        }
    }

    fn launch_ai(&mut self) {
        if runtime::spawn_named_checked(b"ai-console") {
            self.set_status(b"Copilot launched.");
        } else {
            self.set_status(b"Copilot launch failed.");
        }
    }

    fn insert(&mut self, byte: u8) {
        if self.len >= BUF_CAP {
            self.set_status(b"Buffer full.");
            return;
        }
        let mut i = self.len;
        while i > self.cursor {
            self.buf[i] = self.buf[i - 1];
            i -= 1;
        }
        self.buf[self.cursor] = byte;
        self.cursor += 1;
        self.len += 1;
        self.modified = true;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        let mut i = self.cursor;
        while i + 1 < self.len {
            self.buf[i] = self.buf[i + 1];
            i += 1;
        }
        self.len -= 1;
        self.modified = true;
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn move_right(&mut self) {
        if self.cursor < self.len {
            self.cursor += 1;
        }
    }

    fn move_up(&mut self) {
        let line_start = self.current_line_start();
        if line_start == 0 {
            return;
        }
        let prev_end = line_start - 1;
        let prev_start = self.line_start_before(prev_end);
        let col = self.cursor - line_start;
        self.cursor = prev_start + col.min(prev_end - prev_start);
    }

    fn move_down(&mut self) {
        let line_start = self.current_line_start();
        let line_end = self.current_line_end();
        if line_end >= self.len {
            return;
        }
        let next_start = line_end + 1;
        let mut next_end = next_start;
        while next_end < self.len && self.buf[next_end] != b'\n' {
            next_end += 1;
        }
        let col = self.cursor - line_start;
        self.cursor = next_start + col.min(next_end - next_start);
    }

    fn current_line_start(&self) -> usize {
        if self.cursor == 0 {
            return 0;
        }
        let mut i = self.cursor - 1;
        loop {
            if self.buf[i] == b'\n' {
                return i + 1;
            }
            if i == 0 {
                return 0;
            }
            i -= 1;
        }
    }

    fn current_line_end(&self) -> usize {
        let mut i = self.cursor;
        while i < self.len && self.buf[i] != b'\n' {
            i += 1;
        }
        i
    }

    fn line_start_before(&self, before: usize) -> usize {
        if before == 0 {
            return 0;
        }
        let mut i = before - 1;
        loop {
            if self.buf[i] == b'\n' {
                return i + 1;
            }
            if i == 0 {
                return 0;
            }
            i -= 1;
        }
    }

    fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        let mut i = 0usize;
        while i < self.cursor {
            if self.buf[i] == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            i += 1;
        }
        (line, col)
    }

    fn line_count(&self) -> usize {
        let mut count = 1usize;
        let mut i = 0usize;
        while i < self.len {
            if self.buf[i] == b'\n' {
                count += 1;
            }
            i += 1;
        }
        count
    }

    fn adjust_scroll(&mut self, visible_lines: usize) {
        let line = self.cursor_line_col().0.saturating_sub(1);
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + visible_lines {
            self.scroll = line + 1 - visible_lines;
        }
    }

    fn handle_pointer(&mut self, x: i16, y: i16, buttons: u8) -> bool {
        self.pointer_x = x;
        self.pointer_y = y;
        let left_down = buttons & 1 != 0;
        let left_prev = self.prev_buttons & 1 != 0;
        let mut dirty = true;
        if left_down && !left_prev {
            if contains(sidebar_rect(1), x, y) {
                self.launch_files();
            } else if contains(sidebar_rect(2), x, y) {
                self.save();
            } else if contains(sidebar_rect(3), x, y) {
                self.launch_editor();
            } else if contains(sidebar_rect(4), x, y) {
                self.launch_ai();
            } else if contains(action_rect(0), x, y) {
                self.save();
            } else if contains(action_rect(1), x, y) {
                self.clear();
            } else if contains(action_rect(2), x, y) {
                self.launch_editor();
            } else if contains(action_rect(3), x, y) {
                self.launch_ai();
            } else {
                dirty = false;
            }
        }
        self.prev_buttons = buttons;
        dirty
    }
}

fn contains(rect: Rect, x: i16, y: i16) -> bool {
    let x = x as i32;
    let y = y as i32;
    x >= rect.x && y >= rect.y && x < rect.x + rect.w as i32 && y < rect.y + rect.h as i32
}

fn sidebar_rect(index: usize) -> Rect {
    Rect::new(
        12,
        HEADER_H as i32 + 20 + index as i32 * 38,
        SIDEBAR_W - 24,
        30,
    )
}

fn action_rect(index: usize) -> Rect {
    Rect::new(
        (WIN_W - RAIL_W + 18) as i32,
        (HEADER_H + 206 + index as u32 * 40) as i32,
        RAIL_W - 36,
        30,
    )
}

fn visible_lines(editor_h: u32) -> usize {
    (editor_h / LINE_H).max(1) as usize
}

fn write_usize(mut value: usize, out: &mut [u8]) -> usize {
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
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

fn replace_bytes(dst: &mut [u8], len: &mut usize, src: &[u8]) -> bool {
    let next = src.len().min(dst.len());
    if *len == next && &dst[..next] == &src[..next] {
        return false;
    }
    dst[..next].copy_from_slice(&src[..next]);
    *len = next;
    true
}

fn draw_line_number(canvas: &mut Canvas<'_>, x: i32, y: i32, number: usize, color: u32) {
    let mut buf = [0u8; 12];
    let len = write_usize(number, &mut buf);
    canvas.draw_text(x, y, &buf[..len], color, GUTTER_W - 6);
}

fn draw(win: &mut Window, app: &NotesApp) {
    let palette = tokens(THEME);
    let mut canvas = win.canvas();
    canvas.clear(palette.background);

    draw_window_frame(
        &mut canvas,
        Rect::new(0, 0, WIN_W, WIN_H),
        b"GraphOS Notes",
        THEME,
    );

    let content = Rect::new(0, HEADER_H as i32, WIN_W, WIN_H - HEADER_H - FOOTER_H);
    let (sidebar, main) = content.split_left(SIDEBAR_W);
    let (editor_shell, rail_shell) = main.split_left(main.w.saturating_sub(RAIL_W));

    canvas.fill_rect(sidebar.x, sidebar.y, sidebar.w, sidebar.h, palette.surface);
    canvas.draw_vline(
        sidebar.x + sidebar.w as i32 - 1,
        sidebar.y,
        sidebar.h,
        palette.border,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 8,
        b"Capture",
        palette.text_muted,
        sidebar.w,
    );

    let sidebar_rows: [(&[u8], u32); 5] = [
        (b"Quick Note", palette.primary),
        (b"Open Files", palette.success),
        (b"Save Scratch", palette.warning),
        (b"Open Editor", palette.text_muted),
        (b"Copilot", palette.primary),
    ];
    let mut row = 0usize;
    while row < sidebar_rows.len() {
        let hovered = contains(sidebar_rect(row), app.pointer_x, app.pointer_y);
        draw_sidebar_item(
            &mut canvas,
            sidebar_rect(row),
            sidebar_rows[row].0,
            sidebar_rows[row].1,
            row == 0,
            hovered,
            0,
            THEME,
        );
        row += 1;
    }

    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 196,
        b"Shortcuts",
        palette.text_muted,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 214,
        b"Ctrl+S save",
        palette.text,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 228,
        b"Esc quit",
        palette.text,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 242,
        b"Arrows move",
        palette.text,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 272,
        b"Autosave path",
        palette.text_muted,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 288,
        SAVE_PATH,
        palette.primary,
        sidebar.w - 24,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 320,
        b"Graph focus",
        palette.text_muted,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 336,
        app.context_focus(),
        palette.success,
        sidebar.w - 24,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 356,
        b"Context source",
        palette.text_muted,
        sidebar.w,
    );
    canvas.draw_text(
        sidebar.x + 12,
        sidebar.y + 372,
        app.context_source(),
        palette.primary,
        sidebar.w - 24,
    );

    let editor_panel = draw_panel(&mut canvas, editor_shell, b"Quick Capture", THEME);
    let rail_panel = draw_panel(&mut canvas, rail_shell, b"Session", THEME);

    let stat_w = rail_panel.w;
    draw_stat_card(
        &mut canvas,
        Rect::new(rail_panel.x, rail_panel.y, stat_w, 38),
        b"Lines",
        build_count_label(app.line_count(), b" lines"),
        palette.primary,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(rail_panel.x, rail_panel.y + 44, stat_w, 38),
        b"Chars",
        build_count_label(app.len, b" chars"),
        palette.success,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(rail_panel.x, rail_panel.y + 88, stat_w, 38),
        b"State",
        if app.modified { b"Unsaved" } else { b"Synced" },
        palette.warning,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(rail_panel.x, rail_panel.y + 132, stat_w, 38),
        b"Cursor",
        build_cursor_label(app),
        palette.text_muted,
        THEME,
    );

    draw_button(
        &mut canvas,
        action_rect(0),
        b"Save",
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
        b"Editor",
        ButtonKind::Secondary,
        false,
        contains(action_rect(2), app.pointer_x, app.pointer_y),
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        action_rect(3),
        b"Copilot",
        ButtonKind::Ghost,
        false,
        contains(action_rect(3), app.pointer_x, app.pointer_y),
        false,
        THEME,
    );

    let editor_inner = Rect::new(
        editor_panel.x + 6,
        editor_panel.y + 4,
        editor_panel.w.saturating_sub(14),
        editor_panel.h.saturating_sub(8),
    );
    canvas.fill_rect(
        editor_inner.x,
        editor_inner.y,
        editor_inner.w,
        editor_inner.h,
        palette.surface_alt,
    );
    canvas.draw_rect(
        editor_inner.x,
        editor_inner.y,
        editor_inner.w,
        editor_inner.h,
        palette.border,
    );
    canvas.fill_rect(
        editor_inner.x,
        editor_inner.y,
        GUTTER_W,
        editor_inner.h,
        palette.chrome,
    );
    canvas.draw_vline(
        editor_inner.x + GUTTER_W as i32,
        editor_inner.y,
        editor_inner.h,
        palette.border,
    );

    let text_x = editor_inner.x + GUTTER_W as i32 + 8;
    let text_y = editor_inner.y + 8;
    let text_w = editor_inner.w.saturating_sub(GUTTER_W + 20);
    let lines_visible = visible_lines(editor_inner.h.saturating_sub(16));
    let (cursor_line, cursor_col) = app.cursor_line_col();
    let cursor_blink = ((app.now_ms / 420) & 1) == 0;

    if app.len == 0 {
        canvas.draw_text(
            text_x,
            text_y + 18,
            b"Capture the next idea, command, or graph insight here.",
            palette.text_muted,
            text_w,
        );
        canvas.draw_text(
            text_x,
            text_y + 34,
            b"Notes persist to /tmp/notepad.txt until you clear them.",
            palette.text_muted,
            text_w,
        );
        canvas.draw_text(
            text_x,
            text_y + 50,
            app.workspace_scope(),
            palette.primary,
            text_w,
        );
    }

    let mut line_index = 0usize;
    let mut start = 0usize;
    let mut drawn = 0usize;
    while start <= app.len && drawn < lines_visible {
        let mut end = start;
        while end < app.len && app.buf[end] != b'\n' {
            end += 1;
        }
        if line_index >= app.scroll {
            let y = text_y + drawn as i32 * LINE_H as i32;
            if line_index + 1 == cursor_line {
                canvas.fill_rect(
                    editor_inner.x + GUTTER_W as i32 + 1,
                    y - 2,
                    editor_inner.w.saturating_sub(GUTTER_W + 2),
                    LINE_H,
                    palette.surface,
                );
            }
            draw_line_number(
                &mut canvas,
                editor_inner.x + 6,
                y,
                line_index + 1,
                palette.text_muted,
            );
            canvas.draw_text(text_x, y, &app.buf[start..end], palette.text, text_w);
            if cursor_blink && line_index + 1 == cursor_line {
                let prefix = cursor_col.saturating_sub(1).min(end - start);
                let cursor_x = text_x + Canvas::text_width(&app.buf[start..start + prefix]) as i32;
                canvas.fill_rect(cursor_x, y - 1, 2, 11, palette.primary);
            }
            drawn += 1;
        }
        if end >= app.len {
            break;
        }
        start = end + 1;
        line_index += 1;
    }

    draw_scroll_track(
        &mut canvas,
        Rect::new(
            editor_inner.x + editor_inner.w as i32 - 8,
            editor_inner.y,
            8,
            editor_inner.h,
        ),
        (app.line_count() as u32).saturating_mul(LINE_H),
        editor_inner.h,
        (app.scroll as u32).saturating_mul(LINE_H),
        THEME,
    );

    canvas.fill_rect(
        0,
        (WIN_H - FOOTER_H) as i32,
        WIN_W,
        FOOTER_H,
        palette.chrome,
    );
    canvas.draw_hline(0, (WIN_H - FOOTER_H) as i32, WIN_W, palette.border);
    canvas.draw_text(
        10,
        (WIN_H - FOOTER_H + 7) as i32,
        &app.status[..app.status_len],
        palette.text,
        WIN_W - 20,
    );

    win.present();
}

fn build_count_label(count: usize, suffix: &'static [u8]) -> &'static [u8] {
    match (count, suffix) {
        (0, b" lines") => b"0 lines",
        (1, b" lines") => b"1 line",
        (2, b" lines") => b"2 lines",
        (3, b" lines") => b"3 lines",
        (4, b" lines") => b"4 lines",
        (5, b" lines") => b"5 lines",
        (0, b" chars") => b"0 chars",
        (1, b" chars") => b"1 char",
        (2, b" chars") => b"2 chars",
        (3, b" chars") => b"3 chars",
        (4, b" chars") => b"4 chars",
        (5, b" chars") => b"5 chars",
        _ => {
            if suffix == b" lines" {
                b"6+ lines"
            } else {
                b"6+ chars"
            }
        }
    }
}

fn build_cursor_label(app: &NotesApp) -> &'static [u8] {
    let (line, col) = app.cursor_line_col();
    match (line, col) {
        (1, 1) => b"L1 C1",
        (1, 2) => b"L1 C2",
        (1, 3) => b"L1 C3",
        (2, 1) => b"L2 C1",
        (2, 2) => b"L2 C2",
        (2, 3) => b"L2 C3",
        _ => b"active",
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[notepad] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut app = NotesApp::new();
    draw(&mut win, &app);
    win.request_focus();

    loop {
        match win.poll_event() {
            Event::Key {
                pressed: true,
                ascii,
                hid_usage,
                ..
            } => {
                let mut dirty = true;
                match ascii {
                    0x1B => runtime::exit(0),
                    0x13 => app.save(),
                    0x08 | 0x7F => app.backspace(),
                    0x0D | 0x0A => app.insert(b'\n'),
                    0x20..=0x7E => app.insert(ascii),
                    _ => match hid_usage {
                        0x4F => app.move_right(),
                        0x50 => app.move_left(),
                        0x51 => app.move_down(),
                        0x52 => app.move_up(),
                        _ => dirty = false,
                    },
                }
                if dirty {
                    app.adjust_scroll(visible_lines(WIN_H - HEADER_H - FOOTER_H - 44));
                    draw(&mut win, &app);
                }
            }
            Event::PointerMove { x, y, buttons } => {
                if app.handle_pointer(x, y, buttons) {
                    draw(&mut win, &app);
                }
            }
            Event::FrameTick { now_ms, .. } => {
                let blink_before = (app.now_ms / 420) & 1;
                let blink_after = (now_ms / 420) & 1;
                let refresh_due = app.last_context_refresh_ms == 0
                    || now_ms.saturating_sub(app.last_context_refresh_ms) >= 1000;
                app.now_ms = now_ms;
                let context_changed = if refresh_due {
                    app.last_context_refresh_ms = now_ms;
                    app.refresh_context()
                } else {
                    false
                };
                if blink_before != blink_after || context_changed {
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
    runtime::write_line(b"[notepad] panic\n");
    runtime::exit(255)
}
