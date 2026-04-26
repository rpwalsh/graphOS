// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Phase J Session 30 — complete widget renderer library.
//!
//! Every public function renders a widget into a `Canvas<'_>` using
//! the Phase J design-token system.  All layouts are pixel-precise and
//! work at 100 / 125 / 150 / 200 % DPI scales if the caller scales
//! the `Rect` arguments accordingly.
//!
//! # Widget catalogue
//! `draw_window_frame`, `draw_panel`, `draw_stat_card`, `draw_command_bar`,
//! `draw_dialog`, `draw_button`, `draw_text_field`, `draw_toggle`,
//! `draw_slider`, `draw_progress_bar`, `draw_tab_bar`, `draw_list_row`,
//! `draw_scroll_track`, `draw_menu`, `draw_tooltip`, `draw_badge`,
//! `draw_separator`, `draw_notification_toast`, `draw_sidebar_item`,
//! `draw_toolbar`, `draw_breadcrumb`

use graphos_app_sdk::canvas::Canvas;

use crate::geom::Rect;
use crate::tokens::{Theme, space, tokens};

// ── Foundational panels ───────────────────────────────────────────────────────

/// Draw a framed application window with a title bar.
pub fn draw_window_frame(canvas: &mut Canvas<'_>, rect: Rect, title: &[u8], theme: Theme) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.background);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    let header_h = 32u32;
    canvas.fill_rect(rect.x, rect.y, rect.w, header_h, p.chrome);
    // Rim highlight on top edge
    canvas.fill_rect(rect.x, rect.y, rect.w, 1, brighten(p.primary));
    canvas.draw_hline(rect.x, rect.y + header_h as i32, rect.w, p.border);
    // Title centred in header
    let tw = (title.len() as u32).saturating_mul(5);
    let tx = rect.x + ((rect.w.saturating_sub(tw)) / 2) as i32;
    canvas.draw_text(tx, rect.y + 9, title, p.text, rect.w.saturating_sub(80));
    // Traffic-light buttons (left side)
    draw_traffic_lights(canvas, rect.x + 12, rect.y + 8, theme);
}

/// Draw a titled content panel; returns the inner content Rect.
pub fn draw_panel(canvas: &mut Canvas<'_>, rect: Rect, title: &[u8], theme: Theme) -> Rect {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    if !title.is_empty() {
        canvas.fill_rect(rect.x, rect.y, rect.w, 20, p.surface_alt);
        canvas.draw_text(
            rect.x + 6,
            rect.y + 5,
            title,
            p.text_muted,
            rect.w.saturating_sub(12),
        );
        canvas.draw_hline(rect.x, rect.y + 20, rect.w, p.border);
        Rect::new(
            rect.x + space::SM as i32,
            rect.y + 24,
            rect.w.saturating_sub(space::SM * 2),
            rect.h.saturating_sub(28),
        )
    } else {
        rect.inset(space::SM)
    }
}

/// Draw a compact KPI / metric card with an accent left-border.
pub fn draw_stat_card(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    label: &[u8],
    value: &[u8],
    accent: u32,
    theme: Theme,
) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    canvas.fill_rect(rect.x, rect.y, 3, rect.h, accent);
    canvas.draw_text(
        rect.x + 8,
        rect.y + 5,
        label,
        p.text_muted,
        rect.w.saturating_sub(12),
    );
    canvas.draw_text(
        rect.x + 8,
        rect.y + 19,
        value,
        p.text,
        rect.w.saturating_sub(12),
    );
}

// ── Buttons ───────────────────────────────────────────────────────────────────

/// Button variant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Filled with accent colour — primary CTA.
    Primary,
    /// Outlined, transparent fill — secondary action.
    Secondary,
    /// No border, text only — tertiary / inline action.
    Ghost,
    /// Filled with danger colour — destructive action.
    Danger,
}

/// Draw a button widget.
///
/// `focused` draws the 2 px focus ring. `hovered`/`pressed` adjust visual state.
pub fn draw_button(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    label: &[u8],
    kind: ButtonKind,
    focused: bool,
    hovered: bool,
    pressed: bool,
    theme: Theme,
) {
    let p = tokens(theme);
    let (fill, text_col, border_col) = match kind {
        ButtonKind::Primary => {
            let fill = if pressed {
                dim(p.primary)
            } else if hovered {
                brighten(p.primary)
            } else {
                p.primary
            };
            (fill, p.background, fill)
        }
        ButtonKind::Secondary => {
            let fill = if pressed {
                p.surface_alt
            } else if hovered {
                p.surface
            } else {
                0
            };
            (fill, p.primary, p.primary)
        }
        ButtonKind::Ghost => {
            let fill = if pressed {
                p.surface_alt
            } else if hovered {
                p.surface
            } else {
                0
            };
            (fill, p.text, 0)
        }
        ButtonKind::Danger => {
            let fill = if pressed {
                dim(p.danger)
            } else if hovered {
                brighten(p.danger)
            } else {
                p.danger
            };
            (fill, p.background, fill)
        }
    };
    if fill != 0 {
        canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, fill);
    }
    if border_col != 0 {
        canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, border_col);
    }
    // Focus ring: 2 px outline, 2 px offset
    if focused {
        canvas.draw_rect(rect.x - 2, rect.y - 2, rect.w + 4, rect.h + 4, p.primary);
    }
    // Centred label
    let tw = (label.len() as u32).saturating_mul(5);
    let tx = rect.x + ((rect.w.saturating_sub(tw)) / 2) as i32;
    let ty = rect.y + (rect.h.saturating_sub(14)) as i32 / 2;
    canvas.draw_text(tx, ty, label, text_col, rect.w.saturating_sub(8));
}

// ── Text fields ───────────────────────────────────────────────────────────────

/// Draw a single-line text input field.
pub fn draw_text_field(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    label: &[u8], // inline label shown when value is empty
    value: &[u8],
    cursor_col: usize, // byte position of cursor within value
    focused: bool,
    theme: Theme,
) {
    let p = tokens(theme);
    let fill = if focused { p.surface } else { p.surface_alt };
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, fill);
    canvas.draw_rect(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if focused { p.primary } else { p.border },
    );
    if focused {
        canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.primary);
        // Bottom accent bar (2 px)
        canvas.fill_rect(rect.x, rect.y + rect.h as i32 - 2, rect.w, 2, p.primary);
    }
    let inner_x = rect.x + 8;
    let inner_y = rect.y + (rect.h.saturating_sub(14)) as i32 / 2;
    if value.is_empty() {
        canvas.draw_text(
            inner_x,
            inner_y,
            label,
            p.text_muted,
            rect.w.saturating_sub(16),
        );
    } else {
        canvas.draw_text(inner_x, inner_y, value, p.text, rect.w.saturating_sub(16));
        if focused {
            let cx = inner_x + (cursor_col as u32 * 8) as i32;
            canvas.fill_rect(cx, inner_y - 1, 2, 16, p.primary);
        }
    }
}

// ── Toggle switch ─────────────────────────────────────────────────────────────

/// Draw a toggle switch (iOS-style pill).
///
/// `rect` should be about 36×20 px; the label is drawn to the right.
pub fn draw_toggle(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    label: &[u8],
    checked: bool,
    focused: bool,
    theme: Theme,
) {
    let p = tokens(theme);
    let track_col = if checked { p.primary } else { p.border };
    let track = Rect::new(
        rect.x,
        rect.y + (rect.h.saturating_sub(14)) as i32 / 2,
        36,
        14,
    );
    canvas.fill_rect(track.x, track.y, track.w, track.h, track_col);
    canvas.draw_rect(track.x, track.y, track.w, track.h, p.border);
    let thumb_x = if checked { track.x + 22 } else { track.x + 2 };
    canvas.fill_rect(thumb_x, track.y + 2, 10, 10, p.text);
    if focused {
        canvas.draw_rect(
            track.x - 2,
            track.y - 2,
            track.w + 4,
            track.h + 4,
            p.primary,
        );
    }
    canvas.draw_text(
        rect.x + 44,
        rect.y + (rect.h.saturating_sub(14)) as i32 / 2,
        label,
        p.text,
        0,
    );
}

// ── Slider ────────────────────────────────────────────────────────────────────

/// Draw a horizontal slider.
///
/// `value_fp` is 0–1000 (fixed-point, 1000 = 100 %). `rect.h` should be ~20 px.
pub fn draw_slider(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    value_fp: u32, // 0–1000
    focused: bool,
    theme: Theme,
) {
    let p = tokens(theme);
    let track_y = rect.y + rect.h as i32 / 2 - 2;
    let track_h = 4u32;
    // Full track
    canvas.fill_rect(rect.x, track_y, rect.w, track_h, p.surface_alt);
    canvas.draw_rect(rect.x, track_y, rect.w, track_h, p.border);
    // Filled portion
    let filled_w = ((rect.w as u64).saturating_mul(value_fp as u64) / 1000) as u32;
    canvas.fill_rect(rect.x, track_y, filled_w, track_h, p.primary);
    // Thumb
    let thumb_x = rect.x + filled_w.saturating_sub(6) as i32;
    let thumb_y = rect.y + rect.h as i32 / 2 - 6;
    canvas.fill_rect(thumb_x, thumb_y, 12, 12, p.text);
    canvas.draw_rect(thumb_x, thumb_y, 12, 12, p.primary);
    if focused {
        canvas.draw_rect(thumb_x - 2, thumb_y - 2, 16, 16, p.primary);
    }
}

// ── Progress bar ──────────────────────────────────────────────────────────────

/// Draw a horizontal progress bar (indeterminate when `value_fp` == 0xFFFF).
pub fn draw_progress_bar(canvas: &mut Canvas<'_>, rect: Rect, value_fp: u32, theme: Theme) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    if value_fp == 0xFFFF {
        // Indeterminate: draw a 30 % wide moving segment (caller advances tick externally)
        let seg_w = rect.w * 30 / 100;
        canvas.fill_rect(rect.x, rect.y, seg_w, rect.h, p.primary);
    } else {
        let filled = ((rect.w as u64).saturating_mul(value_fp as u64) / 1000) as u32;
        canvas.fill_rect(rect.x, rect.y, filled, rect.h, p.primary);
    }
}

// ── Tab bar ───────────────────────────────────────────────────────────────────

/// Draw a horizontal tab bar. `active` is the index of the selected tab.
/// Returns the content rect below the tab bar.
pub fn draw_tab_bar<'a>(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    tabs: &[&'a [u8]],
    active: usize,
    theme: Theme,
) -> Rect {
    let p = tokens(theme);
    let tab_h = 32u32;
    canvas.fill_rect(rect.x, rect.y, rect.w, tab_h, p.chrome);
    canvas.draw_hline(rect.x, rect.y + tab_h as i32, rect.w, p.border);
    let tab_w = if tabs.is_empty() {
        rect.w
    } else {
        (rect.w / tabs.len() as u32).max(60)
    };
    for (i, label) in tabs.iter().enumerate() {
        let tx = rect.x + (i as u32 * tab_w) as i32;
        let fill = if i == active { p.surface } else { p.chrome };
        canvas.fill_rect(tx, rect.y, tab_w, tab_h, fill);
        if i == active {
            canvas.fill_rect(tx, rect.y + tab_h as i32 - 2, tab_w, 2, p.primary);
        }
        let lw = (label.len() as u32).saturating_mul(5);
        let lx = tx + ((tab_w.saturating_sub(lw)) / 2) as i32;
        let col = if i == active { p.text } else { p.text_muted };
        canvas.draw_text(lx, rect.y + 10, label, col, tab_w.saturating_sub(8));
    }
    Rect::new(
        rect.x,
        rect.y + tab_h as i32,
        rect.w,
        rect.h.saturating_sub(tab_h),
    )
}

// ── List rows ─────────────────────────────────────────────────────────────────

/// Draw a single list row (28 px tall). `selected` highlights it.
pub fn draw_list_row(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    label: &[u8],
    meta: &[u8],     // right-aligned secondary text (file size, date, etc.)
    icon_color: u32, // 8×8 coloured dot as icon placeholder; 0 = no icon
    selected: bool,
    hovered: bool,
    theme: Theme,
) {
    let p = tokens(theme);
    let fill = if selected {
        p.primary
    } else if hovered {
        p.surface_alt
    } else {
        p.surface
    };
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, fill);
    canvas.draw_hline(rect.x, rect.y + rect.h as i32 - 1, rect.w, p.border);
    let text_col = if selected { p.background } else { p.text };
    let meta_col = if selected { p.background } else { p.text_muted };

    let mut lx = rect.x + 8;
    if icon_color != 0 {
        canvas.fill_rect(lx, rect.y + (rect.h as i32 - 8) / 2, 8, 8, icon_color);
        lx += 14;
    }
    canvas.draw_text(
        lx,
        rect.y + (rect.h.saturating_sub(14)) as i32 / 2,
        label,
        text_col,
        rect.w.saturating_sub(80),
    );

    if !meta.is_empty() {
        let mw = (meta.len() as u32).saturating_mul(5);
        let mx = rect.x + rect.w.saturating_sub(mw + 8) as i32;
        canvas.draw_text(
            mx,
            rect.y + (rect.h.saturating_sub(14)) as i32 / 2,
            meta,
            meta_col,
            mw,
        );
    }
}

// ── Scroll track ──────────────────────────────────────────────────────────────

/// Draw a vertical scroll track with thumb.
///
/// `content_h` is the total scrollable content height; `viewport_h` is visible height;
/// `scroll_pos` is current scroll offset (all in pixels).
pub fn draw_scroll_track(
    canvas: &mut Canvas<'_>,
    rect: Rect, // the narrow (8–12 px wide) track rect
    content_h: u32,
    viewport_h: u32,
    scroll_pos: u32,
    theme: Theme,
) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface_alt);
    if content_h <= viewport_h {
        return;
    }
    let thumb_h =
        ((viewport_h as u64).saturating_mul(rect.h as u64) / content_h as u64).max(16) as u32;
    let max_scroll = content_h - viewport_h;
    let thumb_y = rect.y
        + ((scroll_pos as u64).saturating_mul((rect.h - thumb_h) as u64) / max_scroll as u64)
            as i32;
    let thumb_col = brighten(p.border);
    canvas.fill_rect(
        rect.x + 2,
        thumb_y,
        rect.w.saturating_sub(4),
        thumb_h,
        thumb_col,
    );
}

// ── Menu ──────────────────────────────────────────────────────────────────────

/// Maximum items shown in a dropdown/context menu.
pub const MENU_MAX_ITEMS: usize = 16;

/// Draw a floating dropdown/context menu.
///
/// `items` is a slice of (label, disabled) pairs. `hovered` is the index
/// of the item under the pointer (or `usize::MAX` for none).
pub fn draw_menu(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    items: &[(&[u8], bool)],
    hovered: usize,
    theme: Theme,
) {
    let p = tokens(theme);
    // Drop shadow (simple 2 px offset dark fill)
    canvas.fill_rect(rect.x + 3, rect.y + 3, rect.w, rect.h, 0xAA000000);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    let row_h = 24u32;
    for (i, (label, disabled)) in items.iter().take(MENU_MAX_ITEMS).enumerate() {
        let ry = rect.y + (i as u32 * row_h) as i32;
        if label == &b"---".as_slice() {
            // Separator
            canvas.draw_hline(rect.x + 8, ry + 12, rect.w.saturating_sub(16), p.border);
            continue;
        }
        if i == hovered && !disabled {
            canvas.fill_rect(rect.x, ry, rect.w, row_h, p.primary);
        }
        let tc = if *disabled {
            p.text_muted
        } else if i == hovered {
            p.background
        } else {
            p.text
        };
        canvas.draw_text(rect.x + 12, ry + 6, label, tc, rect.w.saturating_sub(20));
    }
}

// ── Tooltip ───────────────────────────────────────────────────────────────────

/// Draw a floating tooltip.  `anchor_x/y` is the tip of the arrow.
pub fn draw_tooltip(
    canvas: &mut Canvas<'_>,
    anchor_x: i32,
    anchor_y: i32,
    text: &[u8],
    theme: Theme,
) {
    let p = tokens(theme);
    let tw = (text.len() as u32).saturating_mul(5) + 16;
    let th = 22u32;
    let tx = anchor_x - tw as i32 / 2;
    let ty = anchor_y - th as i32 - 6;
    canvas.fill_rect(tx, ty, tw, th, p.surface_alt);
    canvas.draw_rect(tx, ty, tw, th, p.border);
    canvas.draw_text(tx + 8, ty + 5, text, p.text, tw.saturating_sub(16));
}

// ── Badge ─────────────────────────────────────────────────────────────────────

/// Draw a small numeric badge (e.g., unread count on an icon).
pub fn draw_badge(canvas: &mut Canvas<'_>, x: i32, y: i32, count: u32, theme: Theme) {
    let p = tokens(theme);
    if count == 0 {
        return;
    }
    let text: [u8; 4] = {
        let mut b = [b' '; 4];
        if count < 10 {
            b[0] = b'0' + count as u8;
            b
        } else if count < 100 {
            b[0] = b'0' + (count / 10) as u8;
            b[1] = b'0' + (count % 10) as u8;
            b
        } else {
            b[0] = b'9';
            b[1] = b'9';
            b[2] = b'+';
            b
        }
    };
    let len = if count < 10 {
        1
    } else if count < 100 {
        2
    } else {
        3
    };
    let bw = len as u32 * 7 + 6;
    canvas.fill_rect(x, y, bw, 14, p.danger);
    canvas.draw_text(x + 3, y + 1, &text[..len], p.background, bw);
}

// ── Separator ─────────────────────────────────────────────────────────────────

/// Draw a horizontal separator with optional label.
pub fn draw_separator(canvas: &mut Canvas<'_>, x: i32, y: i32, w: u32, label: &[u8], theme: Theme) {
    let p = tokens(theme);
    canvas.draw_hline(x, y, w, p.border);
    if !label.is_empty() {
        let lw = (label.len() as u32).saturating_mul(5) + 8;
        let lx = x + (w.saturating_sub(lw)) as i32 / 2;
        canvas.fill_rect(lx, y - 6, lw, 13, p.background);
        canvas.draw_text(lx + 4, y - 5, label, p.text_muted, lw);
    }
}

// ── Toast notification ────────────────────────────────────────────────────────

/// Draw a toast notification card (anchored bottom-right by caller).
pub fn draw_notification_toast(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    title: &[u8],
    body: &[u8],
    accent: u32,
    theme: Theme,
) {
    let p = tokens(theme);
    // Shadow
    canvas.fill_rect(rect.x + 4, rect.y + 4, rect.w, rect.h, 0xAA000000);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    canvas.fill_rect(rect.x, rect.y, 3, rect.h, accent);
    canvas.draw_text(
        rect.x + 10,
        rect.y + 8,
        title,
        p.text,
        rect.w.saturating_sub(16),
    );
    canvas.draw_text(
        rect.x + 10,
        rect.y + 22,
        body,
        p.text_muted,
        rect.w.saturating_sub(16),
    );
}

// ── Sidebar item ──────────────────────────────────────────────────────────────

/// Draw a sidebar navigation item (icon dot + label).
pub fn draw_sidebar_item(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    label: &[u8],
    icon_col: u32,
    active: bool,
    hovered: bool,
    badge_count: u32,
    theme: Theme,
) {
    let p = tokens(theme);
    let fill = if active {
        p.primary
    } else if hovered {
        p.surface_alt
    } else {
        0
    };
    if fill != 0 {
        canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, fill);
    }
    if active {
        canvas.fill_rect(rect.x, rect.y, 3, rect.h, brighten(p.primary));
    }
    let tc = if active { p.background } else { p.text };
    canvas.fill_rect(
        rect.x + 12,
        rect.y + (rect.h as i32 - 10) / 2,
        10,
        10,
        icon_col,
    );
    canvas.draw_text(
        rect.x + 28,
        rect.y + (rect.h.saturating_sub(14)) as i32 / 2,
        label,
        tc,
        rect.w.saturating_sub(44),
    );
    if badge_count > 0 {
        let bx = rect.x + rect.w as i32 - 24;
        let by = rect.y + (rect.h as i32 - 14) / 2;
        draw_badge(canvas, bx, by, badge_count, theme);
    }
}

// ── Toolbar ───────────────────────────────────────────────────────────────────

/// Draw a horizontal toolbar strip.  `tools` is (label, pressed) pairs.
pub fn draw_toolbar(canvas: &mut Canvas<'_>, rect: Rect, tools: &[(&[u8], bool)], theme: Theme) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.chrome);
    canvas.draw_hline(rect.x, rect.y + rect.h as i32, rect.w, p.border);
    let btn_w = 56u32;
    for (i, (label, pressed)) in tools.iter().enumerate() {
        let bx = rect.x + (i as u32 * (btn_w + 2)) as i32 + 4;
        let by = rect.y + 4;
        let bh = rect.h.saturating_sub(8);
        if *pressed {
            canvas.fill_rect(bx, by, btn_w, bh, p.surface_alt);
            canvas.draw_rect(bx, by, btn_w, bh, p.primary);
        }
        let lw = (label.len() as u32).saturating_mul(5);
        let lx = bx + ((btn_w.saturating_sub(lw)) / 2) as i32;
        canvas.draw_text(
            lx,
            by + (bh.saturating_sub(14)) as i32 / 2,
            label,
            p.text,
            btn_w.saturating_sub(4),
        );
    }
}

// ── Breadcrumb ────────────────────────────────────────────────────────────────

/// Draw a path breadcrumb bar.  `segments` is ordered root → current.
pub fn draw_breadcrumb(canvas: &mut Canvas<'_>, rect: Rect, segments: &[&[u8]], theme: Theme) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.chrome);
    canvas.draw_hline(rect.x, rect.y + rect.h as i32 - 1, rect.w, p.border);
    let mut cx = rect.x + 8;
    let ty = rect.y + (rect.h.saturating_sub(14)) as i32 / 2;
    for (i, seg) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        let col = if is_last { p.text } else { p.primary };
        canvas.draw_text(cx, ty, seg, col, 0);
        cx += (seg.len() as i32).saturating_mul(5) + 2;
        if !is_last {
            canvas.draw_text(cx, ty, b">", p.text_muted, 0);
            cx += 14;
        }
    }
}

// ── Command / search bar ──────────────────────────────────────────────────────

/// Draw a command/input bar at the bottom.
pub fn draw_command_bar(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    prompt: &[u8],
    input: &[u8],
    cursor: bool,
    theme: Theme,
) {
    let p = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.chrome);
    canvas.draw_hline(rect.x, rect.y, rect.w, p.border);
    canvas.draw_text(rect.x + 4, rect.y + 6, prompt, p.primary, 0);
    canvas.draw_text(
        rect.x + 14,
        rect.y + 6,
        input,
        p.text,
        rect.w.saturating_sub(20),
    );
    if cursor {
        let input_w = Canvas::text_width(input) as i32;
        canvas.fill_rect(
            rect.x + 14 + input_w + 1,
            rect.y + 5,
            2,
            rect.h.saturating_sub(10),
            p.primary,
        );
    }
}

// ── Dialog ────────────────────────────────────────────────────────────────────

/// Draw a modal dialog with a title, body text, and a primary action button.
pub fn draw_dialog(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    title: &[u8],
    body: &[u8],
    primary_action: &[u8],
    theme: Theme,
) {
    let p = tokens(theme);
    // Dim overlay (caller fills the entire screen with a semi-transparent rect first)
    canvas.fill_rect(rect.x + 4, rect.y + 4, rect.w, rect.h, 0xAA000000); // shadow
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, p.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, p.border);
    // Title bar
    canvas.fill_rect(rect.x, rect.y, rect.w, 28, p.chrome);
    canvas.fill_rect(rect.x, rect.y, rect.w, 1, brighten(p.primary));
    canvas.draw_hline(rect.x, rect.y + 28, rect.w, p.border);
    let tw = (title.len() as u32).saturating_mul(5);
    let tx = rect.x + ((rect.w.saturating_sub(tw)) / 2) as i32;
    canvas.draw_text(tx, rect.y + 8, title, p.text, rect.w.saturating_sub(16));
    // Body
    canvas.draw_text(
        rect.x + 12,
        rect.y + 40,
        body,
        p.text,
        rect.w.saturating_sub(24),
    );
    // Primary button
    let bw = 80u32;
    let bh = 24u32;
    let bx = rect.x + rect.w.saturating_sub(bw + 12) as i32;
    let by = rect.y + rect.h.saturating_sub(bh + 12) as i32;
    canvas.fill_rect(bx, by, bw, bh, p.primary);
    canvas.draw_rect(bx, by, bw, bh, p.border);
    let lw = (primary_action.len() as u32).saturating_mul(5);
    canvas.draw_text(
        bx + ((bw.saturating_sub(lw)) / 2) as i32,
        by + 5,
        primary_action,
        p.background,
        bw,
    );
    // Cancel button
    let cx2 = bx - 88;
    canvas.draw_rect(cx2, by, bw, bh, p.border);
    canvas.draw_text(cx2 + 16, by + 5, b"Cancel", p.text_muted, bw);
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Brighten a BGRA32 colour by ~20 %.
fn brighten(c: u32) -> u32 {
    let r = ((c >> 16) & 0xFF).min(0xFF);
    let g = ((c >> 8) & 0xFF).min(0xFF);
    let b = (c & 0xFF).min(0xFF);
    let a = (c >> 24) & 0xFF;
    let r2 = (r + 50).min(0xFF);
    let g2 = (g + 50).min(0xFF);
    let b2 = (b + 50).min(0xFF);
    (a << 24) | (r2 << 16) | (g2 << 8) | b2
}

/// Darken (dim) a BGRA32 colour by ~20 %.
fn dim(c: u32) -> u32 {
    let r = ((c >> 16) & 0xFF).min(0xFF);
    let g = ((c >> 8) & 0xFF).min(0xFF);
    let b = (c & 0xFF).min(0xFF);
    let a = (c >> 24) & 0xFF;
    (a << 24) | (r.saturating_sub(40) << 16) | (g.saturating_sub(40) << 8) | b.saturating_sub(40)
}

/// Draw three traffic-light dots at (x, y) — 12 px diameter, 8 px gap.
fn draw_traffic_lights(canvas: &mut Canvas<'_>, x: i32, y: i32, _theme: Theme) {
    let positions = [
        (0xFF5F5757u32, x),
        (0xFFFFBD2E, x + 20),
        (0xFF28C940, x + 40),
    ];
    for (col, px) in &positions {
        canvas.fill_rect(*px, y, 12, 12, *col);
    }
}
