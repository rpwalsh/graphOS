// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS App Store — Phase J complete client UI.
//!
//! Browse, install, and manage apps for GraphOS:
//! - Category sidebar: All, Games, Productivity, Dev, Utilities
//! - App grid with icon placeholder, name, version, rating stars
//! - Detail view: description, screenshots, Install/Update button, version history
//! - Search bar (Ctrl+F)
//! - Installed apps management panel
//! - Phase J frosted glass chrome

use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::geom::Rect;
use graphos_ui_sdk::tokens::ThemeTokens;
use graphos_ui_sdk::tokens::{Theme, tokens};
use graphos_ui_sdk::widgets::{
    ButtonKind, draw_badge, draw_button, draw_progress_bar, draw_separator, draw_sidebar_item,
};

const WIN_W: u32 = 1000;
const WIN_H: u32 = 680;
const TITLEBAR_H: u32 = 32;
const SEARCH_H: u32 = 40;
const STATUS_H: u32 = 20;
const SIDEBAR_W: u32 = 180;
const CONTENT_X: i32 = SIDEBAR_W as i32 + 1;
const CONTENT_W: u32 = WIN_W - SIDEBAR_W - 1;
const CONTENT_Y: i32 = (TITLEBAR_H + SEARCH_H) as i32;
const CONTENT_H: u32 = WIN_H - TITLEBAR_H - SEARCH_H - STATUS_H;
const CARD_W: u32 = 160;
const CARD_H: u32 = 180;
const CARD_COLS: u32 = (CONTENT_W - 280) / (CARD_W + 16);

#[derive(Clone, Copy, PartialEq)]
enum Category {
    All,
    Games,
    Productivity,
    Dev,
    Utilities,
    Installed,
}

const CATEGORIES: &[(Category, &[u8], u32)] = &[
    (Category::All, b"All Apps", 0xFF4A90D9),
    (Category::Games, b"Games", 0xFFDC143C),
    (Category::Productivity, b"Productivity", 0xFF28C940),
    (Category::Dev, b"Dev Tools", 0xFFFFBD2E),
    (Category::Utilities, b"Utilities", 0xFF7B68EE),
    (Category::Installed, b"Installed", 0xFF888888),
];

#[derive(Clone, Copy)]
struct AppEntry {
    name: &'static [u8],
    version: &'static [u8],
    author: &'static [u8],
    description: &'static [u8],
    category: Category,
    color: u32,
    rating: u32, // out of 50 (×10)
    size_kb: u32,
    installed: bool,
}

const APPS: &[AppEntry] = &[
    AppEntry { name: b"AI Console",   version: b"1.0.0", author: b"GraphOS Team", description: b"SCCE pipeline AI assistant with evidence provenance and contradiction detection.", category: Category::Productivity, color: 0xFF58A6FF, rating: 48, size_kb: 128, installed: true },
    AppEntry { name: b"Air Hockey",   version: b"1.2.0", author: b"GraphOS Team", description: b"Physics-based air hockey with AI opponent, spring physics, and particle effects.", category: Category::Games, color: 0xFFDC143C, rating: 45, size_kb: 96, installed: true },
    AppEntry { name: b"Terminal",     version: b"2.0.0", author: b"GraphOS Team", description: b"Full xterm-256color terminal emulator with VT220, multi-tab, find overlay.", category: Category::Dev, color: 0xFF4A90D9, rating: 50, size_kb: 256, installed: true },
    AppEntry { name: b"Files",        version: b"1.5.0", author: b"GraphOS Team", description: b"Two-pane file manager with VFS, bookmarks, preview pane, sort, and search.", category: Category::Productivity, color: 0xFF28C940, rating: 47, size_kb: 192, installed: true },
    AppEntry { name: b"Editor",       version: b"1.3.0", author: b"GraphOS Team", description: b"Modal text editor with gap buffer, syntax highlight, undo/redo, Vim keybindings.", category: Category::Dev, color: 0xFFFFBD2E, rating: 46, size_kb: 160, installed: true },
    AppEntry { name: b"Browser",      version: b"0.9.0", author: b"GraphOS Team", description: b"Lightweight browser with HTML subset renderer, multi-tab, bookmarks.", category: Category::Utilities, color: 0xFF7B68EE, rating: 40, size_kb: 144, installed: true },
    AppEntry { name: b"Shell3D",      version: b"1.0.0", author: b"GraphOS Team", description: b"3D spatial app launcher with hemisphere card layout, spring physics, orbit navigation.", category: Category::Utilities, color: 0xFF888888, rating: 44, size_kb: 112, installed: true },
    AppEntry { name: b"Settings",     version: b"1.1.0", author: b"GraphOS Team", description: b"System settings: display, network, users, security, accessibility.", category: Category::Utilities, color: 0xFFAAAAAA, rating: 43, size_kb: 128, installed: true },
    AppEntry { name: b"Pixel Paint",  version: b"0.3.0", author: b"Community",    description: b"Raster graphics editor with layers, blend modes, and palette tools.", category: Category::Productivity, color: 0xFFFF8C00, rating: 35, size_kb: 320, installed: false },
    AppEntry { name: b"Chess",        version: b"1.0.0", author: b"Community",    description: b"Chess engine with minimax AI, piece animations, and PGN export.", category: Category::Games, color: 0xFFCCCCCC, rating: 42, size_kb: 200, installed: false },
    AppEntry { name: b"Calc",         version: b"1.0.0", author: b"GraphOS Team", description: b"Scientific calculator with expression parser and history.", category: Category::Utilities, color: 0xFF00CCAA, rating: 38, size_kb: 64, installed: false },
    AppEntry { name: b"Hex Editor",   version: b"0.5.0", author: b"Community",    description: b"Binary file hex editor with search, patch, and structure annotation.", category: Category::Dev, color: 0xFFCC4444, rating: 41, size_kb: 96, installed: false },
];

struct Detail {
    app_idx: usize,
    install_progress: u32,
    installing: bool,
}

struct App {
    category: Category,
    hover_card: Option<usize>,
    selected: Option<usize>,
    detail: Option<Detail>,
    search: [u8; 64],
    search_len: usize,
    search_active: bool,
    scroll: u32,
    sidebar_hover: Option<usize>,
    theme: Theme,
    status: &'static [u8],
}

impl App {
    fn new() -> Self {
        Self {
            category: Category::All,
            hover_card: None,
            selected: None,
            detail: None,
            search: [0u8; 64],
            search_len: 0,
            search_active: false,
            scroll: 0,
            sidebar_hover: None,
            theme: Theme::DarkGlass,
            status: b"Browse and install apps for GraphOS.",
        }
    }

    fn visible_apps(&self) -> Vec<usize> {
        let q = &self.search[..self.search_len];
        (0..APPS.len())
            .filter(|&i| {
                let app = &APPS[i];
                let cat_ok = match self.category {
                    Category::All => true,
                    Category::Installed => app.installed,
                    c => app.category == c,
                };
                if !cat_ok {
                    return false;
                }
                if q.is_empty() {
                    return true;
                }
                app.name.windows(q.len()).any(|w| {
                    w.iter()
                        .zip(q.iter())
                        .all(|(&a, &b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
                })
            })
            .collect()
    }
}

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
    canvas.draw_text((WIN_W / 2 - 28) as i32, 10, b"App Store", p.text, 80);

    // Search bar
    let sy2 = TITLEBAR_H as i32;
    canvas.fill_rect(0, sy2, WIN_W, SEARCH_H, p.chrome);
    canvas.draw_hline(0, sy2 + SEARCH_H as i32 - 1, WIN_W, p.border);
    let sb_x = SIDEBAR_W as i32 + 8;
    let sb_w = WIN_W - SIDEBAR_W - 16;
    canvas.draw_rect(
        sb_x,
        sy2 + 6,
        sb_w,
        SEARCH_H - 12,
        if app.search_active {
            p.primary
        } else {
            p.border
        },
    );
    if app.search_len > 0 {
        canvas.draw_text(
            sb_x + 8,
            sy2 + 13,
            &app.search[..app.search_len],
            p.text,
            sb_w - 16,
        );
    } else {
        canvas.draw_text(
            sb_x + 8,
            sy2 + 13,
            b"Search apps...",
            p.text_muted,
            sb_w - 16,
        );
    }

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
    let mut siy = TITLEBAR_H as i32 + 8;
    canvas.draw_text(8, siy, b"CATEGORIES", p.text_muted, SIDEBAR_W - 16);
    siy += 16;
    for (i, &(cat, name, col)) in CATEGORIES.iter().enumerate() {
        let cnt = APPS
            .iter()
            .filter(|a| match cat {
                Category::All => true,
                Category::Installed => a.installed,
                c => a.category == c,
            })
            .count();
        draw_sidebar_item(
            canvas,
            Rect::new(0, siy, SIDEBAR_W, 32),
            name,
            col,
            app.category == cat,
            app.sidebar_hover == Some(i),
            cnt as u32,
            app.theme,
        );
        siy += 32;
    }

    // Content area
    if let Some(ref detail) = app.detail {
        render_detail(canvas, &APPS[detail.app_idx], detail, p, app.theme);
    } else {
        render_grid(canvas, app, p, app.theme);
    }

    // Status bar
    let sb2 = (WIN_H - STATUS_H) as i32;
    canvas.fill_rect(0, sb2, WIN_W, STATUS_H, p.chrome);
    canvas.draw_hline(0, sb2, WIN_W, p.border);
    canvas.draw_text(
        CONTENT_X + 8,
        sb2 + 4,
        app.status,
        p.text_muted,
        CONTENT_W - 16,
    );
}

fn render_grid(canvas: &mut Canvas<'_>, app: &App, p: ThemeTokens, theme: Theme) {
    let visible = app.visible_apps();
    let start_y = CONTENT_Y;
    let cols = CARD_COLS.max(1) as usize;
    for (vi, &ai) in visible.iter().enumerate() {
        let col = vi % cols;
        let row = vi / cols;
        let cx = CONTENT_X + 16 + col as i32 * (CARD_W as i32 + 16);
        let cy2 = start_y + row as i32 * (CARD_H as i32 + 16);
        if cy2 > CONTENT_Y + CONTENT_H as i32 {
            break;
        }
        let a = &APPS[ai];
        let hover = app.hover_card == Some(ai);
        let bg = if hover {
            darken_blend(a.color, 0x1A)
        } else {
            p.surface
        };
        canvas.fill_rect(cx, cy2, CARD_W, CARD_H, bg);
        canvas.draw_rect(
            cx,
            cy2,
            CARD_W,
            CARD_H,
            if hover { a.color } else { p.border },
        );
        // Icon
        canvas.fill_rect(cx + (CARD_W as i32 - 48) / 2, cy2 + 12, 48, 48, a.color);
        // Name
        let nx = cx + CARD_W as i32 / 2 - a.name.len() as i32 * 3;
        canvas.draw_text(nx, cy2 + 68, a.name, p.text, CARD_W - 8);
        // Version
        canvas.draw_text(cx + 8, cy2 + 84, a.version, p.text_muted, CARD_W - 16);
        // Stars
        let stars = a.rating / 10;
        let star_row: &[u8] = if stars >= 5 {
            b"*****"
        } else if stars >= 4 {
            b"**** "
        } else if stars >= 3 {
            b"***  "
        } else {
            b"**   "
        };
        canvas.draw_text(cx + 8, cy2 + 98, star_row, 0xFFFFBD2E, CARD_W - 16);
        // Installed badge
        if a.installed {
            canvas.fill_rect(cx + CARD_W as i32 - 28, cy2 + 4, 24, 12, 0xFF28C940);
            canvas.draw_text(cx + CARD_W as i32 - 26, cy2 + 5, b"INST", 0xFF000000, 22);
        }
        // Size
        let mut sz = [0u8; 12];
        let sl = fmt_size(&mut sz, a.size_kb);
        canvas.draw_text(
            cx + 8,
            cy2 + CARD_H as i32 - 20,
            &sz[..sl],
            p.text_muted,
            CARD_W - 16,
        );
        // Install button
        let btn_label: &[u8] = if a.installed { b"Open" } else { b"Get" };
        draw_button(
            canvas,
            Rect::new(cx + 8, cy2 + CARD_H as i32 - 38, CARD_W - 16, 22),
            btn_label,
            if a.installed {
                ButtonKind::Secondary
            } else {
                ButtonKind::Primary
            },
            false,
            hover,
            false,
            theme,
        );
    }
}

fn render_detail(
    canvas: &mut Canvas<'_>,
    app_entry: &AppEntry,
    detail: &Detail,
    p: ThemeTokens,
    theme: Theme,
) {
    let x = CONTENT_X + 24;
    let mut y = CONTENT_Y + 8;
    // Icon + header
    canvas.fill_rect(x, y, 64, 64, app_entry.color);
    canvas.draw_text(x + 76, y + 4, app_entry.name, p.text, 300);
    canvas.draw_text(x + 76, y + 18, app_entry.version, p.text_muted, 200);
    canvas.draw_text(x + 76, y + 32, app_entry.author, p.text_muted, 200);
    let stars = app_entry.rating / 10;
    let star_row: &[u8] = if stars >= 5 {
        b"*****"
    } else if stars >= 4 {
        b"**** "
    } else {
        b"***  "
    };
    canvas.draw_text(x + 76, y + 46, star_row, 0xFFFFBD2E, 60);
    y += 80;
    canvas.draw_hline(x, y, CONTENT_W - 48, p.border);
    y += 12;
    canvas.draw_text(x, y, app_entry.description, p.text, CONTENT_W - 48);
    y += 20;
    canvas.draw_hline(x, y, CONTENT_W - 48, p.border);
    y += 12;
    // Install / progress
    if detail.installing {
        draw_progress_bar(
            canvas,
            Rect::new(x, y, 300, 20),
            detail.install_progress,
            theme,
        );
        canvas.draw_text(x + 308, y + 4, b"Installing...", p.text_muted, 120);
    } else if app_entry.installed {
        draw_button(
            canvas,
            Rect::new(x, y, 100, 28),
            b"Open",
            ButtonKind::Primary,
            false,
            false,
            false,
            theme,
        );
        draw_button(
            canvas,
            Rect::new(x + 112, y, 100, 28),
            b"Uninstall",
            ButtonKind::Danger,
            false,
            false,
            false,
            theme,
        );
    } else {
        draw_button(
            canvas,
            Rect::new(x, y, 120, 28),
            b"Install",
            ButtonKind::Primary,
            false,
            false,
            false,
            theme,
        );
    }
    y += 40;
    let mut sz = [0u8; 12];
    let sl = fmt_size(&mut sz, app_entry.size_kb);
    canvas.draw_text(x, y, b"Size: ", p.text_muted, 40);
    canvas.draw_text(x + 40, y, &sz[..sl], p.text, 80);
    y += 16;
    canvas.draw_text(x, y, b"Category: ", p.text_muted, 60);
    let cat_name: &[u8] = match app_entry.category {
        Category::Games => b"Games",
        Category::Productivity => b"Productivity",
        Category::Dev => b"Dev Tools",
        Category::Utilities => b"Utilities",
        _ => b"All",
    };
    canvas.draw_text(x + 60, y, cat_name, p.text, 120);
    y += 24;
    canvas.draw_hline(x, y, CONTENT_W - 48, p.border);
    y += 12;
    canvas.draw_text(x, y, b"Version History", p.text_muted, 200);
    y += 16;
    canvas.draw_text(x, y, app_entry.version, p.text, 80);
    y += 12;
    canvas.draw_text(x + 100, y - 12, b"  Current", p.text_muted, 80);
    canvas.draw_text(x, y, b"Initial release.", p.text_muted, CONTENT_W - 48);
}

fn darken_blend(c: u32, amount: u32) -> u32 {
    let r = ((c >> 16) & 0xFF).saturating_sub(amount);
    let g = ((c >> 8) & 0xFF).saturating_sub(amount);
    let b2 = (c & 0xFF).saturating_sub(amount);
    0xFF000000 | (r << 16) | (g << 8) | b2
}

fn fmt_size(buf: &mut [u8], kb: u32) -> usize {
    let (n, suf): (u32, &[u8]) = if kb < 1024 {
        (kb, b" KB")
    } else {
        (kb / 1024, b" MB")
    };
    let mut tmp = [0u8; 8];
    let mut l = 0;
    let mut v = n;
    if v == 0 {
        tmp[0] = b'0';
        l = 1;
    } else {
        while v > 0 {
            tmp[l] = b'0' + (v % 10) as u8;
            v /= 10;
            l += 1;
        }
        tmp[..l].reverse();
    }
    buf[..l].copy_from_slice(&tmp[..l]);
    let sl = suf.len();
    buf[l..l + sl].copy_from_slice(suf);
    l + sl
}

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 60, 30, channel) {
        Some(w) => w,
        None => return,
    };
    win.request_focus();
    let mut app = App::new();

    loop {
        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,
                Event::Key {
                    pressed: true,
                    ascii,
                    ..
                } => {
                    if app.search_active {
                        match ascii {
                            0x1B => {
                                app.search_active = false;
                                app.search_len = 0;
                            }
                            0x0D => {
                                app.search_active = false;
                            }
                            0x08 => {
                                if app.search_len > 0 {
                                    app.search_len -= 1;
                                }
                            }
                            32..=126 => {
                                if app.search_len < 64 {
                                    app.search[app.search_len] = ascii;
                                    app.search_len += 1;
                                }
                            }
                            _ => {}
                        }
                    } else {
                        match ascii {
                            0x06 => {
                                app.search_active = true;
                            } // Ctrl+F
                            0x1B => {
                                app.detail = None;
                            }
                            _ => {}
                        }
                    }
                }
                Event::PointerMove { x, y, buttons } => {
                    let xi = x as i32;
                    let yi = y as i32;
                    // Sidebar
                    if xi < SIDEBAR_W as i32 && yi > TITLEBAR_H as i32 {
                        let si = (yi - TITLEBAR_H as i32 - 24) / 32;
                        app.sidebar_hover = if si >= 0 && (si as usize) < CATEGORIES.len() {
                            Some(si as usize)
                        } else {
                            None
                        };
                        if buttons & 1 != 0 {
                            if let Some(i) = app.sidebar_hover {
                                app.category = CATEGORIES[i].0;
                                app.detail = None;
                            }
                        }
                    }
                    // Search bar click
                    let sy3 = TITLEBAR_H as i32;
                    if yi >= sy3
                        && yi < sy3 + SEARCH_H as i32
                        && xi > SIDEBAR_W as i32
                        && buttons & 1 != 0
                    {
                        app.search_active = true;
                    }
                    // Card hover / click
                    if yi > CONTENT_Y && app.detail.is_none() {
                        let vis = app.visible_apps();
                        let cols = CARD_COLS.max(1) as usize;
                        app.hover_card = None;
                        for (vi, &ai) in vis.iter().enumerate() {
                            let col = vi % cols;
                            let row = vi / cols;
                            let cx = CONTENT_X + 16 + col as i32 * (CARD_W as i32 + 16);
                            let cy2 = CONTENT_Y + row as i32 * (CARD_H as i32 + 16);
                            if xi >= cx
                                && xi < cx + CARD_W as i32
                                && yi >= cy2
                                && yi < cy2 + CARD_H as i32
                            {
                                app.hover_card = Some(ai);
                                if buttons & 1 != 0 {
                                    app.detail = Some(Detail {
                                        app_idx: ai,
                                        install_progress: 0,
                                        installing: false,
                                    });
                                }
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
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
