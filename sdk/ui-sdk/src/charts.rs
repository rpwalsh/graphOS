// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Lightweight native chart renderers for ring3 surfaces.

use graphos_app_sdk::canvas::Canvas;

use crate::geom::Rect;
use crate::tokens::{Theme, tokens};

/// Draw a sparkline from a slice of unsigned values.
pub fn draw_sparkline(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    values: &[u32],
    theme: Theme,
    color: u32,
) {
    if values.len() < 2 || rect.w < 2 || rect.h < 2 {
        return;
    }
    let palette = tokens(theme);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, palette.border);
    let plot = rect.inset(2);
    draw_series(canvas, plot, values, color, false, theme);
}

/// Draw a compact line chart with grid lines.
pub fn draw_line_chart(canvas: &mut Canvas<'_>, rect: Rect, values: &[u32], theme: Theme) {
    if values.len() < 2 || rect.w < 8 || rect.h < 8 {
        return;
    }
    let palette = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, palette.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, palette.border);

    let plot = rect.inset(4);
    let grid_step = (plot.h / 4).max(1);
    for row in 0..=4u32 {
        let y = plot.y + (row * grid_step).min(plot.h.saturating_sub(1)) as i32;
        canvas.draw_hline(plot.x, y, plot.w, palette.grid);
    }

    draw_series(canvas, plot, values, palette.palette[0], true, theme);
}

/// Draw a simple bar chart.
pub fn draw_bar_chart(canvas: &mut Canvas<'_>, rect: Rect, values: &[u32], theme: Theme) {
    if values.is_empty() || rect.w < 8 || rect.h < 8 {
        return;
    }
    let palette = tokens(theme);
    canvas.fill_rect(rect.x, rect.y, rect.w, rect.h, palette.surface_alt);
    canvas.draw_rect(rect.x, rect.y, rect.w, rect.h, palette.border);
    let plot = rect.inset(4);
    let max_v = max_value(values).max(1);
    let bar_w = (plot.w / values.len() as u32).max(1);
    for (idx, value) in values.iter().enumerate() {
        let h = ((*value as u64 * plot.h as u64) / max_v as u64) as u32;
        let x = plot.x + idx as i32 * bar_w as i32;
        let y = plot.y + plot.h.saturating_sub(h) as i32;
        let color = palette.palette[idx % palette.palette.len()];
        let draw_w = bar_w.saturating_sub(1).max(1);
        canvas.fill_rect(x, y, draw_w, h.max(1), color);
    }
}

fn draw_series(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    values: &[u32],
    color: u32,
    markers: bool,
    theme: Theme,
) {
    let palette = tokens(theme);
    let max_v = max_value(values).max(1);
    let min_v = min_value(values);
    let span = max_v.saturating_sub(min_v).max(1);
    let denom = (values.len() as u32).saturating_sub(1).max(1);

    let mut prev_x = rect.x;
    let mut prev_y = project_y(rect, values[0], min_v, span);
    for (idx, value) in values.iter().enumerate().skip(1) {
        let x = rect.x + ((idx as u32 * rect.w.saturating_sub(1)) / denom) as i32;
        let y = project_y(rect, *value, min_v, span);
        canvas.draw_line(prev_x, prev_y, x, y, color);
        prev_x = x;
        prev_y = y;
    }

    if markers {
        for (idx, value) in values.iter().enumerate() {
            let x = rect.x + ((idx as u32 * rect.w.saturating_sub(1)) / denom) as i32;
            let y = project_y(rect, *value, min_v, span);
            canvas.fill_rect(x - 1, y - 1, 3, 3, palette.primary);
        }
    }
}

fn project_y(rect: Rect, value: u32, min_v: u32, span: u32) -> i32 {
    let normalized = value.saturating_sub(min_v) as u64;
    let offset = (normalized * rect.h.saturating_sub(1) as u64) / span as u64;
    rect.y + rect.h.saturating_sub(1) as i32 - offset as i32
}

fn max_value(values: &[u32]) -> u32 {
    let mut max_v = 0u32;
    for value in values {
        if *value > max_v {
            max_v = *value;
        }
    }
    max_v
}

fn min_value(values: &[u32]) -> u32 {
    let mut min_v = u32::MAX;
    for value in values {
        if *value < min_v {
            min_v = *value;
        }
    }
    if min_v == u32::MAX { 0 } else { min_v }
}
