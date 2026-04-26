// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Phase J Session 29 — Window chrome rendering (ring-3, `#![no_std]`).
//!
//! Window chrome is the visual frame around an application window:
//! title bar, close/min/max buttons, drop shadow, and rounded corners.
//! Chrome rendering runs entirely in ring-3; the kernel only provides
//! surface primitives via the canvas and surface-commit APIs.
//!
//! ## Title bar
//! The title bar is a frosted-glass panel at the top of the window.
//! It is rendered by calling the `ChromeRenderer::draw_titlebar()` method,
//! which writes into the caller-supplied pixel buffer row by row.  The
//! frosted effect is approximated as:
//!
//!   1. Tint the title-bar region with the active accent colour at 60%.
//!   2. Apply a 1 px specular rim highlight on the very top row.
//!   3. (Optional) when a back-buffer snapshot is available, call the
//!      kernel-side frosted_blur_region API.
//!
//! ## Buttons
//! Traffic-light buttons (close/min/max) appear on the left at 16 px
//! diameter with 8 px gaps (macOS style) or on the right (KDE style).
//! The default is left-side.  A `ButtonLayout` enum controls the style.
//!
//! ## Rounded corners
//! The caller must apply the `in_rounded_rect` mask from the kernel
//! effects module before blitting the chrome surface.  The ring-3
//! implementation approximates this with a simple distance check.
//!
//! ## Integration
//! ```no_run
//! let mut renderer = ChromeRenderer::new(ChromeConfig::default());
//! renderer.draw_titlebar(&mut buf, stride, width, "My App", 0xFF58A6FF, focused);
//! renderer.draw_buttons(&mut buf, stride, width, hovered, pressed);
//! ```

use crate::canvas::Canvas;

// ── Layout ────────────────────────────────────────────────────────────────────

/// Height of the title bar in pixels.
pub const TITLEBAR_HEIGHT: u32 = 32;
/// Diameter of close/min/max buttons.
pub const BUTTON_DIAM: u32 = 16;
/// Gap between buttons.
pub const BUTTON_GAP: u32 = 8;
/// Left margin for left-side buttons.
pub const BUTTON_MARGIN_L: u32 = 12;
/// Right margin for right-side buttons.
pub const BUTTON_MARGIN_R: u32 = 12;

/// Which side the window buttons appear on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonLayout {
    /// macOS-style: close/min/max on the left.
    Left,
    /// KDE-style: min/max/close on the right.
    Right,
}

impl Default for ButtonLayout {
    fn default() -> Self {
        ButtonLayout::Left
    }
}

/// Chrome configuration.
#[derive(Clone, Copy, Debug)]
pub struct ChromeConfig {
    /// Button layout style.
    pub button_layout: ButtonLayout,
    /// Accent colour (BGRA32).
    pub accent: u32,
    /// Title bar opacity percentage (0–100).  100 = fully opaque.
    pub titlebar_opacity_pct: u8,
    /// Whether to draw specular rim highlight on top edge.
    pub rim_highlight: bool,
    /// Corner radius (pixels).
    pub corner_radius: u32,
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            button_layout: ButtonLayout::Left,
            accent: 0xFF58A6FF, // Phase J Dark Glass primary
            titlebar_opacity_pct: 90,
            rim_highlight: true,
            corner_radius: 10,
        }
    }
}

// ── Button hit regions ────────────────────────────────────────────────────────

/// Hit-testable button region.
#[derive(Clone, Copy, Debug)]
pub struct ButtonRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl ButtonRect {
    /// Returns true if `(px, py)` falls within this region.
    pub fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Computed button positions for a window of the given width.
#[derive(Clone, Copy, Debug)]
pub struct ButtonLayout2 {
    pub close: ButtonRect,
    pub minimise: ButtonRect,
    pub maximise: ButtonRect,
}

fn button_y() -> u32 {
    TITLEBAR_HEIGHT / 2 - BUTTON_DIAM / 2
}

/// Compute button regions for a window of `win_width` px.
pub fn button_layout(config: &ChromeConfig, win_width: u32) -> ButtonLayout2 {
    match config.button_layout {
        ButtonLayout::Left => {
            let x0 = BUTTON_MARGIN_L;
            let step = BUTTON_DIAM + BUTTON_GAP;
            let y = button_y();
            ButtonLayout2 {
                close: ButtonRect {
                    x: x0,
                    y,
                    w: BUTTON_DIAM,
                    h: BUTTON_DIAM,
                },
                minimise: ButtonRect {
                    x: x0 + step,
                    y,
                    w: BUTTON_DIAM,
                    h: BUTTON_DIAM,
                },
                maximise: ButtonRect {
                    x: x0 + step * 2,
                    y,
                    w: BUTTON_DIAM,
                    h: BUTTON_DIAM,
                },
            }
        }
        ButtonLayout::Right => {
            let right = win_width.saturating_sub(BUTTON_MARGIN_R);
            let step = BUTTON_DIAM + BUTTON_GAP;
            let y = button_y();
            ButtonLayout2 {
                close: ButtonRect {
                    x: right.saturating_sub(BUTTON_DIAM),
                    y,
                    w: BUTTON_DIAM,
                    h: BUTTON_DIAM,
                },
                minimise: ButtonRect {
                    x: right.saturating_sub(BUTTON_DIAM + step),
                    y,
                    w: BUTTON_DIAM,
                    h: BUTTON_DIAM,
                },
                maximise: ButtonRect {
                    x: right.saturating_sub(BUTTON_DIAM + step * 2),
                    y,
                    w: BUTTON_DIAM,
                    h: BUTTON_DIAM,
                },
            }
        }
    }
}

// ── ChromeRenderer ────────────────────────────────────────────────────────────

/// Renders Phase J window chrome into a BGRA32 pixel buffer.
pub struct ChromeRenderer {
    config: ChromeConfig,
}

impl ChromeRenderer {
    /// Create a renderer with the given config.
    pub fn new(config: ChromeConfig) -> Self {
        Self { config }
    }

    /// Draw the title bar into the top `TITLEBAR_HEIGHT` rows of `buf`.
    ///
    /// `buf` is a BGRA32 pixel slice with `stride` u32 values per row.
    /// `width` is the pixel width of the window.
    /// `focused` tints the bar with the accent colour; unfocused uses a neutral tint.
    pub fn draw_titlebar(&self, buf: &mut [u32], stride: usize, width: u32, focused: bool) {
        let bar_h = TITLEBAR_HEIGHT as usize;
        let bar_w = width as usize;

        // Base colour: accent at opacity% if focused, else mid-grey.
        let tint = if focused {
            self.config.accent
        } else {
            0xFF2A2F38
        };
        let alpha = self.config.titlebar_opacity_pct as u32;

        // Background fill — tint blended over transparent black.
        let r = ((tint >> 16) & 0xFF) * alpha / 100;
        let g = ((tint >> 8) & 0xFF) * alpha / 100;
        let b = (tint & 0xFF) * alpha / 100;
        let fill = 0xFF000000 | (r << 16) | (g << 8) | b;

        for row in 0..bar_h {
            let row_base = row * stride;
            for col in 0..bar_w {
                if row_base + col < buf.len() {
                    buf[row_base + col] = fill;
                }
            }
        }

        // Rim highlight: 1 px top row.
        if self.config.rim_highlight {
            let rim = rim_highlight_color(self.config.accent);
            for col in 0..bar_w {
                if col < buf.len() {
                    buf[col] = rim;
                }
            }
        }

        // Bottom separator: 1 px slightly lighter border.
        if bar_h >= 1 {
            let sep_row = (bar_h - 1) * stride;
            let sep_col = 0xFF3A4050;
            for col in 0..bar_w {
                if sep_row + col < buf.len() {
                    buf[sep_row + col] = sep_col;
                }
            }
        }
    }

    /// Draw close/min/max buttons into `buf`.
    ///
    /// `hovered` and `pressed` are indices: 0 = close, 1 = minimise, 2 = maximise, 3 = none.
    pub fn draw_buttons(
        &self,
        buf: &mut [u32],
        stride: usize,
        width: u32,
        hovered: u8,
        pressed: u8,
    ) {
        let layout = button_layout(&self.config, width);
        let buttons = [
            (layout.close, 0u8, 0xFFFF5F57u32, 0xFFFF3B30u32), // close: red
            (layout.minimise, 1u8, 0xFFFFBD2E, 0xFFFFAB00),    // min: yellow
            (layout.maximise, 2u8, 0xFF28C940, 0xFF1EB33B),    // max: green
        ];

        for (rect, idx, normal_col, hover_col) in &buttons {
            let col = if *idx == pressed {
                hover_col
            } else if *idx == hovered {
                hover_col
            } else {
                normal_col
            };
            self.fill_circle(buf, stride, rect.x, rect.y, BUTTON_DIAM, *col);
        }
    }

    /// Fill a circle of diameter `d` at top-left `(x, y)` with BGRA32 `color`.
    fn fill_circle(&self, buf: &mut [u32], stride: usize, x: u32, y: u32, d: u32, color: u32) {
        let r = (d / 2) as i32;
        let cx = (x + d / 2) as i32;
        let cy = (y + d / 2) as i32;
        for py in (y as i32 - r).max(0)..(y as i32 + r).min(TITLEBAR_HEIGHT as i32) {
            for px in (x as i32 - r).max(0)..(x as i32 + r + d as i32) {
                let dx = px - cx;
                let dy = py - cy;
                if dx * dx + dy * dy <= r * r {
                    let idx = py as usize * stride + px as usize;
                    if idx < buf.len() {
                        buf[idx] = color;
                    }
                }
            }
        }
    }

    /// Return button hit-test layout for this config and window width.
    pub fn button_layout(&self, width: u32) -> ButtonLayout2 {
        button_layout(&self.config, width)
    }
}

// ── Title bar canvas helper ────────────────────────────────────────────────────

/// Draw chrome over an existing canvas.  Convenience wrapper.
pub fn apply_chrome(canvas: &mut Canvas, title: &[u8], focused: bool, config: &ChromeConfig) {
    let w = canvas.width();
    let h = canvas.height();
    if h < TITLEBAR_HEIGHT {
        return;
    }

    let renderer = ChromeRenderer::new(*config);
    // Draw title bar region (rows 0..TITLEBAR_HEIGHT).
    let bar_w = w as usize;
    let bar_h = TITLEBAR_HEIGHT as usize;
    // Write directly via canvas fill_rect approximation.
    let tint = if focused { config.accent } else { 0xFF2A2F38 };
    canvas.fill_rect(0, 0, w, TITLEBAR_HEIGHT, tint);

    if config.rim_highlight {
        let rim = rim_highlight_color(config.accent);
        canvas.fill_rect(0, 0, w, 1, rim);
    }

    // Title text (centred in bar if centre_title, else left-padded).
    if !title.is_empty() {
        let text_x = w / 2 - (title.len() as u32 * 8 / 2); // 8 px per char estimate
        let text_y = TITLEBAR_HEIGHT / 2 - 8;
        canvas.draw_text(text_x as i32, text_y as i32, title, 0xFFE6EDF3, 0);
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

fn rim_highlight_color(accent: u32) -> u32 {
    let r = ((accent >> 16) & 0xFF).min(0xFF);
    let g = ((accent >> 8) & 0xFF).min(0xFF);
    let b = (accent & 0xFF).min(0xFF);
    let r2 = (r + 77).min(0xFF);
    let g2 = (g + 77).min(0xFF);
    let b2 = (b + 77).min(0xFF);
    0xFF000000 | (r2 << 16) | (g2 << 8) | b2
}
