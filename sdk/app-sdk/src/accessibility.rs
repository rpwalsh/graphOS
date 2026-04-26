// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Accessibility settings and utilities for ring-3 GraphOS applications.
//!
//! # Features
//! - **Focus ring** — configurable width and colour painted around the
//!   focused widget rectangle.
//! - **High-contrast mode** — replaces the caller's colour palette with the
//!   system high-contrast palette when active.
//! - **Keyboard navigation** — tab-order traversal helpers for widget lists.

/// Global accessibility configuration (read from the kernel at startup).
#[derive(Clone, Copy, Debug)]
pub struct AccessibilityConfig {
    /// Width in pixels of the focus ring border.
    pub focus_ring_width: u32,
    /// Colour of the focus ring (ARGB u32).
    pub focus_ring_color: u32,
    /// When `true`, high-contrast colour overrides are active.
    pub high_contrast: bool,
    /// Font scale in percent (100 = normal).
    pub font_scale_pct: u32,
}

impl Default for AccessibilityConfig {
    fn default() -> Self {
        Self {
            focus_ring_width: 2,
            focus_ring_color: 0xFF_5A_B4_FF,
            high_contrast: false,
            font_scale_pct: 100,
        }
    }
}

impl AccessibilityConfig {
    /// Return the effective foreground colour, substituting high-contrast
    /// white when `self.high_contrast` is set.
    pub fn effective_fg(&self, normal_fg: u32) -> u32 {
        if self.high_contrast {
            0xFF_FF_FF_FF
        } else {
            normal_fg
        }
    }

    /// Return the effective background colour, substituting high-contrast
    /// black when `self.high_contrast` is set.
    pub fn effective_bg(&self, normal_bg: u32) -> u32 {
        if self.high_contrast {
            0xFF_00_00_00
        } else {
            normal_bg
        }
    }
}

// ---------------------------------------------------------------------------
// Focus ring painter
// ---------------------------------------------------------------------------

/// Paint a focus ring around the rectangle `(x, y, w, h)` using `buf`.
///
/// `stride` is the number of pixels per row.
pub fn paint_focus_ring(
    buf: &mut [u32],
    stride: usize,
    buf_h: usize,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    cfg: &AccessibilityConfig,
) {
    let width = cfg.focus_ring_width.max(1) as i32;
    let color = cfg.focus_ring_color;

    for t in 0..width {
        // Top edge.
        fill_hline(buf, stride, buf_h, x - t, y - t, w as i32 + t * 2, color);
        // Bottom edge.
        fill_hline(
            buf,
            stride,
            buf_h,
            x - t,
            y + h as i32 + t,
            w as i32 + t * 2,
            color,
        );
        // Left edge.
        fill_vline(buf, stride, buf_h, x - t, y - t, h as i32 + t * 2, color);
        // Right edge.
        fill_vline(
            buf,
            stride,
            buf_h,
            x + w as i32 + t,
            y - t,
            h as i32 + t * 2,
            color,
        );
    }
}

fn fill_hline(buf: &mut [u32], stride: usize, buf_h: usize, x: i32, y: i32, len: i32, color: u32) {
    if y < 0 || y as usize >= buf_h {
        return;
    }
    let row = y as usize * stride;
    for i in 0..len.max(0) as usize {
        let px = x + i as i32;
        if px < 0 || px as usize >= stride {
            continue;
        }
        let idx = row + px as usize;
        if idx < buf.len() {
            buf[idx] = color;
        }
    }
}

fn fill_vline(buf: &mut [u32], stride: usize, buf_h: usize, x: i32, y: i32, len: i32, color: u32) {
    if x < 0 || x as usize >= stride {
        return;
    }
    for i in 0..len.max(0) as usize {
        let py = y + i as i32;
        if py < 0 || py as usize >= buf_h {
            continue;
        }
        let idx = py as usize * stride + x as usize;
        if idx < buf.len() {
            buf[idx] = color;
        }
    }
}

// ---------------------------------------------------------------------------
// Keyboard navigation — tab-order traversal
// ---------------------------------------------------------------------------

/// Compute the index of the next focusable widget in tab order.
///
/// `count` is the total number of focusable widgets.
/// Pass `shift = true` for Shift+Tab (reverse order).
pub fn tab_next(current: usize, count: usize, shift: bool) -> usize {
    if count == 0 {
        return 0;
    }
    if shift {
        if current == 0 { count - 1 } else { current - 1 }
    } else {
        (current + 1) % count
    }
}
