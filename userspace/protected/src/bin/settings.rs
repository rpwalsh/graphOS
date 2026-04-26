// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! settings - GraphOS control center.
//!
//! Graph-first ring-3 settings surface with:
//! - live graph and registry status
//! - orchestrator-owned time and animation pacing
//! - service health snapshots for the core desktop stack
//! - modern chrome with pointer and keyboard navigation

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::fmt::{self, Write};
use core::panic::PanicInfo;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::{
    geom::Rect,
    tokens::{tokens, Theme},
    widgets::{
        draw_command_bar, draw_list_row, draw_panel, draw_separator, draw_sidebar_item,
        draw_stat_card, draw_window_frame,
    },
};

const WIN_W: u32 = 940;
const WIN_H: u32 = 580;
const THEME: Theme = Theme::DarkGlass;
const SIDEBAR_W: u32 = 196;
const FOOTER_H: u32 = 24;
const BAR_H: u32 = 28;
const SIDEBAR_ROW_H: u32 = 34;
const CARD_H: u32 = 44;

const TAB_GRAPH: usize = 0;
const TAB_ORCHESTRATOR: usize = 1;
const TAB_DISPLAY: usize = 2;
const TAB_SECURITY: usize = 3;
const TAB_SERVICES: usize = 4;
const TAB_STORAGE: usize = 5;
const TAB_ABOUT: usize = 6;

const CATEGORIES: [(&[u8], u32); 7] = [
    (b"Graph", 0xFF39C5BB),
    (b"Orchestrator", 0xFFD29922),
    (b"Display", 0xFF58A6FF),
    (b"Security", 0xFFF78166),
    (b"Services", 0xFF3FB950),
    (b"Storage", 0xFFA371F7),
    (b"About", 0xFF9AA4B2),
];

const CORE_SERVICES: [&[u8]; 6] = [
    b"graphd",
    b"servicemgr",
    b"sysd",
    b"modeld",
    b"trainerd",
    b"artifactsd",
];

#[derive(Clone, Copy)]
struct ServiceState {
    present: bool,
    alias: u32,
    health: u8,
}

impl ServiceState {
    const fn empty() -> Self {
        Self {
            present: false,
            alias: 0,
            health: 0,
        }
    }
}

struct SecuritySettings {
    smep_smap: bool,
    audit_log: bool,
    seccomp: bool,
    stack_guard: bool,
}

impl SecuritySettings {
    const fn new() -> Self {
        Self {
            smep_smap: true,
            audit_log: true,
            seccomp: true,
            stack_guard: true,
        }
    }
}

struct AppState {
    selected: usize,
    security: SecuritySettings,
    cmd_input: [u8; 64],
    cmd_len: usize,
    status: [u8; 96],
    status_len: usize,
    now_ms: u64,
    last_refresh_ms: u64,
    pointer_x: i16,
    pointer_y: i16,
    prev_buttons: u8,
    services: [ServiceState; CORE_SERVICES.len()],
    registry_generation: u64,
    em_transitions: u32,
    em_epoch: u32,
}

impl AppState {
    fn new() -> Self {
        let mut state = Self {
            selected: TAB_GRAPH,
            security: SecuritySettings::new(),
            cmd_input: [0u8; 64],
            cmd_len: 0,
            status: [0u8; 96],
            status_len: 0,
            now_ms: 0,
            last_refresh_ms: 0,
            pointer_x: 0,
            pointer_y: 0,
            prev_buttons: 0,
            services: [ServiceState::empty(); CORE_SERVICES.len()],
            registry_generation: 0,
            em_transitions: 0,
            em_epoch: 0,
        };
        state.set_status(b"Graph control ready. Try graph, orch, services.");
        state.refresh_runtime();
        state
    }

    fn set_status(&mut self, msg: &[u8]) {
        let len = msg.len().min(self.status.len());
        self.status[..len].copy_from_slice(&msg[..len]);
        self.status_len = len;
    }

    fn status_text(&self) -> &[u8] {
        &self.status[..self.status_len]
    }

    fn select_tab(&mut self, idx: usize) {
        self.selected = idx.min(CATEGORIES.len().saturating_sub(1));
        self.set_status(match self.selected {
            TAB_GRAPH => b"Graph root and edge model are live.",
            TAB_ORCHESTRATOR => b"Desktop time belongs to the orchestrator node.",
            TAB_DISPLAY => b"Display path is compositor-owned and frame-paced.",
            TAB_SECURITY => b"Press 1-4 or click a control to toggle it.",
            TAB_SERVICES => b"Core ring-3 services are sampled from the registry.",
            TAB_STORAGE => b"Namespaces stay graph-first: /graph before legacy habits.",
            _ => b"GraphOS ships as a graph-native desktop, not a generic shell.",
        });
    }

    fn online_services(&self) -> u32 {
        let mut count = 0u32;
        let mut idx = 0usize;
        while idx < self.services.len() {
            if self.services[idx].present {
                count += 1;
            }
            idx += 1;
        }
        count
    }

    fn refresh_runtime(&mut self) {
        let generation = runtime::registry_subscribe(self.registry_generation);
        if generation != 0 && generation != u64::MAX {
            self.registry_generation = generation;
        }

        let mut idx = 0usize;
        while idx < CORE_SERVICES.len() {
            self.services[idx] = if let Some(entry) = runtime::registry_lookup(CORE_SERVICES[idx]) {
                ServiceState {
                    present: true,
                    alias: entry.channel_alias,
                    health: entry.health,
                }
            } else {
                ServiceState::empty()
            };
            idx += 1;
        }

        if let Some((transitions, epoch)) = runtime::graph_em_stats(1, 1) {
            self.em_transitions = transitions;
            self.em_epoch = epoch;
        }
    }

    fn execute_command(&mut self) {
        let cmd = trim_ascii(&self.cmd_input[..self.cmd_len]);
        if cmd.is_empty() {
            self.set_status(b"Commands: graph orch display security services storage about refresh");
            return;
        }

        if eq_ignore_ascii_case(cmd, b"graph") {
            self.select_tab(TAB_GRAPH);
        } else if eq_ignore_ascii_case(cmd, b"orch")
            || eq_ignore_ascii_case(cmd, b"orchestrator")
            || eq_ignore_ascii_case(cmd, b"time")
        {
            self.select_tab(TAB_ORCHESTRATOR);
        } else if eq_ignore_ascii_case(cmd, b"display") {
            self.select_tab(TAB_DISPLAY);
        } else if eq_ignore_ascii_case(cmd, b"security") {
            self.select_tab(TAB_SECURITY);
        } else if eq_ignore_ascii_case(cmd, b"services") || eq_ignore_ascii_case(cmd, b"registry")
        {
            self.select_tab(TAB_SERVICES);
        } else if eq_ignore_ascii_case(cmd, b"storage")
            || eq_ignore_ascii_case(cmd, b"files")
            || eq_ignore_ascii_case(cmd, b"namespaces")
        {
            self.select_tab(TAB_STORAGE);
        } else if eq_ignore_ascii_case(cmd, b"about") {
            self.select_tab(TAB_ABOUT);
        } else if eq_ignore_ascii_case(cmd, b"refresh") {
            self.refresh_runtime();
            self.set_status(b"Runtime status refreshed from graph and registry.");
        } else if eq_ignore_ascii_case(cmd, b"clear") {
            self.set_status(b"Command cleared.");
        } else if eq_ignore_ascii_case(cmd, b"help") {
            self.set_status(b"Try graph, orch, display, services, storage, about, refresh.");
        } else {
            self.set_status(b"Unknown command. Try graph, orch, services, or refresh.");
        }

        self.cmd_len = 0;
    }
}

fn render(win: &mut Window, state: &AppState) {
    let pal = tokens(THEME);
    let root = Rect::new(0, 0, WIN_W, WIN_H);
    draw_window_frame(&mut win.canvas(), root, b"GraphOS Control", THEME);

    let sidebar = sidebar_rect();
    let content = content_rect();
    let content_inner = draw_panel(&mut win.canvas(), content, CATEGORIES[state.selected].0, THEME);

    {
        let mut canvas = win.canvas();
        canvas.fill_rect(sidebar.x, sidebar.y, sidebar.w, sidebar.h, pal.surface);
        canvas.draw_vline(sidebar.x + sidebar.w as i32 - 1, sidebar.y, sidebar.h, pal.border);
    }

    let service_badge = state.online_services();
    let mut idx = 0usize;
    while idx < CATEGORIES.len() {
        let item_rect = sidebar_item_rect(idx);
        let hovered = contains(item_rect, state.pointer_x, state.pointer_y);
        let badge = match idx {
            TAB_GRAPH => {
                if state.services[0].present {
                    1
                } else {
                    0
                }
            }
            TAB_ORCHESTRATOR => {
                if state.now_ms != 0 {
                    1
                } else {
                    0
                }
            }
            TAB_SERVICES => service_badge,
            _ => 0,
        };
        draw_sidebar_item(
            &mut win.canvas(),
            item_rect,
            CATEGORIES[idx].0,
            CATEGORIES[idx].1,
            state.selected == idx,
            hovered,
            badge,
            THEME,
        );
        idx += 1;
    }

    match state.selected {
        TAB_GRAPH => render_graph(win, content_inner, state),
        TAB_ORCHESTRATOR => render_orchestrator(win, content_inner, state),
        TAB_DISPLAY => render_display(win, content_inner, state),
        TAB_SECURITY => render_security(win, content_inner, state),
        TAB_SERVICES => render_services(win, content_inner, state),
        TAB_STORAGE => render_storage(win, content_inner),
        _ => render_about(win, content_inner, state),
    }

    render_footer(win, state);
    draw_command_bar(
        &mut win.canvas(),
        command_bar_rect(),
        b"graph>",
        &state.cmd_input[..state.cmd_len],
        ((state.now_ms / 400) % 2) == 0,
        THEME,
    );
    win.present();
}

fn render_graph(win: &mut Window, area: Rect, state: &AppState) {
    let pal = tokens(THEME);
    let card_w = area.w / 2 - 4;

    let mut edge_buf = [0u8; 24];
    let edge_len = write_u32_decimal(&mut edge_buf, state.em_transitions);
    let mut service_buf = [0u8; 24];
    let mut service_len = write_u32_decimal(&mut service_buf, state.online_services());
    push_bytes(&mut service_buf, &mut service_len, b" / ");
    let total_len = write_u32_decimal(&mut service_buf[service_len..], CORE_SERVICES.len() as u32);
    service_len += total_len;

    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y, card_w, CARD_H),
        b"Graph Root",
        b"/graph",
        pal.primary,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y, card_w, CARD_H),
        b"Time Node",
        b"orchestrator",
        pal.warning,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"Edge Transitions",
        &edge_buf[..edge_len],
        pal.success,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"Core Services",
        &service_buf[..service_len],
        pal.text_muted,
        THEME,
    );

    let rows_y = area.y + (CARD_H as i32 * 2) + 30;
    draw_separator(&mut win.canvas(), area.x, rows_y, area.w, b"Graph Fabric", THEME);

    let mut registry_buf = [0u8; 24];
    let mut registry_len = 0usize;
    push_bytes(&mut registry_buf, &mut registry_len, b"gen ");
    registry_len += write_u64_decimal(&mut registry_buf[registry_len..], state.registry_generation);

    let mut epoch_buf = [0u8; 24];
    let mut epoch_len = 0usize;
    push_bytes(&mut epoch_buf, &mut epoch_len, b"epoch ");
    epoch_len += write_u32_decimal(&mut epoch_buf[epoch_len..], state.em_epoch);

    let row0 = Rect::new(area.x, rows_y + 10, area.w, 28);
    let row1 = Rect::new(area.x, rows_y + 38, area.w, 28);
    let row2 = Rect::new(area.x, rows_y + 66, area.w, 28);
    let row3 = Rect::new(area.x, rows_y + 94, area.w, 28);
    draw_list_row(
        &mut win.canvas(),
        row0,
        b"/graph namespace",
        b"preferred shell root",
        pal.primary,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row1,
        b"graphd presence",
        service_meta(&state.services[0], &mut service_buf),
        service_color(&state.services[0], pal.success, pal.warning, pal.danger),
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row2,
        b"registry generation",
        &registry_buf[..registry_len],
        pal.warning,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row3,
        b"edge model epoch",
        &epoch_buf[..epoch_len],
        pal.success,
        false,
        false,
        THEME,
    );
}

fn render_orchestrator(win: &mut Window, area: Rect, state: &AppState) {
    let pal = tokens(THEME);
    let card_w = area.w / 2 - 4;

    let mut clock_buf = [0u8; 24];
    let mut clock_len = write_u64_decimal(&mut clock_buf, state.now_ms);
    push_bytes(&mut clock_buf, &mut clock_len, b" ms");

    let mut refresh_buf = [0u8; 24];
    let mut refresh_len = write_u64_decimal(&mut refresh_buf, state.last_refresh_ms);
    push_bytes(&mut refresh_buf, &mut refresh_len, b" ms");

    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y, card_w, CARD_H),
        b"Clock",
        &clock_buf[..clock_len],
        pal.warning,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y, card_w, CARD_H),
        b"Authority",
        b"orchestrator node",
        pal.primary,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"UI Pacing",
        b"FrameTick stream",
        pal.success,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"Last Refresh",
        &refresh_buf[..refresh_len],
        pal.text_muted,
        THEME,
    );

    let rows_y = area.y + (CARD_H as i32 * 2) + 30;
    draw_separator(
        &mut win.canvas(),
        area.x,
        rows_y,
        area.w,
        b"Time Ownership",
        THEME,
    );

    let row0 = Rect::new(area.x, rows_y + 10, area.w, 28);
    let row1 = Rect::new(area.x, rows_y + 38, area.w, 28);
    let row2 = Rect::new(area.x, rows_y + 66, area.w, 28);
    let row3 = Rect::new(area.x, rows_y + 94, area.w, 28);

    draw_list_row(
        &mut win.canvas(),
        row0,
        b"Desktop clock",
        b"owned by orchestrator",
        pal.warning,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row1,
        b"Cursor blink",
        b"phase from now_ms",
        pal.primary,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row2,
        b"Settings redraw",
        b"frame paced, no synthetic timer",
        pal.success,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row3,
        b"Compositor contract",
        b"takeover after ring-3 handoff",
        pal.text_muted,
        false,
        false,
        THEME,
    );
}

fn render_display(win: &mut Window, area: Rect, _state: &AppState) {
    let pal = tokens(THEME);
    let card_w = area.w / 2 - 4;

    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y, card_w, CARD_H),
        b"Renderer",
        b"graphos-gl",
        pal.primary,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y, card_w, CARD_H),
        b"Desktop Scene",
        b"native 3D shell",
        pal.success,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"Compositor",
        b"ring-3 takeover",
        pal.warning,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"Clock Source",
        b"orchestrator",
        pal.text_muted,
        THEME,
    );

    let rows_y = area.y + (CARD_H as i32 * 2) + 30;
    draw_separator(&mut win.canvas(), area.x, rows_y, area.w, b"Desktop Path", THEME);

    let row0 = Rect::new(area.x, rows_y + 10, area.w, 28);
    let row1 = Rect::new(area.x, rows_y + 38, area.w, 28);
    let row2 = Rect::new(area.x, rows_y + 66, area.w, 28);
    let row3 = Rect::new(area.x, rows_y + 94, area.w, 28);
    draw_list_row(
        &mut win.canvas(),
        row0,
        b"Boot framebuffer",
        b"serial log until handoff",
        pal.warning,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row1,
        b"Window surfaces",
        b"submitted to compositor",
        pal.primary,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row2,
        b"Virtio scanout",
        b"native backend path",
        pal.success,
        false,
        false,
        THEME,
    );
    draw_list_row(
        &mut win.canvas(),
        row3,
        b"Visual goal",
        b"tasteful immersive desktop",
        pal.text_muted,
        false,
        false,
        THEME,
    );
}

fn render_security(win: &mut Window, area: Rect, state: &AppState) {
    let pal = tokens(THEME);
    {
        let mut canvas = win.canvas();
        canvas.draw_text(
            area.x,
            area.y,
            b"Press 1-4 or click a row to toggle.",
            pal.text_muted,
            area.w,
        );
    }

    let controls: [(&[u8], &[u8], bool); 4] = [
        (
            b"SMEP / SMAP",
            b"CPU execution protection for user code",
            state.security.smep_smap,
        ),
        (
            b"Audit Log",
            b"Security event trail for graph-native services",
            state.security.audit_log,
        ),
        (
            b"Seccomp",
            b"Per-task syscall allowlist for ring-3 workloads",
            state.security.seccomp,
        ),
        (
            b"Stack Guard",
            b"Stack protector for protected userspace",
            state.security.stack_guard,
        ),
    ];

    let mut idx = 0usize;
    while idx < controls.len() {
        let row = security_row_rect(area, idx);
        let hovered = contains(row, state.pointer_x, state.pointer_y);
        let accent = if controls[idx].2 { pal.success } else { pal.danger };
        let fill = if hovered { pal.surface } else { pal.surface_alt };
        let mut canvas = win.canvas();
        canvas.fill_rect(row.x, row.y, row.w, row.h, fill);
        canvas.draw_rect(row.x, row.y, row.w, row.h, pal.border);
        canvas.fill_rect(row.x, row.y, 3, row.h, accent);
        canvas.draw_text(row.x + 10, row.y + 8, controls[idx].0, pal.text, row.w - 64);
        canvas.draw_text(
            row.x + 10,
            row.y + 22,
            controls[idx].1,
            pal.text_muted,
            row.w - 64,
        );
        canvas.fill_rect(row.x + row.w as i32 - 44, row.y + 13, 28, 14, pal.surface);
        canvas.draw_rect(row.x + row.w as i32 - 44, row.y + 13, 28, 14, accent);
        let knob_x = if controls[idx].2 {
            row.x + row.w as i32 - 30
        } else {
            row.x + row.w as i32 - 44
        };
        canvas.fill_rect(knob_x, row.y + 14, 12, 12, accent);
        idx += 1;
    }
}

fn render_services(win: &mut Window, area: Rect, state: &AppState) {
    let pal = tokens(THEME);
    let card_w = area.w / 2 - 4;

    let mut online_buf = [0u8; 24];
    let mut online_len = write_u32_decimal(&mut online_buf, state.online_services());
    push_bytes(&mut online_buf, &mut online_len, b" / ");
    online_len += write_u32_decimal(&mut online_buf[online_len..], CORE_SERVICES.len() as u32);

    let mut gen_buf = [0u8; 24];
    let mut gen_len = 0usize;
    push_bytes(&mut gen_buf, &mut gen_len, b"gen ");
    gen_len += write_u64_decimal(&mut gen_buf[gen_len..], state.registry_generation);

    let mut graphd_buf = [0u8; 24];
    let graphd_meta = service_meta(&state.services[0], &mut graphd_buf);
    let mut mgr_buf = [0u8; 24];
    let mgr_meta = service_meta(&state.services[1], &mut mgr_buf);

    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y, card_w, CARD_H),
        b"Online",
        &online_buf[..online_len],
        pal.success,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y, card_w, CARD_H),
        b"Registry",
        &gen_buf[..gen_len],
        pal.primary,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"graphd",
        graphd_meta,
        pal.warning,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"servicemgr",
        mgr_meta,
        pal.text_muted,
        THEME,
    );

    let rows_y = area.y + (CARD_H as i32 * 2) + 30;
    draw_separator(&mut win.canvas(), area.x, rows_y, area.w, b"Service Registry", THEME);

    let mut idx = 0usize;
    while idx < CORE_SERVICES.len() {
        let row = Rect::new(area.x, rows_y + 10 + (idx as i32 * 28), area.w, 28);
        let mut meta_buf = [0u8; 24];
        let meta = service_meta(&state.services[idx], &mut meta_buf);
        draw_list_row(
            &mut win.canvas(),
            row,
            CORE_SERVICES[idx],
            meta,
            service_color(&state.services[idx], pal.success, pal.warning, pal.danger),
            false,
            false,
            THEME,
        );
        idx += 1;
    }
}

fn render_storage(win: &mut Window, area: Rect) {
    let pal = tokens(THEME);
    let card_w = area.w / 2 - 4;

    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y, card_w, CARD_H),
        b"Preferred Root",
        b"/graph",
        pal.primary,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y, card_w, CARD_H),
        b"Scratch",
        b"/tmp",
        pal.success,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"Assets",
        b"/boot",
        pal.warning,
        THEME,
    );
    draw_stat_card(
        &mut win.canvas(),
        Rect::new(area.x + card_w as i32 + 8, area.y + CARD_H as i32 + 8, card_w, CARD_H),
        b"User State",
        b"/data",
        pal.text_muted,
        THEME,
    );

    let rows_y = area.y + (CARD_H as i32 * 2) + 30;
    draw_separator(
        &mut win.canvas(),
        area.x,
        rows_y,
        area.w,
        b"Namespace Layout",
        THEME,
    );

    let rows: [(&[u8], &[u8], u32); 5] = [
        (b"/graph", b"graph-native workspace and service fabric", pal.primary),
        (b"/data", b"persistent user state", pal.text_muted),
        (b"/tmp", b"scratch pads and transient output", pal.success),
        (b"/boot", b"boot assets and packaging", pal.warning),
        (b"/", b"root namespace bridge", pal.border),
    ];
    let mut idx = 0usize;
    while idx < rows.len() {
        draw_list_row(
            &mut win.canvas(),
            Rect::new(area.x, rows_y + 10 + (idx as i32 * 28), area.w, 28),
            rows[idx].0,
            rows[idx].1,
            rows[idx].2,
            false,
            false,
            THEME,
        );
        idx += 1;
    }
}

fn render_about(win: &mut Window, area: Rect, state: &AppState) {
    let pal = tokens(THEME);
    let card_h = 40u32;

    let mut clock_buf = [0u8; 24];
    let mut clock_len = write_u64_decimal(&mut clock_buf, state.now_ms);
    push_bytes(&mut clock_buf, &mut clock_len, b" ms");

    let rows: [(&[u8], &[u8], u32); 6] = [
        (b"Profile", b"graph-first desktop OS", pal.primary),
        (b"Time Source", b"orchestrator node", pal.warning),
        (b"Desktop Path", b"ring-3 compositor takeover", pal.success),
        (b"Renderer", b"graphos-gl", pal.primary),
        (b"Clock", &clock_buf[..clock_len], pal.text_muted),
        (b"Goal", b"awesome over legacy", pal.success),
    ];

    let mut idx = 0usize;
    while idx < rows.len() {
        draw_stat_card(
            &mut win.canvas(),
            Rect::new(area.x, area.y + idx as i32 * (card_h as i32 + 4), area.w, card_h),
            rows[idx].0,
            rows[idx].1,
            rows[idx].2,
            THEME,
        );
        idx += 1;
    }
}

fn render_footer(win: &mut Window, state: &AppState) {
    let pal = tokens(THEME);
    let footer = footer_rect();
    let mut canvas = win.canvas();
    canvas.fill_rect(footer.x, footer.y, footer.w, footer.h, pal.surface);
    canvas.draw_hline(footer.x, footer.y, footer.w, pal.border);
    canvas.draw_text(footer.x + 8, footer.y + 5, state.status_text(), pal.text_muted, footer.w / 2);

    let mut clock_buf = [0u8; 32];
    let mut clock_len = 0usize;
    push_bytes(&mut clock_buf, &mut clock_len, b"orch ");
    clock_len += write_u64_decimal(&mut clock_buf[clock_len..], state.now_ms);
    push_bytes(&mut clock_buf, &mut clock_len, b" ms");
    let clock_w = (clock_len as u32).saturating_mul(5);
    let clock_x = footer.x + footer.w as i32 - clock_w as i32 - 8;
    canvas.draw_text(clock_x, footer.y + 5, &clock_buf[..clock_len], pal.primary, clock_w);
}

fn body_rect() -> Rect {
    Rect::new(0, 28, WIN_W, WIN_H - 28 - FOOTER_H - BAR_H)
}

fn sidebar_rect() -> Rect {
    let body = body_rect();
    Rect::new(body.x, body.y, SIDEBAR_W, body.h)
}

fn sidebar_item_rect(index: usize) -> Rect {
    let sidebar = sidebar_rect();
    Rect::new(
        sidebar.x,
        sidebar.y + index as i32 * SIDEBAR_ROW_H as i32,
        sidebar.w,
        SIDEBAR_ROW_H,
    )
}

fn content_rect() -> Rect {
    let body = body_rect();
    Rect::new(body.x + SIDEBAR_W as i32, body.y, body.w - SIDEBAR_W, body.h)
}

fn content_inner_rect() -> Rect {
    let rect = content_rect();
    Rect::new(rect.x + 8, rect.y + 24, rect.w - 16, rect.h - 28)
}

fn footer_rect() -> Rect {
    Rect::new(0, (WIN_H - BAR_H - FOOTER_H) as i32, WIN_W, FOOTER_H)
}

fn command_bar_rect() -> Rect {
    Rect::new(0, (WIN_H - BAR_H) as i32, WIN_W, BAR_H)
}

fn security_row_rect(area: Rect, index: usize) -> Rect {
    Rect::new(area.x, area.y + 22 + index as i32 * 42, area.w, 36)
}

fn service_color(service: &ServiceState, healthy: u32, warning: u32, danger: u32) -> u32 {
    if !service.present {
        danger
    } else if service.health == 0 {
        warning
    } else {
        healthy
    }
}

fn service_meta<'a>(service: &ServiceState, out: &'a mut [u8; 24]) -> &'a [u8] {
    let mut len = 0usize;
    if !service.present {
        push_bytes(out, &mut len, b"offline");
        return &out[..len];
    }
    push_bytes(out, &mut len, b"ch ");
    len += write_u32_decimal(&mut out[len..], service.alias);
    if service.health != 0 {
        push_bytes(out, &mut len, b" ok");
    } else {
        push_bytes(out, &mut len, b" up");
    }
    &out[..len]
}

fn contains(rect: Rect, x: i16, y: i16) -> bool {
    let x = x as i32;
    let y = y as i32;
    x >= rect.x && y >= rect.y && x < rect.x + rect.w as i32 && y < rect.y + rect.h as i32
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < bytes.len() && matches!(bytes[start], b' ' | b'\t' | b'\r' | b'\n') {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }
    &bytes[start..end]
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut idx = 0usize;
    while idx < a.len() {
        if to_ascii_lower(a[idx]) != to_ascii_lower(b[idx]) {
            return false;
        }
        idx += 1;
    }
    true
}

fn to_ascii_lower(byte: u8) -> u8 {
    if byte.is_ascii_uppercase() {
        byte + 32
    } else {
        byte
    }
}

fn push_bytes(out: &mut [u8], len: &mut usize, bytes: &[u8]) {
    let space = out.len().saturating_sub(*len);
    let take = bytes.len().min(space);
    if take == 0 {
        return;
    }
    out[*len..*len + take].copy_from_slice(&bytes[..take]);
    *len += take;
}

fn write_u32_decimal(out: &mut [u8], value: u32) -> usize {
    write_u64_decimal(out, value as u64)
}

fn write_u64_decimal(out: &mut [u8], mut value: u64) -> usize {
    if out.is_empty() {
        return 0;
    }
    if value == 0 {
        out[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut digits = 0usize;
    while value > 0 && digits < tmp.len() {
        tmp[digits] = b'0' + (value % 10) as u8;
        value /= 10;
        digits += 1;
    }
    let take = digits.min(out.len());
    let mut idx = 0usize;
    while idx < take {
        out[idx] = tmp[digits - idx - 1];
        idx += 1;
    }
    take
}

fn handle_pointer(state: &mut AppState, x: i16, y: i16, buttons: u8) -> bool {
    let mut dirty = state.pointer_x != x || state.pointer_y != y;
    state.pointer_x = x;
    state.pointer_y = y;

    let pressed = (buttons & 1) != 0;
    let was_pressed = (state.prev_buttons & 1) != 0;
    if pressed && !was_pressed {
        let mut idx = 0usize;
        while idx < CATEGORIES.len() {
            if contains(sidebar_item_rect(idx), x, y) {
                state.select_tab(idx);
                dirty = true;
            }
            idx += 1;
        }

        if state.selected == TAB_SECURITY {
            let area = content_inner_rect();
            if contains(security_row_rect(area, 0), x, y) {
                state.security.smep_smap = !state.security.smep_smap;
                dirty = true;
            } else if contains(security_row_rect(area, 1), x, y) {
                state.security.audit_log = !state.security.audit_log;
                dirty = true;
            } else if contains(security_row_rect(area, 2), x, y) {
                state.security.seccomp = !state.security.seccomp;
                dirty = true;
            } else if contains(security_row_rect(area, 3), x, y) {
                state.security.stack_guard = !state.security.stack_guard;
                dirty = true;
            }
        }
    }

    state.prev_buttons = buttons;
    dirty
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[settings] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => {
            runtime::write_line(b"[settings] channel_create failed\n");
            runtime::exit(1)
        }
    };

    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => {
            runtime::write_line(b"[settings] window open failed\n");
            runtime::exit(2)
        }
    };

    let mut state = AppState::new();
    render(&mut win, &state);
    win.request_focus();

    loop {
        let event = win.poll_event();
        let mut dirty = false;

        match event {
            Event::None => runtime::yield_now(),
            Event::FrameTick { now_ms } => {
                let old_phase = state.now_ms / 250;
                let new_phase = now_ms / 250;
                state.now_ms = now_ms;
                if state.last_refresh_ms == 0 || now_ms.saturating_sub(state.last_refresh_ms) >= 1000 {
                    state.refresh_runtime();
                    state.last_refresh_ms = now_ms;
                    dirty = true;
                }
                if old_phase != new_phase {
                    dirty = true;
                }
            }
            Event::PointerMove { x, y, buttons } => {
                dirty |= handle_pointer(&mut state, x, y, buttons);
            }
            Event::Key {
                pressed: true,
                ascii,
                hid_usage,
            } => {
                dirty = match ascii {
                    0x1B => runtime::exit(0),
                    0x08 | 0x7F => {
                        if state.cmd_len > 0 {
                            state.cmd_len -= 1;
                            true
                        } else {
                            false
                        }
                    }
                    0x0D | 0x0A => {
                        state.execute_command();
                        true
                    }
                    b'1' if state.selected == TAB_SECURITY => {
                        state.security.smep_smap = !state.security.smep_smap;
                        true
                    }
                    b'2' if state.selected == TAB_SECURITY => {
                        state.security.audit_log = !state.security.audit_log;
                        true
                    }
                    b'3' if state.selected == TAB_SECURITY => {
                        state.security.seccomp = !state.security.seccomp;
                        true
                    }
                    b'4' if state.selected == TAB_SECURITY => {
                        state.security.stack_guard = !state.security.stack_guard;
                        true
                    }
                    0x20..=0x7E => {
                        if state.cmd_len < state.cmd_input.len() {
                            state.cmd_input[state.cmd_len] = ascii;
                            state.cmd_len += 1;
                            true
                        } else {
                            false
                        }
                    }
                    _ => match hid_usage {
                        0x51 => {
                            state.select_tab((state.selected + 1) % CATEGORIES.len());
                            true
                        }
                        0x52 => {
                            let next = if state.selected == 0 {
                                CATEGORIES.len() - 1
                            } else {
                                state.selected - 1
                            };
                            state.select_tab(next);
                            true
                        }
                        _ => false,
                    },
                };
            }
            _ => {}
        }

        if dirty {
            render(&mut win, &state);
        }
    }
}

struct LineWriter {
    buf: [u8; 128],
    len: usize,
}

impl LineWriter {
    fn new() -> Self {
        Self {
            buf: [0u8; 128],
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
        let take = bytes.len().min(space);
        self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
        self.len += take;
        Ok(())
    }
}

fn append_u64_decimal(buf: &mut [u8], len: &mut usize, value: u64) {
    *len += write_u64_decimal(&mut buf[*len..], value);
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    let mut line = [0u8; 96];
    let mut len = 0usize;
    let prefix = b"[settings] panic at line ";
    line[..prefix.len()].copy_from_slice(prefix);
    len += prefix.len();
    if let Some(loc) = info.location() {
        append_u64_decimal(&mut line, &mut len, loc.line() as u64);
    }
    if len + 1 < line.len() {
        line[len] = b'\n';
        len += 1;
    }
    runtime::write_line(&line[..len]);

    let mut w = LineWriter::new();
    let _ = writeln!(&mut w, "[settings] {}", info);
    runtime::write_line(w.bytes());
    runtime::exit(255)
}
