// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS-native first-class widgets.
//!
//! These are core controls for GraphOS product language:
//! graph view, timeline view, and inspector view.

use graphos_app_sdk::canvas::Canvas;

use crate::geom::Rect;
use crate::tokens::{Theme, tokens};
use crate::widgets::draw_panel;

/// One graph node in the Graph View widget.
#[derive(Clone, Copy, Debug)]
pub struct GraphNode {
    /// Node label.
    pub label: &'static [u8],
    /// Position in graph-local coordinates.
    pub x: i32,
    /// Position in graph-local coordinates.
    pub y: i32,
    /// Node color.
    pub color: u32,
}

/// One graph edge in the Graph View widget.
#[derive(Clone, Copy, Debug)]
pub struct GraphEdge {
    /// Source node index.
    pub from: usize,
    /// Target node index.
    pub to: usize,
}

/// Draw a graph canvas widget with explicit node/edge input.
pub fn draw_graph_view(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    title: &[u8],
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    selected: Option<usize>,
    theme: Theme,
) {
    let p = tokens(theme);
    let content = draw_panel(canvas, rect, title, theme);

    // Draw edges first.
    for edge in edges {
        if edge.from >= nodes.len() || edge.to >= nodes.len() {
            continue;
        }
        let a = nodes[edge.from];
        let b = nodes[edge.to];
        let x0 = content.x + a.x;
        let y0 = content.y + a.y;
        let x1 = content.x + b.x;
        let y1 = content.y + b.y;
        canvas.draw_line(x0, y0, x1, y1, p.grid);
    }

    // Draw nodes and labels.
    for (idx, node) in nodes.iter().enumerate() {
        let x = content.x + node.x;
        let y = content.y + node.y;
        canvas.fill_rect(x - 3, y - 3, 7, 7, node.color);
        if selected == Some(idx) {
            canvas.draw_rect(x - 6, y - 6, 13, 13, p.primary);
        }
        canvas.draw_text(
            x + 6,
            y - 4,
            node.label,
            p.text_muted,
            content.w.saturating_sub(6),
        );
    }
}

/// One timeline point in the Timeline View widget.
#[derive(Clone, Copy, Debug)]
pub struct TimelinePoint {
    /// Label for the milestone.
    pub label: &'static [u8],
    /// Offset in fixed point 0..1000 across width.
    pub offset_fp: u16,
    /// Whether this milestone is complete.
    pub complete: bool,
}

/// Draw a timeline widget with milestones and active cursor.
pub fn draw_timeline_view(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    title: &[u8],
    points: &[TimelinePoint],
    cursor: usize,
    theme: Theme,
) {
    let p = tokens(theme);
    let content = draw_panel(canvas, rect, title, theme);
    if points.is_empty() {
        return;
    }

    let mid_y = content.y + (content.h as i32 / 2);
    canvas.draw_hline(content.x, mid_y, content.w, p.border);

    for (idx, point) in points.iter().enumerate() {
        let x = content.x
            + ((content.w.saturating_sub(1) as u64 * point.offset_fp as u64) / 1000) as i32;
        let color = if point.complete {
            p.success
        } else {
            p.text_muted
        };
        canvas.fill_rect(x - 3, mid_y - 3, 7, 7, color);
        if idx == cursor {
            canvas.draw_rect(x - 5, mid_y - 5, 11, 11, p.primary);
        }
        canvas.draw_text(x - 8, mid_y + 8, point.label, p.text_muted, 96);
    }
}

/// One inspector row (label + value) in the Inspector View widget.
#[derive(Clone, Copy, Debug)]
pub struct InspectorRow {
    /// Property name.
    pub key: &'static [u8],
    /// Property value.
    pub value: &'static [u8],
}

/// Draw an inspector panel with key/value rows.
pub fn draw_inspector_view(
    canvas: &mut Canvas<'_>,
    rect: Rect,
    title: &[u8],
    rows: &[InspectorRow],
    theme: Theme,
) {
    let p = tokens(theme);
    let content = draw_panel(canvas, rect, title, theme);

    let mut y = content.y;
    for row in rows {
        if y + 16 > content.y + content.h as i32 {
            break;
        }
        canvas.draw_text(content.x, y, row.key, p.text_muted, content.w / 2);
        canvas.draw_text(
            content.x + (content.w as i32 / 2),
            y,
            row.value,
            p.text,
            content.w / 2,
        );
        y += 14;
    }
}
