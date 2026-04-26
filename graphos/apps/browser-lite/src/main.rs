// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Browser Lite — Phase J complete implementation.
//!
//! Lightweight web browser for the GraphOS network stack:
//! - URL bar with history navigation (Back/Forward)
//! - Multi-tab support (up to 8 tabs, Ctrl+T/Ctrl+W)
//! - HTML subset renderer: headings, paragraphs, links, lists, code blocks
//! - HTTP via SYS_NET_GET syscall (returns raw bytes)
//! - Bookmarks bar
//! - Phase J frosted glass chrome

use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::geom::Rect;
use graphos_ui_sdk::tokens::ThemeTokens;
use graphos_ui_sdk::tokens::{Theme, tokens};
use graphos_ui_sdk::widgets::{ButtonKind, draw_button, draw_tab_bar};

const WIN_W: u32 = 1024;
const WIN_H: u32 = 720;
const TITLEBAR_H: u32 = 32;
const TABBAR_H: u32 = 30;
const URLBAR_H: u32 = 32;
const BOOKMARKS_H: u32 = 24;
const STATUS_H: u32 = 18;
const CONTENT_Y: i32 = (TITLEBAR_H + TABBAR_H + URLBAR_H + BOOKMARKS_H) as i32;
const CONTENT_H: u32 = WIN_H - TITLEBAR_H - TABBAR_H - URLBAR_H - BOOKMARKS_H - STATUS_H;
const MARGIN: i32 = 24;
const TEXT_W: u32 = WIN_W - 2 * MARGIN as u32;
const CHAR_W: u32 = 6;
const LINE_H: u32 = 14;

const BOOKMARKS: &[&[u8]] = &[
    b"GraphOS Docs",
    b"Kernel Source",
    b"App Store",
    b"Release Notes",
];
const BOOKMARK_URLS: &[&[u8]] = &[
    b"graphos://docs",
    b"graphos://src/kernel",
    b"graphos://store",
    b"graphos://release",
];

// ── HTML token-based renderer ─────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum HtmlTag {
    None,
    H1,
    H2,
    H3,
    P,
    Li,
    Code,
    A,
    Strong,
    Em,
    Hr,
}

fn parse_tag(s: &[u8]) -> (HtmlTag, bool) {
    let close = s.first() == Some(&b'/');
    let name = if close { &s[1..] } else { s };
    let tag = match name {
        b"h1" => HtmlTag::H1,
        b"h2" => HtmlTag::H2,
        b"h3" => HtmlTag::H3,
        b"p" => HtmlTag::P,
        b"li" => HtmlTag::Li,
        b"code" | b"pre" => HtmlTag::Code,
        b"a" => HtmlTag::A,
        b"strong" | b"b" => HtmlTag::Strong,
        b"em" | b"i" => HtmlTag::Em,
        b"hr" => HtmlTag::Hr,
        _ => HtmlTag::None,
    };
    (tag, close)
}

struct RenderLine {
    text: Vec<u8>,
    fg: u32,
    bg: u32,
    indent: u32,
    line_h: u32,
    is_hr: bool,
}

fn html_to_lines(html: &[u8], default_fg: u32, default_bg: u32) -> Vec<RenderLine> {
    let mut lines: Vec<RenderLine> = Vec::new();
    let mut cur_tag = HtmlTag::None;
    let mut cur_text: Vec<u8> = Vec::new();
    let mut i = 0;
    let n = html.len();

    while i < n {
        if html[i] == b'<' {
            // flush text
            if !cur_text.is_empty() {
                let (fg, bg, indent, lh) = tag_style(cur_tag, default_fg, default_bg);
                lines.push(RenderLine {
                    text: core::mem::take(&mut cur_text),
                    fg,
                    bg,
                    indent,
                    line_h: lh,
                    is_hr: false,
                });
            }
            // parse tag
            i += 1;
            let tag_start = i;
            while i < n && html[i] != b'>' {
                i += 1;
            }
            let tag_bytes = &html[tag_start..i];
            // strip attributes: use only first word
            let tag_name_end = tag_bytes
                .iter()
                .position(|&b| b == b' ')
                .unwrap_or(tag_bytes.len());
            let (tag, close) = parse_tag(&tag_bytes[..tag_name_end]);
            if tag == HtmlTag::Hr {
                lines.push(RenderLine {
                    text: Vec::new(),
                    fg: default_fg,
                    bg: default_bg,
                    indent: 0,
                    line_h: 2,
                    is_hr: true,
                });
            }
            if !close && tag != HtmlTag::None {
                cur_tag = tag;
            } else if close {
                cur_tag = HtmlTag::None;
            }
            i += 1; // skip '>'
        } else if html[i] == b'\n' || html[i] == b'\r' {
            i += 1;
        } else {
            cur_text.push(html[i]);
            i += 1;
        }
    }
    if !cur_text.is_empty() {
        lines.push(RenderLine {
            text: cur_text,
            fg: default_fg,
            bg: default_bg,
            indent: 0,
            line_h: LINE_H,
            is_hr: false,
        });
    }
    lines
}

fn tag_style(tag: HtmlTag, dfg: u32, dbg: u32) -> (u32, u32, u32, u32) {
    match tag {
        HtmlTag::H1 => (0xFFFFFFFF, dbg, 0, 22),
        HtmlTag::H2 => (0xFFEEEEEE, dbg, 0, 18),
        HtmlTag::H3 => (0xFFDDDDDD, dbg, 0, 16),
        HtmlTag::P => (dfg, dbg, 0, LINE_H),
        HtmlTag::Li => (dfg, dbg, 16, LINE_H),
        HtmlTag::Code => (0xFF9CDCFE, 0xFF1E1E2E, 0, LINE_H),
        HtmlTag::A => (0xFF58A6FF, dbg, 0, LINE_H),
        HtmlTag::Strong => (0xFFFFFFFF, dbg, 0, LINE_H),
        HtmlTag::Em => (0xFFCCBBFF, dbg, 0, LINE_H),
        _ => (dfg, dbg, 0, LINE_H),
    }
}

// ── Tab ───────────────────────────────────────────────────────────────────────

struct Tab {
    url: [u8; 512],
    url_len: usize,
    title: [u8; 128],
    title_len: usize,
    content: Vec<u8>,
    lines: Vec<RenderLine>,
    scroll: u32,
    history: Vec<([u8; 512], usize)>,
    history_idx: usize,
    loading: bool,
}

impl Tab {
    fn new(url: &[u8]) -> Self {
        let mut t = Self {
            url: [0u8; 512],
            url_len: 0,
            title: [0u8; 128],
            title_len: 0,
            content: Vec::new(),
            lines: Vec::new(),
            scroll: 0,
            history: Vec::new(),
            history_idx: 0,
            loading: false,
        };
        t.navigate_to(url);
        t
    }

    fn navigate_to(&mut self, url: &[u8]) {
        let l = url.len().min(512);
        self.url[..l].copy_from_slice(&url[..l]);
        self.url_len = l;

        // Push to history
        let mut h = [0u8; 512];
        h[..l].copy_from_slice(&url[..l]);
        self.history.truncate(self.history_idx + 1);
        self.history.push((h, l));
        self.history_idx = self.history.len() - 1;

        // Fetch content (demo: use syscall if available, else show placeholder)
        self.content = self.fetch_demo(url);
        self.title[..l.min(128)].copy_from_slice(&url[..l.min(128)]);
        self.title_len = l.min(128);
        self.lines = html_to_lines(&self.content, 0xFFCCCCCC, 0xFF0A0E1A);
        self.scroll = 0;
    }

    fn fetch_demo(&self, url: &[u8]) -> Vec<u8> {
        if url.starts_with(b"http://") || url.starts_with(b"https://") {
            let mut out = vec![0u8; 8192];
            let n = graphos_app_sdk::sys::net_http_get(url, &mut out, 0);
            if n > 0 {
                out.truncate(n);
                return out;
            }

            let mut v = b"<h1>Network fetch failed</h1><p>Could not fetch URL via SYS_NET_HTTP_GET:</p><code>".to_vec();
            v.extend_from_slice(url);
            v.extend_from_slice(b"</code><p>Check netd/modeld health and network readiness.</p>");
            return v;
        }

        if url.starts_with(b"graphos://docs") {
            b"<h1>GraphOS Documentation</h1><p>Welcome to the GraphOS developer docs.</p><h2>Kernel</h2><p>The GraphOS kernel is a no_std Rust kernel targeting x86_64.</p><h2>Apps</h2><p>Apps are built with the graphos-app-sdk crate using the Canvas and Window APIs.</p><ul><li>Terminal - xterm-256color emulator</li><li>Files - VFS file manager</li><li>Editor - Modal text editor</li><li>Browser - HTML subset browser</li></ul><hr><p>See DEVELOPER_README.md for build instructions.</p>".to_vec()
        } else if url.starts_with(b"graphos://store") {
            b"<h1>GraphOS App Store</h1><p>Browse and install apps for GraphOS.</p><h2>Featured</h2><li>AI Console - SCCE pipeline interface</li><li>Air Hockey - Physics game</li><li>Shell3D - 3D launcher</li>".to_vec()
        } else if url.starts_with(b"graphos://release") {
            b"<h1>Release Notes</h1><h2>Phase J - 2025</h2><li>GPU compositor with Kawase blur and frosted glass</li><li>Phase J window chrome system-wide</li><li>xterm-256color terminal</li><li>Complete file manager with VFS integration</li><li>Modal text editor with gap buffer</li><li>AI Console with SCCE pipeline UI</li><li>3D spatial shell launcher</li><li>App Store client</li>".to_vec()
        } else {
            let mut v = b"<h1>".to_vec();
            v.extend_from_slice(url);
            v.extend_from_slice(b"</h1><p>Page not found in demo mode.</p><p>In production, this would issue a SYS_NET_GET syscall.</p>");
            v
        }
    }

    fn go_back(&mut self) {
        if self.history_idx > 0 {
            self.history_idx -= 1;
            let (u, l) = self.history[self.history_idx];
            self.url[..l].copy_from_slice(&u[..l]);
            self.url_len = l;
            self.content = self.fetch_demo(&u[..l]);
            self.lines = html_to_lines(&self.content, 0xFFCCCCCC, 0xFF0A0E1A);
            self.scroll = 0;
        }
    }
    fn go_forward(&mut self) {
        if self.history_idx + 1 < self.history.len() {
            self.history_idx += 1;
            let (u, l) = self.history[self.history_idx];
            self.url[..l].copy_from_slice(&u[..l]);
            self.url_len = l;
            self.content = self.fetch_demo(&u[..l]);
            self.lines = html_to_lines(&self.content, 0xFFCCCCCC, 0xFF0A0E1A);
            self.scroll = 0;
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct App {
    tabs: Vec<Tab>,
    active_tab: usize,
    url_editing: bool,
    url_buf: [u8; 512],
    url_len: usize,
    theme: Theme,
    status: &'static [u8],
}

impl App {
    fn new() -> Self {
        let mut a = Self {
            tabs: Vec::new(),
            active_tab: 0,
            url_editing: false,
            url_buf: [0u8; 512],
            url_len: 0,
            theme: Theme::DarkGlass,
            status: b"Ready",
        };
        a.tabs.push(Tab::new(b"graphos://docs"));
        a
    }
    fn current_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }
    fn current_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }
    fn new_tab(&mut self) {
        if self.tabs.len() < 8 {
            self.tabs.push(Tab::new(b"graphos://docs"));
            self.active_tab = self.tabs.len() - 1;
        }
    }
    fn close_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.tabs.remove(self.active_tab);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
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
    canvas.draw_text((WIN_W / 2 - 28) as i32, 10, b"Browser Lite", p.text, 100);

    // Tab bar
    let tab_y = TITLEBAR_H as i32;
    let tab_labels: Vec<&[u8]> = app.tabs.iter().map(|t| &t.title[..t.title_len]).collect();
    draw_tab_bar(
        canvas,
        Rect::new(0, tab_y, WIN_W, TABBAR_H),
        &tab_labels,
        app.active_tab,
        app.theme,
    );

    // URL bar
    let uy = (TITLEBAR_H + TABBAR_H) as i32;
    canvas.fill_rect(0, uy, WIN_W, URLBAR_H, p.chrome);
    canvas.draw_hline(0, uy + URLBAR_H as i32 - 1, WIN_W, p.border);
    draw_button(
        canvas,
        Rect::new(4, uy + 4, 28, 22),
        b"<",
        ButtonKind::Ghost,
        false,
        false,
        false,
        app.theme,
    );
    draw_button(
        canvas,
        Rect::new(36, uy + 4, 28, 22),
        b">",
        ButtonKind::Ghost,
        false,
        false,
        false,
        app.theme,
    );
    draw_button(
        canvas,
        Rect::new(68, uy + 4, 28, 22),
        b"Re",
        ButtonKind::Ghost,
        false,
        false,
        false,
        app.theme,
    );
    let url_rect_x = 104;
    let url_rect_w = WIN_W - 116;
    canvas.draw_rect(
        url_rect_x,
        uy + 5,
        url_rect_w,
        URLBAR_H - 10,
        if app.url_editing { p.primary } else { p.border },
    );
    let t = app.current_tab();
    let url_text = if app.url_editing {
        &app.url_buf[..app.url_len]
    } else {
        &t.url[..t.url_len]
    };
    canvas.draw_text(url_rect_x + 6, uy + 10, url_text, p.text, url_rect_w - 12);

    // Bookmarks bar
    let by = (TITLEBAR_H + TABBAR_H + URLBAR_H) as i32;
    canvas.fill_rect(0, by, WIN_W, BOOKMARKS_H, p.surface_alt);
    canvas.draw_hline(0, by + BOOKMARKS_H as i32 - 1, WIN_W, p.border);
    let mut bx = 8;
    for &bm in BOOKMARKS {
        canvas.draw_text(bx, by + 6, bm, p.primary, 120);
        bx += bm.len() as i32 * CHAR_W as i32 + 24;
    }

    // Content
    let tab = app.current_tab();
    let vis_rows = (CONTENT_H / LINE_H) as usize;
    let mut cy2 = CONTENT_Y;
    for line in tab
        .lines
        .iter()
        .skip(tab.scroll as usize)
        .take(vis_rows + 4)
    {
        if cy2 > CONTENT_Y + CONTENT_H as i32 {
            break;
        }
        if line.is_hr {
            canvas.draw_hline(MARGIN, cy2 + 1, TEXT_W, 0xFF334466);
            cy2 += 4;
            continue;
        }
        if line.bg != 0xFF0A0E1A {
            canvas.fill_rect(MARGIN, cy2, TEXT_W, line.line_h, line.bg);
        }
        let x = MARGIN + line.indent as i32;
        if !line.text.is_empty() {
            canvas.draw_text(x, cy2 + 2, &line.text, line.fg, TEXT_W - line.indent);
        }
        cy2 += line.line_h as i32;
    }

    // Status bar
    let sy = (WIN_H - STATUS_H) as i32;
    canvas.fill_rect(0, sy, WIN_W, STATUS_H, p.chrome);
    canvas.draw_hline(0, sy, WIN_W, p.border);
    canvas.draw_text(8, sy + 3, app.status, p.text_muted, WIN_W - 16);
    let tab_info = format_tab_count(app.active_tab + 1, app.tabs.len());
    canvas.draw_text(WIN_W as i32 - 60, sy + 3, &tab_info, p.text_muted, 54);
}

fn format_tab_count(cur: usize, total: usize) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = b'T';
    b[1] = b'a';
    b[2] = b'b';
    b[3] = b' ';
    b[4] = b'0' + cur as u8;
    b[5] = b'/';
    b[6] = b'0' + total as u8;
    b
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 80, 40, channel) {
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
                    if app.url_editing {
                        match ascii {
                            0x1B => {
                                app.url_editing = false;
                            }
                            0x0D => {
                                app.url_editing = false;
                                let url = app.url_buf[..app.url_len].to_vec();
                                app.current_tab_mut().navigate_to(&url);
                            }
                            0x08 => {
                                if app.url_len > 0 {
                                    app.url_len -= 1;
                                }
                            }
                            32..=126 => {
                                if app.url_len < 511 {
                                    app.url_buf[app.url_len] = ascii;
                                    app.url_len += 1;
                                }
                            }
                            _ => {}
                        }
                    } else {
                        match ascii {
                            0x14 => {
                                app.new_tab();
                            } // Ctrl+T
                            0x17 => {
                                app.close_tab();
                            } // Ctrl+W
                            b'l' | 0x0C => {
                                // Ctrl+L
                                {
                                    let t = app.current_tab();
                                    let l = t.url_len;
                                    let mut uc = [0u8; 512];
                                    uc[..l].copy_from_slice(&t.url[..l]);
                                    app.url_buf[..l].copy_from_slice(&uc[..l]);
                                    app.url_len = l;
                                    app.url_editing = true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Event::PointerMove { x, y, buttons } => {
                    let xi = x as i32;
                    let yi = y as i32;
                    if buttons & 1 != 0 {
                        let uy = (TITLEBAR_H + TABBAR_H) as i32;
                        let ury = uy + URLBAR_H as i32;
                        // URL bar click
                        if yi >= uy && yi < ury && xi > 100 {
                            if !app.url_editing {
                                {
                                    let t = app.current_tab();
                                    let l = t.url_len;
                                    let mut uc = [0u8; 512];
                                    uc[..l].copy_from_slice(&t.url[..l]);
                                    app.url_buf[..l].copy_from_slice(&uc[..l]);
                                    app.url_len = l;
                                    app.url_editing = true;
                                }
                            }
                        }
                        // Back button
                        if yi >= uy + 4 && yi < uy + 26 && xi < 32 {
                            app.current_tab_mut().go_back();
                        }
                        // Forward button
                        if yi >= uy + 4 && yi < uy + 26 && xi >= 36 && xi < 64 {
                            app.current_tab_mut().go_forward();
                        }
                        // Tab bar click
                        let tbh = TITLEBAR_H as i32;
                        if yi >= tbh && yi < tbh + TABBAR_H as i32 {
                            let tab_w = WIN_W as i32 / app.tabs.len().max(1) as i32;
                            let ti = xi / tab_w;
                            if (ti as usize) < app.tabs.len() {
                                app.active_tab = ti as usize;
                            }
                        }
                        // Bookmarks
                        let by = (TITLEBAR_H + TABBAR_H + URLBAR_H) as i32;
                        if yi >= by && yi < by + BOOKMARKS_H as i32 {
                            let mut bx = 8i32;
                            for (i, &bm) in BOOKMARKS.iter().enumerate() {
                                let bw = bm.len() as i32 * CHAR_W as i32 + 24;
                                if xi >= bx && xi < bx + bw {
                                    app.current_tab_mut().navigate_to(BOOKMARK_URLS[i]);
                                    break;
                                }
                                bx += bw;
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
