// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! editor - GraphOS text editor.
//!
//! Modern ring-3 authoring surface with:
//! - GraphOS shell chrome and inspector panel
//! - scratch-file restore/save on `/tmp/untitled.txt`
//! - compositor-paced cursor blink
//! - consistent keyboard navigation and quick toolbar actions

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
        ButtonKind, draw_button, draw_panel, draw_scroll_track, draw_stat_card, draw_window_frame,
    },
};

const WIN_W: u32 = 980;
const WIN_H: u32 = 620;
const THEME: Theme = Theme::DarkGlass;

const HEADER_H: u32 = 32;
const TOOLBAR_H: u32 = 38;
const FOOTER_H: u32 = 28;
const INFO_W: u32 = 232;
const GUTTER_W: u32 = 44;
const LINE_H: u32 = 12;
const MARGIN: u32 = 8;

const BUF_CAP: usize = 8192;
const MAX_PATH: usize = 64;
const STATUS_CAP: usize = 96;
const CONTROL_SERVICE_COUNT: usize = 3;

const CONTROL_SERVICES: [(&[u8], &[u8], u32); CONTROL_SERVICE_COUNT] = [
    (b"Files", b"files", 0xFF58A6FF),
    (b"Copilot", b"ai-console", 0xFF39C5BB),
    (b"Shell", b"terminal", 0xFF28C940),
];

struct Editor {
    buf: [u8; BUF_CAP],
    len: usize,
    cursor: usize,
    scroll: usize,
    fname: [u8; 48],
    fname_len: usize,
    save_path: [u8; MAX_PATH],
    save_path_len: usize,
    context_scope: [u8; workspace_context::PATH_CAP],
    context_scope_len: usize,
    context_source: [u8; workspace_context::SOURCE_CAP],
    context_source_len: usize,
    modified: bool,
    status: [u8; STATUS_CAP],
    status_len: usize,
    now_ms: u64,
    last_refresh_ms: u64,
    registry_generation: u64,
    graph_transitions: u32,
    graph_epoch: u32,
    service_online: [bool; CONTROL_SERVICE_COUNT],
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
}

impl Editor {
    fn new() -> Self {
        let mut ed = Self {
            buf: [0u8; BUF_CAP],
            len: 0,
            cursor: 0,
            scroll: 0,
            fname: [0u8; 48],
            fname_len: 0,
            save_path: [0u8; MAX_PATH],
            save_path_len: 0,
            context_scope: [0u8; workspace_context::PATH_CAP],
            context_scope_len: 0,
            context_source: [0u8; workspace_context::SOURCE_CAP],
            context_source_len: 0,
            modified: false,
            status: [0u8; STATUS_CAP],
            status_len: 0,
            now_ms: 0,
            last_refresh_ms: 0,
            registry_generation: 0,
            graph_transitions: 0,
            graph_epoch: 0,
            service_online: [false; CONTROL_SERVICE_COUNT],
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
        };
        ed.fname[..12].copy_from_slice(b"untitled.txt");
        ed.fname_len = 12;
        ed.save_path[..17].copy_from_slice(b"/tmp/untitled.txt");
        ed.save_path_len = 17;
        let used_context = ed.apply_workspace_context();
        ed.restore_scratch();
        if used_context {
            if ed.len > 0 {
                ed.set_status(b"Graph artifact loaded.");
            } else {
                ed.set_status(b"Graph workspace ready.");
            }
        } else if ed.len == 0 {
            ed.set_status(b"Scratch buffer ready.");
        }
        ed.sync_workspace_context(b"editor");
        ed.refresh_runtime();
        ed
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn save_path(&self) -> &[u8] {
        &self.save_path[..self.save_path_len]
    }

    fn workspace_scope(&self) -> &[u8] {
        if self.context_scope_len > 0 {
            &self.context_scope[..self.context_scope_len]
        } else {
            workspace_context::parent_path(self.save_path())
        }
    }

    fn context_source(&self) -> &[u8] {
        if self.context_source_len > 0 {
            &self.context_source[..self.context_source_len]
        } else {
            b"editor"
        }
    }

    fn apply_workspace_context(&mut self) -> bool {
        let Some(ctx) = workspace_context::read() else {
            return false;
        };

        self.context_scope_len = copy_bytes(&mut self.context_scope, ctx.scope());
        self.context_source_len = copy_bytes(&mut self.context_source, ctx.source());

        let target = if ctx.has_focus() {
            ctx.focus()
        } else {
            ctx.scope()
        };
        if target.is_empty() {
            return false;
        }

        if ctx.is_dir {
            self.save_path_len = join_path(target, b"untitled.txt", &mut self.save_path);
            self.fname[..12].copy_from_slice(b"untitled.txt");
            self.fname_len = 12;
        } else {
            self.save_path_len = copy_bytes(&mut self.save_path, target);
            let leaf = workspace_context::leaf_name(&self.save_path[..self.save_path_len]);
            self.fname_len = copy_bytes(&mut self.fname, leaf);
        }
        true
    }

    fn restore_scratch(&mut self) {
        let fd = runtime::vfs_open(self.save_path());
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
            self.set_status(b"Restored scratch buffer.");
        }
    }

    fn save_scratch(&mut self) {
        let fd = runtime::vfs_create(self.save_path());
        if fd == u64::MAX {
            self.set_status(b"Save failed.");
            return;
        }
        let written = runtime::vfs_write(fd, &self.buf[..self.len]) as usize;
        runtime::vfs_close(fd);
        if written == self.len {
            self.modified = false;
            self.sync_workspace_context(b"editor");
            self.set_status(b"Saved scratch buffer.");
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
        self.set_status(b"New document.");
    }

    fn insert(&mut self, ch: u8) {
        if self.len >= BUF_CAP {
            return;
        }
        let mut i = self.len;
        while i > self.cursor {
            self.buf[i] = self.buf[i - 1];
            i -= 1;
        }
        self.buf[self.cursor] = ch;
        self.len += 1;
        self.cursor += 1;
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
        let prev_len = prev_end - prev_start;
        self.cursor = prev_start + col.min(prev_len);
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

    fn cursor_pos(&self) -> (usize, usize) {
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

    fn adjust_scroll(&mut self, visible: usize) {
        let line0 = self.cursor_pos().0.saturating_sub(1);
        if line0 < self.scroll {
            self.scroll = line0;
        } else if line0 >= self.scroll + visible {
            self.scroll = line0 + 1 - visible;
        }
    }

    fn refresh_runtime(&mut self) {
        let generation = runtime::registry_subscribe(self.registry_generation);
        if generation != 0 && generation != u64::MAX {
            self.registry_generation = generation;
        }

        let mut idx = 0usize;
        while idx < CONTROL_SERVICE_COUNT {
            self.service_online[idx] = runtime::registry_lookup(CONTROL_SERVICES[idx].1).is_some();
            idx += 1;
        }

        if let Some((transitions, epoch)) = runtime::graph_em_stats(1, 1) {
            self.graph_transitions = transitions;
            self.graph_epoch = epoch;
        }
    }

    fn online_control_count(&self) -> usize {
        let mut total = 0usize;
        let mut idx = 0usize;
        while idx < self.service_online.len() {
            if self.service_online[idx] {
                total += 1;
            }
            idx += 1;
        }
        total
    }

    fn launch_surface(&mut self, name: &[u8], ok: &[u8], fail: &[u8]) {
        self.sync_workspace_context(b"editor");
        if runtime::spawn_named_checked(name) {
            self.set_status(ok);
        } else {
            self.set_status(fail);
        }
    }

    fn sync_workspace_context(&self, source: &[u8]) {
        let _ = workspace_context::write(self.workspace_scope(), self.save_path(), source, false);
    }

    fn handle_pointer(&mut self, x: i16, y: i16, buttons: u8) -> bool {
        self.pointer_x = x;
        self.pointer_y = y;
        let left_down = buttons & 1 != 0;
        let left_prev = self.prev_buttons & 1 != 0;
        let mut dirty = true;
        if left_down && !left_prev {
            if contains(toolbar_rect(0), x, y) {
                self.save_scratch();
            } else if contains(toolbar_rect(1), x, y) {
                self.clear();
            } else if contains(toolbar_rect(2), x, y) {
                self.restore_scratch();
            } else if contains(toolbar_rect(3), x, y) {
                self.launch_surface(b"files", b"Files opened.", b"Files unavailable.");
            } else if contains(toolbar_rect(4), x, y) {
                self.launch_surface(b"ai-console", b"Copilot opened.", b"Copilot unavailable.");
            } else if contains(toolbar_rect(5), x, y) {
                self.launch_surface(b"terminal", b"Shell opened.", b"Shell unavailable.");
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

fn toolbar_rect(index: usize) -> Rect {
    Rect::new(10 + index as i32 * 80, HEADER_H as i32 + 5, 74, 28)
}

fn visible_lines() -> usize {
    let editor_panel_h = WIN_H - HEADER_H - TOOLBAR_H - FOOTER_H;
    ((editor_panel_h - 28) / LINE_H) as usize
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
        len += 1;
        value /= 10;
    }
    let mut i = 0usize;
    while i < len {
        out[i] = tmp[len - 1 - i];
        i += 1;
    }
    len
}

fn format_clock(now_ms: u64, out: &mut [u8; 16]) -> usize {
    let total_secs = (now_ms / 1000) as usize;
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;
    out[0] = b'0' + (hours / 10) as u8;
    out[1] = b'0' + (hours % 10) as u8;
    out[2] = b':';
    out[3] = b'0' + (minutes / 10) as u8;
    out[4] = b'0' + (minutes % 10) as u8;
    out[5] = b':';
    out[6] = b'0' + (seconds / 10) as u8;
    out[7] = b'0' + (seconds % 10) as u8;
    out[8] = b' ';
    out[9] = b'O';
    out[10] = b'R';
    out[11] = b'C';
    12
}

fn format_registry_label(generation: u64, out: &mut [u8; 24]) -> usize {
    let mut len = 0usize;
    out[len] = b'G';
    len += 1;
    out[len] = b'e';
    len += 1;
    out[len] = b'n';
    len += 1;
    out[len] = b' ';
    len += 1;
    len += write_usize(generation as usize, &mut out[len..]);
    len
}

fn format_graph_label(transitions: u32, epoch: u32, out: &mut [u8; 24]) -> usize {
    let mut len = 0usize;
    out[len] = b'T';
    len += 1;
    len += write_usize(transitions as usize, &mut out[len..]);
    out[len] = b' ';
    len += 1;
    out[len] = b'E';
    len += 1;
    len += write_usize(epoch as usize, &mut out[len..]);
    len
}

fn format_online_label(online: usize, total: usize, out: &mut [u8; 12]) -> usize {
    let mut len = write_usize(online, out);
    out[len] = b'/';
    len += 1;
    len += write_usize(total, &mut out[len..]);
    len
}

fn copy_bytes(dst: &mut [u8], src: &[u8]) -> usize {
    let len = src.len().min(dst.len());
    dst[..len].copy_from_slice(&src[..len]);
    len
}

fn join_path(dir: &[u8], leaf: &[u8], out: &mut [u8; MAX_PATH]) -> usize {
    if dir.is_empty() {
        return copy_bytes(out, leaf);
    }

    let mut len = copy_bytes(out, dir);
    if len > 0 && out[len - 1] != b'/' && len < out.len() {
        out[len] = b'/';
        len += 1;
    }

    let rem = out.len().saturating_sub(len);
    let take = leaf.len().min(rem);
    if take > 0 {
        out[len..len + take].copy_from_slice(&leaf[..take]);
        len += take;
    }
    len
}

fn draw(win: &mut Window, ed: &Editor) {
    let palette = tokens(THEME);
    let mut canvas = win.canvas();
    canvas.clear(palette.background);

    let root = Rect::new(0, 0, WIN_W, WIN_H);
    draw_window_frame(&mut canvas, root, b"GraphOS Editor", THEME);

    let save_hover = contains(toolbar_rect(0), ed.pointer_x, ed.pointer_y);
    let new_hover = contains(toolbar_rect(1), ed.pointer_x, ed.pointer_y);
    let reload_hover = contains(toolbar_rect(2), ed.pointer_x, ed.pointer_y);
    let files_hover = contains(toolbar_rect(3), ed.pointer_x, ed.pointer_y);
    let copilot_hover = contains(toolbar_rect(4), ed.pointer_x, ed.pointer_y);
    let shell_hover = contains(toolbar_rect(5), ed.pointer_x, ed.pointer_y);

    draw_button(
        &mut canvas,
        toolbar_rect(0),
        b"Save",
        ButtonKind::Primary,
        false,
        save_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_rect(1),
        b"New",
        ButtonKind::Secondary,
        false,
        new_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_rect(2),
        b"Reload",
        ButtonKind::Ghost,
        false,
        reload_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_rect(3),
        b"Files",
        ButtonKind::Secondary,
        false,
        files_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_rect(4),
        b"Copilot",
        ButtonKind::Ghost,
        false,
        copilot_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_rect(5),
        b"Shell",
        ButtonKind::Ghost,
        false,
        shell_hover,
        false,
        THEME,
    );

    let body_y = (HEADER_H + TOOLBAR_H) as i32;
    let body_h = WIN_H - HEADER_H - TOOLBAR_H - FOOTER_H;
    let editor_panel = Rect::new(0, body_y, WIN_W - INFO_W, body_h);
    let info_panel = Rect::new((WIN_W - INFO_W) as i32, body_y, INFO_W, body_h);

    let editor_rect = draw_panel(&mut canvas, editor_panel, b"Document", THEME);
    let info_rect = draw_panel(&mut canvas, info_panel, b"Inspector", THEME);

    let text_rect = Rect::new(editor_rect.x, editor_rect.y, editor_rect.w, editor_rect.h);
    canvas.fill_rect(
        text_rect.x,
        text_rect.y,
        text_rect.w,
        text_rect.h,
        palette.surface_alt,
    );
    canvas.draw_rect(
        text_rect.x,
        text_rect.y,
        text_rect.w,
        text_rect.h,
        palette.border,
    );
    canvas.fill_rect(
        text_rect.x,
        text_rect.y,
        GUTTER_W,
        text_rect.h,
        palette.chrome,
    );
    canvas.draw_vline(
        text_rect.x + GUTTER_W as i32,
        text_rect.y,
        text_rect.h,
        palette.border,
    );

    let visible = visible_lines();
    let cursor_blink = ((ed.now_ms / 400) % 2) == 0;
    let (cur_line, cur_col) = ed.cursor_pos();

    let mut line_idx = 0usize;
    let mut pos = 0usize;
    let mut vis_row = 0usize;
    while pos <= ed.len {
        let line_start = pos;
        while pos < ed.len && ed.buf[pos] != b'\n' {
            pos += 1;
        }
        let line_end = pos;
        pos += 1;

        if line_idx >= ed.scroll {
            if vis_row >= visible {
                break;
            }
            let ry = text_rect.y + vis_row as i32 * LINE_H as i32;

            if line_idx + 1 == cur_line {
                canvas.fill_rect(
                    text_rect.x + GUTTER_W as i32 + 1,
                    ry,
                    text_rect.w.saturating_sub(GUTTER_W + 2),
                    LINE_H,
                    palette.surface,
                );
            }

            let mut line_label = [0u8; 8];
            let label_len = write_usize(line_idx + 1, &mut line_label);
            let label_w = Canvas::text_width(&line_label[..label_len]);
            canvas.draw_text(
                text_rect.x + GUTTER_W as i32 - label_w as i32 - 4,
                ry,
                &line_label[..label_len],
                palette.text_muted,
                GUTTER_W - 8,
            );

            let text = &ed.buf[line_start..line_end];
            canvas.draw_text(
                text_rect.x + GUTTER_W as i32 + MARGIN as i32,
                ry,
                text,
                palette.text,
                text_rect.w.saturating_sub(GUTTER_W + MARGIN * 2 + 10),
            );

            if line_idx + 1 == cur_line && cursor_blink {
                let col = cur_col.saturating_sub(1).min(text.len());
                let cx = text_rect.x
                    + GUTTER_W as i32
                    + MARGIN as i32
                    + Canvas::text_width(&text[..col]) as i32;
                canvas.fill_rect(cx, ry, 2, LINE_H, palette.primary);
            }

            vis_row += 1;
        }
        line_idx += 1;
    }

    draw_scroll_track(
        &mut canvas,
        Rect::new(
            text_rect.x + text_rect.w as i32 - 8,
            text_rect.y,
            8,
            text_rect.h,
        ),
        (ed.line_count() as u32).saturating_mul(LINE_H),
        text_rect.h,
        (ed.scroll as u32).saturating_mul(LINE_H),
        THEME,
    );

    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y, info_rect.w, 40),
        b"File",
        &ed.fname[..ed.fname_len],
        palette.primary,
        THEME,
    );

    let mut cursor_label = [0u8; 24];
    let mut cursor_len = 0usize;
    cursor_label[cursor_len] = b'L';
    cursor_len += 1;
    cursor_len += write_usize(cur_line, &mut cursor_label[cursor_len..]);
    cursor_label[cursor_len] = b':';
    cursor_len += 1;
    cursor_label[cursor_len] = b'C';
    cursor_len += 1;
    cursor_len += write_usize(cur_col, &mut cursor_label[cursor_len..]);
    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 46, info_rect.w, 40),
        b"Cursor",
        &cursor_label[..cursor_len],
        palette.success,
        THEME,
    );

    let state_label: &[u8] = if ed.modified { b"Unsaved" } else { b"Saved" };
    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 92, info_rect.w, 40),
        b"State",
        state_label,
        palette.warning,
        THEME,
    );

    let mut lines_label = [0u8; 16];
    let lines_len = write_usize(ed.line_count(), &mut lines_label);
    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 138, info_rect.w, 40),
        b"Lines",
        &lines_label[..lines_len],
        palette.text_muted,
        THEME,
    );

    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 184, info_rect.w, 40),
        b"Workspace",
        ed.workspace_scope(),
        palette.primary,
        THEME,
    );

    let mut clock_label = [0u8; 16];
    let clock_len = format_clock(ed.now_ms, &mut clock_label);
    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 230, info_rect.w, 40),
        b"Clock",
        &clock_label[..clock_len],
        palette.success,
        THEME,
    );

    let mut registry_label = [0u8; 24];
    let registry_len = format_registry_label(ed.registry_generation, &mut registry_label);
    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 276, info_rect.w, 40),
        b"Registry",
        &registry_label[..registry_len],
        palette.primary,
        THEME,
    );

    let mut graph_label = [0u8; 24];
    let graph_len = format_graph_label(ed.graph_transitions, ed.graph_epoch, &mut graph_label);
    draw_stat_card(
        &mut canvas,
        Rect::new(info_rect.x, info_rect.y + 322, info_rect.w, 40),
        b"Graph",
        &graph_label[..graph_len],
        palette.warning,
        THEME,
    );

    canvas.draw_text(
        info_rect.x,
        info_rect.y + 378,
        b"Focus Path",
        palette.text_muted,
        info_rect.w,
    );
    canvas.draw_text(
        info_rect.x,
        info_rect.y + 396,
        ed.save_path(),
        palette.text,
        info_rect.w,
    );
    canvas.draw_text(
        info_rect.x,
        info_rect.y + 422,
        b"Context Source",
        palette.text_muted,
        info_rect.w,
    );
    canvas.draw_text(
        info_rect.x,
        info_rect.y + 440,
        ed.context_source(),
        palette.primary,
        info_rect.w,
    );
    canvas.draw_text(
        info_rect.x,
        info_rect.y + 462,
        b"Control Plane",
        palette.text_muted,
        info_rect.w,
    );
    let mut online_label = [0u8; 12];
    let online_len = format_online_label(
        ed.online_control_count(),
        CONTROL_SERVICE_COUNT,
        &mut online_label,
    );
    canvas.draw_text(
        info_rect.x + 92,
        info_rect.y + 462,
        &online_label[..online_len],
        palette.warning,
        info_rect.w.saturating_sub(92),
    );

    let mut idx = 0usize;
    while idx < CONTROL_SERVICE_COUNT {
        let y = info_rect.y + 482 + idx as i32 * 16;
        let color = if ed.service_online[idx] {
            palette.success
        } else {
            palette.warning
        };
        canvas.fill_rect(info_rect.x, y + 4, 6, 6, color);
        canvas.draw_text(
            info_rect.x + 14,
            y,
            CONTROL_SERVICES[idx].0,
            color,
            info_rect.w.saturating_sub(14),
        );
        idx += 1;
    }

    let footer_y = (WIN_H - FOOTER_H) as i32;
    canvas.fill_rect(0, footer_y, WIN_W, FOOTER_H, palette.chrome);
    canvas.draw_hline(0, footer_y, WIN_W, palette.border);
    canvas.draw_text(
        10,
        footer_y + 7,
        &ed.status[..ed.status_len],
        palette.text,
        WIN_W / 2,
    );
    canvas.draw_text(
        WIN_W as i32 - 334,
        footer_y + 7,
        b"/graph authoring  toolbar: Files / Copilot / Shell",
        palette.text_muted,
        324,
    );

    win.present();
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[editor] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut ed = Editor::new();
    draw(&mut win, &ed);
    win.request_focus();

    loop {
        match win.poll_event() {
            Event::PointerMove { x, y, buttons } => {
                if ed.handle_pointer(x, y, buttons) {
                    draw(&mut win, &ed);
                }
            }
            Event::FrameTick { now_ms } => {
                let old_phase = ((ed.now_ms / 400) % 2) == 0;
                let new_phase = ((now_ms / 400) % 2) == 0;
                let refresh_due =
                    ed.last_refresh_ms == 0 || now_ms.saturating_sub(ed.last_refresh_ms) >= 1000;
                ed.now_ms = now_ms;
                if refresh_due {
                    ed.refresh_runtime();
                    ed.last_refresh_ms = now_ms;
                }
                if old_phase != new_phase || refresh_due {
                    draw(&mut win, &ed);
                }
            }
            Event::Key {
                pressed: true,
                ascii,
                hid_usage,
            } => {
                let dirty = match ascii {
                    0x08 => {
                        ed.backspace();
                        true
                    }
                    0x0D | 0x0A => {
                        ed.insert(b'\n');
                        true
                    }
                    0x13 => {
                        ed.save_scratch();
                        true
                    }
                    0x20..=0x7E => {
                        ed.insert(ascii);
                        true
                    }
                    0x1B => runtime::exit(0),
                    _ => match hid_usage {
                        0x4F => {
                            ed.move_right();
                            true
                        }
                        0x50 => {
                            ed.move_left();
                            true
                        }
                        0x51 => {
                            ed.move_down();
                            true
                        }
                        0x52 => {
                            ed.move_up();
                            true
                        }
                        _ => false,
                    },
                };
                if dirty {
                    ed.adjust_scroll(visible_lines());
                    draw(&mut win, &ed);
                }
            }
            Event::None => runtime::yield_now(),
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    runtime::write_line(b"[editor] panic\n");
    runtime::exit(255)
}
