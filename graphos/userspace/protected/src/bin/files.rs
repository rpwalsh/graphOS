// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! files - GraphOS file browser.
//!
//! Modern ring-3 shell surface with:
//! - places sidebar for the mounted GraphOS namespaces
//! - breadcrumb navigation and toolbar actions
//! - keyboard and pointer navigation
//! - launch flow for executable entries
//! - inline preview for text and binary files

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
        ButtonKind, draw_breadcrumb, draw_button, draw_list_row, draw_panel, draw_scroll_track,
        draw_sidebar_item, draw_stat_card, draw_window_frame,
    },
};

const WIN_W: u32 = 940;
const WIN_H: u32 = 580;
const THEME: Theme = Theme::DarkGlass;

const HEADER_H: u32 = 32;
const BREADCRUMB_H: u32 = 28;
const TOOLBAR_H: u32 = 38;
const STATUS_H: u32 = 28;
const SIDEBAR_W: u32 = 188;
const PREVIEW_W: u32 = 260;
const ROW_H: u32 = 28;
const MAX_ENTRIES: usize = 96;
const ENTRY_NAME_CAP: usize = 64;
const PREVIEW_CAP: usize = 768;
const MAX_PATH: usize = 64;
const MAX_SEGMENTS: usize = 8;
const ROWS_VISIBLE: usize =
    ((WIN_H - HEADER_H - BREADCRUMB_H - TOOLBAR_H - STATUS_H - 28) / ROW_H) as usize;

const PLACE_GRAPH: usize = 1;
const CONTROL_SERVICE_COUNT: usize = 4;

const PLACES: [(&[u8], &[u8], u32); 5] = [
    (b"Root", b"/", 0xFF58A6FF),
    (b"Graph", b"/graph", 0xFF39C5BB),
    (b"Boot", b"/boot", 0xFFD29922),
    (b"Data", b"/data", 0xFFF78166),
    (b"Temp", b"/tmp", 0xFF3FB950),
];

const CONTROL_SERVICES: [(&[u8], &[u8], u32); CONTROL_SERVICE_COUNT] = [
    (b"Graph", b"graphd", 0xFF39C5BB),
    (b"Copilot", b"ai-console", 0xFF58A6FF),
    (b"Studio", b"editor", 0xFF7B68EE),
    (b"Shell", b"terminal", 0xFF28C940),
];

#[derive(Clone, Copy)]
struct Path {
    data: [u8; MAX_PATH],
    len: usize,
}

impl Path {
    const fn root() -> Self {
        let mut data = [0u8; MAX_PATH];
        data[0] = b'/';
        Self { data, len: 1 }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut path = Self::root();
        let max_len = bytes.len().min(MAX_PATH);
        if max_len == 0 {
            return path;
        }
        path.data[..max_len].copy_from_slice(&bytes[..max_len]);
        path.len = max_len;
        if path.data[0] != b'/' {
            path.data[0] = b'/';
            path.len = 1;
        }
        path
    }

    fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }

    fn is_root(&self) -> bool {
        self.len <= 1
    }

    fn join(&self, name: &[u8]) -> Self {
        let mut next = *self;
        if next.len == 0 {
            next.data[0] = b'/';
            next.len = 1;
        }
        if next.data[next.len - 1] != b'/' && next.len < MAX_PATH {
            next.data[next.len] = b'/';
            next.len += 1;
        }
        let rem = MAX_PATH.saturating_sub(next.len);
        let take = rem.min(name.len());
        if take > 0 {
            next.data[next.len..next.len + take].copy_from_slice(&name[..take]);
            next.len += take;
        }
        next
    }

    fn parent(&self) -> Self {
        if self.is_root() {
            return Self::root();
        }
        let mut end = self.len.saturating_sub(1);
        while end > 1 && self.data[end] != b'/' {
            end -= 1;
        }
        Self::from_bytes(&self.data[..end])
    }

    fn is_under(&self, prefix: &[u8]) -> bool {
        if prefix == b"/" {
            return true;
        }
        let path = self.as_bytes();
        if path.len() < prefix.len() || &path[..prefix.len()] != prefix {
            return false;
        }
        path.len() == prefix.len() || path[prefix.len()] == b'/'
    }

    fn breadcrumb_segments<'a>(&'a self, out: &mut [&'a [u8]; MAX_SEGMENTS]) -> usize {
        out[0] = b"root";
        if self.is_root() {
            return 1;
        }
        let mut count = 1usize;
        let mut start = 1usize;
        while start < self.len && count < MAX_SEGMENTS {
            let mut end = start;
            while end < self.len && self.data[end] != b'/' {
                end += 1;
            }
            if end > start {
                out[count] = &self.data[start..end];
                count += 1;
            }
            start = end + 1;
        }
        count
    }
}

#[derive(Clone, Copy)]
struct Entry {
    name: [u8; ENTRY_NAME_CAP],
    name_len: usize,
    is_dir: bool,
}

impl Entry {
    const fn empty() -> Self {
        Self {
            name: [0u8; ENTRY_NAME_CAP],
            name_len: 0,
            is_dir: false,
        }
    }

    fn name_bytes(&self) -> &[u8] {
        &self.name[..self.name_len]
    }

    fn set(&mut self, bytes: &[u8], is_dir: bool) {
        self.name = [0u8; ENTRY_NAME_CAP];
        let len = bytes.len().min(ENTRY_NAME_CAP);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name_len = len;
        self.is_dir = is_dir;
    }

    fn is_parent(&self) -> bool {
        self.name_len == 2 && self.name[0] == b'.' && self.name[1] == b'.'
    }
}

struct State {
    path: Path,
    entries: [Entry; MAX_ENTRIES],
    count: usize,
    scroll: usize,
    selected: Option<usize>,
    hover: Option<usize>,
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
    sidebar_hover: Option<usize>,
    status: [u8; 96],
    status_len: usize,
    preview: [u8; PREVIEW_CAP],
    preview_len: usize,
    preview_is_text: bool,
    now_ms: u64,
    last_refresh_ms: u64,
    registry_generation: u64,
    graph_transitions: u32,
    graph_epoch: u32,
    service_online: [bool; CONTROL_SERVICE_COUNT],
    last_click_idx: Option<usize>,
    last_click_ms: u64,
}

impl State {
    fn new() -> Self {
        Self {
            path: Path::root(),
            entries: core::array::from_fn(|_| Entry::empty()),
            count: 0,
            scroll: 0,
            selected: None,
            hover: None,
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
            sidebar_hover: Some(PLACE_GRAPH),
            status: [0u8; 96],
            status_len: 0,
            preview: [0u8; PREVIEW_CAP],
            preview_len: 0,
            preview_is_text: true,
            now_ms: 0,
            last_refresh_ms: 0,
            registry_generation: 0,
            graph_transitions: 0,
            graph_epoch: 0,
            service_online: [false; CONTROL_SERVICE_COUNT],
            last_click_idx: None,
            last_click_ms: 0,
        }
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn set_count_status(&mut self) {
        let mut buf = [0u8; 24];
        let mut len = write_usize(self.count, &mut buf);
        let tail: &[u8] = if self.count == 1 { b" item" } else { b" items" };
        buf[len..len + tail.len()].copy_from_slice(tail);
        len += tail.len();
        self.set_status(&buf[..len]);
    }

    fn entry_name(&self, idx: usize) -> &[u8] {
        self.entries[idx].name_bytes()
    }

    fn is_executable(&self, idx: usize) -> bool {
        self.entry_name(idx).ends_with(b".elf")
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|idx| self.entries.get(idx))
    }

    fn load_dir(&mut self, path: Path) {
        self.path = path;
        self.count = 0;
        self.scroll = 0;
        self.selected = None;
        self.hover = None;

        if !self.path.is_root() && self.count < MAX_ENTRIES {
            self.entries[self.count].set(b"..", true);
            self.count += 1;
        }

        let fd = runtime::vfs_open(self.path.as_bytes());
        if fd == u64::MAX {
            self.preview_message(b"Directory unavailable.");
            self.set_status(b"directory open failed");
            return;
        }

        let mut buf = [0u8; 1536];
        let bytes = runtime::vfs_read(fd, &mut buf) as usize;
        runtime::vfs_close(fd);

        if bytes == 0 {
            self.preview_message(b"This directory is empty.");
            self.set_count_status();
            self.sync_workspace_context();
            return;
        }

        let sort_start = self.count;
        let mut cursor = 0usize;
        while cursor < bytes && self.count < MAX_ENTRIES {
            let start = cursor;
            while cursor < bytes && buf[cursor] != b'\n' && buf[cursor] != 0 {
                cursor += 1;
            }
            let raw = &buf[start..cursor];
            if !raw.is_empty() {
                let is_dir = raw[0] == b'd';
                let name = if is_dir && raw.len() > 1 {
                    &raw[1..]
                } else {
                    raw
                };
                if !name.is_empty() {
                    self.entries[self.count].set(name, is_dir);
                    self.count += 1;
                }
            }
            cursor += 1;
        }

        self.sort_entries(sort_start);
        if self.count > 0 {
            self.select(0);
        } else {
            self.preview_message(b"This directory is empty.");
        }
        self.set_count_status();
        self.sync_workspace_context();
    }

    fn sort_entries(&mut self, start: usize) {
        let mut i = start + 1;
        while i < self.count {
            let key = self.entries[i];
            let mut j = i;
            while j > start && entry_cmp(&key, &self.entries[j - 1]) < 0 {
                self.entries[j] = self.entries[j - 1];
                j -= 1;
            }
            self.entries[j] = key;
            i += 1;
        }
    }

    fn select(&mut self, idx: usize) {
        if idx >= self.count {
            return;
        }
        self.selected = Some(idx);
        self.ensure_visible();
        self.refresh_preview();
        self.sync_workspace_context();
    }

    fn ensure_visible(&mut self) {
        if let Some(idx) = self.selected {
            if idx < self.scroll {
                self.scroll = idx;
            } else if idx >= self.scroll + ROWS_VISIBLE {
                self.scroll = idx + 1 - ROWS_VISIBLE;
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.count == 0 {
            return;
        }
        let current = self.selected.unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, self.count.saturating_sub(1) as i32) as usize;
        self.select(next);
    }

    fn activate_selected(&mut self) {
        let Some(idx) = self.selected else {
            return;
        };
        let entry = self.entries[idx];
        if entry.is_parent() {
            self.load_dir(self.path.parent());
            return;
        }
        if entry.is_dir {
            self.load_dir(self.path.join(entry.name_bytes()));
            return;
        }
        if self.try_launch(idx) {
            return;
        }
        self.refresh_preview();
        self.set_status(b"Previewing file.");
    }

    fn try_launch(&mut self, idx: usize) -> bool {
        let name = self.entry_name(idx);
        let launched = if self.is_executable(idx) {
            runtime::spawn_named_checked(&name[..name.len().saturating_sub(4)])
        } else {
            runtime::spawn_named_checked(name)
        };
        if launched {
            self.set_status(b"Launching application...");
        }
        launched
    }

    fn preview_message(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.preview.len());
        self.preview[..len].copy_from_slice(&msg[..len]);
        self.preview_len = len;
        self.preview_is_text = true;
    }

    fn refresh_preview(&mut self) {
        let Some(entry) = self.selected_entry() else {
            self.preview_message(b"Select an item to inspect it.");
            return;
        };
        if entry.is_parent() {
            self.preview_message(b"Navigate back to the parent location.");
            return;
        }
        if entry.is_dir {
            self.preview_message(b"Directory selected. Press Enter or double-click to open.");
            return;
        }
        let path = self.path.join(entry.name_bytes());
        let fd = runtime::vfs_open(path.as_bytes());
        if fd == u64::MAX {
            self.preview_message(b"Preview unavailable for this file.");
            return;
        }
        let read = runtime::vfs_read(fd, &mut self.preview) as usize;
        runtime::vfs_close(fd);
        if read == 0 {
            self.preview_message(b"Empty file.");
            return;
        }
        self.preview_len = read.min(self.preview.len());
        self.preview_is_text = preview_is_text(&self.preview[..self.preview_len]);
    }

    fn context_focus_path(&self) -> Path {
        let Some(entry) = self.selected_entry() else {
            return self.path;
        };
        if entry.is_parent() {
            self.path
        } else {
            self.path.join(entry.name_bytes())
        }
    }

    fn context_focus_is_dir(&self) -> bool {
        self.selected_entry()
            .map(|entry| entry.is_parent() || entry.is_dir)
            .unwrap_or(true)
    }

    fn sync_workspace_context(&self) {
        let focus = self.context_focus_path();
        let _ = workspace_context::write(
            self.path.as_bytes(),
            focus.as_bytes(),
            b"files",
            self.context_focus_is_dir(),
        );
    }

    fn list_row_at(&self, x: i16, y: i16) -> Option<usize> {
        let (list_rect, _) = content_panels();
        if !contains(list_rect, x, y) {
            return None;
        }
        let row = ((y as i32 - list_rect.y) as u32 / ROW_H) as usize;
        let idx = self.scroll + row;
        if idx < self.count { Some(idx) } else { None }
    }

    fn sidebar_place_at(&self, x: i16, y: i16) -> Option<usize> {
        let body = Rect::new(0, HEADER_H as i32, WIN_W, WIN_H - HEADER_H - STATUS_H);
        let sidebar = Rect::new(body.x, body.y, SIDEBAR_W, body.h);
        if !contains(sidebar, x, y) {
            return None;
        }
        let top = sidebar.y + 96;
        if (y as i32) < top {
            return None;
        }
        let row = ((y as i32 - top) / 32) as usize;
        if row < PLACES.len() { Some(row) } else { None }
    }

    fn handle_pointer(&mut self, x: i16, y: i16, buttons: u8) -> bool {
        self.pointer_x = x;
        self.pointer_y = y;
        let mut dirty = false;

        let new_hover = self.list_row_at(x, y);
        if new_hover != self.hover {
            self.hover = new_hover;
            dirty = true;
        }

        let new_sidebar = self.sidebar_place_at(x, y);
        if new_sidebar != self.sidebar_hover {
            self.sidebar_hover = new_sidebar;
            dirty = true;
        }

        let left_down = buttons & 1 != 0;
        let left_prev = self.prev_buttons & 1 != 0;
        if left_down && !left_prev {
            if handle_toolbar_click(self, x, y) {
                dirty = true;
            } else if let Some(place) = new_sidebar {
                self.load_dir(Path::from_bytes(PLACES[place].1));
                dirty = true;
            } else if let Some(idx) = new_hover {
                let double_click = self.last_click_idx == Some(idx)
                    && self.now_ms.saturating_sub(self.last_click_ms) < 360;
                self.select(idx);
                if double_click {
                    self.activate_selected();
                }
                self.last_click_idx = Some(idx);
                self.last_click_ms = self.now_ms;
                dirty = true;
            }
        }

        self.prev_buttons = buttons;
        dirty
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
        if runtime::spawn_named_checked(name) {
            self.set_status(ok);
        } else {
            self.set_status(fail);
        }
    }
}

fn preview_is_text(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .take(256)
        .all(|&b| b == b'\n' || b == b'\r' || b == b'\t' || (0x20..=0x7E).contains(&b))
}

fn entry_cmp(a: &Entry, b: &Entry) -> i32 {
    if a.is_dir != b.is_dir {
        return if a.is_dir { -1 } else { 1 };
    }
    let al = a.name_len.min(b.name_len);
    let mut i = 0usize;
    while i < al {
        let ac = a.name[i].to_ascii_lowercase();
        let bc = b.name[i].to_ascii_lowercase();
        if ac != bc {
            return if ac < bc { -1 } else { 1 };
        }
        i += 1;
    }
    if a.name_len == b.name_len {
        0
    } else if a.name_len < b.name_len {
        -1
    } else {
        1
    }
}

fn contains(rect: Rect, x: i16, y: i16) -> bool {
    let x = x as i32;
    let y = y as i32;
    x >= rect.x && y >= rect.y && x < rect.x + rect.w as i32 && y < rect.y + rect.h as i32
}

fn toolbar_button_rect(index: usize) -> Rect {
    let origin_x = SIDEBAR_W as i32 + 10;
    let origin_y = (HEADER_H + BREADCRUMB_H + 4) as i32;
    Rect::new(origin_x + index as i32 * 86, origin_y, 80, 28)
}

fn handle_toolbar_click(state: &mut State, x: i16, y: i16) -> bool {
    if contains(toolbar_button_rect(0), x, y) {
        state.load_dir(state.path.parent());
        return true;
    }
    if contains(toolbar_button_rect(1), x, y) {
        state.activate_selected();
        return true;
    }
    if contains(toolbar_button_rect(2), x, y) {
        state.launch_surface(b"editor", b"Studio opened.", b"Studio unavailable.");
        return true;
    }
    if contains(toolbar_button_rect(3), x, y) {
        state.launch_surface(b"ai-console", b"Copilot opened.", b"Copilot unavailable.");
        return true;
    }
    if contains(toolbar_button_rect(4), x, y) {
        state.load_dir(state.path);
        return true;
    }
    false
}

fn content_panels() -> (Rect, Rect) {
    let top = (HEADER_H + BREADCRUMB_H + TOOLBAR_H) as i32;
    let height = WIN_H - HEADER_H - BREADCRUMB_H - TOOLBAR_H - STATUS_H;
    let list_w = WIN_W - SIDEBAR_W - PREVIEW_W - 12;
    (
        Rect::new(SIDEBAR_W as i32, top, list_w, height),
        Rect::new((SIDEBAR_W + list_w + 12) as i32, top, PREVIEW_W, height),
    )
}

fn draw(win: &mut Window, state: &State) {
    let palette = tokens(THEME);
    let mut canvas = win.canvas();
    canvas.clear(palette.background);

    let root = Rect::new(0, 0, WIN_W, WIN_H);
    draw_window_frame(&mut canvas, root, b"GraphOS Files", THEME);

    let body = Rect::new(0, HEADER_H as i32, WIN_W, WIN_H - HEADER_H - STATUS_H);
    let sidebar = Rect::new(body.x, body.y, SIDEBAR_W, body.h);
    let right = Rect::new(
        body.x + SIDEBAR_W as i32,
        body.y,
        body.w - SIDEBAR_W,
        body.h,
    );
    canvas.fill_rect(sidebar.x, sidebar.y, sidebar.w, sidebar.h, palette.surface);
    canvas.draw_vline(
        sidebar.x + sidebar.w as i32 - 1,
        sidebar.y,
        sidebar.h,
        palette.border,
    );

    draw_stat_card(
        &mut canvas,
        Rect::new(sidebar.x + 10, sidebar.y + 10, sidebar.w - 20, 42),
        b"Scope",
        current_scope_label(state.path.as_bytes()),
        palette.primary,
        THEME,
    );
    draw_stat_card(
        &mut canvas,
        Rect::new(sidebar.x + 10, sidebar.y + 58, sidebar.w - 20, 42),
        b"Selection",
        selection_kind(state),
        palette.success,
        THEME,
    );

    let mut place = 0usize;
    while place < PLACES.len() {
        let rect = Rect::new(
            sidebar.x + 8,
            sidebar.y + 96 + place as i32 * 32,
            sidebar.w - 16,
            28,
        );
        draw_sidebar_item(
            &mut canvas,
            rect,
            PLACES[place].0,
            PLACES[place].2,
            state.path.is_under(PLACES[place].1),
            state.sidebar_hover == Some(place),
            0,
            THEME,
        );
        place += 1;
    }

    let mut online_label = [0u8; 12];
    let mut online_len = write_usize(state.online_control_count(), &mut online_label);
    online_label[online_len] = b'/';
    online_len += 1;
    online_len += write_usize(CONTROL_SERVICE_COUNT, &mut online_label[online_len..]);
    let control_top = sidebar.y + sidebar.h as i32 - 122;
    draw_stat_card(
        &mut canvas,
        Rect::new(sidebar.x + 10, control_top, sidebar.w - 20, 40),
        b"Control",
        &online_label[..online_len],
        palette.warning,
        THEME,
    );

    let mut service = 0usize;
    while service < CONTROL_SERVICE_COUNT {
        let row_y = control_top + 50 + service as i32 * 16;
        let color = if state.service_online[service] {
            palette.success
        } else {
            palette.warning
        };
        canvas.fill_rect(sidebar.x + 14, row_y + 4, 6, 6, color);
        canvas.draw_text(
            sidebar.x + 26,
            row_y,
            CONTROL_SERVICES[service].0,
            color,
            sidebar.w - 36,
        );
        service += 1;
    }

    let mut epoch_label = [0u8; 24];
    let epoch_len =
        format_graph_label(state.graph_transitions, state.graph_epoch, &mut epoch_label);
    canvas.draw_text(
        sidebar.x + 10,
        sidebar.y + sidebar.h as i32 - 12,
        &epoch_label[..epoch_len],
        palette.text_muted,
        sidebar.w - 20,
    );

    let mut crumbs = [b"" as &[u8]; MAX_SEGMENTS];
    let crumb_count = state.path.breadcrumb_segments(&mut crumbs);
    draw_breadcrumb(
        &mut canvas,
        Rect::new(right.x, right.y, right.w, BREADCRUMB_H),
        &crumbs[..crumb_count],
        THEME,
    );

    let back_hover = contains(toolbar_button_rect(0), state.pointer_x, state.pointer_y);
    let open_hover = contains(toolbar_button_rect(1), state.pointer_x, state.pointer_y);
    let studio_hover = contains(toolbar_button_rect(2), state.pointer_x, state.pointer_y);
    let copilot_hover = contains(toolbar_button_rect(3), state.pointer_x, state.pointer_y);
    let refresh_hover = contains(toolbar_button_rect(4), state.pointer_x, state.pointer_y);

    draw_button(
        &mut canvas,
        toolbar_button_rect(0),
        b"Back",
        ButtonKind::Secondary,
        false,
        back_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_button_rect(1),
        b"Open",
        ButtonKind::Primary,
        false,
        open_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_button_rect(2),
        b"Studio",
        ButtonKind::Secondary,
        false,
        studio_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_button_rect(3),
        b"Copilot",
        ButtonKind::Ghost,
        false,
        copilot_hover,
        false,
        THEME,
    );
    draw_button(
        &mut canvas,
        toolbar_button_rect(4),
        b"Refresh",
        ButtonKind::Ghost,
        false,
        refresh_hover,
        false,
        THEME,
    );

    let (list_panel, preview_panel) = content_panels();
    let list_rect = draw_panel(&mut canvas, list_panel, b"Directory", THEME);
    let preview_rect = draw_panel(&mut canvas, preview_panel, b"Preview", THEME);

    let mut row = 0usize;
    while row < ROWS_VISIBLE {
        let idx = state.scroll + row;
        if idx >= state.count {
            break;
        }
        let entry = &state.entries[idx];
        let y = list_rect.y + row as i32 * ROW_H as i32;
        let row_rect = Rect::new(list_rect.x, y, list_rect.w.saturating_sub(12), ROW_H);
        let (meta, icon) = if entry.is_parent() {
            (b"up".as_slice(), palette.warning)
        } else if entry.is_dir {
            (b"folder".as_slice(), palette.primary)
        } else if state.is_executable(idx) {
            (b"app".as_slice(), palette.success)
        } else {
            (b"file".as_slice(), palette.text_muted)
        };
        draw_list_row(
            &mut canvas,
            row_rect,
            entry.name_bytes(),
            meta,
            icon,
            state.selected == Some(idx),
            state.hover == Some(idx),
            THEME,
        );
        row += 1;
    }

    draw_scroll_track(
        &mut canvas,
        Rect::new(
            list_rect.x + list_rect.w as i32 - 10,
            list_rect.y,
            8,
            list_rect.h,
        ),
        (state.count as u32).saturating_mul(ROW_H),
        list_rect.h,
        (state.scroll as u32).saturating_mul(ROW_H),
        THEME,
    );

    render_preview(&mut canvas, preview_rect, state, palette.primary);

    let footer_y = (WIN_H - STATUS_H) as i32;
    canvas.fill_rect(0, footer_y, WIN_W, STATUS_H, palette.chrome);
    canvas.draw_hline(0, footer_y, WIN_W, palette.border);
    canvas.draw_text(
        10,
        footer_y + 7,
        &state.status[..state.status_len],
        palette.text,
        WIN_W / 2,
    );
    canvas.draw_text(
        WIN_W as i32 - 320,
        footer_y + 7,
        b"Enter opens  E studio  A copilot  T terminal  Backspace up",
        palette.text_muted,
        310,
    );

    win.present();
}

fn render_preview(canvas: &mut Canvas<'_>, rect: Rect, state: &State, accent: u32) {
    draw_stat_card(
        canvas,
        Rect::new(rect.x, rect.y, rect.w, 40),
        b"Target",
        selected_name(state),
        accent,
        THEME,
    );
    draw_stat_card(
        canvas,
        Rect::new(rect.x, rect.y + 46, rect.w, 40),
        b"Mode",
        selection_kind(state),
        tokens(THEME).success,
        THEME,
    );

    let mut clock_label = [0u8; 16];
    let clock_len = format_clock(state.now_ms, &mut clock_label);
    draw_stat_card(
        canvas,
        Rect::new(rect.x, rect.y + 92, rect.w, 40),
        b"Clock",
        &clock_label[..clock_len],
        tokens(THEME).primary,
        THEME,
    );

    let mut graph_label = [0u8; 24];
    let graph_len =
        format_graph_label(state.graph_transitions, state.graph_epoch, &mut graph_label);
    draw_stat_card(
        canvas,
        Rect::new(rect.x, rect.y + 138, rect.w, 40),
        b"Graph",
        &graph_label[..graph_len],
        tokens(THEME).warning,
        THEME,
    );

    let text_rect = Rect::new(rect.x, rect.y + 188, rect.w, rect.h.saturating_sub(188));
    canvas.fill_rect(
        text_rect.x,
        text_rect.y,
        text_rect.w,
        text_rect.h,
        tokens(THEME).surface_alt,
    );
    canvas.draw_rect(
        text_rect.x,
        text_rect.y,
        text_rect.w,
        text_rect.h,
        tokens(THEME).border,
    );

    if state.preview_is_text {
        let mut row = 0usize;
        let mut start = 0usize;
        while start <= state.preview_len && row < 16 {
            let mut end = start;
            while end < state.preview_len && state.preview[end] != b'\n' {
                end += 1;
            }
            if end > start || start == state.preview_len {
                canvas.draw_text(
                    text_rect.x + 8,
                    text_rect.y + 8 + row as i32 * 14,
                    &state.preview[start..end],
                    tokens(THEME).text,
                    text_rect.w.saturating_sub(16),
                );
                row += 1;
            }
            if end >= state.preview_len {
                break;
            }
            start = end + 1;
        }
    } else {
        let mut row = 0usize;
        let mut offset = 0usize;
        while offset < state.preview_len && row < 8 {
            let line_len = (state.preview_len - offset).min(8);
            let mut line = [b' '; 48];
            let mut pos = 0usize;
            pos += write_hex_u16(offset as u16, &mut line[pos..]);
            line[pos] = b':';
            pos += 1;
            line[pos] = b' ';
            pos += 1;
            let mut i = 0usize;
            while i < line_len {
                let byte = state.preview[offset + i];
                line[pos] = nybble(byte >> 4);
                line[pos + 1] = nybble(byte & 0x0F);
                line[pos + 2] = b' ';
                pos += 3;
                i += 1;
            }
            canvas.draw_text(
                text_rect.x + 8,
                text_rect.y + 8 + row as i32 * 14,
                &line[..pos],
                tokens(THEME).text,
                text_rect.w.saturating_sub(16),
            );
            offset += line_len;
            row += 1;
        }
    }
}

fn selected_name(state: &State) -> &[u8] {
    state
        .selected_entry()
        .map(|entry| {
            if entry.is_parent() {
                b"..".as_slice()
            } else {
                entry.name_bytes()
            }
        })
        .unwrap_or(b"Nothing selected")
}

fn selection_kind(state: &State) -> &[u8] {
    let Some(entry) = state.selected_entry() else {
        return b"Idle";
    };
    if entry.is_parent() {
        b"Parent directory"
    } else if entry.is_dir {
        b"Folder"
    } else if entry.name_bytes().ends_with(b".elf") {
        b"Launchable app"
    } else if state.preview_is_text {
        b"Document"
    } else {
        b"Binary"
    }
}

fn current_scope_label(path: &[u8]) -> &[u8] {
    if path.starts_with(b"/graph") {
        b"Graph namespace"
    } else if path.starts_with(b"/boot") {
        b"Boot assets"
    } else if path.starts_with(b"/data") {
        b"Persistent data"
    } else if path.starts_with(b"/tmp") {
        b"Temporary space"
    } else {
        b"System root"
    }
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

fn nybble(value: u8) -> u8 {
    match value & 0x0F {
        0..=9 => b'0' + (value & 0x0F),
        v => b'A' + (v - 10),
    }
}

fn write_hex_u16(value: u16, out: &mut [u8]) -> usize {
    let bytes = value.to_be_bytes();
    out[0] = nybble(bytes[0] >> 4);
    out[1] = nybble(bytes[0]);
    out[2] = nybble(bytes[1] >> 4);
    out[3] = nybble(bytes[1]);
    4
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[files] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => runtime::exit(1),
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => runtime::exit(2),
    };

    let mut state = State::new();
    state.load_dir(Path::from_bytes(b"/graph"));
    state.refresh_runtime();
    draw(&mut win, &state);
    win.request_focus();

    loop {
        match win.poll_event() {
            Event::PointerMove { x, y, buttons } => {
                if state.handle_pointer(x, y, buttons) {
                    draw(&mut win, &state);
                }
            }
            Event::FrameTick { now_ms } => {
                let refresh_due = state.last_refresh_ms == 0
                    || now_ms.saturating_sub(state.last_refresh_ms) >= 1000;
                state.now_ms = now_ms;
                if refresh_due {
                    state.refresh_runtime();
                    state.last_refresh_ms = now_ms;
                    draw(&mut win, &state);
                }
            }
            Event::Key {
                pressed: true,
                ascii,
                hid_usage,
            } => {
                let dirty = match ascii {
                    b'q' | 0x1B => runtime::exit(0),
                    0x08 => {
                        state.load_dir(state.path.parent());
                        true
                    }
                    0x0D | 0x0A => {
                        state.activate_selected();
                        true
                    }
                    b'r' => {
                        state.load_dir(state.path);
                        true
                    }
                    b'g' => {
                        state.load_dir(Path::from_bytes(b"/graph"));
                        true
                    }
                    b'e' => {
                        state.launch_surface(b"editor", b"Studio opened.", b"Studio unavailable.");
                        true
                    }
                    b'a' => {
                        state.launch_surface(
                            b"ai-console",
                            b"Copilot opened.",
                            b"Copilot unavailable.",
                        );
                        true
                    }
                    b't' => {
                        state.launch_surface(
                            b"terminal",
                            b"Terminal opened.",
                            b"Terminal unavailable.",
                        );
                        true
                    }
                    _ => match hid_usage {
                        0x51 => {
                            state.move_selection(1);
                            true
                        }
                        0x52 => {
                            state.move_selection(-1);
                            true
                        }
                        0x50 => {
                            state.load_dir(state.path.parent());
                            true
                        }
                        0x4F => {
                            state.activate_selected();
                            true
                        }
                        _ => false,
                    },
                };
                if dirty {
                    draw(&mut win, &state);
                }
            }
            Event::None => runtime::yield_now(),
            _ => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    runtime::write_line(b"[files] panic\n");
    runtime::exit(255)
}
