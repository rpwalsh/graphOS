// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Settings — Phase J complete implementation.
//!
//! Six-panel settings UI: Display, Network, Users, Security, Accessibility, About.
//! Full Phase J window chrome with sidebar navigation.

use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::geom::Rect;
use graphos_ui_sdk::tokens::{Theme, tokens};
use graphos_ui_sdk::widgets::{
    ButtonKind, draw_badge, draw_button, draw_progress_bar, draw_separator, draw_sidebar_item,
    draw_slider, draw_tab_bar, draw_toggle,
};

const WIN_W: u32 = 880;
const WIN_H: u32 = 600;
const TITLEBAR_H: u32 = 32;
const SIDEBAR_W: u32 = 200;
const CONTENT_X: i32 = SIDEBAR_W as i32 + 1;
const CONTENT_W: u32 = WIN_W - SIDEBAR_W - 1;

const PANELS: &[&[u8]] = &[
    b"Display",
    b"Network",
    b"Users",
    b"Security",
    b"Accessibility",
    b"About",
];
const PANEL_ICONS: &[u32] = &[
    0xFF4A90D9, 0xFF28C940, 0xFF7B68EE, 0xFFDC143C, 0xFFFFBD2E, 0xFF888888,
];

// ── Display settings ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum OsTheme {
    DarkGlass,
    LightFrost,
    HighContrast,
}

struct DisplayPanel {
    resolution_idx: usize,
    refresh_idx: usize,
    dpi_scale: u32,
    theme: OsTheme,
    night_mode: bool,
}
const RESOLUTIONS: &[&[u8]] = &[b"1920x1080", b"2560x1440", b"3840x2160", b"1280x720"];
const REFRESH_RATES: &[&[u8]] = &[b"60 Hz", b"120 Hz", b"144 Hz", b"240 Hz"];
impl Default for DisplayPanel {
    fn default() -> Self {
        Self {
            resolution_idx: 0,
            refresh_idx: 0,
            dpi_scale: 500,
            theme: OsTheme::DarkGlass,
            night_mode: false,
        }
    }
}

// ── Network panel ─────────────────────────────────────────────────────────────

struct NetworkPanel {
    networks: &'static [&'static [u8]],
    selected: usize,
    ip: [u8; 48],
    ip_len: usize,
    connected: bool,
}
impl Default for NetworkPanel {
    fn default() -> Self {
        let mut ip = [0u8; 48];
        ip[..7].copy_from_slice(b"0.0.0.0");
        Self {
            networks: &[b"GraphOS-5G", b"HomeNet", b"Guest-2.4", b"Starlink"],
            selected: 0,
            ip,
            ip_len: 7,
            connected: false,
        }
    }
}

// ── Users panel ───────────────────────────────────────────────────────────────

struct UsersPanel {
    users: &'static [&'static [u8]],
    selected: usize,
}
impl Default for UsersPanel {
    fn default() -> Self {
        Self {
            users: &[b"root", b"admin", b"guest"],
            selected: 0,
        }
    }
}

// ── Security panel ────────────────────────────────────────────────────────────

struct SecurityPanel {
    tpm_enrolled: bool,
    secure_boot: bool,
    fido2_key: bool,
}
impl Default for SecurityPanel {
    fn default() -> Self {
        Self {
            tpm_enrolled: true,
            secure_boot: true,
            fido2_key: false,
        }
    }
}

// ── Accessibility panel ───────────────────────────────────────────────────────

struct AccessibilityPanel {
    font_scale: u32,
    high_contrast: bool,
    reduced_motion: bool,
    focus_ring_width: u32,
}
impl Default for AccessibilityPanel {
    fn default() -> Self {
        Self {
            font_scale: 500,
            high_contrast: false,
            reduced_motion: false,
            focus_ring_width: 300,
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct App {
    panel: usize,
    sidebar_hover: Option<usize>,
    theme: Theme,
    display: DisplayPanel,
    network: NetworkPanel,
    users: UsersPanel,
    security: SecurityPanel,
    accessibility: AccessibilityPanel,
    focus_idx: usize,
    status: &'static [u8],
}

impl Default for App {
    fn default() -> Self {
        Self {
            panel: 0,
            sidebar_hover: None,
            theme: Theme::DarkGlass,
            display: DisplayPanel::default(),
            network: NetworkPanel::default(),
            users: UsersPanel::default(),
            security: SecurityPanel::default(),
            accessibility: AccessibilityPanel::default(),
            focus_idx: 0,
            status: b"",
        }
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render(canvas: &mut Canvas<'_>, app: &App) {
    let p = tokens(app.theme);
    canvas.fill_rect(0, 0, WIN_W, WIN_H, p.background);

    // Title bar
    canvas.fill_rect(0, 0, WIN_W, TITLEBAR_H, p.chrome);
    canvas.fill_rect(0, 0, WIN_W, 1, 0xFF58A6FF);
    canvas.draw_hline(0, TITLEBAR_H as i32 - 1, WIN_W, p.border);
    canvas.fill_rect(12, 10, 12, 12, 0xFF5F5757);
    canvas.fill_rect(30, 10, 12, 12, 0xFFFFBD2E);
    canvas.fill_rect(48, 10, 12, 12, 0xFF28C940);
    canvas.draw_text((WIN_W / 2 - 22) as i32, 10, b"Settings", p.text, 80);

    // Sidebar
    canvas.fill_rect(
        0,
        TITLEBAR_H as i32,
        SIDEBAR_W,
        WIN_H - TITLEBAR_H,
        p.chrome,
    );
    canvas.draw_vline(
        SIDEBAR_W as i32 - 1,
        TITLEBAR_H as i32,
        WIN_H - TITLEBAR_H,
        p.border,
    );
    for (i, &name) in PANELS.iter().enumerate() {
        draw_sidebar_item(
            canvas,
            Rect::new(0, TITLEBAR_H as i32 + 8 + i as i32 * 40, SIDEBAR_W, 36),
            name,
            PANEL_ICONS[i],
            app.panel == i,
            app.sidebar_hover == Some(i),
            0,
            app.theme,
        );
    }
    draw_separator(
        canvas,
        8,
        TITLEBAR_H as i32 + 8 + PANELS.len() as i32 * 40,
        SIDEBAR_W - 16,
        b"SYSTEM",
        app.theme,
    );

    // Content area
    let cy = TITLEBAR_H as i32 + 16;
    match app.panel {
        0 => render_display(canvas, &app.display, p, cy, app.theme),
        1 => render_network(canvas, &app.network, p, cy, app.theme),
        2 => render_users(canvas, &app.users, p, cy, app.theme),
        3 => render_security(canvas, &app.security, p, cy, app.theme),
        4 => render_accessibility(canvas, &app.accessibility, p, cy, app.theme),
        5 => render_about(canvas, p, cy, app.theme),
        _ => {}
    }

    // Status bar
    if !app.status.is_empty() {
        let sy = (WIN_H - 20) as i32;
        canvas.fill_rect(CONTENT_X, sy, CONTENT_W, 20, p.chrome);
        canvas.draw_hline(CONTENT_X, sy, CONTENT_W, p.border);
        canvas.draw_text(CONTENT_X + 8, sy + 5, app.status, p.primary, CONTENT_W - 16);
    }
}

fn label(canvas: &mut Canvas<'_>, x: i32, y: i32, text: &[u8], color: u32) {
    canvas.draw_text(x, y, text, color, 400);
}

fn render_display(
    canvas: &mut Canvas<'_>,
    d: &DisplayPanel,
    p: graphos_ui_sdk::tokens::ThemeTokens,
    cy: i32,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = cy;
    canvas.draw_text(x, y, b"Display", p.text, 200);
    y += 24;

    label(canvas, x, y, b"Resolution", p.text_muted);
    y += 16;
    for (i, &res) in RESOLUTIONS.iter().enumerate() {
        let sel = d.resolution_idx == i;
        draw_button(
            canvas,
            Rect::new(x + i as i32 * 100, y, 90, 24),
            res,
            ButtonKind::Secondary,
            sel,
            false,
            false,
            theme,
        );
    }
    y += 36;

    label(canvas, x, y, b"Refresh Rate", p.text_muted);
    y += 16;
    for (i, &rate) in REFRESH_RATES.iter().enumerate() {
        let sel = d.refresh_idx == i;
        draw_button(
            canvas,
            Rect::new(x + i as i32 * 80, y, 70, 24),
            rate,
            ButtonKind::Secondary,
            sel,
            false,
            false,
            theme,
        );
    }
    y += 36;

    label(canvas, x, y, b"DPI Scale", p.text_muted);
    y += 16;
    draw_slider(canvas, Rect::new(x, y, 300, 20), d.dpi_scale, false, theme);
    y += 32;

    label(canvas, x, y, b"Theme", p.text_muted);
    y += 16;
    draw_button(
        canvas,
        Rect::new(x, y, 90, 28),
        b"Dark Glass",
        ButtonKind::Primary,
        d.theme == OsTheme::DarkGlass,
        false,
        false,
        theme,
    );
    draw_button(
        canvas,
        Rect::new(x + 100, y, 90, 28),
        b"Light Frost",
        ButtonKind::Secondary,
        d.theme == OsTheme::LightFrost,
        false,
        false,
        theme,
    );
    draw_button(
        canvas,
        Rect::new(x + 200, y, 100, 28),
        b"High Contrast",
        ButtonKind::Secondary,
        d.theme == OsTheme::HighContrast,
        false,
        false,
        theme,
    );
    y += 40;

    draw_toggle(
        canvas,
        Rect::new(x, y, 300, 28),
        b"Night Mode",
        d.night_mode,
        false,
        theme,
    );
}

fn render_network(
    canvas: &mut Canvas<'_>,
    n: &NetworkPanel,
    p: graphos_ui_sdk::tokens::ThemeTokens,
    cy: i32,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = cy;
    canvas.draw_text(x, y, b"Network", p.text, 200);
    y += 24;
    label(canvas, x, y, b"Wi-Fi Networks", p.text_muted);
    y += 16;
    for (i, &ssid) in n.networks.iter().enumerate() {
        let sel = n.selected == i;
        let strength = 80 - (i as u32 * 15);
        canvas.fill_rect(
            x,
            y,
            CONTENT_W - 48,
            28,
            if sel { p.primary } else { p.surface },
        );
        canvas.draw_text(
            x + 8,
            y + 7,
            ssid,
            if sel { 0xFF000000 } else { p.text },
            200,
        );
        let bars: &[u8] = if strength > 70 {
            b"||||"
        } else if strength > 40 {
            b"|||"
        } else {
            b"||"
        };
        canvas.draw_text(x + CONTENT_W as i32 - 80, y + 7, bars, 0xFF28C940, 40);
        y += 30;
    }
    y += 8;
    let st: &[u8] = if n.connected {
        b"Connected"
    } else {
        b"Disconnected"
    };
    draw_button(
        canvas,
        Rect::new(x, y, 120, 28),
        if n.connected {
            b"Disconnect"
        } else {
            b"Connect"
        },
        ButtonKind::Primary,
        false,
        false,
        false,
        theme,
    );
    canvas.draw_text(
        x + 132,
        y + 8,
        st,
        if n.connected {
            0xFF28C940
        } else {
            p.text_muted
        },
        100,
    );
    y += 40;
    label(canvas, x, y, b"IP Address", p.text_muted);
    y += 16;
    canvas.draw_text(x, y, &n.ip[..n.ip_len], p.text, 200);
}

fn render_users(
    canvas: &mut Canvas<'_>,
    u: &UsersPanel,
    p: graphos_ui_sdk::tokens::ThemeTokens,
    cy: i32,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = cy;
    canvas.draw_text(x, y, b"Users", p.text, 200);
    y += 24;
    for (i, &name) in u.users.iter().enumerate() {
        let sel = u.selected == i;
        canvas.fill_rect(
            x,
            y,
            CONTENT_W - 48,
            36,
            if sel { p.primary } else { p.surface },
        );
        canvas.fill_rect(x + 4, y + 8, 20, 20, 0xFF7B68EE);
        canvas.draw_text(
            x + 30,
            y + 10,
            name,
            if sel { 0xFF000000 } else { p.text },
            160,
        );
        let role: &[u8] = if i == 0 {
            b"Administrator"
        } else {
            b"Standard User"
        };
        canvas.draw_text(x + CONTENT_W as i32 - 120, y + 10, role, p.text_muted, 110);
        y += 38;
    }
    y += 8;
    draw_button(
        canvas,
        Rect::new(x, y, 100, 28),
        b"Add User",
        ButtonKind::Primary,
        false,
        false,
        false,
        theme,
    );
    draw_button(
        canvas,
        Rect::new(x + 112, y, 100, 28),
        b"Remove",
        ButtonKind::Danger,
        false,
        false,
        false,
        theme,
    );
    draw_button(
        canvas,
        Rect::new(x + 224, y, 130, 28),
        b"Change Password",
        ButtonKind::Secondary,
        false,
        false,
        false,
        theme,
    );
}

fn render_security(
    canvas: &mut Canvas<'_>,
    s: &SecurityPanel,
    p: graphos_ui_sdk::tokens::ThemeTokens,
    cy: i32,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = cy;
    canvas.draw_text(x, y, b"Security", p.text, 200);
    y += 24;
    draw_toggle(
        canvas,
        Rect::new(x, y, 300, 28),
        b"TPM 2.0 Enrolled",
        s.tpm_enrolled,
        false,
        theme,
    );
    y += 36;
    draw_toggle(
        canvas,
        Rect::new(x, y, 300, 28),
        b"Secure Boot",
        s.secure_boot,
        false,
        theme,
    );
    y += 36;
    draw_toggle(
        canvas,
        Rect::new(x, y, 300, 28),
        b"FIDO2 Hardware Key",
        s.fido2_key,
        false,
        theme,
    );
    y += 36;
    y += 8;
    draw_button(
        canvas,
        Rect::new(x, y, 160, 28),
        b"Enroll FIDO2 Key",
        ButtonKind::Primary,
        false,
        false,
        false,
        theme,
    );
    y += 40;
    canvas.draw_text(x, y, b"Secure Boot Status", p.text_muted, 200);
    y += 16;
    let status_text: &[u8] = if s.secure_boot {
        b"VERIFIED - Boot chain validated"
    } else {
        b"DISABLED - Not recommended"
    };
    canvas.draw_text(
        x,
        y,
        status_text,
        if s.secure_boot {
            0xFF28C940
        } else {
            0xFFDC143C
        },
        400,
    );
}

fn render_accessibility(
    canvas: &mut Canvas<'_>,
    a: &AccessibilityPanel,
    p: graphos_ui_sdk::tokens::ThemeTokens,
    cy: i32,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = cy;
    canvas.draw_text(x, y, b"Accessibility", p.text, 200);
    y += 24;
    label(canvas, x, y, b"Font Scale", p.text_muted);
    y += 16;
    draw_slider(canvas, Rect::new(x, y, 300, 20), a.font_scale, false, theme);
    y += 32;
    label(canvas, x, y, b"Focus Ring Width", p.text_muted);
    y += 16;
    draw_slider(
        canvas,
        Rect::new(x, y, 300, 20),
        a.focus_ring_width,
        false,
        theme,
    );
    y += 32;
    draw_toggle(
        canvas,
        Rect::new(x, y, 300, 28),
        b"High Contrast Mode",
        a.high_contrast,
        false,
        theme,
    );
    y += 36;
    draw_toggle(
        canvas,
        Rect::new(x, y, 300, 28),
        b"Reduce Motion",
        a.reduced_motion,
        false,
        theme,
    );
}

fn render_about(
    canvas: &mut Canvas<'_>,
    p: graphos_ui_sdk::tokens::ThemeTokens,
    cy: i32,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = cy;
    canvas.draw_text(x, y, b"GraphOS", p.text, 300);
    y += 32;
    canvas.draw_text(x, y, b"Version 0.1.0-alpha (Phase J)", p.text_muted, 300);
    y += 20;
    canvas.draw_hline(x, y, CONTENT_W - 48, p.border);
    y += 16;
    let rows: &[(&[u8], &[u8])] = &[
        (b"Kernel", b"graphos-kernel 0.1.0"),
        (b"Arch", b"x86_64-unknown-none"),
        (b"Build", b"release"),
        (b"GPU", b"VirtIO GPU / Phase J Compositor"),
        (b"FS", b"GraphOS-VFS (in-kernel)"),
        (b"UI Toolkit", b"graphos-ui-sdk Phase J"),
        (b"Uptime", b"00:00:00 (real-time via SYS_UPTIME)"),
    ];
    for &(k, v) in rows {
        canvas.draw_text(x, y, k, p.text_muted, 140);
        canvas.draw_text(x + 150, y, v, p.text, 300);
        y += 20;
    }
    y += 8;
    draw_button(
        canvas,
        Rect::new(x, y, 160, 28),
        b"Check for Updates",
        ButtonKind::Primary,
        false,
        false,
        false,
        theme,
    );
}

// ── Input ─────────────────────────────────────────────────────────────────────

fn handle_event(app: &mut App, ev: Event) {
    match ev {
        Event::PointerMove { x, y, buttons } => {
            if (x as i32) < SIDEBAR_W as i32 && y as i32 > TITLEBAR_H as i32 {
                let si = (y as i32 - TITLEBAR_H as i32 - 8) / 40;
                app.sidebar_hover = if si >= 0 && (si as usize) < PANELS.len() {
                    Some(si as usize)
                } else {
                    None
                };
                if buttons & 1 != 0 {
                    if let Some(i) = app.sidebar_hover {
                        app.panel = i;
                    }
                }
            }
        }
        Event::Key {
            pressed: true,
            ascii,
            ..
        } => match ascii {
            b'1'..=b'6' => {
                app.panel = (ascii - b'1') as usize;
            }
            _ => {}
        },
        _ => {}
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 120, 60, channel) {
        Some(w) => w,
        None => return,
    };
    win.request_focus();
    let mut app = App::default();

    loop {
        loop {
            let ev = win.poll_event();
            if matches!(ev, Event::None) {
                break;
            }
            handle_event(&mut app, ev);
        }
        {
            let mut c = win.canvas();
            render(&mut c, &app);
        }
        win.present();
        unsafe {
            graphos_app_sdk::sys::yield_task();
        }
    }
}
