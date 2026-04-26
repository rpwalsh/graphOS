// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Shell3D — Phase J complete implementation.
//!
//! 3D spatial app launcher with:
//! - Apps as floating cards on a hemisphere, perspective-projected
//! - Mouse orbit / drag to rotate the scene
//! - Spring physics for card hover animation
//! - Click-to-launch via SYS_SPAWN
//! - Dock strip at bottom with pinned apps + clock
//! - System taskbar with open windows list
//! - Exposé overview mode (Tab key)
//! - Phase J frosted glass window chrome

extern crate alloc;

use gl3d::GlScene;
use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::tokens::{Theme, tokens};

mod gl3d;

// Keep Shell3D within the compositor display mode configured by boot.ps1
// (virtio-gpu xres=1280, yres=800) to avoid off-screen clipping artifacts.
const WIN_W: u32 = 1280;
const WIN_H: u32 = 800;
const TITLEBAR_H: u32 = 36;
const DOCK_H: u32 = 84;
const TASKBAR_H: u32 = 54;
const SCENE_H: u32 = WIN_H - TITLEBAR_H - DOCK_H - TASKBAR_H;

// ── Fixed-point math (×1000) ──────────────────────────────────────────────────

type Fp = i32;
const FP: Fp = 1000;

#[derive(Clone, Copy, Default)]
struct V3 {
    x: Fp,
    y: Fp,
    z: Fp,
}
impl V3 {
    fn new(xf: f32, yf: f32, zf: f32) -> Self {
        Self {
            x: (xf * FP as f32) as i32,
            y: (yf * FP as f32) as i32,
            z: (zf * FP as f32) as i32,
        }
    }
    fn dot(self, o: V3) -> i64 {
        self.x as i64 * o.x as i64 + self.y as i64 * o.y as i64 + self.z as i64 * o.z as i64
    }
}
impl core::ops::Add for V3 {
    type Output = V3;
    fn add(self, r: V3) -> V3 {
        V3 {
            x: self.x + r.x,
            y: self.y + r.y,
            z: self.z + r.z,
        }
    }
}
impl core::ops::Sub for V3 {
    type Output = V3;
    fn sub(self, r: V3) -> V3 {
        V3 {
            x: self.x - r.x,
            y: self.y - r.y,
            z: self.z - r.z,
        }
    }
}

fn fp_cos(angle_deg: i32) -> Fp {
    // Lookup for multiples of 15°
    const C: [i32; 25] = [
        1000, 966, 866, 707, 500, 259, 0, -259, -500, -707, -866, -966, -1000, -966, -866, -707,
        -500, -259, 0, 259, 500, 707, 866, 966, 1000,
    ];
    let i = ((angle_deg.rem_euclid(360)) / 15).min(24) as usize;
    C[i]
}
fn fp_sin(angle_deg: i32) -> Fp {
    fp_cos(angle_deg - 90)
}

fn rotate_y(v: V3, deg: i32) -> V3 {
    let c = fp_cos(deg);
    let s = fp_sin(deg);
    V3 {
        x: (v.x * c / FP - v.z * s / FP),
        y: v.y,
        z: (v.x * s / FP + v.z * c / FP),
    }
}
fn rotate_x(v: V3, deg: i32) -> V3 {
    let c = fp_cos(deg);
    let s = fp_sin(deg);
    V3 {
        x: v.x,
        y: (v.y * c / FP - v.z * s / FP),
        z: (v.y * s / FP + v.z * c / FP),
    }
}

fn project(v: V3, fov: Fp, cx: i32, cy: i32) -> (i32, i32) {
    let z = v.z + 3000 * FP / 1000; // camera at z=3
    if z <= 0 {
        return (-9999, -9999);
    }
    let sx = cx + (v.x * fov / z) as i32;
    let sy = cy - (v.y * fov / z) as i32;
    (sx, sy)
}

// ── App cards ─────────────────────────────────────────────────────────────────

struct AppCard {
    label: &'static [u8],
    color: u32,
    path: &'static [u8],
    pos: V3,         // position on hemisphere
    hover_scale: Fp, // spring toward FP when hovered, FP/2 otherwise
    spring_vel: Fp,
}

const APPS: &[(&[u8], u32, &[u8])] = &[
    (b"Mission", 0xFF4A90D9, b"/apps/settings"),
    (b"Signal", 0xFF58A6FF, b"/apps/ai-console"),
    (b"Dependency", 0xFFFFA75A, b"/apps/editor"),
    (b"Asset", 0xFF7B68EE, b"/apps/files"),
    (b"Terminal", 0xFF28C940, b"/apps/terminal"),
    (b"Store", 0xFFFF8C00, b"/tools/appstore"),
    (b"Copilot", 0xFF5BA9FF, b"/apps/ai-console"),
    (b"Graph Viz", 0xFF9A84FF, b"/apps/browser-lite"),
];

const DOCK_APPS: &[(&[u8], u32, &[u8])] = &[
    (b"Terminal", 0xFF4A90D9, b"/apps/terminal"),
    (b"Files", 0xFF28C940, b"/apps/files"),
    (b"Browser", 0xFF7B68EE, b"/apps/browser-lite"),
    (b"Settings", 0xFF888888, b"/apps/settings"),
];

fn hemisphere_pos(i: usize, n: usize) -> V3 {
    let cols = 4usize;
    let row = i / cols;
    let col = i % cols;
    let cols_in_row = cols.min(n - row * cols);
    let ax = if cols_in_row > 1 {
        (col as i32 - (cols_in_row as i32 - 1) / 2) * 55
    } else {
        0
    };
    let ay = row as i32 * -40 + 20;
    V3::new(ax as f32 / 100.0, ay as f32 / 100.0, 0.0)
}

// ── Scene ─────────────────────────────────────────────────────────────────────

struct Scene {
    cards: Vec<AppCard>,
    orbit_x: i32,
    orbit_y: i32,
    drag: bool,
    drag_ox: i32,
    drag_oy: i32,
    drag_mx: i32,
    drag_my: i32,
    hover: Option<usize>,
    expose: bool,
    // Last known cursor position in window coordinates.
    cursor_x: i32,
    cursor_y: i32,
    // Per-frame projected cards, updated once before rendering.
    projected: Vec<(usize, i32, i32, Fp)>,
    // Pointer state for robust click/drag handling.
    mouse_down: bool,
    press_x: i32,
    press_y: i32,
}

impl Scene {
    fn new() -> Self {
        let cards = APPS
            .iter()
            .enumerate()
            .map(|(i, &(label, color, path))| AppCard {
                label,
                color,
                path,
                pos: hemisphere_pos(i, APPS.len()),
                hover_scale: FP / 2,
                spring_vel: 0,
            })
            .collect();
        Self {
            cards,
            orbit_x: 0,
            orbit_y: 0,
            drag: false,
            drag_ox: 0,
            drag_oy: 0,
            drag_mx: 0,
            drag_my: 0,
            hover: None,
            expose: false,
            cursor_x: (WIN_W / 2) as i32,
            cursor_y: (TITLEBAR_H + SCENE_H / 2) as i32,
            projected: Vec::new(),
            mouse_down: false,
            press_x: 0,
            press_y: 0,
        }
    }

    /// Reproject all cards and update hover from current cursor. Call once per frame.
    fn update_projected(&mut self) {
        self.projected = self.project_cards();
        let xi = self.cursor_x;
        let yi = self.cursor_y;
        self.hover = None;
        for &(i, sx, sy, scale) in &self.projected {
            let hw = (120 * scale / FP) as i32;
            let hh = (80 * scale / FP) as i32;
            if xi >= sx - hw / 2 && xi <= sx + hw / 2 && yi >= sy - hh / 2 && yi <= sy + hh / 2 {
                self.hover = Some(i);
            }
        }
    }

    fn update_springs(&mut self) {
        for (i, card) in self.cards.iter_mut().enumerate() {
            let target = if self.hover == Some(i) {
                FP
            } else {
                FP * 7 / 10
            };
            let diff = target - card.hover_scale;
            card.spring_vel = card.spring_vel * 8 / 10 + diff * 2 / 10;
            card.hover_scale += card.spring_vel;
            card.hover_scale = card.hover_scale.clamp(FP / 2, FP * 12 / 10);
        }
    }

    fn project_cards(&self) -> Vec<(usize, i32, i32, Fp)> {
        let cx = WIN_W as i32 / 2;
        let cy = TITLEBAR_H as i32 + SCENE_H as i32 / 2;
        let fov = 600 * FP / 1000;
        let mut out = Vec::new();
        for (i, card) in self.cards.iter().enumerate() {
            let mut v = card.pos;
            v = rotate_y(v, self.orbit_y);
            v = rotate_x(v, self.orbit_x);
            let (sx, sy) = project(v, fov, cx, cy);
            out.push((i, sx, sy, card.hover_scale));
        }
        // Z-sort (painter's): sort by z after orbit
        out.sort_by(|a, b| {
            let za = {
                let mut v = self.cards[a.0].pos;
                v = rotate_y(v, self.orbit_y);
                v = rotate_x(v, self.orbit_x);
                v.z
            };
            let zb = {
                let mut v = self.cards[b.0].pos;
                v = rotate_y(v, self.orbit_y);
                v = rotate_x(v, self.orbit_x);
                v.z
            };
            za.cmp(&zb)
        });
        out
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render(canvas: &mut Canvas<'_>, scene: &Scene, theme: Theme) {
    let p = tokens(theme);
    canvas.fill_rect(0, 0, WIN_W, WIN_H, 0xFF060A16);

    // Space-like backdrop with deterministic stars.
    for y in (TITLEBAR_H as usize..(TITLEBAR_H + SCENE_H) as usize).step_by(2) {
        let t = ((y as u32 - TITLEBAR_H) * 255 / SCENE_H).min(255);
        let row = blend(0xFF070C1B, 0xFF101A31, t / 2);
        canvas.fill_rect(0, y as i32, WIN_W, 2, row);
    }
    for y in (TITLEBAR_H..(TITLEBAR_H + SCENE_H)).step_by(9) {
        for x in (0..WIN_W).step_by(11) {
            let h = star_hash(x, y);
            if h & 0x1F == 0 {
                let c = if h & 0x200 == 0 {
                    0x55A9D8FF
                } else {
                    0x55FFB67A
                };
                canvas.fill_rect(x as i32, y as i32, 2, 2, c);
            }
        }
    }

    // Top status bar.
    canvas.fill_rect(0, 0, WIN_W, TITLEBAR_H, blend(p.chrome, 0xFF000000, 90));
    canvas.draw_hline(0, 0, WIN_W, 0xFF4D78BA);
    canvas.draw_hline(0, TITLEBAR_H as i32 - 1, WIN_W, p.border);
    canvas.draw_text(12, 11, b"GraphOS // NODE: local.host", p.text, 260);
    canvas.draw_text(
        (WIN_W / 2 - 150) as i32,
        11,
        b"TRUST: VERIFIED // MODE: FUSION",
        0xFFFFC47A,
        340,
    );
    canvas.draw_text((WIN_W - 122) as i32, 11, b"14:24:22 UTC", p.text_muted, 116);

    // Graph core rendering.
    let projected = scene.projected.as_slice();
    let cx = (WIN_W / 2) as i32;
    let cy = (TITLEBAR_H + SCENE_H / 2) as i32 - 24;
    canvas.fill_rect(cx - 60, cy - 60, 120, 120, 0xCC1A233F);
    canvas.draw_rect(cx - 60, cy - 60, 120, 120, 0xFFFFAD66);
    canvas.draw_text(cx - 34, cy - 5, b"fusion.core", 0xFFFFE8CF, 90);

    for (i, sx, sy, _) in projected {
        let card = &scene.cards[*i];
        if *sx < 6
            || *sx > WIN_W as i32 - 6
            || *sy < TITLEBAR_H as i32 + 6
            || *sy > (TITLEBAR_H + SCENE_H) as i32 - 6
        {
            continue;
        }
        canvas.draw_line(cx, cy, *sx, *sy, blend(card.color, 0xFFFFFFFF, 80));
    }

    for (i, sx, sy, scale) in projected {
        let card = &scene.cards[*i];
        let r = (10 * scale / FP).max(7) as u32;
        let body = if scene.hover == Some(*i) {
            blend(card.color, 0xFFFFFFFF, 85)
        } else {
            darken(card.color)
        };
        canvas.fill_rect(*sx - r as i32, *sy - r as i32, r * 2, r * 2, body);
        canvas.draw_rect(
            *sx - r as i32,
            *sy - r as i32,
            r * 2,
            r * 2,
            blend(card.color, 0xFFFFFFFF, 100),
        );
        canvas.draw_text(*sx + r as i32 + 4, *sy - 4, card.label, p.text, 120);
    }

    // Left inspector and right telemetry panels.
    draw_panel(
        canvas,
        22,
        TITLEBAR_H as i32 + 28,
        260,
        330,
        0xFF2F4D80,
        0xB20A1328,
    );
    canvas.draw_text(36, TITLEBAR_H as i32 + 40, b"Inspector", p.text, 120);
    canvas.draw_text(36, TITLEBAR_H as i32 + 86, b"fusion.core", 0xFFEAF1FF, 180);
    canvas.draw_text(
        36,
        TITLEBAR_H as i32 + 124,
        b"Status: Active",
        0xFFD7E7FF,
        180,
    );
    canvas.draw_text(
        36,
        TITLEBAR_H as i32 + 149,
        b"Centrality: 0.82",
        0xFFD7E7FF,
        180,
    );
    canvas.draw_text(
        36,
        TITLEBAR_H as i32 + 180,
        b"Dependencies:",
        p.text_muted,
        160,
    );
    canvas.draw_text(
        36,
        TITLEBAR_H as i32 + 205,
        b"- sensor.data: power.rail",
        p.text_muted,
        220,
    );
    canvas.fill_rect(36, TITLEBAR_H as i32 + 248, 84, 28, 0xFF142949);
    canvas.draw_rect(36, TITLEBAR_H as i32 + 248, 84, 28, 0xFF3F71BA);
    canvas.draw_text(58, TITLEBAR_H as i32 + 257, b"TRACE", p.text, 60);
    canvas.fill_rect(132, TITLEBAR_H as i32 + 248, 84, 28, 0xFF142949);
    canvas.draw_rect(132, TITLEBAR_H as i32 + 248, 84, 28, 0xFF3F71BA);
    canvas.draw_text(153, TITLEBAR_H as i32 + 257, b"REPLAY", p.text, 60);

    let panel_x = WIN_W as i32 - 360;
    draw_panel(
        canvas,
        panel_x,
        TITLEBAR_H as i32 + 40,
        330,
        188,
        0xFF2F4D80,
        0xB20A1328,
    );
    canvas.draw_text(
        panel_x + 14,
        TITLEBAR_H as i32 + 52,
        b"Power System Dashboard",
        p.text,
        220,
    );
    canvas.fill_rect(panel_x + 14, TITLEBAR_H as i32 + 82, 300, 1, 0xFF2A3E63);
    canvas.fill_rect(panel_x + 14, TITLEBAR_H as i32 + 106, 286, 2, 0xFF65C5FF);
    canvas.fill_rect(panel_x + 40, TITLEBAR_H as i32 + 118, 240, 2, 0xFFFFA96C);
    canvas.draw_text(
        panel_x + 14,
        TITLEBAR_H as i32 + 154,
        b"Alerts: 2 active",
        0xFFFFB072,
        140,
    );

    draw_panel(
        canvas,
        panel_x + 28,
        TITLEBAR_H as i32 + 260,
        302,
        194,
        0xFF2F4D80,
        0xB20A1328,
    );
    canvas.draw_text(
        panel_x + 42,
        TITLEBAR_H as i32 + 272,
        b"Log Viewer",
        p.text,
        120,
    );
    canvas.draw_text(
        panel_x + 42,
        TITLEBAR_H as i32 + 310,
        b"[14:24:16] power.rail fluctuation +2%",
        p.text_muted,
        270,
    );
    canvas.draw_text(
        panel_x + 42,
        TITLEBAR_H as i32 + 336,
        b"[14:24:18] fusion.core recalculation",
        p.text_muted,
        270,
    );
    canvas.draw_text(
        panel_x + 42,
        TITLEBAR_H as i32 + 362,
        b"[14:24:18] fusion.core resiliency ok",
        p.text_muted,
        270,
    );

    // Expose mode overlay.
    if scene.expose {
        canvas.fill_rect(0, TITLEBAR_H as i32, WIN_W, SCENE_H, 0xA0000000);
        canvas.draw_text(
            (WIN_W / 2 - 180) as i32,
            (TITLEBAR_H + SCENE_H / 2) as i32,
            b"EXPOSE OVERVIEW ACTIVE (Tab to exit)",
            p.text,
            360,
        );
    }

    // Timeline strip.
    let ty = (WIN_H - DOCK_H - TASKBAR_H) as i32;
    canvas.fill_rect(0, ty, WIN_W, TASKBAR_H, 0xFF0A1224);
    canvas.draw_hline(0, ty, WIN_W, 0xFF294A7C);
    canvas.draw_text(20, ty + 8, b"Event Timeline", p.text, 160);
    canvas.draw_hline(170, ty + 26, WIN_W - 390, 0xFF5D86C6);
    for i in 0..18 {
        let px = 220 + i * 52;
        let c = if i % 5 == 0 { 0xFFFFAF75 } else { 0xFF8BD8FF };
        canvas.fill_rect(px, ty + 22, 6, 6, c);
    }

    // Command strip + app launcher dock.
    let dy = (WIN_H - DOCK_H) as i32;
    canvas.fill_rect(0, dy, WIN_W, DOCK_H, 0xFF081020);
    canvas.draw_hline(0, dy, WIN_W, 0xFF294A7C);
    canvas.fill_rect(20, dy + 14, WIN_W - 420, 48, 0xFF0B162C);
    canvas.draw_rect(20, dy + 14, WIN_W - 420, 48, 0xFF335C99);
    canvas.draw_text(
        40,
        dy + 31,
        b"> trace power.rail    > replay -300ms",
        p.text,
        WIN_W - 460,
    );

    let dock_icon_w = 56;
    let total_w = DOCK_APPS.len() as u32 * (dock_icon_w + 10);
    let dock_x = (WIN_W - total_w) as i32 - 24;
    for (i, &(label, col, _)) in DOCK_APPS.iter().enumerate() {
        let ix = dock_x + i as i32 * (dock_icon_w as i32 + 10);
        let iy = dy + 16;
        canvas.fill_rect(ix, iy, dock_icon_w, dock_icon_w - 6, darken(col));
        canvas.draw_rect(
            ix,
            iy,
            dock_icon_w,
            dock_icon_w - 6,
            blend(col, 0xFFFFFFFF, 90),
        );
        let lx = ix + dock_icon_w as i32 / 2 - label.len() as i32 * 3;
        canvas.draw_text(
            lx,
            iy + dock_icon_w as i32 - 3,
            label,
            p.text_muted,
            dock_icon_w,
        );
    }
}

fn blend(a: u32, b: u32, t: u32) -> u32 {
    let t = t.min(255);
    let ar = (a >> 16) & 0xFF;
    let ag = (a >> 8) & 0xFF;
    let ab = a & 0xFF;
    let br = (b >> 16) & 0xFF;
    let bg = (b >> 8) & 0xFF;
    let bb = b & 0xFF;
    let rr = (ar * (255 - t) + br * t) / 255;
    let rg = (ag * (255 - t) + bg * t) / 255;
    let rb = (ab * (255 - t) + bb * t) / 255;
    0xFF000000 | (rr << 16) | (rg << 8) | rb
}
fn darken(c: u32) -> u32 {
    blend(c, 0xFF000000, 120)
}

fn draw_panel(canvas: &mut Canvas<'_>, x: i32, y: i32, w: u32, h: u32, border: u32, fill: u32) {
    canvas.fill_rect(x, y, w, h, blend(fill, 0xFF030711, 44));

    // Pseudo-glass gradient and soft scanline texture to avoid flat panel blocks.
    for row in 0..h {
        let t = ((row * 255) / h.max(1)) as u32;
        let grad = blend(0xFF2A4A78, 0xFF0A162A, t);
        let tint = if row % 3 == 0 {
            blend(grad, 0xFFFFFFFF, 18)
        } else {
            blend(grad, 0xFF000000, 22)
        };
        canvas.draw_hline(
            x + 1,
            y + row as i32,
            w.saturating_sub(2),
            blend(tint, fill, 96),
        );
    }

    // Inner highlights and outer frame.
    canvas.draw_hline(
        x + 1,
        y + 1,
        w.saturating_sub(2),
        blend(border, 0xFFFFFFFF, 120),
    );
    canvas.draw_hline(
        x + 1,
        y + 2,
        w.saturating_sub(2),
        blend(border, 0xFFFFFFFF, 70),
    );
    canvas.draw_hline(
        x + 1,
        y + h as i32 - 2,
        w.saturating_sub(2),
        blend(border, 0xFF000000, 90),
    );
    canvas.draw_rect(x, y, w, h, border);
    canvas.draw_hline(x, y + 30, w, blend(border, 0xFFFFFFFF, 60));
}

/// Software arrow cursor drawn directly into the framebuffer.
/// Shape is a 12×20 left-pointing arrow. `hovering` tints the tip amber.
fn draw_cursor(canvas: &mut Canvas<'_>, cx: i32, cy: i32, hovering: bool) {
    // High-visibility hotspot marker so pointer motion is always obvious.
    let ring = if hovering {
        0xFFFFC874u32
    } else {
        0xFF78D5FFu32
    };
    canvas.draw_rect(cx - 4, cy - 4, 9, 9, ring);
    canvas.fill_rect(cx - 1, cy - 1, 3, 3, 0xFFFFFFFF);

    // Arrow bitmap: each row is a bitmask of the 12 leftmost pixels (bit11=left).
    const ROWS: u32 = 20;
    const ARROW: [u16; 20] = [
        0b1000_0000_0000,
        0b1100_0000_0000,
        0b1110_0000_0000,
        0b1111_0000_0000,
        0b1111_1000_0000,
        0b1111_1100_0000,
        0b1111_1110_0000,
        0b1111_1111_0000,
        0b1111_1111_1000,
        0b1111_1111_1100,
        0b1111_1111_1110,
        0b1111_1111_0000,
        0b1111_0111_0000,
        0b1110_0011_1000,
        0b1100_0011_1000,
        0b1000_0001_1100,
        0b0000_0001_1100,
        0b0000_0000_1110,
        0b0000_0000_1110,
        0b0000_0000_0000,
    ];
    const OUTLINE: [u16; 20] = [
        0b1000_0000_0000,
        0b1100_0000_0000,
        0b1010_0000_0000,
        0b1001_0000_0000,
        0b1000_1000_0000,
        0b1000_0100_0000,
        0b1000_0010_0000,
        0b1000_0001_0000,
        0b1000_0000_1000,
        0b1000_0000_0100,
        0b1111_1111_1110,
        0b1000_0000_0000,
        0b1001_0000_0000,
        0b1010_0010_1000,
        0b1100_0010_1000,
        0b1000_0001_0100,
        0b0000_0001_0100,
        0b0000_0000_1010,
        0b0000_0000_0110,
        0b0000_0000_0000,
    ];
    let fill = if hovering {
        0xFFFFBF60u32
    } else {
        0xFFE8EEF8u32
    };
    let outline = 0xFF1A2640u32;
    for row in 0..ROWS as i32 {
        let fy = cy + row;
        if fy < 0 || fy >= canvas.height() as i32 {
            continue;
        }
        let fill_mask = ARROW[row as usize];
        let out_mask = OUTLINE[row as usize];
        for col in 0..12i32 {
            let bit = 1u16 << (11 - col);
            let fx = cx + col;
            if fx < 0 || fx >= canvas.width() as i32 {
                continue;
            }
            if out_mask & bit != 0 {
                canvas.set_pixel(fx, fy, outline);
            } else if fill_mask & bit != 0 {
                canvas.set_pixel(fx, fy, fill);
            }
        }
    }
}

fn star_hash(x: u32, y: u32) -> u32 {
    let mut v = x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ 0xA53A_9B4D;
    v ^= v >> 13;
    v = v.wrapping_mul(1274126177);
    v ^ (v >> 16)
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, channel) {
        Some(w) => w,
        None => return,
    };
    win.request_focus();
    let mut scene = Scene::new();
    let mut gl_scene = GlScene::new(WIN_W, WIN_H);
    let theme = Theme::DarkGlass;
    let dock_icon_w = 56i32;
    let total_w = DOCK_APPS.len() as i32 * (dock_icon_w + 8);
    let dock_x = (WIN_W as i32 - total_w) / 2;
    let dy = (WIN_H - DOCK_H) as i32;

    loop {
        // Drain all pending input events.
        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,
                Event::FrameTick { .. } => {
                    // Frame tick received — fall through to render below.
                    break;
                }
                Event::Key {
                    pressed: true,
                    ascii,
                    ..
                } => match ascii {
                    b'\t' => {
                        scene.expose = !scene.expose;
                    }
                    _ => {}
                },
                Event::PointerMove { x, y, buttons } => {
                    let xi = x as i32;
                    let yi = y as i32;
                    scene.cursor_x = xi;
                    scene.cursor_y = yi;

                    let left_down = (buttons & 1) != 0;
                    if left_down && !scene.mouse_down {
                        scene.mouse_down = true;
                        scene.drag = true;
                        scene.press_x = xi;
                        scene.press_y = yi;
                        scene.drag_ox = scene.orbit_x;
                        scene.drag_oy = scene.orbit_y;
                        scene.drag_mx = xi;
                        scene.drag_my = yi;
                    }

                    if left_down && scene.drag {
                        scene.orbit_y = scene.drag_oy + (xi - scene.drag_mx) / 3;
                        scene.orbit_x = scene.drag_ox + (yi - scene.drag_my) / 3;
                        scene.orbit_x = scene.orbit_x.clamp(-40, 40);
                    }

                    if !left_down && scene.mouse_down {
                        let moved = (xi - scene.press_x).abs() + (yi - scene.press_y).abs();
                        // Treat tiny movement as click, larger movement as drag release.
                        if moved <= 8 {
                            if let Some(i) = scene.hover {
                                let path = scene.cards[i].path;
                                unsafe {
                                    graphos_app_sdk::sys::spawn(path);
                                }
                            }
                            if yi >= dy {
                                let di = (xi - dock_x) / (dock_icon_w + 8);
                                if di >= 0 && (di as usize) < DOCK_APPS.len() {
                                    unsafe {
                                        graphos_app_sdk::sys::spawn(DOCK_APPS[di as usize].2);
                                    }
                                }
                            }
                        }
                        scene.mouse_down = false;
                        scene.drag = false;
                    }
                }
                _ => {}
            }
        }

        scene.update_projected();
        scene.update_springs();
        {
            let mut c = win.canvas();
            render(&mut c, &scene, theme);
            if let Some(gl) = gl_scene.as_mut() {
                gl.render(
                    &mut c,
                    scene.orbit_x,
                    scene.orbit_y,
                    scene.hover,
                    TITLEBAR_H,
                    SCENE_H,
                );
            }
            draw_cursor(
                &mut c,
                scene.cursor_x,
                scene.cursor_y,
                scene.hover.is_some(),
            );
        }
        win.present();
        unsafe {
            graphos_app_sdk::sys::yield_task();
        }
    }
}
