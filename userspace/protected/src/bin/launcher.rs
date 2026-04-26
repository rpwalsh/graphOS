// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! launcher — GraphOS Shell3D spatial desktop shell.
//!
//! 3D hemisphere launcher with orbit drag, spring hover, star backdrop,
//! inspector/telemetry panels, timeline strip, and dock. Ported from
//! apps/shell3d to no_std using fixed-size arrays instead of Vec.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

extern crate alloc;

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;
use core::fmt::{self, Write};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::tokens::{tokens, Theme};

const HEAP_SIZE: usize = 16 * 1024 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    offset: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.heap.get() as usize;
        let align = layout.align();
        let size = layout.size();
        loop {
            let cur = self.offset.load(Ordering::Relaxed);
            let aligned = (base + cur + align - 1) & !(align - 1);
            let offset = aligned - base;
            let next = offset + size;
            if next > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .offset
                .compare_exchange(cur, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator; dealloc is intentionally a no-op.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: UnsafeCell::new([0u8; HEAP_SIZE]),
    offset: AtomicUsize::new(0),
};

// ---------------------------------------------------------------------------
// Layout constants (must match GPU resolution 1280×800)
// ---------------------------------------------------------------------------

const WIN_W: u32 = 1280;
const WIN_H: u32 = 800;
const TITLEBAR_H: u32 = 36;
const DOCK_H: u32 = 84;
const TASKBAR_H: u32 = 54;
const SCENE_H: u32 = WIN_H - TITLEBAR_H - DOCK_H - TASKBAR_H;

const THEME: Theme = Theme::DarkGlass;
const FAST_RENDERER: bool = true;

// ── Fixed-point math (×1000) ────────────────────────────────────────────────

type Fp = i32;
const FP: Fp = 1000;

#[derive(Clone, Copy, Default)]
struct V3 { x: Fp, y: Fp, z: Fp }

fn v3_add(a: V3, b: V3) -> V3 { V3 { x: a.x + b.x, y: a.y + b.y, z: a.z + b.z } }
fn v3_sub(a: V3, b: V3) -> V3 { V3 { x: a.x - b.x, y: a.y - b.y, z: a.z - b.z } }

fn fp_cos(angle_deg: i32) -> Fp {
    const C: [i32; 25] = [1000,966,866,707,500,259,0,-259,-500,-707,-866,-966,-1000,
                          -966,-866,-707,-500,-259,0,259,500,707,866,966,1000];
    let i = ((angle_deg.rem_euclid(360)) / 15).min(24) as usize;
    C[i]
}
fn fp_sin(angle_deg: i32) -> Fp { fp_cos(angle_deg - 90) }

fn rotate_y(v: V3, deg: i32) -> V3 {
    let c = fp_cos(deg); let s = fp_sin(deg);
    V3 { x: v.x * c / FP - v.z * s / FP, y: v.y, z: v.x * s / FP + v.z * c / FP }
}
fn rotate_x(v: V3, deg: i32) -> V3 {
    let c = fp_cos(deg); let s = fp_sin(deg);
    V3 { x: v.x, y: v.y * c / FP - v.z * s / FP, z: v.y * s / FP + v.z * c / FP }
}
fn project(v: V3, fov: Fp, cx: i32, cy: i32) -> (i32, i32) {
    let z = v.z + 3 * FP;
    if z <= 0 { return (-9999, -9999); }
    (cx + v.x * fov / z, cy - v.y * fov / z)
}

// ── App cards ──────────────────────────────────────────────────────────────

const N_APPS: usize = 8;
const N_DOCK: usize = 4;
const N_LANES: usize = 4;
const N_CORE_SERVICES: usize = 5;

struct AppCard {
    label:       &'static [u8],
    color:       u32,
    spawn:       &'static [u8],  // short name for runtime::spawn_named
    pos:         V3,
    hover_scale: Fp,
    spring_vel:  Fp,
}

// (label, ARGB color, spawn name)
const APP_DATA: [(&[u8], u32, &[u8]); N_APPS] = [
    (b"Graph",      0xFF39C5BB, b"ai-console"),
    (b"Context",    0xFF58A6FF, b"files"),
    (b"Copilot",    0xFFFFA75A, b"ai-console"),
    (b"Studio",     0xFF7B68EE, b"editor"),
    (b"Terminal",   0xFF28C940, b"terminal"),
    (b"Control",    0xFFFF8C00, b"settings"),
    (b"Recovery",   0xFF5BA9FF, b"terminal"),
    (b"Research",   0xFF9A84FF, b"ai-console"),
];

const DOCK_DATA: [(&[u8], u32, &[u8]); N_DOCK] = [
    (b"AI",       0xFF4A90D9, b"ai-console"),
    (b"Files",    0xFF28C940, b"files"),
    (b"Editor",   0xFF7B68EE, b"editor"),
    (b"Settings", 0xFF888888, b"settings"),
];

const LANE_TITLES: [&[u8]; N_LANES] = [
    b"Observe",
    b"Context",
    b"Copilot",
    b"Build",
];

const LANE_CAPTIONS: [&[u8]; N_LANES] = [
    b"Graph-first situational awareness",
    b"Files, context, and provenance surfaces",
    b"SCCE evidence synthesis with Walsh math",
    b"Artifacts, recovery, and execution tools",
];

const LANE_COMMANDS: [&[u8]; N_LANES] = [
    b"> graph status    > trace /graph",
    b"> open files      > focus vault",
    b"> ai walsh        > verify provenance",
    b"> open editor     > build artifact",
];

const LANE_TIMELINE: [[&[u8]; 5]; N_LANES] = [
    [b"observe", b"sample", b"align", b"verify", b"act"],
    [b"vault", b"chunk", b"link", b"trace", b"open"],
    [b"perceive", b"think", b"mouth", b"verify", b"answer"],
    [b"spec", b"edit", b"bundle", b"ship", b"recover"],
];

const LANE_LOGS: [[&[u8]; 5]; N_LANES] = [
    [
        b"[orch] frame-tick routed to shell",
        b"[graph] core nodes aligned",
        b"[registry] core services sampled",
        b"[powerwalk] temporal walk basis warm",
        b"[verify] desktop path trusted",
    ],
    [
        b"[vault] /graph marked preferred root",
        b"[files] namespaces exposed",
        b"[editor] scratch bound to orchestrator",
        b"[provenance] paths retain source context",
        b"[open] graph-first workspaces ready",
    ],
    [
        b"[scce] retrieval-not-generation active",
        b"[concept] in-memory graph warmed",
        b"[mouth] evidence synthesis armed",
        b"[prove] provenance verification waiting",
        b"[walsh] PowerWalk deck selected",
    ],
    [
        b"[spec] action rail pinned",
        b"[editor] studio path available",
        b"[artifact] bundle lane online",
        b"[recover] terminal fallback standing by",
        b"[ship] awesome over legacy",
    ],
];

const CORE_SERVICES: [&[u8]; N_CORE_SERVICES] = [
    b"graphd",
    b"servicemgr",
    b"sysd",
    b"compositor",
    b"ai-console",
];

fn hemisphere_pos(i: usize) -> V3 {
    let cols = 4usize;
    let row = i / cols;
    let col = i % cols;
    let remaining = N_APPS - row * cols;
    let cols_in_row = if remaining < cols { remaining } else { cols };
    let ax = if cols_in_row > 1 {
        (col as i32 - (cols_in_row as i32 - 1) / 2) * 55
    } else {
        0
    };
    let ay = row as i32 * -40 + 20;
    // integer conversion: FP * ax / 100
    V3 { x: ax * FP / 100, y: ay * FP / 100, z: 0 }
}

// ── Scene ──────────────────────────────────────────────────────────────────

struct Scene {
    cards:    [AppCard; N_APPS],
    orbit_x:  i32,
    orbit_y:  i32,
    drag:     bool,
    drag_ox:  i32,
    drag_oy:  i32,
    drag_mx:  i32,
    drag_my:  i32,
    hover:    Option<usize>,
    expose:   bool,
    tick:     u32,
    active_lane: usize,
    now_ms: u64,
    last_refresh_ms: u64,
    registry_generation: u64,
    graph_transitions: u32,
    graph_epoch: u32,
    services_online: [bool; N_CORE_SERVICES],
}

impl Scene {
    fn new() -> Self {
        let cards: [AppCard; N_APPS] = core::array::from_fn(|i| {
            AppCard {
                label:       APP_DATA[i].0,
                color:       APP_DATA[i].1,
                spawn:       APP_DATA[i].2,
                pos:         hemisphere_pos(i),
                hover_scale: FP * 7 / 10,
                spring_vel:  0,
            }
        });
        let mut scene = Self {
            cards,
            orbit_x: 0, orbit_y: 0,
            drag: false, drag_ox: 0, drag_oy: 0, drag_mx: 0, drag_my: 0,
            hover: None, expose: false, tick: 0,
            active_lane: 0,
            now_ms: 0,
            last_refresh_ms: 0,
            registry_generation: 0,
            graph_transitions: 0,
            graph_epoch: 0,
            services_online: [false; N_CORE_SERVICES],
        };
        scene.refresh_runtime();
        scene
    }

    fn update_springs(&mut self) {
        for (i, card) in self.cards.iter_mut().enumerate() {
            let target = if self.hover == Some(i) { FP } else { FP * 7 / 10 };
            let diff = target - card.hover_scale;
            card.spring_vel = card.spring_vel * 8 / 10 + diff * 2 / 10;
            card.hover_scale = (card.hover_scale + card.spring_vel).clamp(FP / 2, FP * 12 / 10);
        }
    }

    // Returns sorted [(card_index, sx, sy, scale); N_APPS] + count (always N_APPS)
    fn project_cards(&self) -> [(usize, i32, i32, Fp); N_APPS] {
        let cx = WIN_W as i32 / 2;
        let cy = TITLEBAR_H as i32 + SCENE_H as i32 / 2;
        let fov = 600 * FP / 1000;

        let mut out: [(usize, i32, i32, Fp); N_APPS] = [(0, 0, 0, 0); N_APPS];
        for i in 0..N_APPS {
            let v = rotate_x(rotate_y(self.cards[i].pos, self.orbit_y), self.orbit_x);
            let (sx, sy) = project(v, fov, cx, cy);
            out[i] = (i, sx, sy, self.cards[i].hover_scale);
        }

        // Insertion sort by Z depth (painter's algorithm, back-to-front).
        for k in 1..N_APPS {
            let (idx, _sx, _sy, _sc) = out[k];
            let za = rotate_x(rotate_y(self.cards[idx].pos, self.orbit_y), self.orbit_x).z;
            let mut j = k;
            while j > 0 {
                let (prev_idx, _, _, _) = out[j - 1];
                let zb = rotate_x(rotate_y(self.cards[prev_idx].pos, self.orbit_y), self.orbit_x).z;
                if za >= zb { break; }
                out.swap(j, j - 1);
                j -= 1;
            }
        }
        out
    }

    fn online_core(&self) -> u32 {
        let mut count = 0u32;
        let mut idx = 0usize;
        while idx < self.services_online.len() {
            if self.services_online[idx] {
                count += 1;
            }
            idx += 1;
        }
        count
    }

    fn focus_index(&self) -> usize {
        self.hover.unwrap_or(self.active_lane.min(self.cards.len().saturating_sub(1)))
    }

    fn refresh_runtime(&mut self) {
        let generation = runtime::registry_subscribe(self.registry_generation);
        if generation != 0 && generation != u64::MAX {
            self.registry_generation = generation;
        }

        let mut idx = 0usize;
        while idx < CORE_SERVICES.len() {
            self.services_online[idx] = runtime::registry_lookup(CORE_SERVICES[idx]).is_some();
            idx += 1;
        }

        if let Some((transitions, epoch)) = runtime::graph_em_stats(1, 1) {
            self.graph_transitions = transitions;
            self.graph_epoch = epoch;
        }
    }
}

// ── Color helpers ──────────────────────────────────────────────────────────

fn blend(a: u32, b: u32, t: u32) -> u32 {
    let t = t.min(255);
    let ar = (a >> 16) & 0xFF; let ag = (a >> 8) & 0xFF; let ab = a & 0xFF;
    let br = (b >> 16) & 0xFF; let bg = (b >> 8) & 0xFF; let bb = b & 0xFF;
    0xFF000000
        | (((ar * (255 - t) + br * t) / 255) << 16)
        | (((ag * (255 - t) + bg * t) / 255) << 8)
        |  ((ab * (255 - t) + bb * t) / 255)
}
fn darken(c: u32) -> u32 { blend(c, 0xFF000000, 120) }

/// Pack an opaque RGB color and an alpha byte into the BGRA32 layout the
/// rasterizer expects (0xAARRGGBB).
fn argb(rgb: u32, alpha: u32) -> u32 {
    ((alpha & 0xFF) << 24) | (rgb & 0x00FF_FFFF)
}

fn star_hash(x: u32, y: u32) -> u32 {
    let mut v = x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663) ^ 0xA53A_9B4D;
    v ^= v >> 13;
    v = v.wrapping_mul(1274126177);
    v ^ (v >> 16)
}

// ── 3D primitives ──────────────────────────────────────────────────────────
// The card hemisphere is rasterised as real triangle quads through the
// canvas's barycentric `fill_triangle_blend`. Each card is two triangles in
// world-space, transformed and projected per frame.

#[derive(Clone, Copy)]
struct Quad3 {
    /// Four corners in world space (TL, TR, BR, BL).
    p: [V3; 4],
}

fn world_quad(center: V3, half_w: Fp, half_h: Fp) -> Quad3 {
    Quad3 {
        p: [
            V3 { x: center.x - half_w, y: center.y + half_h, z: center.z },
            V3 { x: center.x + half_w, y: center.y + half_h, z: center.z },
            V3 { x: center.x + half_w, y: center.y - half_h, z: center.z },
            V3 { x: center.x - half_w, y: center.y - half_h, z: center.z },
        ],
    }
}

fn project_quad(q: Quad3, orbit_x: i32, orbit_y: i32, fov: Fp, cx: i32, cy: i32)
    -> ([(i32, i32); 4], Fp)
{
    let mut s = [(0i32, 0i32); 4];
    let mut zsum: Fp = 0;
    for (i, v) in q.p.iter().enumerate() {
        let r = rotate_x(rotate_y(*v, orbit_y), orbit_x);
        zsum += r.z;
        s[i] = project(r, fov, cx, cy);
    }
    (s, zsum / 4)
}

/// Rasterise a glassy 3D quad as two alpha-blended triangles with a
/// per-vertex Gouraud highlight. Edge lines are drawn afterwards for
/// frosted-rim definition.
fn draw_glass_quad(canvas: &mut Canvas<'_>, s: &[(i32, i32); 4], tint: u32, alpha: u32) {
    let base = argb(tint & 0x00FF_FFFF, alpha);
    let bright = argb(blend(tint, 0xFFFFFFFF, 160) & 0x00FF_FFFF, (alpha + 80).min(255));
    let dark = argb(blend(tint, 0xFF000000, 120) & 0x00FF_FFFF, alpha);
    // Two triangles forming the quad TL-TR-BR / TL-BR-BL.
    canvas.fill_triangle_blend(
        s[0].0, s[0].1, bright,
        s[1].0, s[1].1, base,
        s[2].0, s[2].1, dark,
    );
    canvas.fill_triangle_blend(
        s[0].0, s[0].1, bright,
        s[2].0, s[2].1, dark,
        s[3].0, s[3].1, base,
    );
}

fn draw_quad_edge(canvas: &mut Canvas<'_>, s: &[(i32, i32); 4], color: u32) {
    canvas.draw_line(s[0].0, s[0].1, s[1].0, s[1].1, color);
    canvas.draw_line(s[1].0, s[1].1, s[2].0, s[2].1, color);
    canvas.draw_line(s[2].0, s[2].1, s[3].0, s[3].1, color);
    canvas.draw_line(s[3].0, s[3].1, s[0].0, s[0].1, color);
}

// ── Glass surface helpers ──────────────────────────────────────────────────

fn draw_panel(canvas: &mut Canvas<'_>, x: i32, y: i32, w: u32, h: u32, border: u32, fill: u32) {
    canvas.fill_round_rect(x, y, w, h, 8, fill);
    // simulated 1-px stroke via 4 thin rects
    canvas.fill_rect(x, y, w, 1, border);
    canvas.fill_rect(x, y + h as i32 - 1, w, 1, border);
    canvas.fill_rect(x, y, 1, h, border);
    canvas.fill_rect(x + w as i32 - 1, y, 1, h, border);
    canvas.draw_hline(x, y + 30, w, blend(border, 0xFFFFFFFF, 60));
}

/// Real frosted-glass panel: blurs the area beneath, lays a translucent
/// tint over it, paints a top highlight gradient, and strokes the border.
fn draw_glass_panel(canvas: &mut Canvas<'_>, x: i32, y: i32, w: u32, h: u32, tint: u32) {
    // 1. Frost the underlying scene so panel content reads through softly.
    canvas.box_blur(x, y, w, h, 2);
    // 2. Translucent tinted body.
    let body = argb(blend(0xFF0A1426, tint, 35) & 0x00FF_FFFF, 170);
    canvas.fill_round_rect_blend(x, y, w, h, 14, body);
    // 3. Top highlight ribbon (specular-style).
    let hi = argb(blend(0xFFFFFFFF, tint, 60) & 0x00FF_FFFF, 60);
    canvas.fill_round_rect_blend(x + 2, y + 2, w.saturating_sub(4), 22, 12, hi);
    // 4. Inner shadow at the bottom.
    let sh = argb(0x000000, 90);
    canvas.fill_rect_blend(x + 2, y + h as i32 - 6, w.saturating_sub(4), 4, sh);
    // 5. Frosted rim — two strokes for depth.
    let rim_outer = argb(blend(tint, 0xFFFFFFFF, 90) & 0x00FF_FFFF, 200);
    let rim_inner = argb(0xFFFFFF, 50);
    canvas.fill_rect_blend(x, y, w, 1, rim_outer);
    canvas.fill_rect_blend(x, y + h as i32 - 1, w, 1, rim_outer);
    canvas.fill_rect_blend(x, y, 1, h, rim_outer);
    canvas.fill_rect_blend(x + w as i32 - 1, y, 1, h, rim_outer);
    canvas.fill_rect_blend(x + 1, y + 1, w.saturating_sub(2), 1, rim_inner);
}

// ── Render ─────────────────────────────────────────────────────────────────
//
// Style brief (matches docs/ui-design-ref):
//   * Deep cosmic backdrop with soft nebula glow + parallax starfield.
//   * Central 3D graph network: glowing core node, orbiting application
//     nodes connected by gradient edges with travelling pulse particles.
//   * Frosted-glass HUD panels at the corners with real box-blur backing.
//   * Glowing dock + scrubbable event timeline strip.
//
// All composition uses the alpha-blended primitives in `Canvas` (rounded
// rects, AA lines, glow halos, Gouraud triangles), so the launcher pipeline
// behaves like a tiny software OpenGL: vertex transform → triangle/disc
// rasteriser → blending compositor.
//
// The central scene region is now driven by the real `graphos-gl` software
// rasteriser (see `mod gl3d` below): vertex shader → near-plane clip →
// perspective divide → viewport → edge-function rasterisation with
// perspective-correct varying interpolation → programmable Phong fragment
// shader → depth test → BGRA32 framebuffer write.

#[allow(static_mut_refs)]
mod gl3d {
    //! Real 3D rendering for the launcher's central scene region using the
    //! `graphos-gl` software OpenGL pipeline. Renders the fusion-core sphere
    //! plus one tinted sphere per `AppCard` orbiting on a hemisphere.

    use super::{Scene, N_APPS, SCENE_H, TITLEBAR_H, WIN_W};
    use graphos_app_sdk::canvas::Canvas;
    use graphos_gl::gl::{Context, DrawMode, IndexType};
    use graphos_gl::math::{Mat4, Vec2, Vec3, Vec4};
    use graphos_gl::mesh::{build_sphere, StdVarying, Vertex};
    use graphos_gl::pipeline::Target;
    use graphos_gl::shader::Shader;
    use libm::{cosf, sinf};

    // Sphere tessellation parameters. 14×10 ⇒ 165 verts, 840 indices.
    const SPH_LON: u32 = 14;
    const SPH_LAT: u32 = 10;
    const SPH_VLEN: usize = ((SPH_LON + 1) * (SPH_LAT + 1)) as usize;
    const SPH_ILEN: usize = (SPH_LON * SPH_LAT * 6) as usize;
    const SCENE_PIXELS: usize = (WIN_W * SCENE_H) as usize;

    // Static depth buffer (1280 × 626 ≈ 3.2 MB BSS) — single-threaded,
    // accessed only from the launcher's render loop.
    static mut DEPTH: [f32; SCENE_PIXELS] = [1.0; SCENE_PIXELS];

    // Sphere geometry — built once on first render via `libm` trig.
    static mut SPH_V: [Vertex; SPH_VLEN] = [Vertex {
        pos: Vec3::ZERO,
        normal: Vec3::ZERO,
        uv: Vec2 { x: 0.0, y: 0.0 },
        color: Vec3::ZERO,
    }; SPH_VLEN];
    static mut SPH_I: [u32; SPH_ILEN] = [0u32; SPH_ILEN];
    static mut GL_CTX: Option<Context> = None;
    static mut EBO: u32 = 0;
    static mut MESH_BUILT: bool = false;

    fn ensure_mesh() {
        unsafe {
            if !MESH_BUILT {
                build_sphere(&mut SPH_V, &mut SPH_I, SPH_LON, SPH_LAT, Vec3::ONE);
                let mut ctx = Context::new();
                let mut bufs = [0u32; 1];
                if ctx.gen_buffers(&mut bufs) == 1 {
                    EBO = bufs[0];
                    // Safety: SPH_I is a static u32 index array; this byte view is read-only.
                    let idx_bytes = core::slice::from_raw_parts(
                        SPH_I.as_ptr() as *const u8,
                        SPH_ILEN * core::mem::size_of::<u32>(),
                    );
                    ctx.buffer_data(EBO, idx_bytes);
                    ctx.bind_element_buffer(EBO);
                }
                GL_CTX = Some(ctx);
                MESH_BUILT = true;
            }
        }
    }

    /// Lambert + ambient + emissive shader. One instance per draw call,
    /// carrying the current MVP, model matrix (for normals), tint, light
    /// direction, and lighting biases.
    struct PhongShader {
        mvp: Mat4,
        model: Mat4,
        tint: Vec3,
        light: Vec3,
        ambient: f32,
        emissive: f32,
    }

    impl Shader for PhongShader {
        type Vertex = Vertex;
        type Varying = StdVarying;

        fn vertex(&self, v: &Vertex) -> (Vec4, StdVarying) {
            let p = Vec4::new(v.pos.x, v.pos.y, v.pos.z, 1.0);
            let n = Vec4::new(v.normal.x, v.normal.y, v.normal.z, 0.0);
            let world = self.model.mul_vec4(p);
            let nw = self.model.mul_vec4(n);
            let clip = self.mvp.mul_vec4(p);
            let vy = StdVarying {
                world_pos: Vec3::new(world.x, world.y, world.z),
                normal: Vec3::new(nw.x, nw.y, nw.z),
                uv: v.uv,
                color: self.tint,
            };
            (clip, vy)
        }

        fn fragment(&self, v: &StdVarying) -> Option<Vec4> {
            let n = v.normal.normalize();
            let d = n.dot(self.light).max(0.0);
            let lit = (self.ambient + (1.0 - self.ambient) * d + self.emissive).min(1.4);
            Some(Vec4::new(
                (v.color.x * lit).min(1.0),
                (v.color.y * lit).min(1.0),
                (v.color.z * lit).min(1.0),
                1.0,
            ))
        }
    }

    fn rgb_to_vec3(c: u32) -> Vec3 {
        Vec3::new(
            ((c >> 16) & 0xFF) as f32 / 255.0,
            ((c >> 8) & 0xFF) as f32 / 255.0,
            (c & 0xFF) as f32 / 255.0,
        )
    }

    /// Render the central scene region into the canvas. Preserves the
    /// cosmic backdrop already painted under it (we only clear depth, not
    /// color); opaque 3D pixels overwrite the backdrop where they land.
    pub fn render_scene(canvas: &mut Canvas<'_>, scene: &Scene) {
        ensure_mesh();

        let row_offset = (TITLEBAR_H * WIN_W) as usize;
        let scene_pixels = (WIN_W * SCENE_H) as usize;

        let pixels_all = canvas.pixels_mut();
        let color = &mut pixels_all[row_offset..row_offset + scene_pixels];
        // Safety: single-threaded ring3 task; DEPTH never aliases.
        let depth: &mut [f32] = unsafe { &mut DEPTH[..] };

        let mut target = Target {
            color,
            extra_colors: alloc::vec::Vec::new(),
            depth,
            stencil: None,
            width: WIN_W,
            height: SCENE_H,
        };
        // Reset depth to far; keep color so backdrop shines through gaps.
        target.depth.fill(1.0);

        // ── Camera (orbit driven by drag state) ─────────────────────────
        let aspect = WIN_W as f32 / SCENE_H as f32;
        let proj = Mat4::perspective(
            60.0_f32 * core::f32::consts::PI / 180.0,
            aspect,
            0.1,
            50.0,
        );
        let oy = (scene.orbit_y as f32) * (core::f32::consts::PI / 180.0);
        let ox = (scene.orbit_x as f32) * (core::f32::consts::PI / 180.0);
        let cam_dist = 6.0;
        let eye = Vec3::new(
            sinf(oy) * cosf(ox) * cam_dist,
            sinf(ox) * cam_dist,
            cosf(oy) * cosf(ox) * cam_dist,
        );
        let view = Mat4::look_at(eye, Vec3::ZERO, Vec3::Y);
        let vp = proj.mul_mat(&view);
        let light = Vec3::new(0.4, 0.7, 0.6).normalize();

        // Safety: single-threaded launcher render loop owns this static context.
        let gl = unsafe { GL_CTX.as_mut() };
        let Some(gl) = gl else { return; };
        gl.enable_depth_test(true);
        gl.depth_mask(true);
        gl.enable_blend(false);
        gl.enable_scissor_test(false);
        gl.bind_element_buffer(unsafe { EBO });

        // Animation phase.
        let t = scene.tick as f32 * 0.01;

        // Borrow the sphere geometry once for all draws.
        // Safety: ensure_mesh ran; geometry is immutable from here on.
        let verts: &[Vertex] = unsafe { &SPH_V[..] };
        let inds: &[u32] = unsafe { &SPH_I[..] };

        // ── Central fusion-core sphere (large, emissive amber) ──────────
        {
            let model = Mat4::rotation_y(t).mul_mat(&Mat4::scale(Vec3::new(0.95, 0.95, 0.95)));
            let mvp = vp.mul_mat(&model);
            let shader = PhongShader {
                mvp,
                model,
                tint: Vec3::new(1.0, 0.84, 0.44),
                light,
                ambient: 0.55,
                emissive: 0.45,
            };
            let _ = gl.draw_elements(
                &mut target,
                &shader,
                verts,
                inds.len(),
                IndexType::U32,
                0,
                DrawMode::Triangles,
            );
        }

        // ── App spheres orbiting on a tilted ring ──────────────────────
        let radius = 2.6;
        for i in 0..N_APPS {
            let a = (i as f32) / (N_APPS as f32) * core::f32::consts::TAU + t * 0.3;
            let lift = if i % 2 == 0 { 0.6 } else { -0.4 };
            let pos = Vec3::new(cosf(a) * radius, lift, sinf(a) * radius);
            let scale = if scene.hover == Some(i) { 0.55 } else { 0.42 };
            let model = Mat4::translation(pos)
                .mul_mat(&Mat4::rotation_y(t * 1.5 + i as f32))
                .mul_mat(&Mat4::scale(Vec3::new(scale, scale, scale)));
            let mvp = vp.mul_mat(&model);
            let tint = rgb_to_vec3(scene.cards[i].color);
            let shader = PhongShader {
                mvp,
                model,
                tint,
                light,
                ambient: 0.25,
                emissive: if scene.hover == Some(i) { 0.35 } else { 0.15 },
            };
            let _ = gl.draw_elements(
                &mut target,
                &shader,
                verts,
                inds.len(),
                IndexType::U32,
                0,
                DrawMode::Triangles,
            );
        }
    }
}

fn render_fast(canvas: &mut Canvas<'_>, scene: &Scene) {
    let p = tokens(THEME);
    let lane = scene.active_lane.min(N_LANES.saturating_sub(1));
    let focus_idx = scene.focus_index();
    let focus_card = &scene.cards[focus_idx];
    let online_core = scene.online_core();

    canvas.fill_rect(0, 0, WIN_W, WIN_H, 0xFF07111B);

    let mut y = 0u32;
    while y < WIN_H {
        let tone = if y < WIN_H / 3 {
            argb(0x102640, 255)
        } else if y < (WIN_H * 2) / 3 {
            argb(0x0C1D32, 255)
        } else {
            argb(0x091521, 255)
        };
        canvas.fill_rect(0, y as i32, WIN_W, 12, tone);
        y += 12;
    }

    canvas.fill_rect_blend(0, 0, WIN_W, TITLEBAR_H, argb(0x0E2038, 232));
    canvas.fill_rect_blend(0, TITLEBAR_H as i32 - 1, WIN_W, 1, argb(0x65C5FF, 220));
    canvas.draw_text(16, 12, b"GraphOS Shell3D // runtime fallback", p.text, 320);
    canvas.draw_text((WIN_W / 2 - 120) as i32, 12, LANE_CAPTIONS[lane], 0xFFFFC47A, 240);
    let mut clock = [0u8; 16];
    canvas.draw_text((WIN_W - 110) as i32, 12, format_clock(scene.now_ms, &mut clock), p.text_muted, 96);

    let left_x = 24;
    let left_y = TITLEBAR_H as i32 + 28;
    canvas.fill_round_rect_blend(left_x, left_y, 300, 214, 12, argb(0x0C182B, 220));
    canvas.fill_rect_blend(left_x, left_y, 300, 2, argb(0x65C5FF, 210));
    canvas.draw_text(left_x + 16, left_y + 16, b"Desktop Status", p.text, 180);
    canvas.draw_text(left_x + 16, left_y + 46, focus_card.label, 0xFFEAF1FF, 180);
    canvas.draw_text(left_x + 16, left_y + 68, LANE_TITLES[lane], 0xFF8BD8FF, 180);
    canvas.draw_text(left_x + 16, left_y + 92, b"3D scene deferred for QEMU stability", 0xFFC0DCFF, 250);
    let mut svc_line = LineWriter::new();
    let _ = write!(&mut svc_line, "Core services online: {}/{}", online_core, N_CORE_SERVICES);
    canvas.draw_text(left_x + 16, left_y + 114, svc_line.bytes(), p.text_muted, 250);
    canvas.draw_text(left_x + 16, left_y + 140, b"Tab: expose   1-4: switch lane   q: quit", 0xFFD7E7FF, 250);
    canvas.draw_text(left_x + 16, left_y + 164, b"/graph-first workspace online", 0xFF8DE0AC, 250);
    glass_button(canvas, left_x + 16, left_y + 176, 120, 28, b"OPEN", 0xFF4D8BD8);
    glass_button(canvas, left_x + 156, left_y + 176, 120, 28, b"VERIFY", 0xFFFFA75A);

    let projected = scene.project_cards();
    for &(i, sx, sy, scale) in &projected {
        if sx == -9999 {
            continue;
        }
        let card = &scene.cards[i];
        let width = ((118 * scale) / FP).clamp(88, 144) as u32;
        let height = ((72 * scale) / FP).clamp(52, 92) as u32;
        let x = sx - width as i32 / 2;
        let y = sy - height as i32 / 2;
        let accent = if scene.hover == Some(i) {
            blend(card.color, 0xFFFFFFFF, 48)
        } else {
            card.color
        };
        let fill = argb(blend(card.color, 0xFF06101B, 150) & 0x00FF_FFFF, 230);
        canvas.fill_round_rect_blend(x, y, width, height, 12, fill);
        canvas.fill_rect_blend(x, y, width, 2, argb(accent & 0x00FF_FFFF, 220));
        canvas.draw_text(x + 12, y + 16, card.label, 0xFFEAF1FF, width.saturating_sub(24));
        canvas.draw_text(x + 12, y + 38, card.spawn, 0xFFB0CFFF, width.saturating_sub(24));
        if i == focus_idx {
            canvas.fill_rect_blend(x, y + height as i32 - 3, width, 3, argb(0xFFFFAF75, 220));
        }
    }

    let lane_y = TITLEBAR_H as i32 + SCENE_H as i32 - 72;
    let lane_w = 170u32;
    for idx in 0..N_LANES {
        let x = 360 + idx as i32 * 188;
        let tint = if idx == lane { 0xFFFFA75A } else { 0xFF4D8BD8 };
        canvas.fill_round_rect_blend(x, lane_y, lane_w, 46, 10, argb(0x0C182B, 215));
        canvas.fill_rect_blend(x, lane_y, lane_w, 2, argb(tint & 0x00FF_FFFF, 220));
        canvas.draw_text(x + 12, lane_y + 12, LANE_TITLES[idx], p.text, 120);
        canvas.draw_text(x + 12, lane_y + 28, LANE_TIMELINE[idx][0], p.text_muted, 120);
    }

    if scene.expose {
        canvas.fill_rect_blend(0, TITLEBAR_H as i32, WIN_W, SCENE_H, argb(0x020A12, 180));
        canvas.draw_text((WIN_W / 2 - 182) as i32, TITLEBAR_H as i32 + 34, b"Deck Overview // 1-4 switch lanes // Tab closes", p.text, 364);
        let card_w = 252u32;
        let card_h = 112u32;
        let start_x = (WIN_W as i32 - (card_w as i32 * 2 + 32)) / 2;
        let start_y = TITLEBAR_H as i32 + 96;
        for idx in 0..N_LANES {
            let col = (idx % 2) as i32;
            let row = (idx / 2) as i32;
            let px = start_x + col * (card_w as i32 + 32);
            let py = start_y + row * (card_h as i32 + 28);
            let tint = if idx == lane { 0xFFFFA75A } else { 0xFF4D8BD8 };
            canvas.fill_round_rect_blend(px, py, card_w, card_h, 12, argb(0x0C182B, 224));
            canvas.fill_rect_blend(px, py, card_w, 2, argb(tint & 0x00FF_FFFF, 220));
            canvas.draw_text(px + 16, py + 18, LANE_TITLES[idx], p.text, 160);
            canvas.draw_text(px + 16, py + 42, LANE_CAPTIONS[idx], 0xFFC0DCFF, 208);
            canvas.draw_text(px + 16, py + 72, LANE_COMMANDS[idx], if idx == lane { 0xFFFFC47A } else { p.text_muted }, 208);
        }
    }

    let ty = (WIN_H - DOCK_H - TASKBAR_H) as i32;
    canvas.fill_rect_blend(0, ty, WIN_W, TASKBAR_H, argb(0x06101F, 232));
    canvas.fill_rect_blend(0, ty, WIN_W, 1, argb(0x65C5FF, 210));
    canvas.draw_text(20, ty + 8, b"Orchestrator Timeline", p.text, 180);
    for i in 0..5usize {
        let px = 220 + i as i32 * 190;
        let c = if i == ((scene.now_ms / 1600) % 5) as usize { 0xFFFFAF75 } else { 0xFF8BD8FF };
        canvas.fill_circle_blend(px, ty + 26, 3, argb(c & 0x00FF_FFFF, 230));
        canvas.draw_text(px - 24, ty + 34, LANE_TIMELINE[lane][i], if c == 0xFFFFAF75 { 0xFFFFC47A } else { p.text_muted }, 88);
    }

    let dy = (WIN_H - DOCK_H) as i32;
    canvas.fill_rect_blend(0, dy, WIN_W, DOCK_H, argb(0x040A18, 236));
    canvas.fill_rect_blend(0, dy, WIN_W, 1, argb(0x65C5FF, 220));
    canvas.fill_round_rect_blend(20, dy + 14, WIN_W - 400, 48, 12, argb(0x0A1834, 224));
    canvas.draw_text(40, dy + 31, LANE_COMMANDS[lane], p.text, WIN_W - 440);

    const DOCK_ICON_W: u32 = 56;
    const DOCK_GAP: u32 = 10;
    let total_dock_w = N_DOCK as u32 * (DOCK_ICON_W + DOCK_GAP) - DOCK_GAP;
    let dock_x = (WIN_W - total_dock_w) as i32 - 24;
    for i in 0..N_DOCK {
        let (label, col, _) = DOCK_DATA[i];
        let ix = dock_x + i as i32 * (DOCK_ICON_W as i32 + DOCK_GAP as i32);
        let iy = dy + 14;
        canvas.fill_round_rect_blend(ix, iy, DOCK_ICON_W, DOCK_ICON_W - 6, 10, argb(blend(col, 0xFF06101B, 130) & 0x00FF_FFFF, 230));
        canvas.fill_rect_blend(ix, iy, DOCK_ICON_W, 2, argb(col & 0x00FF_FFFF, 220));
        let lx = ix + DOCK_ICON_W as i32 / 2 - (Canvas::text_width(label) / 2) as i32;
        canvas.draw_text(lx, iy + DOCK_ICON_W as i32 - 4, label, p.text_muted, DOCK_ICON_W);
    }
}

fn render(canvas: &mut Canvas<'_>, scene: &Scene) {
    if FAST_RENDERER {
        render_fast(canvas, scene);
        return;
    }

    let p = tokens(THEME);
    let lane = scene.active_lane.min(N_LANES.saturating_sub(1));
    let focus_idx = scene.focus_index();
    let focus_card = &scene.cards[focus_idx];
    let online_core = scene.online_core();

    // ── 1. Cosmic backdrop ────────────────────────────────────────────
    canvas.fill_rect(0, 0, WIN_W, WIN_H, 0xFF03060F);

    // Vertical nebula gradient
    let mut y = 0u32;
    while y < WIN_H {
        let t = (y * 255 / WIN_H).min(255);
        let row = blend(0xFF050B1C, 0xFF0F1830, t);
        canvas.fill_rect(0, y as i32, WIN_W, 4, row);
        y += 4;
    }

    // Two large soft nebula halos (cyan + amber), low alpha → moody depth.
    canvas.fill_glow(WIN_W as i32 / 3, WIN_H as i32 / 3,     320, 0x3C7AC8, 130);
    canvas.fill_glow(WIN_W as i32 * 2 / 3, WIN_H as i32 * 2 / 3, 280, 0xFF8A4A, 110);

    // Parallax starfield (two layers, second twinkles).
    let mut sy = 0u32;
    while sy < WIN_H {
        let mut sx: u32 = 0;
        while sx < WIN_W {
            let h = star_hash(sx, sy);
            if h & 0x3F == 0 {
                let bright = if h & 0x200 == 0 { 0xCCD9F2FF } else { 0xCCFFD2A0 };
                canvas.blend_pixel(sx as i32, sy as i32, bright);
            } else if h & 0xFF == 1 {
                canvas.blend_pixel(sx as i32, sy as i32, 0x66B0CFFF);
            }
            sx += 7;
        }
        sy += 5;
    }

    // ── 2. Top status bar (glassy) ────────────────────────────────────
    canvas.fill_rect_blend(0, 0, WIN_W, TITLEBAR_H, argb(0x0A1628, 220));
    canvas.fill_rect_blend(0, TITLEBAR_H as i32 - 1, WIN_W, 1, argb(0x4D78BA, 230));
    // accent bar tick marks
    for i in 0..40u32 {
        let tx = (i * (WIN_W / 40)) as i32;
        canvas.fill_rect_blend(tx, 0, 1, 3, argb(0x4D78BA, 100));
    }
    canvas.draw_text(16, 12, b"GraphOS // /graph // orchestrator-owned desktop", p.text, 420);
    canvas.draw_text((WIN_W / 2 - 178) as i32, 12, LANE_CAPTIONS[lane], 0xFFFFC47A, 356);
    let mut clock = [0u8; 16];
    canvas.draw_text((WIN_W - 110) as i32, 12, format_clock(scene.now_ms, &mut clock), p.text_muted, 96);

    // ── 3. Central 3D graph network (real graphos-gl pipeline) ───────
    // Replaces the old fixed-point fake-3D card hemisphere. The scene
    // region [0..WIN_W) × [TITLEBAR_H..TITLEBAR_H+SCENE_H) is rendered by
    // the software OpenGL rasteriser: perspective projection, depth test,
    // perspective-correct varying interpolation, Phong fragment shader.
    gl3d::render_scene(canvas, scene);

    // Floating label tags (kept as 2D chrome — drawn over the 3D scene).
    let projected = scene.project_cards();
    for &(i, sx, sy_, scale) in &projected {
        if sx == -9999 { continue; }
        let card = &scene.cards[i];
        let r = ((22 * scale) / FP).max(10) as i32;
        let label_w = (Canvas::text_width(card.label) + 14) as i32;
        let lx = sx - label_w / 2;
        let ly = sy_ + r + 8;
        canvas.fill_round_rect_blend(lx, ly, label_w as u32, 18, 9, argb(0x0A1426, 180));
        canvas.fill_rect_blend(lx, ly, label_w as u32, 1, argb(blend(card.color,0xFFFFFFFF,80) & 0x00FF_FFFF, 200));
        canvas.draw_text(lx + 7, ly + 5, card.label, p.text, label_w as u32 - 8);
    }
    let cx = (WIN_W / 2) as i32;
    let cy = TITLEBAR_H as i32 + SCENE_H as i32 / 2;
    let focus_tw = Canvas::text_width(focus_card.label) as i32;
    canvas.draw_text(cx - focus_tw / 2, cy + 92, focus_card.label, 0xFFFFE8CF, 132);
    canvas.draw_text(cx - 84, cy + 108, LANE_TITLES[lane], 0xFF9CC8FF, 168);

    // ── 4. Inspector panel (left, glassy) ─────────────────────────────
    let ip_x = 22;
    let ip_y = TITLEBAR_H as i32 + 28;
    draw_glass_panel(canvas, ip_x, ip_y, 274, 336, 0x4D8BD8);
    canvas.draw_text(ip_x + 16, ip_y + 14, b"Inspector", p.text, 110);
    canvas.fill_rect_blend(ip_x + 16, ip_y + 32, 242, 1, argb(0x4D8BD8, 180));
    canvas.draw_text(ip_x + 16, ip_y + 50, focus_card.label, 0xFFEAF1FF, 236);
    canvas.draw_text(ip_x + 16, ip_y + 72, LANE_TITLES[lane], 0xFF8BD8FF, 236);
    canvas.draw_text(ip_x + 16, ip_y + 96, b"Workspace: /graph", p.text_muted, 236);
    canvas.draw_text(ip_x + 16, ip_y + 116, b"SCCE: provenance-first retrieval", 0xFFD7E7FF, 236);
    canvas.draw_text(ip_x + 16, ip_y + 136, b"Kernel: Walsh PowerWalk temporal basis", 0xFFD7E7FF, 236);
    let mut spawn_line = LineWriter::new();
    let _ = write!(&mut spawn_line, "Launch target: {}", core::str::from_utf8(focus_card.spawn).unwrap_or("app"));
    canvas.draw_text(ip_x + 16, ip_y + 162, spawn_line.bytes(), 0xFFC0DCFF, 236);
    let mut core_line = LineWriter::new();
    let _ = write!(&mut core_line, "Core services online: {}/{}", online_core, N_CORE_SERVICES);
    canvas.draw_text(ip_x + 16, ip_y + 184, core_line.bytes(), 0xFFC0DCFF, 236);
    canvas.draw_text(ip_x + 16, ip_y + 212, b"Core graph", p.text_muted, 220);
    for idx in 0..N_CORE_SERVICES {
        let row_y = ip_y + 232 + idx as i32 * 16;
        let ok = scene.services_online[idx];
        let color = if ok { 0xFF8DE0AC } else { 0xFFFFA75A };
        canvas.fill_circle_blend(ip_x + 20, row_y + 4, 3, argb(color & 0x00FF_FFFF, 255));
        canvas.draw_text(ip_x + 30, row_y, CORE_SERVICES[idx], color, 170);
    }
    glass_button(canvas, ip_x + 16, ip_y + 310, 112, 30, b"VERIFY",  0xFF4D8BD8);
    glass_button(canvas, ip_x + 146, ip_y + 310, 112, 30, b"OPEN", 0xFFFFA75A);

    // ── 5. Telemetry panel (top right) ────────────────────────────────
    let panel_x = WIN_W as i32 - 312;
    draw_glass_panel(canvas, panel_x, TITLEBAR_H as i32 + 40, 290, 176, 0x4D8BD8);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 54, b"Walsh Telemetry", p.text, 260);
    canvas.fill_rect_blend(panel_x + 16, TITLEBAR_H as i32 + 80, 256, 1, argb(0x2A3E63, 200));
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 92, b"PowerWalk decay 0.009..0.28/day", 0xFFB0CFFF, 256);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 110, b"Walk L* 12..94 // warm-start x2", 0xFFB0CFFF, 256);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 128, b"AUC 0.96 // temporal node2vec 0.93", 0xFF8DE0AC, 256);
    let bar_w = 240u32;
    let bar_x = panel_x + 24;
    canvas.fill_rect_blend(bar_x, TITLEBAR_H as i32 + 148, bar_w, 3, argb(0x243E63, 210));
    canvas.fill_rect_blend(bar_x, TITLEBAR_H as i32 + 148, bar_w * online_core / N_CORE_SERVICES as u32, 3, argb(0x65C5FF, 220));
    canvas.fill_rect_blend(bar_x, TITLEBAR_H as i32 + 164, bar_w, 3, argb(0x2F263E, 210));
    let epoch_fill = (scene.graph_epoch % 1000).max(120);
    canvas.fill_rect_blend(bar_x, TITLEBAR_H as i32 + 164, (bar_w as u64 * epoch_fill as u64 / 1000) as u32, 3, argb(0xFFA96C, 220));
    let mut registry_line = LineWriter::new();
    let _ = write!(&mut registry_line, "Registry gen {}", scene.registry_generation);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 178, registry_line.bytes(), 0xFFD7E7FF, 220);
    let mut em_line = LineWriter::new();
    let _ = write!(&mut em_line, "EM transitions {} // epoch {}", scene.graph_transitions, scene.graph_epoch);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 194, em_line.bytes(), 0xFFFFB072, 248);

    // ── 6. Log Viewer panel (bottom right) ────────────────────────────
    draw_glass_panel(canvas, panel_x, TITLEBAR_H as i32 + 220, 290, 196, 0x4D8BD8);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 234, b"Log Viewer", p.text, 220);
    canvas.fill_rect_blend(panel_x + 16, TITLEBAR_H as i32 + 252, 256, 1, argb(0x2A3E63, 200));
    let mut now_line = LineWriter::new();
    let _ = write!(&mut now_line, "[orch] {} frame-tick delivered", scene.now_ms);
    canvas.draw_text(panel_x + 16, TITLEBAR_H as i32 + 268, now_line.bytes(), 0xFF8DE0AC, 270);
    for idx in 0..5 {
        let row_y = TITLEBAR_H as i32 + 290 + idx as i32 * 18;
        let color = if idx == 2 { 0xFF8DE0AC } else { 0xFFB0CFFF };
        canvas.draw_text(panel_x + 16, row_y, LANE_LOGS[lane][idx], color, 270);
    }

    // ── 7. Expose overlay (toggle with Tab) ───────────────────────────
    if scene.expose {
        canvas.fill_rect_blend(0, TITLEBAR_H as i32, WIN_W, SCENE_H, argb(0x000814, 180));
        canvas.draw_text((WIN_W / 2 - 182) as i32, TITLEBAR_H as i32 + 34, b"Deck Overview // 1-4 switch lanes // Tab closes", p.text, 364);
        let card_w = 252u32;
        let card_h = 112u32;
        let start_x = (WIN_W as i32 - (card_w as i32 * 2 + 32)) / 2;
        let start_y = TITLEBAR_H as i32 + 96;
        for idx in 0..N_LANES {
            let col = (idx % 2) as i32;
            let row = (idx / 2) as i32;
            let px = start_x + col * (card_w as i32 + 32);
            let py = start_y + row * (card_h as i32 + 28);
            let tint = if idx == lane { 0xFFFFA75A } else { 0xFF4D8BD8 };
            draw_glass_panel(canvas, px, py, card_w, card_h, tint);
            canvas.draw_text(px + 16, py + 18, LANE_TITLES[idx], p.text, 160);
            canvas.draw_text(px + 16, py + 42, LANE_CAPTIONS[idx], 0xFFC0DCFF, 208);
            canvas.draw_text(px + 16, py + 72, LANE_COMMANDS[idx], if idx == lane { 0xFFFFC47A } else { p.text_muted }, 208);
        }
    }

    // ── 8. Event timeline strip ───────────────────────────────────────
    let ty = (WIN_H - DOCK_H - TASKBAR_H) as i32;
    canvas.fill_rect_blend(0, ty, WIN_W, TASKBAR_H, argb(0x06101F, 230));
    canvas.fill_rect_blend(0, ty, WIN_W, 1, argb(0x4D8BD8, 200));
    canvas.draw_text(20, ty + 8, b"Orchestrator Timeline", p.text, 172);
    let sweep_x = 200 + ((scene.now_ms as u32 / 3) % (WIN_W - 400)) as i32;
    canvas.fill_rect_blend(160, ty + 26, WIN_W - 360, 1, argb(0x294A7C, 200));
    canvas.fill_glow(sweep_x, ty + 26, 18, 0x65C5FF, 200);
    let timeline_cursor = ((scene.now_ms / 1600) % 5) as usize;
    for i in 0..5usize {
        let px = 220 + i as i32 * 190;
        let c = if i == timeline_cursor { 0xFFFFAF75 } else { 0xFF8BD8FF };
        canvas.fill_circle_blend(px, ty + 26, 3, argb(c & 0x00FF_FFFF, 230));
        canvas.draw_text(px - 24, ty + 34, LANE_TIMELINE[lane][i], if i == timeline_cursor { 0xFFFFC47A } else { p.text_muted }, 88);
    }
    let mut registry_strip = LineWriter::new();
    let _ = write!(&mut registry_strip, "registry {} // epoch {}", scene.registry_generation, scene.graph_epoch);
    canvas.draw_text((WIN_W - 244) as i32, ty + 8, registry_strip.bytes(), p.text_muted, 220);

    // ── 9. Dock + command strip ───────────────────────────────────────
    let dy = (WIN_H - DOCK_H) as i32;
    canvas.fill_rect_blend(0, dy, WIN_W, DOCK_H, argb(0x040A18, 235));
    canvas.fill_rect_blend(0, dy, WIN_W, 1, argb(0x4D8BD8, 220));
    // Command line
    let cmd_w = WIN_W - 400;
    canvas.fill_round_rect_blend(20, dy + 14, cmd_w, 48, 12, argb(0x0A1834, 220));
    canvas.fill_rect_blend(20, dy + 14, cmd_w, 1, argb(0x4D8BD8, 180));
    canvas.draw_text(40, dy + 31, LANE_COMMANDS[lane], p.text, cmd_w - 40);

    // Glowing dock icons
    const DOCK_ICON_W: u32 = 56;
    const DOCK_GAP: u32 = 10;
    let total_dock_w = N_DOCK as u32 * (DOCK_ICON_W + DOCK_GAP) - DOCK_GAP;
    let dock_x = (WIN_W - total_dock_w) as i32 - 24;
    for i in 0..N_DOCK {
        let (label, col, _) = DOCK_DATA[i];
        let ix = dock_x + i as i32 * (DOCK_ICON_W as i32 + DOCK_GAP as i32);
        let iy = dy + 14;
        canvas.fill_glow(ix + DOCK_ICON_W as i32 / 2, iy + DOCK_ICON_W as i32 / 2, 28, col & 0x00FF_FFFF, 130);
        canvas.fill_round_rect_blend(ix, iy, DOCK_ICON_W, DOCK_ICON_W - 6, 10, argb(blend(col, 0xFF000000, 140) & 0x00FF_FFFF, 230));
        canvas.fill_round_rect_blend(ix + 4, iy + 4, DOCK_ICON_W - 8, 14, 8, argb(blend(col, 0xFFFFFFFF, 100) & 0x00FF_FFFF, 110));
        let lx = ix + DOCK_ICON_W as i32 / 2 - (Canvas::text_width(label) / 2) as i32;
        canvas.draw_text(lx, iy + DOCK_ICON_W as i32 - 4, label, p.text_muted, DOCK_ICON_W);
    }
}

/// Small frosted pill button with a glow underline.
fn glass_button(canvas: &mut Canvas<'_>, x: i32, y: i32, w: u32, h: u32, label: &[u8], accent: u32) {
    canvas.fill_round_rect_blend(x, y, w, h, 6, argb(0x101F3A, 220));
    canvas.fill_rect_blend(x, y + h as i32 - 2, w, 2, argb(accent & 0x00FF_FFFF, 230));
    canvas.fill_glow(x + w as i32 / 2, y + h as i32 - 2, 14, accent & 0x00FF_FFFF, 150);
    let tw = Canvas::text_width(label);
    let tx = x + (w as i32 - tw as i32) / 2;
    canvas.draw_text(tx, y + h as i32 / 2 - 4, label, 0xFFFFFFFF, w);
}



// ── Entry point ────────────────────────────────────────────────────────────
fn format_clock<'a>(now_ms: u64, out: &'a mut [u8; 16]) -> &'a [u8] {
    let total_secs = now_ms / 1000;
    let hours = (total_secs / 3600) % 24;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;

    out[0] = b'0' + (hours / 10) as u8;
    out[1] = b'0' + (hours % 10) as u8;
    out[2] = b':';
    out[3] = b'0' + (minutes / 10) as u8;
    out[4] = b'0' + (minutes % 10) as u8;
    out[5] = b':';
    out[6] = b'0' + (seconds / 10) as u8;
    out[7] = b'0' + (seconds % 10) as u8;
    out[8] = b' ';
    out[9] = b'O';
    out[10] = b'R';
    out[11] = b'C';
    &out[..12]
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[shell3d] starting\n");

    let input_channel = match runtime::channel_create(64) {
        Some(ch) => ch,
        None => {
            runtime::write_line(b"[shell3d] channel_create failed\n");
            runtime::exit(1)
        }
    };

    runtime::write_line(b"[shell3d] opening window\n");
    let mut win = match Window::open(WIN_W, WIN_H, 0, 0, input_channel) {
        Some(w) => w,
        None => {
            runtime::write_line(b"[shell3d] window open failed\n");
            runtime::exit(2)
        }
    };
    runtime::write_line(b"[shell3d] window open ok\n");

    win.request_focus();
    runtime::write_line(b"[shell3d] focus requested\n");
    runtime::write_line(b"[shell3d] building scene\n");
    let mut scene = Scene::new();
    runtime::write_line(b"[shell3d] scene ready\n");

    const DOCK_ICON_W: i32 = 52;
    const DOCK_GAP: i32 = 8;
    let total_dock_w = N_DOCK as i32 * (DOCK_ICON_W + DOCK_GAP) - DOCK_GAP;
    let dock_icon_left = (WIN_W as i32 - total_dock_w) - 20;
    let dy = (WIN_H - DOCK_H) as i32;

    // First frame
    runtime::write_line(b"[shell3d] first render begin\n");
    { let mut c = win.canvas(); render(&mut c, &scene); }
    runtime::write_line(b"[shell3d] first render complete\n");
    runtime::write_line(b"[shell3d] first present begin\n");
    win.present();
    runtime::write_line(b"[shell3d] first present complete\n");
    runtime::write_line(b"[shell3d] first frame submitted\n");
    runtime::yield_now();

    loop {
        let mut dirty = false;

        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,
                Event::FrameTick { now_ms } => {
                    scene.now_ms = now_ms;
                    scene.tick = scene.tick.wrapping_add(1);
                    scene.update_springs();
                    if scene.last_refresh_ms == 0 || now_ms.saturating_sub(scene.last_refresh_ms) >= 1000 {
                        scene.refresh_runtime();
                        scene.last_refresh_ms = now_ms;
                    }
                    dirty = true;
                }
                Event::Key { pressed: true, ascii, .. } => {
                    match ascii {
                        b'\t' => {
                            scene.expose = !scene.expose;
                            dirty = true;
                        }
                        b'1'..=b'4' => {
                            scene.active_lane = (ascii - b'1') as usize;
                            scene.hover = Some(scene.active_lane.min(N_APPS.saturating_sub(1)));
                            dirty = true;
                        }
                        b'q' => runtime::exit(0),
                        _ => {}
                    }
                }
                Event::PointerMove { x, y, buttons } => {
                    let xi = x as i32;
                    let yi = y as i32;

                    let projected = scene.project_cards();
                    let previous_hover = scene.hover;
                    scene.hover = None;
                    for &(i, sx, sy, scale) in &projected {
                        let cw = (110 * scale / FP).max(60) / 2;
                        let ch = (66 * scale / FP).max(36) / 2;
                        let dx = xi - sx;
                        let dy = yi - sy;
                        if dx.abs() <= cw && dy.abs() <= ch {
                            scene.hover = Some(i);
                        }
                    }
                    if scene.hover != previous_hover {
                        dirty = true;
                    }

                    if buttons & 1 != 0 {
                        if !scene.drag {
                            scene.drag = true;
                            scene.drag_ox = scene.orbit_x;
                            scene.drag_oy = scene.orbit_y;
                            scene.drag_mx = xi;
                            scene.drag_my = yi;
                        } else {
                            scene.orbit_y = scene.drag_oy + (xi - scene.drag_mx) / 3;
                            scene.orbit_x = (scene.drag_ox + (yi - scene.drag_my) / 3).clamp(-40, 40);
                        }
                        dirty = true;
                    } else {
                        if scene.drag {
                            if let Some(hover_i) = scene.hover {
                                let _ = runtime::spawn_named(scene.cards[hover_i].spawn);
                            }
                            if yi >= dy {
                                let di = (xi - dock_icon_left) / (DOCK_ICON_W + DOCK_GAP);
                                if di >= 0 && (di as usize) < N_DOCK {
                                    let _ = runtime::spawn_named(DOCK_DATA[di as usize].2);
                                }
                            }
                            dirty = true;
                        }
                        scene.drag = false;
                    }
                }
                _ => {}
            }
        }

        if dirty {
            { let mut c = win.canvas(); render(&mut c, &scene); }
            win.present();
        }
        runtime::yield_now();
    }
}

fn append_u64_decimal(buf: &mut [u8], len: &mut usize, mut n: u64) {
    let start = *len;
    if n == 0 {
        if *len < buf.len() { buf[*len] = b'0'; *len += 1; }
        return;
    }
    while n > 0 && *len < buf.len() {
        buf[*len] = b'0' + (n % 10) as u8;
        *len += 1;
        n /= 10;
    }
    buf[start..*len].reverse();
}

struct LineWriter { buf: [u8; 192], len: usize }
impl LineWriter {
    fn new() -> Self { Self { buf: [0u8; 192], len: 0 } }
    fn bytes(&self) -> &[u8] { &self.buf[..self.len] }
}
impl Write for LineWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let b = s.as_bytes();
        let space = self.buf.len().saturating_sub(self.len);
        let n = b.len().min(space);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
        Ok(())
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    let mut line = [0u8; 96];
    let mut len = 0usize;
    let prefix = b"[shell3d] panic at line ";
    line[..prefix.len()].copy_from_slice(prefix);
    len += prefix.len();
    if let Some(loc) = _info.location() {
        append_u64_decimal(&mut line, &mut len, loc.line() as u64);
    } else {
        line[len] = b'?'; len += 1;
    }
    if len + 1 < line.len() { line[len] = b'\n'; len += 1; }
    runtime::write_line(&line[..len]);
    let mut w = LineWriter::new();
    let _ = writeln!(&mut w, "[shell3d] panic: {}", _info);
    runtime::write_line(w.bytes());
    runtime::exit(255)
}

