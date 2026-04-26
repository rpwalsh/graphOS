// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ai-console -- GraphOS graph-first AI cockpit.
//!
//! The console is orchestrator-driven, graph-aware, and centered on
//! provenance-first AI operation:
//!   - Synthesis driven by SCCE concepts
//!   - Evidence and concept graph surfaces
//!   - Walsh temporal-walk telemetry
//!   - Direct launch surfaces for the rest of the desktop

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;
#[path = "../workspace_context.rs"]
mod workspace_context;

use core::fmt::{self, Write};
use core::panic::PanicInfo;
use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::geom::Rect;
use graphos_ui_sdk::native_views::{
    GraphEdge, GraphNode, TimelinePoint, draw_graph_view, draw_timeline_view,
};
use graphos_ui_sdk::tokens::{Theme, tokens};
use graphos_ui_sdk::widgets::{self, ButtonKind};

const WIN_W: u32 = 920;
const WIN_H: u32 = 600;
const CMD_MAX: usize = 96;
const LOG_CAP: usize = 8;
const LOG_LEN: usize = 96;
const TAB_COUNT: usize = 5;
const ACTION_COUNT: usize = 5;
const SERVICE_COUNT: usize = 6;

const THEME: Theme = Theme::DarkGlass;

struct ActionDef {
    label: &'static [u8],
    meta: &'static [u8],
    spawn: &'static [u8],
    accent: u32,
}

const ACTIONS: [ActionDef; ACTION_COUNT] = [
    ActionDef {
        label: b"Files",
        meta: b"Context vault and graph paths",
        spawn: b"files",
        accent: 0xFF58A6FF,
    },
    ActionDef {
        label: b"Studio",
        meta: b"Specs, editor, and build surfaces",
        spawn: b"editor",
        accent: 0xFF7B68EE,
    },
    ActionDef {
        label: b"Terminal",
        meta: b"Recovery, tools, and bash lane",
        spawn: b"terminal",
        accent: 0xFF28C940,
    },
    ActionDef {
        label: b"Settings",
        meta: b"Orchestrator and graph controls",
        spawn: b"settings",
        accent: 0xFFFFA75A,
    },
    ActionDef {
        label: b"Shell",
        meta: b"Direct 3D shell cube path",
        spawn: b"cube",
        accent: 0xFF39C5BB,
    },
];

const TABS: [&[u8]; TAB_COUNT] = [
    b"Synthesize",
    b"Evidence",
    b"Concepts",
    b"Walsh",
    b"Actions",
];

const SERVICE_NAMES: [&[u8]; SERVICE_COUNT] = [
    b"graphd",
    b"servicemgr",
    b"modeld",
    b"artifactsd",
    b"compositor",
    b"terminal",
];

const GRAPH_NODES: [GraphNode; 8] = [
    GraphNode {
        label: b"orch",
        x: 18,
        y: 34,
        color: 0xFF58A6FF,
    },
    GraphNode {
        label: b"query",
        x: 96,
        y: 16,
        color: 0xFF39C5BB,
    },
    GraphNode {
        label: b"evidence",
        x: 162,
        y: 46,
        color: 0xFFFFA75A,
    },
    GraphNode {
        label: b"concept",
        x: 232,
        y: 18,
        color: 0xFF7B68EE,
    },
    GraphNode {
        label: b"memory",
        x: 288,
        y: 72,
        color: 0xFF8BD8FF,
    },
    GraphNode {
        label: b"prove",
        x: 182,
        y: 116,
        color: 0xFF8DE0AC,
    },
    GraphNode {
        label: b"walsh",
        x: 92,
        y: 120,
        color: 0xFFFFC47A,
    },
    GraphNode {
        label: b"action",
        x: 288,
        y: 124,
        color: 0xFF28C940,
    },
];

const GRAPH_EDGES: [GraphEdge; 10] = [
    GraphEdge { from: 0, to: 1 },
    GraphEdge { from: 1, to: 2 },
    GraphEdge { from: 2, to: 3 },
    GraphEdge { from: 3, to: 4 },
    GraphEdge { from: 2, to: 5 },
    GraphEdge { from: 5, to: 7 },
    GraphEdge { from: 6, to: 2 },
    GraphEdge { from: 6, to: 3 },
    GraphEdge { from: 0, to: 6 },
    GraphEdge { from: 4, to: 7 },
];

const REASONING_TIMELINE: [TimelinePoint; 5] = [
    TimelinePoint {
        label: b"observe",
        offset_fp: 0,
        complete: true,
    },
    TimelinePoint {
        label: b"retrieve",
        offset_fp: 250,
        complete: true,
    },
    TimelinePoint {
        label: b"synthesize",
        offset_fp: 500,
        complete: true,
    },
    TimelinePoint {
        label: b"verify",
        offset_fp: 750,
        complete: true,
    },
    TimelinePoint {
        label: b"act",
        offset_fp: 1000,
        complete: true,
    },
];

const EVIDENCE_ITEMS: [(&[u8], &[u8], u32); 5] = [
    (b"/graph/session", b"active workspace root", 0xFF58A6FF),
    (
        b"orchestrator tick",
        b"time node for all reasoning",
        0xFF39C5BB,
    ),
    (
        b"registry stream",
        b"service and context presence",
        0xFFFFA75A,
    ),
    (
        b"artifact ledger",
        b"build output with provenance",
        0xFF7B68EE,
    ),
    (
        b"desktop surfaces",
        b"live app state and shell context",
        0xFF8BD8FF,
    ),
];

const MEMORY_ITEMS: [(&[u8], &[u8], u32); 4] = [
    (
        b"Concept engine",
        b"in-memory graph of current work",
        0xFF39C5BB,
    ),
    (b"Learned memory", b"verified context only", 0xFF58A6FF),
    (b"Provenance gate", b"source before synthesis", 0xFF8DE0AC),
    (
        b"Compute dispatcher",
        b"one-path action routing",
        0xFFFFA75A,
    ),
];

const WALSH_ITEMS: [(&[u8], &[u8], u32); 4] = [
    (
        b"PowerWalk",
        b"type-conditional temporal random walks",
        0xFFFFC47A,
    ),
    (b"RW Embeddings", b"baseline graph prior", 0xFF58A6FF),
    (
        b"Temporal GNN",
        b"warm-start dynamic graph updates",
        0xFF39C5BB,
    ),
    (
        b"Spectral Drift",
        b"Laplacian anomaly and topology watch",
        0xFF7B68EE,
    ),
];

const MENU_ITEMS: [(&[u8], bool); 5] = [
    (b"Refresh Graph", false),
    (b"Verify Provenance", false),
    (b"---", false),
    (b"Launch Terminal", false),
    (b"Open Settings", false),
];

struct AppState {
    cmd: [u8; CMD_MAX],
    cmd_len: usize,
    frame: u64,
    now_ms: u64,
    last_refresh_ms: u64,
    active_tab: usize,
    selected_action: usize,
    hovered_action: Option<usize>,
    hovered_tab: Option<usize>,
    pressed_action: Option<usize>,
    pressed_tab: Option<usize>,
    pressed_menu: Option<usize>,
    menu_open: bool,
    menu_hovered: usize,
    registry_generation: u64,
    graph_transitions: u32,
    graph_epoch: u32,
    service_online: [bool; SERVICE_COUNT],
    context_scope: [u8; workspace_context::PATH_CAP],
    context_scope_len: usize,
    context_focus: [u8; workspace_context::PATH_CAP],
    context_focus_len: usize,
    context_source: [u8; workspace_context::SOURCE_CAP],
    context_source_len: usize,
    context_is_dir: bool,
    log: [[u8; LOG_LEN]; LOG_CAP],
    log_lens: [usize; LOG_CAP],
    log_head: usize,
    log_count: usize,
    toast_title: [u8; 24],
    toast_title_len: usize,
    toast_body: [u8; 88],
    toast_body_len: usize,
    toast_accent: u32,
    toast_frames: u16,
}

impl AppState {
    const fn new() -> Self {
        Self {
            cmd: [0; CMD_MAX],
            cmd_len: 0,
            frame: 0,
            now_ms: 0,
            last_refresh_ms: 0,
            active_tab: 0,
            selected_action: 0,
            hovered_action: None,
            hovered_tab: None,
            pressed_action: None,
            pressed_tab: None,
            pressed_menu: None,
            menu_open: false,
            menu_hovered: 0,
            registry_generation: 0,
            graph_transitions: 0,
            graph_epoch: 0,
            service_online: [false; SERVICE_COUNT],
            context_scope: [0; workspace_context::PATH_CAP],
            context_scope_len: 0,
            context_focus: [0; workspace_context::PATH_CAP],
            context_focus_len: 0,
            context_source: [0; workspace_context::SOURCE_CAP],
            context_source_len: 0,
            context_is_dir: true,
            log: [[0; LOG_LEN]; LOG_CAP],
            log_lens: [0; LOG_CAP],
            log_head: 0,
            log_count: 0,
            toast_title: [0; 24],
            toast_title_len: 0,
            toast_body: [0; 88],
            toast_body_len: 0,
            toast_accent: 0xFF58A6FF,
            toast_frames: 0,
        }
    }

    fn initialize(&mut self) {
        self.refresh_runtime();
        self.refresh_context();
        self.push_log(b"[orch] AI console attached to time node");
        self.push_log(b"[scce] grounded synthesis lane active");
        self.push_log(b"[walsh] PowerWalk temporal deck loaded");
        if self.context_focus_len > 0 {
            self.push_log(b"[context] graph workspace focus detected");
        }
        self.set_toast(b"AI Console", b"Graph-first cockpit ready", 0xFF58A6FF);
    }

    fn service_count_online(&self) -> u32 {
        let mut total = 0u32;
        let mut idx = 0usize;
        while idx < self.service_online.len() {
            if self.service_online[idx] {
                total += 1;
            }
            idx += 1;
        }
        total
    }

    fn push_log(&mut self, msg: &[u8]) {
        let idx = self.log_head % LOG_CAP;
        let len = msg.len().min(LOG_LEN);
        self.log[idx][..len].copy_from_slice(&msg[..len]);
        self.log_lens[idx] = len;
        self.log_head = self.log_head.wrapping_add(1);
        if self.log_count < LOG_CAP {
            self.log_count += 1;
        }
    }

    fn set_toast(&mut self, title: &[u8], body: &[u8], accent: u32) {
        self.toast_title_len = title.len().min(self.toast_title.len());
        self.toast_title[..self.toast_title_len].copy_from_slice(&title[..self.toast_title_len]);
        self.toast_body_len = body.len().min(self.toast_body.len());
        self.toast_body[..self.toast_body_len].copy_from_slice(&body[..self.toast_body_len]);
        self.toast_accent = accent;
        self.toast_frames = 220;
    }

    fn refresh_runtime(&mut self) {
        let generation = runtime::registry_subscribe(self.registry_generation);
        if generation != 0 && generation != u64::MAX {
            self.registry_generation = generation;
        }

        let mut idx = 0usize;
        while idx < SERVICE_COUNT {
            self.service_online[idx] = runtime::registry_lookup(SERVICE_NAMES[idx]).is_some();
            idx += 1;
        }

        if let Some((transitions, epoch)) = runtime::graph_em_stats(1, 1) {
            self.graph_transitions = transitions;
            self.graph_epoch = epoch;
        }
    }

    fn context_scope(&self) -> &[u8] {
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
            self.context_scope()
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

        if self.context_is_dir != ctx.is_dir {
            self.context_is_dir = ctx.is_dir;
            changed = true;
        }
        changed
    }

    fn manual_refresh(&mut self) {
        self.refresh_runtime();
        self.push_log(b"[graph] registry and embedding stats refreshed");
        self.set_toast(
            b"Refresh",
            b"Runtime state sampled from orchestrator graph",
            0xFF39C5BB,
        );
    }

    fn launch_action(&mut self, idx: usize) {
        if idx >= ACTION_COUNT {
            return;
        }
        self.selected_action = idx;
        if runtime::spawn_named_checked(ACTIONS[idx].spawn) {
            self.push_log(action_log(idx));
            self.set_toast(ACTIONS[idx].label, ACTIONS[idx].meta, ACTIONS[idx].accent);
        } else {
            self.push_log(b"[launch] target unavailable");
            self.set_toast(
                b"Launch Failed",
                b"Target service is not registered yet",
                0xFFFFA75A,
            );
        }
    }

    fn execute_menu(&mut self, idx: usize) {
        match idx {
            0 => self.manual_refresh(),
            1 => {
                self.active_tab = 1;
                self.push_log(b"[prove] provenance verification requested");
                self.set_toast(
                    b"Verify",
                    b"Evidence lane focused for source review",
                    0xFF8DE0AC,
                );
            }
            3 => self.launch_action(2),
            4 => self.launch_action(3),
            _ => {}
        }
        self.menu_open = false;
    }

    fn execute_command(&mut self) {
        let cmd = &self.cmd[..self.cmd_len];
        if cmd.is_empty() {
            self.launch_action(self.selected_action);
            return;
        }

        if cmd == b"refresh" || cmd == b"graph refresh" {
            self.manual_refresh();
        } else if cmd == b"context" || cmd == b"focus" {
            self.active_tab = 1;
            self.push_log(b"[context] workspace focus promoted to evidence lane");
            let mut focus = [0u8; workspace_context::PATH_CAP];
            let focus_src = self.context_focus();
            let focus_len = focus_src.len().min(focus.len());
            focus[..focus_len].copy_from_slice(&focus_src[..focus_len]);
            self.set_toast(b"Context", &focus[..focus_len], 0xFF58A6FF);
        } else if cmd == b"verify" || cmd == b"provenance" {
            self.active_tab = 1;
            self.push_log(b"[prove] provenance lane focused");
            self.set_toast(
                b"Provenance",
                b"Evidence surfaces brought forward",
                0xFF8DE0AC,
            );
        } else if cmd == b"concepts" {
            self.active_tab = 2;
            self.push_log(b"[concept] concept graph highlighted");
        } else if cmd == b"walsh" {
            self.active_tab = 3;
            self.push_log(b"[walsh] temporal kernel deck selected");
        } else if cmd == b"files" {
            self.launch_action(0);
        } else if cmd == b"studio" || cmd == b"editor" {
            self.launch_action(1);
        } else if cmd == b"terminal" {
            self.launch_action(2);
        } else if cmd == b"settings" {
            self.launch_action(3);
        } else if cmd == b"shell" || cmd == b"launcher" {
            self.launch_action(4);
        } else {
            self.push_log(b"[ai] unknown command");
            self.set_toast(
                b"Command",
                b"No graph-native action matched that request",
                0xFFFFA75A,
            );
        }
        self.cmd_len = 0;
    }

    fn handle_key(&mut self, ascii: u8, hid_usage: u8, pressed: bool) {
        if !pressed {
            return;
        }

        match ascii {
            b'\r' | b'\n' => self.execute_command(),
            b'\t' => {
                self.active_tab = (self.active_tab + 1) % TAB_COUNT;
                self.menu_open = false;
            }
            b'1'..=b'5' => {
                self.active_tab = (ascii - b'1') as usize;
                self.menu_open = false;
            }
            b'm' => self.menu_open = !self.menu_open,
            b'r' => self.manual_refresh(),
            b'j' => self.selected_action = (self.selected_action + 1) % ACTION_COUNT,
            b'k' => {
                self.selected_action = if self.selected_action == 0 {
                    ACTION_COUNT - 1
                } else {
                    self.selected_action - 1
                };
            }
            0x08 | 0x7F => {
                if self.cmd_len > 0 {
                    self.cmd_len -= 1;
                }
            }
            0x20..=0x7E if self.cmd_len < CMD_MAX => {
                self.cmd[self.cmd_len] = ascii;
                self.cmd_len += 1;
            }
            _ => match hid_usage {
                0x51 => self.selected_action = (self.selected_action + 1) % ACTION_COUNT,
                0x52 => {
                    self.selected_action = if self.selected_action == 0 {
                        ACTION_COUNT - 1
                    } else {
                        self.selected_action - 1
                    };
                }
                _ => {}
            },
        }
    }

    fn handle_pointer(&mut self, x: i32, y: i32, buttons: u8) -> bool {
        let mut dirty = false;

        let action = hit_action_button(x, y);
        if action != self.hovered_action {
            self.hovered_action = action;
            dirty = true;
        }

        let tab = hit_tab(x, y);
        if tab != self.hovered_tab {
            self.hovered_tab = tab;
            dirty = true;
        }

        let menu_item = if self.menu_open {
            hit_menu_item(x, y)
        } else {
            None
        };
        if let Some(idx) = menu_item {
            if idx != self.menu_hovered {
                self.menu_hovered = idx;
                dirty = true;
            }
        }

        if buttons & 1 != 0 {
            if self.pressed_action.is_none() {
                self.pressed_action = action;
            }
            if self.pressed_tab.is_none() {
                self.pressed_tab = tab;
            }
            if self.pressed_menu.is_none() {
                self.pressed_menu = menu_item;
            }
        } else {
            if let Some(idx) = self.pressed_action.take() {
                if action == Some(idx) {
                    self.launch_action(idx);
                    dirty = true;
                }
            }
            if let Some(idx) = self.pressed_tab.take() {
                if tab == Some(idx) {
                    self.active_tab = idx;
                    dirty = true;
                }
            }
            if let Some(idx) = self.pressed_menu.take() {
                if menu_item == Some(idx) {
                    self.execute_menu(idx);
                    dirty = true;
                }
            }
        }

        dirty
    }

    fn render(&self, canvas: &mut Canvas<'_>) {
        let palette = tokens(THEME);
        canvas.clear(palette.background);

        widgets::draw_window_frame(
            canvas,
            Rect::new(0, 0, WIN_W, WIN_H),
            b"GraphOS AI Console",
            THEME,
        );
        draw_stats_strip(canvas, self);
        draw_action_toolbar(canvas, self);
        let content =
            widgets::draw_tab_bar(canvas, content_host_rect(), &TABS, self.active_tab, THEME);

        match self.active_tab {
            0 => draw_synthesize_tab(canvas, content, self),
            1 => draw_evidence_tab(canvas, content, self),
            2 => draw_concepts_tab(canvas, content, self),
            3 => draw_walsh_tab(canvas, content, self),
            _ => draw_actions_tab(canvas, content, self),
        }

        let cursor_blink = ((self.frame / 30) & 1) == 0;
        widgets::draw_command_bar(
            canvas,
            command_bar_rect(),
            b">",
            &self.cmd[..self.cmd_len],
            cursor_blink,
            THEME,
        );

        canvas.draw_text(12, WIN_H as i32 - 24, b"Tab/1-5 lanes  context/verify/walsh  M menu  Enter runs selected action if command is empty", palette.text_muted, WIN_W - 24);

        if self.menu_open {
            widgets::draw_menu(canvas, menu_rect(), &MENU_ITEMS, self.menu_hovered, THEME);
        }

        if self.toast_frames > 0 {
            widgets::draw_notification_toast(
                canvas,
                Rect::new(WIN_W as i32 - 258, WIN_H as i32 - 92, 242, 52),
                &self.toast_title[..self.toast_title_len],
                &self.toast_body[..self.toast_body_len],
                self.toast_accent,
                THEME,
            );
        }
    }
}

fn draw_stats_strip(canvas: &mut Canvas<'_>, state: &AppState) {
    let strip = stats_rect();
    let gap = 8u32;
    let card_w = (strip.w.saturating_sub(gap * 6)) / 5;
    let mut clock = [0u8; 16];
    let mut registry = LineWriter::new();
    let _ = write!(&mut registry, "gen {}", state.registry_generation);
    let mut graph = LineWriter::new();
    let _ = write!(
        &mut graph,
        "{} / {}",
        state.graph_transitions, state.graph_epoch
    );
    let mut online = LineWriter::new();
    let _ = write!(
        &mut online,
        "{}/{}",
        state.service_count_online(),
        SERVICE_COUNT
    );

    widgets::draw_stat_card(
        canvas,
        Rect::new(strip.x + gap as i32, strip.y + 8, card_w, 50),
        b"Clock",
        format_clock(state.now_ms, &mut clock),
        0xFF58A6FF,
        THEME,
    );
    widgets::draw_stat_card(
        canvas,
        Rect::new(strip.x + (card_w + gap * 2) as i32, strip.y + 8, card_w, 50),
        b"Registry",
        registry.bytes(),
        0xFF39C5BB,
        THEME,
    );
    widgets::draw_stat_card(
        canvas,
        Rect::new(
            strip.x + (card_w * 2 + gap * 3) as i32,
            strip.y + 8,
            card_w,
            50,
        ),
        b"Transitions",
        graph.bytes(),
        0xFFFFA75A,
        THEME,
    );
    widgets::draw_stat_card(
        canvas,
        Rect::new(
            strip.x + (card_w * 3 + gap * 4) as i32,
            strip.y + 8,
            card_w,
            50,
        ),
        b"Source",
        state.context_source(),
        0xFF8BD8FF,
        THEME,
    );
    widgets::draw_stat_card(
        canvas,
        Rect::new(
            strip.x + (card_w * 4 + gap * 5) as i32,
            strip.y + 8,
            card_w,
            50,
        ),
        b"Core Online",
        online.bytes(),
        0xFF8DE0AC,
        THEME,
    );
}

fn draw_action_toolbar(canvas: &mut Canvas<'_>, state: &AppState) {
    let rect = toolbar_rect();
    let p = tokens(THEME);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.chrome);
    canvas.draw_hline(rect.x, rect.y + rect.h as i32, rect.w, p.border);
    canvas.draw_text(rect.x + 12, rect.y + 9, b"Actions", p.text_muted, 80);

    for idx in 0..ACTION_COUNT {
        let hovered = state.hovered_action == Some(idx);
        let pressed = state.pressed_action == Some(idx);
        let focused = state.selected_action == idx;
        let kind = if focused {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        };
        widgets::draw_button(
            canvas,
            action_button_rect(idx),
            ACTIONS[idx].label,
            kind,
            focused,
            hovered,
            pressed,
            THEME,
        );
    }

    widgets::draw_badge(
        canvas,
        rect.x + rect.w as i32 - 22,
        rect.y + 9,
        state.log_count as u32,
        THEME,
    );
}

fn draw_synthesize_tab(canvas: &mut Canvas<'_>, rect: Rect, state: &AppState) {
    let (left, right) = rect.split_left(rect.w * 58 / 100);
    let (graph_rect, log_rect) = left.split_top(left.h * 58 / 100);
    let (brief_rect, timeline_rect) = right.split_top(right.h * 56 / 100);

    draw_graph_view(
        canvas,
        graph_rect,
        b"Orchestrator Concept Graph",
        &GRAPH_NODES,
        &GRAPH_EDGES,
        Some((state.frame as usize / 30) % GRAPH_NODES.len()),
        THEME,
    );

    let log_inner = widgets::draw_panel(canvas, log_rect, b"Evidence Synthesis", THEME);
    canvas.draw_text(
        log_inner.x,
        log_inner.y,
        b"SCCE contract",
        tokens(THEME).text_muted,
        log_inner.w,
    );
    canvas.draw_text(
        log_inner.x,
        log_inner.y + 16,
        b"The Mouth synthesizes evidence, not guesses",
        tokens(THEME).text,
        log_inner.w,
    );
    canvas.draw_text(
        log_inner.x,
        log_inner.y + 32,
        b"Every answer traces back to graph evidence",
        tokens(THEME).text,
        log_inner.w,
    );
    draw_recent_logs(
        canvas,
        Rect::new(
            log_inner.x,
            log_inner.y + 58,
            log_inner.w,
            log_inner.h.saturating_sub(58),
        ),
        state,
    );

    let brief = widgets::draw_panel(canvas, brief_rect, b"Operator Contract", THEME);
    let mut reg = LineWriter::new();
    let _ = write!(
        &mut reg,
        "Registry generation {}",
        state.registry_generation
    );
    let mut epoch = LineWriter::new();
    let _ = write!(
        &mut epoch,
        "Embedding transitions {} // epoch {}",
        state.graph_transitions, state.graph_epoch
    );
    canvas.draw_text(
        brief.x,
        brief.y,
        b"Graph-first workspace root:",
        tokens(THEME).text,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 16,
        state.context_scope(),
        0xFF58A6FF,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 36,
        b"Current focus",
        tokens(THEME).text_muted,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 52,
        state.context_focus(),
        0xFF8DE0AC,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 72,
        b"Context source",
        tokens(THEME).text_muted,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 88,
        state.context_source(),
        0xFFFFC47A,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 110,
        b"Orchestrator owns time for all AI surfaces",
        tokens(THEME).text,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 128,
        b"Retrieval-not-generation keeps answers anchored",
        tokens(THEME).text,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 146,
        b"Provenance is the product",
        0xFF8DE0AC,
        brief.w,
    );
    canvas.draw_text(
        brief.x,
        brief.y + 164,
        reg.bytes(),
        tokens(THEME).text_muted,
        brief.w,
    );
    canvas.draw_text(brief.x, brief.y + 182, epoch.bytes(), 0xFFFFC47A, brief.w);

    draw_timeline_view(
        canvas,
        timeline_rect,
        b"Reasoning Cycle",
        &REASONING_TIMELINE,
        ((state.now_ms / 1300) % 5) as usize,
        THEME,
    );
}

fn draw_evidence_tab(canvas: &mut Canvas<'_>, rect: Rect, state: &AppState) {
    let (left, right) = rect.split_left(rect.w * 54 / 100);
    let (prove_rect, graph_rect) = right.split_top(right.h * 54 / 100);

    let evidence = widgets::draw_panel(canvas, left, b"Evidence Lanes", THEME);
    for idx in 0..EVIDENCE_ITEMS.len() {
        let row = Rect::new(evidence.x, evidence.y + idx as i32 * 28, evidence.w, 28);
        widgets::draw_list_row(
            canvas,
            row,
            EVIDENCE_ITEMS[idx].0,
            EVIDENCE_ITEMS[idx].1,
            EVIDENCE_ITEMS[idx].2,
            idx == (state.frame as usize / 45) % EVIDENCE_ITEMS.len(),
            false,
            THEME,
        );
    }

    let prove = widgets::draw_panel(canvas, prove_rect, b"Provenance Gate", THEME);
    let mut core = LineWriter::new();
    let _ = write!(
        &mut core,
        "Core online {}/{}",
        state.service_count_online(),
        SERVICE_COUNT
    );
    canvas.draw_text(
        prove.x,
        prove.y,
        b"Sources stay attached to every context edge",
        tokens(THEME).text,
        prove.w,
    );
    canvas.draw_text(
        prove.x,
        prove.y + 18,
        b"Artifacts and files remain tied to graph identity",
        tokens(THEME).text,
        prove.w,
    );
    canvas.draw_text(
        prove.x,
        prove.y + 36,
        b"Evidence must survive verification before synthesis",
        0xFF8DE0AC,
        prove.w,
    );
    canvas.draw_text(
        prove.x,
        prove.y + 54,
        b"Orchestrator tick is the only clock in this lane",
        tokens(THEME).text_muted,
        prove.w,
    );
    canvas.draw_text(prove.x, prove.y + 72, core.bytes(), 0xFFFFC47A, prove.w);

    draw_graph_view(
        canvas,
        graph_rect,
        b"Source Graph",
        &GRAPH_NODES,
        &GRAPH_EDGES,
        Some(5),
        THEME,
    );
}

fn draw_concepts_tab(canvas: &mut Canvas<'_>, rect: Rect, state: &AppState) {
    let (left, right) = rect.split_left(rect.w * 56 / 100);
    let (memory_rect, timeline_rect) = right.split_top(right.h * 58 / 100);

    draw_graph_view(
        canvas,
        left,
        b"Concept Engine",
        &GRAPH_NODES,
        &GRAPH_EDGES,
        Some((state.graph_epoch as usize) % GRAPH_NODES.len()),
        THEME,
    );

    let memory = widgets::draw_panel(canvas, memory_rect, b"Learned Memory", THEME);
    for idx in 0..MEMORY_ITEMS.len() {
        let row = Rect::new(memory.x, memory.y + idx as i32 * 28, memory.w, 28);
        widgets::draw_list_row(
            canvas,
            row,
            MEMORY_ITEMS[idx].0,
            MEMORY_ITEMS[idx].1,
            MEMORY_ITEMS[idx].2,
            idx == ((state.frame / 48) as usize % MEMORY_ITEMS.len()),
            false,
            THEME,
        );
    }

    draw_timeline_view(
        canvas,
        timeline_rect,
        b"Context Aging",
        &REASONING_TIMELINE,
        ((state.graph_epoch as usize) / 3) % 5,
        THEME,
    );
}

fn draw_walsh_tab(canvas: &mut Canvas<'_>, rect: Rect, state: &AppState) {
    let (left, right) = rect.split_left(rect.w * 52 / 100);
    let (kernel_rect, bridge_rect) = right.split_top(right.h * 56 / 100);

    let deck = widgets::draw_panel(canvas, left, b"Walsh Math Deck", THEME);
    for idx in 0..WALSH_ITEMS.len() {
        let row = Rect::new(deck.x, deck.y + idx as i32 * 28, deck.w, 28);
        widgets::draw_list_row(
            canvas,
            row,
            WALSH_ITEMS[idx].0,
            WALSH_ITEMS[idx].1,
            WALSH_ITEMS[idx].2,
            idx == ((state.frame / 52) as usize % WALSH_ITEMS.len()),
            false,
            THEME,
        );
    }

    let kernel = widgets::draw_panel(canvas, kernel_rect, b"Temporal Kernel", THEME);
    canvas.draw_text(
        kernel.x,
        kernel.y,
        b"Decay window 0.009..0.28 per day",
        tokens(THEME).text,
        kernel.w,
    );
    widgets::draw_progress_bar(
        canvas,
        Rect::new(kernel.x, kernel.y + 18, kernel.w.saturating_sub(8), 12),
        280,
        THEME,
    );
    canvas.draw_text(
        kernel.x,
        kernel.y + 42,
        b"Optimal walk length 12..94",
        tokens(THEME).text,
        kernel.w,
    );
    widgets::draw_progress_bar(
        canvas,
        Rect::new(kernel.x, kernel.y + 60, kernel.w.saturating_sub(8), 12),
        640,
        THEME,
    );
    canvas.draw_text(
        kernel.x,
        kernel.y + 84,
        b"AUC 0.96 versus 0.93 baseline",
        0xFF8DE0AC,
        kernel.w,
    );
    widgets::draw_progress_bar(
        canvas,
        Rect::new(kernel.x, kernel.y + 102, kernel.w.saturating_sub(8), 12),
        960,
        THEME,
    );
    let mut epoch = LineWriter::new();
    let _ = write!(&mut epoch, "Graph epoch {}", state.graph_epoch);
    canvas.draw_text(
        kernel.x,
        kernel.y + 126,
        epoch.bytes(),
        0xFFFFC47A,
        kernel.w,
    );

    let bridge = widgets::draw_panel(canvas, bridge_rect, b"SCCE Bridge", THEME);
    canvas.draw_text(
        bridge.x,
        bridge.y,
        b"Walsh temporal walks seed retrieval order",
        tokens(THEME).text,
        bridge.w,
    );
    canvas.draw_text(
        bridge.x,
        bridge.y + 18,
        b"Concept engine uses graph drift to re-rank context",
        tokens(THEME).text,
        bridge.w,
    );
    canvas.draw_text(
        bridge.x,
        bridge.y + 36,
        b"Spectral signals help gate anomaly-heavy evidence",
        tokens(THEME).text,
        bridge.w,
    );
    canvas.draw_text(
        bridge.x,
        bridge.y + 54,
        b"Warm-start protocol keeps the shell responsive",
        0xFF8DE0AC,
        bridge.w,
    );
}

fn draw_actions_tab(canvas: &mut Canvas<'_>, rect: Rect, state: &AppState) {
    let (left, right) = rect.split_left(rect.w * 52 / 100);
    let (graph_rect, notes_rect) = right.split_top(right.h * 54 / 100);

    let launch = widgets::draw_panel(canvas, left, b"Launch Surfaces", THEME);
    for idx in 0..ACTION_COUNT {
        let row = Rect::new(launch.x, launch.y + idx as i32 * 30, launch.w, 30);
        widgets::draw_list_row(
            canvas,
            row,
            ACTIONS[idx].label,
            ACTIONS[idx].meta,
            ACTIONS[idx].accent,
            idx == state.selected_action,
            state.hovered_action == Some(idx),
            THEME,
        );
    }

    draw_graph_view(
        canvas,
        graph_rect,
        b"Service Mesh",
        &GRAPH_NODES,
        &GRAPH_EDGES,
        Some(7),
        THEME,
    );

    let notes = widgets::draw_panel(canvas, notes_rect, b"Operator Notes", THEME);
    canvas.draw_text(
        notes.x,
        notes.y,
        b"Enter on an empty command line launches the selected action",
        tokens(THEME).text,
        notes.w,
    );
    canvas.draw_text(
        notes.x,
        notes.y + 18,
        b"Files, Studio, Terminal, Settings, and Shell are all graph-linked",
        tokens(THEME).text,
        notes.w,
    );
    canvas.draw_text(
        notes.x,
        notes.y + 36,
        b"Keep apps tied to orchestrator time and graph identity",
        0xFF8DE0AC,
        notes.w,
    );
    canvas.draw_text(
        notes.x,
        notes.y + 54,
        b"Awesome over legacy",
        0xFFFFC47A,
        notes.w,
    );
}

fn draw_recent_logs(canvas: &mut Canvas<'_>, rect: Rect, state: &AppState) {
    let visible = (rect.h / 14) as usize;
    let start = state.log_count.saturating_sub(visible);
    for row in start..state.log_count {
        let ring_idx = (state.log_head.wrapping_sub(state.log_count - row)) % LOG_CAP;
        let bytes = &state.log[ring_idx][..state.log_lens[ring_idx]];
        let y = rect.y + (row - start) as i32 * 14;
        canvas.draw_text(rect.x, y, bytes, tokens(THEME).text_muted, rect.w);
    }
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

fn stats_rect() -> Rect {
    Rect::new(0, 32, WIN_W, 70)
}

fn toolbar_rect() -> Rect {
    Rect::new(0, 102, WIN_W, 34)
}

fn content_host_rect() -> Rect {
    Rect::new(0, 136, WIN_W, WIN_H - 164)
}

fn command_bar_rect() -> Rect {
    Rect::new(0, WIN_H as i32 - 28, WIN_W, 28)
}

fn action_button_rect(idx: usize) -> Rect {
    let rect = toolbar_rect();
    let w = 100u32;
    let gap = 8i32;
    Rect::new(
        rect.x + 88 + idx as i32 * (w as i32 + gap),
        rect.y + 4,
        w,
        rect.h.saturating_sub(8),
    )
}

fn menu_rect() -> Rect {
    Rect::new(WIN_W as i32 - 198, 176, 184, 122)
}

fn hit_action_button(x: i32, y: i32) -> Option<usize> {
    for idx in 0..ACTION_COUNT {
        if point_in_rect(x, y, action_button_rect(idx)) {
            return Some(idx);
        }
    }
    None
}

fn hit_tab(x: i32, y: i32) -> Option<usize> {
    let host = content_host_rect();
    if y < host.y || y >= host.y + 32 {
        return None;
    }
    let tab_w = (host.w / TAB_COUNT as u32).max(60);
    for idx in 0..TAB_COUNT {
        let rect = Rect::new(host.x + idx as i32 * tab_w as i32, host.y, tab_w, 32);
        if point_in_rect(x, y, rect) {
            return Some(idx);
        }
    }
    None
}

fn hit_menu_item(x: i32, y: i32) -> Option<usize> {
    let rect = menu_rect();
    if !point_in_rect(x, y, rect) {
        return None;
    }
    let row_h = 24i32;
    let row = (y - rect.y) / row_h;
    if row < 0 {
        return None;
    }
    let idx = row as usize;
    if idx >= MENU_ITEMS.len() || MENU_ITEMS[idx].0 == b"---" {
        return None;
    }
    Some(idx)
}

fn point_in_rect(x: i32, y: i32, rect: Rect) -> bool {
    x >= rect.x && y >= rect.y && x < rect.x + rect.w as i32 && y < rect.y + rect.h as i32
}

fn action_log(idx: usize) -> &'static [u8] {
    match idx {
        0 => b"[launch] files vault opened",
        1 => b"[launch] studio/editor opened",
        2 => b"[launch] terminal tools opened",
        3 => b"[launch] settings control center opened",
        4 => b"[launch] shell deck opened",
        _ => b"[launch] action requested",
    }
}

fn format_clock<'a>(now_ms: u64, out: &'a mut [u8; 16]) -> &'a [u8] {
    let total_secs = now_ms / 1000;
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
    &out[..12]
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[ai-console] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => {
            runtime::write_line(b"[ai-console] channel create failed\n");
            runtime::exit(1)
        }
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => {
            runtime::write_line(b"[ai-console] window open failed\n");
            runtime::exit(1)
        }
    };
    runtime::write_line(b"[ai-console] window open ok\n");

    if win.pixel_addr() < (1u64 << 39) {
        runtime::write_line(b"[ai-console] invalid surface mapping\n");
        runtime::exit(1);
    }

    let mut state = AppState::new();
    state.initialize();

    {
        let mut canvas = win.canvas();
        state.render(&mut canvas);
    }
    let _ = win.present();
    win.request_focus();

    loop {
        let mut dirty = false;

        loop {
            match win.poll_event() {
                Event::FrameTick { now_ms } => {
                    state.now_ms = now_ms;
                    state.frame = state.frame.wrapping_add(1);
                    if state.toast_frames > 0 {
                        state.toast_frames -= 1;
                    }
                    if state.last_refresh_ms == 0
                        || now_ms.saturating_sub(state.last_refresh_ms) >= 1000
                    {
                        state.refresh_runtime();
                        state.refresh_context();
                        state.last_refresh_ms = now_ms;
                    }
                    dirty = true;
                }
                Event::Key {
                    pressed,
                    ascii,
                    hid_usage,
                } => {
                    state.handle_key(ascii, hid_usage, pressed);
                    dirty = true;
                }
                Event::PointerMove { x, y, buttons } => {
                    if state.handle_pointer(x as i32, y as i32, buttons) {
                        dirty = true;
                    }
                }
                Event::None => break,
            }
        }

        if dirty {
            let mut canvas = win.canvas();
            state.render(&mut canvas);
            if !win.present() {
                runtime::write_line(b"[ai-console] present failed\n");
                runtime::exit(1);
            }
        }

        graphos_app_sdk::sys::yield_now();
    }
}

struct LineWriter {
    buf: [u8; 192],
    len: usize,
}

impl LineWriter {
    fn new() -> Self {
        Self {
            buf: [0; 192],
            len: 0,
        }
    }

    fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for LineWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let space = self.buf.len().saturating_sub(self.len);
        let count = bytes.len().min(space);
        self.buf[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}
