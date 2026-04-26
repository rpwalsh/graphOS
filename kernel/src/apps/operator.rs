// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::bootstrap_manifest::GraphManifest;
use crate::drivers::{self, display};
use crate::gfx::canvas::Canvas;
use crate::gfx::surface::Surface;
use crate::graph::bootstrap;
use crate::input::event::InputEvent;
use crate::{mm, storage, task, vfs};

const TRANSCRIPT_COLS: usize = 72;
const TRANSCRIPT_LINES: usize = 8;

const PANEL_BG: u32 = 0x00111820;
const PANEL_EDGE: u32 = 0x00253442;
const PANEL_TEXT: u32 = 0x00dce7ef;
const PANEL_MUTED: u32 = 0x00889baa;
const HERO_BG: u32 = 0x00131f2d;
const HERO_OK: u32 = 0x0046b37b;
const HERO_WARN: u32 = 0x00d68e4a;
const HERO_TEXT: u32 = 0x00f3f7fb;

const ACTION_BUTTONS: [(i32, i32, u32, u32); 6] = [
    (28, 204, 112, 24),
    (156, 204, 112, 24),
    (28, 238, 112, 24),
    (156, 238, 112, 24),
    (28, 272, 112, 24),
    (156, 272, 112, 24),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum OperatorAction {
    Health,
    Graph,
    Launcher,
    RestartFabric,
    Reboot,
    Shutdown,
}

impl OperatorAction {
    const ALL: [Self; 6] = [
        Self::Health,
        Self::Graph,
        Self::Launcher,
        Self::RestartFabric,
        Self::Reboot,
        Self::Shutdown,
    ];

    const fn hotkey(self) -> u8 {
        match self {
            Self::Health => b'h',
            Self::Graph => b'g',
            Self::Launcher => b'l',
            Self::RestartFabric => b'p',
            Self::Reboot => b'r',
            Self::Shutdown => b's',
        }
    }

    const fn label(self) -> &'static [u8] {
        match self {
            Self::Health => b"Health",
            Self::Graph => b"Graph",
            Self::Launcher => b"Apps",
            Self::RestartFabric => b"Restart",
            Self::Reboot => b"Reboot",
            Self::Shutdown => b"Shutdown",
        }
    }

    const fn accent(self) -> u32 {
        match self {
            Self::Health => 0x004fa7d4,
            Self::Graph => 0x0048b19a,
            Self::Launcher => 0x005c9a68,
            Self::RestartFabric => 0x00d19a4e,
            Self::Reboot => 0x0082a4d5,
            Self::Shutdown => 0x00c86c62,
        }
    }

    const fn from_hotkey(byte: u8) -> Option<Self> {
        match byte {
            b'h' => Some(Self::Health),
            b'g' => Some(Self::Graph),
            b'l' => Some(Self::Launcher),
            b'p' => Some(Self::RestartFabric),
            b'r' => Some(Self::Reboot),
            b's' => Some(Self::Shutdown),
            _ => None,
        }
    }
}

pub struct OperatorApp {
    test_failures: u32,
    protected_ok: bool,
    transcript: [[u8; TRANSCRIPT_COLS]; TRANSCRIPT_LINES],
    transcript_lens: [u8; TRANSCRIPT_LINES],
    transcript_head: usize,
    transcript_count: usize,
    selected_action: OperatorAction,
}

impl OperatorApp {
    pub fn new(test_failures: u32, protected_ok: bool) -> Self {
        let mut app = Self {
            test_failures,
            protected_ok,
            transcript: [[0; TRANSCRIPT_COLS]; TRANSCRIPT_LINES],
            transcript_lens: [0; TRANSCRIPT_LINES],
            transcript_head: 0,
            transcript_count: 0,
            selected_action: OperatorAction::Health,
        };
        app.push_line(b"gui operator mode online");
        if protected_ok {
            app.push_line(b"layer3 fabric healthy -> desktop handoff locked");
        } else {
            app.push_line(b"layer3 fabric degraded -> desktop still available");
        }
        app.push_line(b"click cards or press h/g/l/p/r/s");
        app
    }

    pub fn handle_event(&mut self, event: InputEvent) {
        match event {
            InputEvent::Text(byte) => {
                if let Some(action) = OperatorAction::from_hotkey(byte) {
                    self.execute(action);
                }
            }
            InputEvent::Enter => self.execute(self.selected_action),
            _ => {}
        }
    }

    pub fn handle_click(&mut self, x: i32, y: i32) -> bool {
        for (index, action) in OperatorAction::ALL.iter().enumerate() {
            let (bx, by, bw, bh) = ACTION_BUTTONS[index];
            if x >= bx
                && y >= by
                && x < bx.saturating_add(bw as i32)
                && y < by.saturating_add(bh as i32)
            {
                self.execute(*action);
                return true;
            }
        }
        false
    }

    pub fn render(&self, surface: &mut Surface) {
        let display = display::telemetry_snapshot();
        let manifest = bootstrap::manifest();
        let width = surface.width();
        let mut canvas = Canvas::new(surface);
        canvas.clear(0x000b1016);

        canvas.fill_rect(0, 0, width, 48, HERO_BG);
        canvas.fill_rect(
            18,
            15,
            96,
            16,
            if self.protected_ok {
                HERO_OK
            } else {
                HERO_WARN
            },
        );
        canvas.draw_text(
            24,
            19,
            b"layer3",
            HERO_TEXT,
            if self.protected_ok {
                HERO_OK
            } else {
                HERO_WARN
            },
        );
        canvas.draw_text(132, 12, b"operator console", HERO_TEXT, HERO_BG);
        canvas.draw_text(
            132,
            24,
            if self.protected_ok {
                b"gui handoff active"
            } else {
                b"gui handoff degraded"
            },
            0x00a9c6dc,
            HERO_BG,
        );

        draw_panel(&mut canvas, 16, 60, 280, 128, b"runtime");
        draw_panel(&mut canvas, 16, 192, 280, 118, b"actions");
        draw_panel(&mut canvas, 308, 60, 308, 250, b"service fabric");
        draw_panel(&mut canvas, 16, 320, 600, 70, b"activity");

        let mut metric = MetricLine::new();
        metric.push_bytes(b"fabric ");
        metric.push_bytes(if self.protected_ok {
            b"healthy"
        } else {
            b"degraded"
        });
        draw_metric(
            &mut canvas,
            28,
            84,
            &metric.buf[..metric.len],
            status_color(self.protected_ok),
        );

        let mut metric = MetricLine::new();
        metric.push_bytes(b"tests ");
        metric.push_u64(self.test_failures as u64);
        metric.push_bytes(b" failures");
        draw_metric(&mut canvas, 28, 98, &metric.buf[..metric.len], PANEL_TEXT);

        let mut metric = MetricLine::new();
        metric.push_bytes(b"tasks ");
        metric.push_u64(task::table::count() as u64);
        metric.push_bytes(b" live");
        draw_metric(&mut canvas, 28, 112, &metric.buf[..metric.len], PANEL_TEXT);

        let mut metric = MetricLine::new();
        metric.push_bytes(b"frames ");
        metric.push_u64(mm::frame_alloc::available_frames() as u64);
        metric.push_bytes(b" free");
        draw_metric(&mut canvas, 28, 126, &metric.buf[..metric.len], PANEL_TEXT);

        let mut metric = MetricLine::new();
        metric.push_bytes(b"heap ");
        metric.push_u64((mm::heap::stats().free_large_bytes / 1024) as u64);
        metric.push_bytes(b" KiB large");
        draw_metric(&mut canvas, 28, 140, &metric.buf[..metric.len], PANEL_TEXT);

        let mut metric = MetricLine::new();
        metric.push_bytes(b"display ");
        metric.push_u64(display.mode.width as u64);
        metric.push_bytes(b"x");
        metric.push_u64(display.mode.height as u64);
        draw_metric(&mut canvas, 28, 154, &metric.buf[..metric.len], PANEL_TEXT);

        let mut metric = MetricLine::new();
        metric.push_bytes(b"drivers ");
        metric.push_u64(drivers::driver_count() as u64);
        metric.push_bytes(b" mounts ");
        metric.push_u64(vfs::mount_count() as u64);
        draw_metric(&mut canvas, 28, 168, &metric.buf[..metric.len], PANEL_MUTED);

        for (index, action) in OperatorAction::ALL.iter().enumerate() {
            let (x, y, w, h) = ACTION_BUTTONS[index];
            let focused = *action == self.selected_action;
            let bg = if focused { action.accent() } else { 0x00131d28 };
            let edge = if focused { 0x00eff4f8 } else { action.accent() };
            canvas.fill_rect(x, y, w, h, bg);
            canvas.stroke_rect(x, y, w, h, edge);

            let mut label = MetricLine::new();
            label.push_bytes(action.label());
            label.push_bytes(b" ");
            label.push_bytes(&[action.hotkey()]);
            canvas.draw_text(x + 8, y + 8, &label.buf[..label.len], 0x00f4f8fb, bg);
        }

        let ready_count = count_ready_services(manifest.as_ref());
        let degraded_count = count_degraded_services(manifest.as_ref());
        let mut summary = MetricLine::new();
        summary.push_bytes(b"ready ");
        summary.push_u64(ready_count as u64);
        summary.push_bytes(b" degraded ");
        summary.push_u64(degraded_count as u64);
        summary.push_bytes(b" persist ");
        summary.push_u64(storage::meta::entry_count() as u64);
        canvas.draw_text(320, 84, &summary.buf[..summary.len], PANEL_MUTED, PANEL_BG);

        render_services(&mut canvas, 320, 104, manifest.as_ref());

        render_transcript(
            &mut canvas,
            28,
            344,
            &self.transcript,
            &self.transcript_lens,
            self.transcript_head,
            self.transcript_count,
        );
    }

    fn execute(&mut self, action: OperatorAction) {
        self.selected_action = action;
        match action {
            OperatorAction::Health => {
                self.push_line(b"health report mirrored to runtime log");
                crate::emit_runtime_health_report_for_desktop(
                    self.test_failures,
                    self.protected_ok,
                );
            }
            OperatorAction::Graph => {
                let manifest = bootstrap::manifest();
                let ready = count_ready_services(manifest.as_ref());
                let mut line = MetricLine::new();
                line.push_bytes(b"graph snapshot ready=");
                line.push_u64(ready as u64);
                self.push_line(&line.buf[..line.len]);
                crate::emit_bootstrap_graph_dump_for_desktop();
            }
            OperatorAction::Launcher => {
                if crate::userland::spawn_named_service(b"launcher").is_some() {
                    self.push_line(b"apps launcher opened");
                } else {
                    self.push_line(b"apps launcher failed");
                }
            }
            OperatorAction::RestartFabric => {
                self.push_line(b"restarting protected fabric");
                self.protected_ok =
                    crate::restart_protected_fabric_from_desktop(self.test_failures);
                self.push_line(if self.protected_ok {
                    b"fabric restart complete: healthy"
                } else {
                    b"fabric restart complete: degraded"
                });
            }
            OperatorAction::Reboot => {
                self.push_line(b"operator requested reboot");
                crate::reboot_graphos_from_desktop();
            }
            OperatorAction::Shutdown => {
                self.push_line(b"operator requested shutdown");
                crate::shutdown_graphos_from_desktop();
            }
        }
    }

    fn push_line(&mut self, bytes: &[u8]) {
        let slot = self.transcript_head;
        let len = bytes.len().min(TRANSCRIPT_COLS);
        self.transcript[slot].fill(0);
        self.transcript[slot][..len].copy_from_slice(&bytes[..len]);
        self.transcript_lens[slot] = len as u8;
        self.transcript_head = (self.transcript_head + 1) % TRANSCRIPT_LINES;
        self.transcript_count = self.transcript_count.min(TRANSCRIPT_LINES - 1) + 1;
    }
}

fn draw_panel(canvas: &mut Canvas<'_, Surface>, x: i32, y: i32, w: u32, h: u32, title: &[u8]) {
    canvas.fill_rect(x, y, w, h, PANEL_BG);
    canvas.stroke_rect(x, y, w, h, PANEL_EDGE);
    canvas.fill_rect(x, y, w, 1, 0x00365063);
    canvas.draw_text(x + 12, y + 10, title, 0x00eef4f8, PANEL_BG);
}

fn draw_metric(canvas: &mut Canvas<'_, Surface>, x: i32, y: i32, text: &[u8], color: u32) {
    canvas.draw_text(x, y, text, color, PANEL_BG);
}

fn render_services(
    canvas: &mut Canvas<'_, Surface>,
    x: i32,
    mut y: i32,
    manifest: Option<&GraphManifest>,
) {
    let Some(manifest) = manifest else {
        canvas.draw_text(x, y, b"manifest unavailable", PANEL_MUTED, PANEL_BG);
        return;
    };

    for service in manifest.services.iter().take(10) {
        let health = bootstrap::service_health(&service.name)
            .unwrap_or(crate::graph::bootstrap::ServiceHealth::Defined);
        let bg = service_health_bg(health);
        let fg = service_health_fg(health);

        let mut line = MetricLine::new();
        line.push_bytes(if service.critical { b"* " } else { b"  " });
        line.push_bytes(&service.name);
        canvas.fill_rect(x, y - 2, 284, 14, bg);
        canvas.draw_text(x + 8, y, &line.buf[..line.len], fg, bg);
        canvas.draw_text(x + 188, y, health.as_bytes(), fg, bg);
        y += 16;
    }
}

fn render_transcript(
    canvas: &mut Canvas<'_, Surface>,
    x: i32,
    y: i32,
    lines: &[[u8; TRANSCRIPT_COLS]; TRANSCRIPT_LINES],
    lens: &[u8; TRANSCRIPT_LINES],
    head: usize,
    count: usize,
) {
    let visible = count.min(TRANSCRIPT_LINES);
    let first = count.saturating_sub(visible);
    let mut row_y = y;
    for row in 0..visible {
        let logical = first + row;
        let slot = if count < TRANSCRIPT_LINES {
            logical
        } else {
            (head + logical) % TRANSCRIPT_LINES
        };
        canvas.draw_text(
            x,
            row_y,
            &lines[slot][..lens[slot] as usize],
            PANEL_TEXT,
            PANEL_BG,
        );
        row_y += 10;
    }
}

fn count_ready_services(manifest: Option<&GraphManifest>) -> usize {
    let Some(manifest) = manifest else {
        return 0;
    };
    manifest
        .services
        .iter()
        .filter(|service| {
            matches!(
                bootstrap::service_health(&service.name),
                Some(crate::graph::bootstrap::ServiceHealth::Ready)
            )
        })
        .count()
}

fn count_degraded_services(manifest: Option<&GraphManifest>) -> usize {
    let Some(manifest) = manifest else {
        return 0;
    };
    manifest
        .services
        .iter()
        .filter(|service| {
            matches!(
                bootstrap::service_health(&service.name),
                Some(crate::graph::bootstrap::ServiceHealth::Degraded)
                    | Some(crate::graph::bootstrap::ServiceHealth::Failed)
                    | Some(crate::graph::bootstrap::ServiceHealth::Missing)
            )
        })
        .count()
}

fn status_color(healthy: bool) -> u32 {
    if healthy { 0x0066d199 } else { 0x00e0a46b }
}

fn service_health_bg(health: crate::graph::bootstrap::ServiceHealth) -> u32 {
    match health {
        crate::graph::bootstrap::ServiceHealth::Ready => 0x0017261d,
        crate::graph::bootstrap::ServiceHealth::Degraded => 0x00231c15,
        crate::graph::bootstrap::ServiceHealth::Failed
        | crate::graph::bootstrap::ServiceHealth::Missing => 0x002a1515,
        crate::graph::bootstrap::ServiceHealth::Launched => 0x001a1f27,
        _ => 0x00121820,
    }
}

fn service_health_fg(health: crate::graph::bootstrap::ServiceHealth) -> u32 {
    match health {
        crate::graph::bootstrap::ServiceHealth::Ready => 0x008ee0b0,
        crate::graph::bootstrap::ServiceHealth::Degraded => 0x00efc079,
        crate::graph::bootstrap::ServiceHealth::Failed
        | crate::graph::bootstrap::ServiceHealth::Missing => 0x00f0918b,
        crate::graph::bootstrap::ServiceHealth::Launched => 0x00b7cae0,
        _ => PANEL_TEXT,
    }
}

struct MetricLine {
    buf: [u8; 80],
    len: usize,
}

impl MetricLine {
    const fn new() -> Self {
        Self {
            buf: [0; 80],
            len: 0,
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if self.len >= self.buf.len() {
            return;
        }
        let count = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
    }

    fn push_u64(&mut self, mut value: u64) {
        if value == 0 {
            self.push_bytes(b"0");
            return;
        }

        let mut digits = [0u8; 20];
        let mut len = 0usize;
        while value > 0 {
            digits[len] = b'0' + (value % 10) as u8;
            value /= 10;
            len += 1;
        }
        while len > 0 {
            len -= 1;
            self.push_bytes(&digits[len..len + 1]);
        }
    }
}
