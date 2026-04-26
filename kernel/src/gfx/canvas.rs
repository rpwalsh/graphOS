// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use font8x8::UnicodeFonts;

use crate::bootinfo::FramebufferFormat;
use crate::drivers::display;
use crate::gfx::surface::{DrawTarget, Surface};

#[derive(Clone, Copy)]
pub struct ClipRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl ClipRect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    fn right(self) -> u32 {
        self.x.saturating_add(self.w)
    }

    fn bottom(self) -> u32 {
        self.y.saturating_add(self.h)
    }
}

pub struct Canvas<'a, T: DrawTarget + ?Sized> {
    target: &'a mut T,
    clip: Option<ClipRect>,
}

impl<'a, T: DrawTarget + ?Sized> Canvas<'a, T> {
    pub fn new(target: &'a mut T) -> Self {
        Self { target, clip: None }
    }

    pub fn with_clip(target: &'a mut T, clip: ClipRect) -> Self {
        Self {
            target,
            clip: Some(clip),
        }
    }

    pub fn clear(&mut self, color: u32) {
        if let Some(clip) = self.clip {
            self.target
                .fill_rect_region(clip.x, clip.y, clip.w, clip.h, color);
        } else {
            self.target.clear(color);
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        let Some((x0, y0, w0, h0)) = self.clipped_rect(x, y, w, h) else {
            return;
        };
        self.target.fill_rect_region(x0, y0, w0, h0, color);
    }

    pub fn stroke_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        if w < 2 || h < 2 {
            self.fill_rect(x, y, w, h, color);
            return;
        }
        self.fill_rect(x, y, w, 1, color);
        self.fill_rect(x, y + h as i32 - 1, w, 1, color);
        self.fill_rect(x, y, 1, h, color);
        self.fill_rect(x + w as i32 - 1, y, 1, h, color);
    }

    pub fn draw_text(&mut self, x: i32, y: i32, text: &[u8], fg: u32, bg: u32) {
        let mut cursor = x;
        for &byte in text {
            let ch = match byte {
                0x20..=0x7e => byte as char,
                b'\t' => ' ',
                _ => '?',
            };
            self.draw_char(cursor, y, ch, fg, bg);
            cursor += 8;
        }
    }

    pub fn blit(&mut self, src: &Surface, dst_x: i32, dst_y: i32) {
        let Some((src_x, src_y, dst_x, dst_y, w, h)) =
            self.clipped_blit(src.width(), src.height(), dst_x, dst_y)
        else {
            return;
        };
        self.target
            .blit_surface_region(src, src_x, src_y, w, h, dst_x, dst_y);
    }

    fn draw_char(&mut self, x: i32, y: i32, ch: char, fg: u32, bg: u32) {
        let glyph = font8x8::BASIC_FONTS
            .get(ch)
            .or_else(|| font8x8::BASIC_FONTS.get('?'))
            .unwrap_or([0; 8]);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8u32 {
                let color = if (bits >> col) & 1 == 1 { fg } else { bg };
                let px = x.saturating_add(col as i32);
                let py = y.saturating_add(row as i32);
                if !self.contains(px, py) {
                    continue;
                }
                self.target.set_pixel(px as u32, py as u32, color);
            }
        }
    }

    fn clipped_rect(&self, x: i32, y: i32, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
        if w == 0 || h == 0 {
            return None;
        }
        let mut x0 = x.max(0) as u32;
        let mut y0 = y.max(0) as u32;
        let mut x1 = x
            .saturating_add(w as i32)
            .min(self.target.width() as i32)
            .max(0) as u32;
        let mut y1 = y
            .saturating_add(h as i32)
            .min(self.target.height() as i32)
            .max(0) as u32;

        if let Some(clip) = self.clip {
            x0 = x0.max(clip.x);
            y0 = y0.max(clip.y);
            x1 = x1.min(clip.right());
            y1 = y1.min(clip.bottom());
        }

        if x0 >= x1 || y0 >= y1 {
            return None;
        }

        Some((x0, y0, x1 - x0, y1 - y0))
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        let x = x as u32;
        let y = y as u32;
        if x >= self.target.width() || y >= self.target.height() {
            return false;
        }
        if let Some(clip) = self.clip {
            x >= clip.x && x < clip.right() && y >= clip.y && y < clip.bottom()
        } else {
            true
        }
    }

    /// Draw an anti-aliased line from (x0,y0) to (x1,y1) using Bresenham's algorithm.
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx: i32 = if x0 < x1 { 1 } else { -1 };
        let sy: i32 = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let mut cx = x0;
        let mut cy = y0;
        loop {
            if self.contains(cx, cy) {
                self.target.set_pixel(cx as u32, cy as u32, color);
            }
            if cx == x1 && cy == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                if cx == x1 {
                    break;
                }
                err += dy;
                cx += sx;
            }
            if e2 <= dx {
                if cy == y1 {
                    break;
                }
                err += dx;
                cy += sy;
            }
        }
    }

    /// Draw a polyline through the given points (pairs: x,y,x,y,...).
    pub fn draw_polyline(&mut self, points: &[(i32, i32)], color: u32) {
        for pair in points.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            self.draw_line(x0, y0, x1, y1, color);
        }
    }

    /// Draw a thick line (width pixels wide) by repeating the Bresenham line.
    pub fn draw_line_thick(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, width: u32) {
        if width <= 1 {
            self.draw_line(x0, y0, x1, y1, color);
            return;
        }
        let half = width as i32 / 2;
        let dx = x1 - x0;
        let dy = y1 - y0;
        // Perpendicular unit-ish offsets (integer approximation)
        let (px, py) = if dx.abs() > dy.abs() {
            (0i32, 1i32)
        } else {
            (1i32, 0i32)
        };
        for off in -half..=half {
            self.draw_line(
                x0 + px * off,
                y0 + py * off,
                x1 + px * off,
                y1 + py * off,
                color,
            );
        }
    }

    /// Fill a circle at (cx, cy) with radius r.
    pub fn fill_circle(&mut self, cx: i32, cy: i32, r: i32, color: u32) {
        let r2 = r * r;
        for dy in -r..=r {
            let dx_max = integer_sqrt_i32(r2 - dy * dy);
            let x0 = cx - dx_max;
            let y = cy + dy;
            let w = (dx_max * 2 + 1) as u32;
            self.fill_rect(x0, y, w, 1, color);
        }
    }

    /// Draw a circle outline at (cx, cy) with radius r.
    pub fn stroke_circle(&mut self, cx: i32, cy: i32, r: i32, color: u32) {
        let mut x = r;
        let mut y = 0i32;
        let mut decision = 1 - r;
        while x >= y {
            for (ox, oy) in [
                (x, y),
                (-x, y),
                (x, -y),
                (-x, -y),
                (y, x),
                (-y, x),
                (y, -x),
                (-y, -x),
            ] {
                let px = cx + ox;
                let py = cy + oy;
                if self.contains(px, py) {
                    self.target.set_pixel(px as u32, py as u32, color);
                }
            }
            y += 1;
            if decision <= 0 {
                decision += 2 * y + 1;
            } else {
                x -= 1;
                decision += 2 * (y - x) + 1;
            }
        }
    }

    /// Fill a pie slice from start_angle to end_angle (degrees, 0=right, CW).
    /// Uses scanline fill with trigonometric-free integer approximation via
    /// Bresenham circle + scanlines.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_arc(
        &mut self,
        cx: i32,
        cy: i32,
        r: i32,
        inner_r: i32,   // 0 for pie, >0 for donut
        start_deg: i32, // 0–359
        end_deg: i32,   // 0–359
        color: u32,
    ) {
        if r <= 0 {
            return;
        }
        // Sweep all pixels inside the bounding box, check if they're in the arc.
        for dy in -r..=r {
            for dx in -r..=r {
                let dist2 = dx * dx + dy * dy;
                if dist2 > r * r {
                    continue;
                }
                if inner_r > 0 && dist2 < inner_r * inner_r {
                    continue;
                }
                // Compute angle of this pixel in degrees (0=right, CW).
                // Use integer atan2 approximation.
                let angle = int_atan2_deg(dy, dx);
                let in_slice = if start_deg <= end_deg {
                    angle >= start_deg && angle < end_deg
                } else {
                    // Wraps around 360
                    angle >= start_deg || angle < end_deg
                };
                if in_slice {
                    let px = cx + dx;
                    let py = cy + dy;
                    if self.contains(px, py) {
                        self.target.set_pixel(px as u32, py as u32, color);
                    }
                }
            }
        }
    }

    /// Fill a convex polygon using scanline rasterisation.
    /// `vertices` is a slice of (x, y) points.
    pub fn fill_polygon(&mut self, vertices: &[(i32, i32)], color: u32) {
        if vertices.len() < 3 {
            return;
        }

        let min_y = vertices.iter().map(|v| v.1).min().unwrap_or(0);
        let max_y = vertices.iter().map(|v| v.1).max().unwrap_or(0);
        let n = vertices.len();

        for y in min_y..=max_y {
            let mut xs = [0i32; 64];
            let mut count = 0usize;
            for i in 0..n {
                let j = (i + 1) % n;
                let (x0, y0) = vertices[i];
                let (x1, y1) = vertices[j];
                if (y0 <= y && y < y1) || (y1 <= y && y < y0) {
                    // Intersection of edge with scanline y
                    let ix = x0 + (y - y0) * (x1 - x0) / (y1 - y0);
                    if count < 64 {
                        xs[count] = ix;
                        count += 1;
                    }
                }
            }
            // Sort intersections
            let xs_slice = &mut xs[..count];
            xs_slice.sort_unstable();
            // Fill spans
            let mut i = 0usize;
            while i + 1 < count {
                let x_start = xs_slice[i];
                let x_end = xs_slice[i + 1];
                if x_end > x_start {
                    self.fill_rect(x_start, y, (x_end - x_start) as u32, 1, color);
                }
                i += 2;
            }
        }
    }

    /// Blend a colour with alpha onto the canvas (src-over, premultiplied).
    /// `alpha` is 0–255.
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: u32, alpha: u8) {
        if !self.contains(x, y) {
            return;
        }
        if alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.target.set_pixel(x as u32, y as u32, color);
            return;
        }
        let px = x as u32;
        let py = y as u32;
        let dst = self.target.get_pixel(px, py);
        let blended = alpha_blend(dst, color, alpha);
        self.target.set_pixel(px, py, blended);
    }

    fn clipped_blit(
        &self,
        src_w: u32,
        src_h: u32,
        dst_x: i32,
        dst_y: i32,
    ) -> Option<(u32, u32, u32, u32, u32, u32)> {
        if src_w == 0 || src_h == 0 {
            return None;
        }

        let mut src_x = 0u32;
        let mut src_y = 0u32;
        let mut out_x = dst_x;
        let mut out_y = dst_y;
        let mut width = src_w;
        let mut height = src_h;

        if out_x < 0 {
            let delta = out_x.unsigned_abs().min(width);
            src_x = src_x.saturating_add(delta);
            width = width.saturating_sub(delta);
            out_x = 0;
        }
        if out_y < 0 {
            let delta = out_y.unsigned_abs().min(height);
            src_y = src_y.saturating_add(delta);
            height = height.saturating_sub(delta);
            out_y = 0;
        }

        width = width.min(self.target.width().saturating_sub(out_x as u32));
        height = height.min(self.target.height().saturating_sub(out_y as u32));

        if let Some(clip) = self.clip {
            if (out_x as u32) < clip.x {
                let delta = clip.x - out_x as u32;
                src_x = src_x.saturating_add(delta);
                width = width.saturating_sub(delta.min(width));
                out_x = clip.x as i32;
            }
            if (out_y as u32) < clip.y {
                let delta = clip.y - out_y as u32;
                src_y = src_y.saturating_add(delta);
                height = height.saturating_sub(delta.min(height));
                out_y = clip.y as i32;
            }
            width = width.min(clip.right().saturating_sub(out_x as u32));
            height = height.min(clip.bottom().saturating_sub(out_y as u32));
        }

        if width == 0 || height == 0 {
            return None;
        }

        Some((src_x, src_y, out_x as u32, out_y as u32, width, height))
    }
}

// ── Free helper functions used by the drawing primitives ───────────────────

/// Integer square root (floor).
fn integer_sqrt_i32(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Integer atan2 approximation returning degrees 0–359 (0=right, clockwise).
fn int_atan2_deg(dy: i32, dx: i32) -> i32 {
    // Use a lookup-table-free integer approximation via CORDIC-style division.
    if dx == 0 && dy == 0 {
        return 0;
    }
    // Compute octant-based angle using integer division.
    // abs(dy) <= abs(dx): angle in [0, 45]
    let abs_dx = dx.unsigned_abs() as i32;
    let abs_dy = dy.unsigned_abs() as i32;
    // Primary angle in degrees 0–45 via ratio
    let primary = if abs_dx >= abs_dy {
        // Angle from x-axis: atan(dy/dx) ≈ 45 * |dy| / |dx|
        (45 * abs_dy) / abs_dx.max(1)
    } else {
        // Angle from y-axis: 90 - 45 * |dx| / |dy|
        90 - (45 * abs_dx) / abs_dy.max(1)
    };
    // Map to quadrant: our coordinate system: dx right, dy down, CW from right.
    let angle = match (dx >= 0, dy >= 0) {
        (true, true) => primary,         // Q1: 0–90
        (false, true) => 180 - primary,  // Q2: 90–180
        (false, false) => 180 + primary, // Q3: 180–270
        (true, false) => 360 - primary,  // Q4: 270–360
    };
    angle % 360
}

/// Software src-over alpha blend.  All channels 8-bit, no pre-multiply.
/// `alpha` is 0–255 (opacity of `src`).
fn alpha_blend(dst: u32, src: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let ia = 255 - a;
    let r = ((((dst >> 16) & 0xFF) * ia + ((src >> 16) & 0xFF) * a) / 255) & 0xFF;
    let g = ((((dst >> 8) & 0xFF) * ia + ((src >> 8) & 0xFF) * a) / 255) & 0xFF;
    let b = (((dst & 0xFF) * ia + (src & 0xFF) * a) / 255) & 0xFF;
    0xFF000000 | (r << 16) | (g << 8) | b
}

/// Software global fill path used by the Session 28 compositor fallback.
///
/// This writes directly to the active scanout framebuffer as an interim path
/// until full backbuffer/swapchain routing lands.
// ── 3-D sphere graph renderer ─────────────────────────────────────────────────
//
// Renders the graph-first desktop chrome: glowing orbital nodes arranged on
// the surface of a sphere, connected by glow-lit arcs, matching the design
// reference ("KDE-like 3D graph first" visual language).
//
// All math uses integer / fixed-point arithmetic (no libm) so it works in
// no_std kernel context.

/// One graph node positioned in 3-D spherical coords.
/// azimuth and elevation are in degrees × 10 (fixed point, 0–3599 and -900–900).
#[derive(Clone, Copy)]
pub struct GraphNode {
    /// Degrees × 10 around Y axis (0 = front).
    pub azimuth_d10: i32,
    /// Degrees × 10 above equator (−900 = south pole, +900 = north).
    pub elevation_d10: i32,
    /// Radius scale × 1000 (1000 = full sphere_r).
    pub radius_fp: u32,
    /// BGRA node colour (used for glow tint).
    pub color: u32,
    /// Short node label (up to 12 bytes, null-terminated).
    pub label: [u8; 12],
}

impl GraphNode {
    pub const fn new(az: i32, el: i32, r: u32, color: u32) -> Self {
        Self {
            azimuth_d10: az,
            elevation_d10: el,
            radius_fp: r,
            color,
            label: [0; 12],
        }
    }
}

/// Edge between two node indices.
#[derive(Clone, Copy)]
pub struct GraphEdge {
    pub a: u8,
    pub b: u8,
}

// ── Integer sin/cos lookup (10-degree steps) ─────────────────────────────────
//  sin_lut[i] = sin(i * 10°) × 1024, for i in 0..36
static SIN_LUT: [i32; 38] = [
    0, 178, 354, 527, 695, 857, 1009, 1152, 1282, 1398, 1499, 1583, 1649, 1695, 1720, 1724, 1707,
    1668, 1609, 1530, 1432, 1316, 1184, 1036, 875, 702, 520, 330, 134, -64, -261, -455, -642, -822,
    -991, -1147, -1289, -1412,
];

fn isin1024(deg: i32) -> i32 {
    let d = ((deg % 360) + 360) % 360;
    let step = d / 10;
    let frac = d % 10;
    let s0 = SIN_LUT[step.min(36) as usize];
    let s1 = SIN_LUT[(step + 1).min(36) as usize];
    s0 + (s1 - s0) * frac / 10
}

fn icos1024(deg: i32) -> i32 {
    isin1024(deg + 90)
}

pub fn isin1024_pub(deg: i32) -> i32 {
    isin1024(deg)
}

pub fn icos1024_pub(deg: i32) -> i32 {
    icos1024(deg)
}

/// Project a spherical node to 2-D screen coordinates.
/// Returns `(screen_x, screen_y, depth_z)`.  depth_z > 0 = in front.
fn project_node(
    node: &GraphNode,
    cx: i32,
    cy: i32,
    sphere_r: i32,
    rot_deg: i32,
) -> (i32, i32, i32) {
    let az = node.azimuth_d10 / 10 + rot_deg;
    let el = node.elevation_d10 / 10;
    let r = (sphere_r as i64 * node.radius_fp as i64 / 1000) as i32;

    // 3-D position on sphere surface.
    let cos_el = icos1024(el);
    let sin_el = isin1024(el);
    let cos_az = icos1024(az);
    let sin_az = isin1024(az);

    // x = r * cos_el * sin_az / 1024^2
    // y = r * -sin_el / 1024
    // z = r * cos_el * cos_az / 1024^2
    let x3 = (r as i64 * cos_el as i64 * sin_az as i64 / (1024 * 1024)) as i32;
    let y3 = (r as i64 * (-sin_el) as i64 / 1024) as i32;
    let z3 = (r as i64 * cos_el as i64 * cos_az as i64 / (1024 * 1024)) as i32;

    // Simple orthographic projection (no perspective — keeps it crisp).
    (cx + x3, cy + y3, z3)
}

/// Additive alpha blend of a glow halo — brighter blending for node highlights.
fn additive_blend(dst: u32, src_rgb: u32, intensity: u32) -> u32 {
    let r = (((dst >> 16) & 0xFF) + ((src_rgb >> 16) & 0xFF) * intensity / 255).min(255);
    let g = (((dst >> 8) & 0xFF) + ((src_rgb >> 8) & 0xFF) * intensity / 255).min(255);
    let b = ((dst & 0xFF) + (src_rgb & 0xFF) * intensity / 255).min(255);
    (r << 16) | (g << 8) | b
}

/// Draw a glowing node circle: solid core + soft radial halo.
fn draw_glow_node<T: DrawTarget + ?Sized>(
    target: &mut T,
    px: i32,
    py: i32,
    depth: i32,
    sphere_r: i32,
    color: u32,
) {
    // Size scales with depth (farther = smaller, min 3px).
    let base_r: i32 = 8;
    let depth_scale =
        (512 + depth.max(-sphere_r).min(sphere_r) + sphere_r) * 1024 / (2 * sphere_r.max(1) + 1);
    let r = (base_r * depth_scale as i32 / 1024).clamp(2, base_r + 4);
    let halo_r = r + r / 2 + 3;

    let w = target.width() as i32;
    let h = target.height() as i32;
    // Halo (additive glow rings).
    for dy in -halo_r..=halo_r {
        for dx in -halo_r..=halo_r {
            let d2 = dx * dx + dy * dy;
            let hr2 = halo_r * halo_r;
            let cr2 = r * r;
            if d2 > hr2 {
                continue;
            }
            let px2 = px + dx;
            let py2 = py + dy;
            if px2 < 0 || py2 < 0 || px2 >= w || py2 >= h {
                continue;
            }
            if d2 <= cr2 {
                // Solid core.
                target.set_pixel(px2 as u32, py2 as u32, color | 0xFF000000);
            } else {
                // Glow falloff: intensity decreases with distance^2.
                let falloff = (hr2 - d2) * 200 / (hr2 - cr2).max(1);
                let existing = target.get_pixel(px2 as u32, py2 as u32);
                let blended = additive_blend(existing, color, falloff as u32);
                target.set_pixel(px2 as u32, py2 as u32, blended | 0xFF000000);
            }
        }
    }
}

/// Draw a glowing edge line between two screen points.
fn draw_glow_line<T: DrawTarget + ?Sized>(
    target: &mut T,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: u32,
    opacity: u8,
) {
    // Bresenham with additive blend, single pixel width.
    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx - dy;
    let mut cx = x0;
    let mut cy = y0;
    let w = target.width() as i32;
    let h = target.height() as i32;
    let intensity = opacity as u32;
    loop {
        if cx >= 0 && cy >= 0 && cx < w && cy < h {
            let existing = target.get_pixel(cx as u32, cy as u32);
            let blended = additive_blend(existing, color, intensity);
            target.set_pixel(cx as u32, cy as u32, blended | 0xFF000000);
        }
        if cx == x1 && cy == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            cx += sx;
        }
        if e2 < dx {
            err += dx;
            cy += sy;
        }
    }
}

/// Render the complete 3-D sphere graph into a `DrawTarget`.
///
/// - `cx`, `cy`  — centre of sphere in target coordinates.
/// - `sphere_r`  — radius in pixels.
/// - `rot_deg`   — current rotation angle in degrees (animate by incrementing).
/// - `nodes`     — node array (positions in spherical coords, colours).
/// - `edges`     — edge array (pairs of node indices).
///
/// The background is NOT cleared — call `canvas.clear(bg)` before this.
pub fn draw_sphere_graph<T: DrawTarget + ?Sized>(
    target: &mut T,
    cx: i32,
    cy: i32,
    sphere_r: i32,
    rot_deg: i32,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) {
    // 1. Project all nodes.
    let mut projected = [(0i32, 0i32, 0i32); 32];
    let n = nodes.len().min(projected.len());
    for i in 0..n {
        projected[i] = project_node(&nodes[i], cx, cy, sphere_r, rot_deg);
    }

    // 2. Draw a faint "orbit ring" (equatorial circle) for depth cue.
    {
        let steps = 72usize;
        let mut prev_x = cx + sphere_r;
        let mut prev_y = cy;
        for step in 1..=steps {
            let deg = (step * 360 / steps) as i32;
            let nx = cx + icos1024(deg) * sphere_r / 1024;
            let ny = cy + isin1024(deg) * sphere_r / 1024 / 3; // squash Y for tilt illusion
            let w = target.width() as i32;
            let h = target.height() as i32;
            if prev_x >= 0
                && prev_y >= 0
                && prev_x < w
                && prev_y < h
                && nx >= 0
                && ny >= 0
                && nx < w
                && ny < h
            {
                draw_glow_line(target, prev_x, prev_y, nx, ny, 0x1a3a5c, 40);
            }
            prev_x = nx;
            prev_y = ny;
        }
    }

    // 3. Draw edges (back-to-front: draw back-facing edges first, dimmer).
    for pass in 0..2u8 {
        for edge in edges.iter() {
            let ai = edge.a as usize;
            let bi = edge.b as usize;
            if ai >= n || bi >= n {
                continue;
            }
            let (ax, ay, az) = projected[ai];
            let (bx, by, bz) = projected[bi];
            let avg_z = az + bz;
            let is_back = avg_z < 0;
            if pass == 0 && !is_back {
                continue;
            }
            if pass == 1 && is_back {
                continue;
            }
            let ca = nodes[ai].color;
            let cb = nodes[bi].color;
            // Blend edge colour from both endpoints.
            let edge_r = ((ca >> 16 & 0xFF) + (cb >> 16 & 0xFF)) / 2;
            let edge_g = ((ca >> 8 & 0xFF) + (cb >> 8 & 0xFF)) / 2;
            let edge_b = ((ca & 0xFF) + (cb & 0xFF)) / 2;
            let edge_col = (edge_r << 16) | (edge_g << 8) | edge_b;
            let opacity = if is_back { 30u8 } else { 80u8 };
            draw_glow_line(target, ax, ay, bx, by, edge_col, opacity);
        }
    }

    // 4. Draw nodes (back-to-front order).
    // Simple sort: collect depth order.
    let mut order: [usize; 32] = [0; 32];
    for i in 0..n {
        order[i] = i;
    }
    // Insertion sort by z (ascending = back first).
    for i in 1..n {
        let key = order[i];
        let key_z = projected[key].2;
        let mut j = i;
        while j > 0 && projected[order[j - 1]].2 > key_z {
            order[j] = order[j - 1];
            j -= 1;
        }
        order[j] = key;
    }
    for &idx in &order[..n] {
        let (px, py, pz) = projected[idx];
        draw_glow_node(target, px, py, pz, sphere_r, nodes[idx].color);
    }
}

pub fn fill_rect_global(x: i32, y: i32, w: u32, h: u32, color: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let snap = display::telemetry_snapshot();
    if !snap.online || snap.framebuffer_addr == 0 {
        return;
    }

    let fb_w = snap.mode.width as i32;
    let fb_h = snap.mode.height as i32;
    let x0 = x.clamp(0, fb_w.max(0));
    let y0 = y.clamp(0, fb_h.max(0));
    let x1 = x.saturating_add(w as i32).clamp(0, fb_w.max(0));
    let y1 = y.saturating_add(h as i32).clamp(0, fb_h.max(0));
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let out = native_color_for_mode(snap.mode.format, color);
    let stride = snap.mode.stride as usize;
    let base = snap.framebuffer_addr as *mut u32;
    for row in y0 as usize..y1 as usize {
        let row_ptr = unsafe { base.add(row.saturating_mul(stride) + x0 as usize) };
        let span = (x1 - x0) as usize;
        for col in 0..span {
            unsafe {
                row_ptr.add(col).write_volatile(out);
            }
        }
    }

    let dirty_pixels = (x1 - x0) as u64 * (y1 - y0) as u64;
    display::note_native_fill(dirty_pixels);
}

/// Software global blit path used by the Session 28 compositor fallback.
///
/// Accepts a BGRA/ARGB source buffer and target placement/transform metadata.
pub fn blit_pixels_global(
    src: &[u32],
    src_w: usize,
    src_h: usize,
    dst_x: i32,
    dst_y: i32,
    scale_fp: u16,
    opacity: u8,
) {
    if src_w == 0 || src_h == 0 || src.len() < src_w.saturating_mul(src_h) || opacity == 0 {
        return;
    }
    let snap = display::telemetry_snapshot();
    if !snap.online || snap.framebuffer_addr == 0 {
        return;
    }

    let scale = scale_fp.max(1) as u32;
    let dst_w = ((src_w as u64).saturating_mul(scale as u64) / 1024) as i32;
    let dst_h = ((src_h as u64).saturating_mul(scale as u64) / 1024) as i32;
    if dst_w <= 0 || dst_h <= 0 {
        return;
    }

    let fb_w = snap.mode.width as i32;
    let fb_h = snap.mode.height as i32;
    let x0 = dst_x.clamp(0, fb_w.max(0));
    let y0 = dst_y.clamp(0, fb_h.max(0));
    let x1 = dst_x.saturating_add(dst_w).clamp(0, fb_w.max(0));
    let y1 = dst_y.saturating_add(dst_h).clamp(0, fb_h.max(0));
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let base = snap.framebuffer_addr as *mut u32;
    let stride = snap.mode.stride as usize;
    let use_blend = opacity < 255;
    let mode = snap.mode.format;

    for dy in y0..y1 {
        let sy = (((dy - dst_y) as i64).saturating_mul(1024) / scale as i64) as usize;
        if sy >= src_h {
            continue;
        }
        let row_ptr = unsafe { base.add(dy as usize * stride) };
        for dx in x0..x1 {
            let sx = (((dx - dst_x) as i64).saturating_mul(1024) / scale as i64) as usize;
            if sx >= src_w {
                continue;
            }
            let src_px = src[sy * src_w + sx];
            let out = if use_blend {
                let dst_native = unsafe { row_ptr.add(dx as usize).read_volatile() };
                let dst_px = from_native_color_for_mode(mode, dst_native);
                let mixed = alpha_blend(dst_px, src_px, opacity);
                native_color_for_mode(mode, mixed)
            } else {
                native_color_for_mode(mode, src_px)
            };
            unsafe {
                row_ptr.add(dx as usize).write_volatile(out);
            }
        }
    }

    let dirty_pixels = (x1 - x0) as u64 * (y1 - y0) as u64;
    display::note_native_fill(dirty_pixels);
}

fn native_color_for_mode(format: FramebufferFormat, color: u32) -> u32 {
    match format {
        FramebufferFormat::Rgb | FramebufferFormat::BltOnly => color,
        FramebufferFormat::Bgr | FramebufferFormat::Bitmask | FramebufferFormat::Unknown => {
            let r = (color >> 16) & 0xFF;
            let g = (color >> 8) & 0xFF;
            let b = color & 0xFF;
            (b << 16) | (g << 8) | r
        }
    }
}

fn from_native_color_for_mode(format: FramebufferFormat, native: u32) -> u32 {
    match format {
        FramebufferFormat::Rgb | FramebufferFormat::BltOnly => native,
        FramebufferFormat::Bgr | FramebufferFormat::Bitmask | FramebufferFormat::Unknown => {
            let r = native & 0xFF;
            let g = (native >> 8) & 0xFF;
            let b = (native >> 16) & 0xFF;
            0xFF000000 | (r << 16) | (g << 8) | b
        }
    }
}
