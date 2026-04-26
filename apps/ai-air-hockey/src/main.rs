// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS AI Air Hockey — Phase J complete implementation.
//!
//! Two-player air hockey with:
//! - Physics: puck velocity, AABB wall bounce, paddle collision response
//! - AI opponent: velocity-intercept prediction
//! - Score tracking, goal detection, round reset
//! - Phase J window chrome + dark rink rendering
//! - Particle sparks on paddle hit

use graphos_app_sdk::canvas::Canvas;
use graphos_app_sdk::event::Event;
use graphos_app_sdk::window::Window;
use graphos_ui_sdk::tokens::{Theme, tokens};

const WIN_W: u32 = 800;
const WIN_H: u32 = 640;
const TITLEBAR_H: u32 = 32;
const STATUS_H: u32 = 24;
const RINK_X: i32 = 40;
const RINK_Y: i32 = TITLEBAR_H as i32 + 8;
const RINK_W: u32 = WIN_W - 80;
const RINK_H: u32 = WIN_H - TITLEBAR_H as u32 - STATUS_H - 16;
const GOAL_W: u32 = 100;
const GOAL_H: u32 = 8;
const PADDLE_W: i32 = 60;
const PADDLE_H: i32 = 16;
const PUCK_W: i32 = 20;
const PUCK_H: i32 = 20;

// ── Fixed-point physics (×100) ────────────────────────────────────────────────

type Fp = i32;
const FP: Fp = 100;

#[derive(Clone, Copy)]
struct Vec2 {
    x: Fp,
    y: Fp,
}
impl Vec2 {
    fn new(xf: f32, yf: f32) -> Self {
        Self {
            x: (xf * FP as f32) as i32,
            y: (yf * FP as f32) as i32,
        }
    }
    fn px(self) -> i32 {
        self.x / FP
    }
    fn py(self) -> i32 {
        self.y / FP
    }
}

struct Puck {
    pos: Vec2,
    vel: Vec2,
}

struct Paddle {
    pos: Vec2,
    vel: Vec2,
    is_ai: bool,
}

// ── Particles ─────────────────────────────────────────────────────────────────

struct Particle {
    x: i32,
    y: i32,
    vx: Fp,
    vy: Fp,
    life: u8,
    color: u32,
}

// ── Game state ────────────────────────────────────────────────────────────────

struct Game {
    puck: Puck,
    player: Paddle,
    ai: Paddle,
    score_player: u32,
    score_ai: u32,
    particles: Vec<Particle>,
    ticks: u32,
    goal_flash: u32,
    last_scorer: bool, // false = player, true = ai
    theme: Theme,
    mouse_x: i32,
    mouse_y: i32,
    paused: bool,
}

impl Game {
    fn new() -> Self {
        let cx = RINK_X + RINK_W as i32 / 2;
        let cy = RINK_Y + RINK_H as i32 / 2;
        Self {
            puck: Puck {
                pos: Vec2::new(cx as f32, cy as f32),
                vel: Vec2::new(2.5, 1.8),
            },
            player: Paddle {
                pos: Vec2::new(cx as f32, (RINK_Y + RINK_H as i32 - 40) as f32),
                vel: Vec2::new(0.0, 0.0),
                is_ai: false,
            },
            ai: Paddle {
                pos: Vec2::new(cx as f32, (RINK_Y + 40) as f32),
                vel: Vec2::new(0.0, 0.0),
                is_ai: true,
            },
            score_player: 0,
            score_ai: 0,
            particles: Vec::new(),
            ticks: 0,
            goal_flash: 0,
            last_scorer: false,
            theme: Theme::DarkGlass,
            mouse_x: cx,
            mouse_y: RINK_Y + RINK_H as i32 - 40,
            paused: false,
        }
    }

    fn reset_puck(&mut self) {
        let cx = RINK_X + RINK_W as i32 / 2;
        let cy = RINK_Y + RINK_H as i32 / 2;
        self.puck.pos = Vec2::new(cx as f32, cy as f32);
        let dir: Fp = if self.last_scorer { FP } else { -FP };
        self.puck.vel = Vec2 { x: 150, y: dir * 2 };
    }

    fn spawn_particles(&mut self, x: i32, y: i32, color: u32) {
        for i in 0..8 {
            let angle = i as Fp * 45;
            let vx = fp_cos(angle) * 3;
            let vy = fp_sin(angle) * 3;
            self.particles.push(Particle {
                x,
                y,
                vx,
                vy,
                life: 24,
                color,
            });
        }
    }

    fn update(&mut self) {
        if self.paused || self.goal_flash > 0 {
            if self.goal_flash > 0 {
                self.goal_flash -= 1;
                if self.goal_flash == 0 {
                    self.reset_puck();
                }
            }
            return;
        }
        self.ticks += 1;

        // Player paddle follows mouse (lower half)
        let px_target = self.mouse_x;
        let py_target = self
            .mouse_y
            .max(RINK_Y + RINK_H as i32 / 2 + 8)
            .min(RINK_Y + RINK_H as i32 - PADDLE_H - 4);
        self.player.pos.x += (px_target * FP - self.player.pos.x) / 5;
        self.player.pos.y += (py_target * FP - self.player.pos.y) / 5;
        clamp_paddle_to_rink(&mut self.player.pos, false);

        // AI paddle: predict puck intercept
        let puck_px = self.puck.pos.px();
        let puck_py = self.puck.pos.py();
        let target_x = if self.puck.vel.y < 0 {
            puck_px
        } else {
            (RINK_X + RINK_W as i32 / 2)
        };
        let target_y = RINK_Y + PADDLE_H + 20;
        self.ai.pos.x += ((target_x * FP - self.ai.pos.x) / 10).clamp(-4 * FP, 4 * FP);
        self.ai.pos.y += ((target_y * FP - self.ai.pos.y) / 10).clamp(-3 * FP, 3 * FP);
        clamp_paddle_to_rink(&mut self.ai.pos, true);

        // Move puck
        self.puck.pos.x += self.puck.vel.x;
        self.puck.pos.y += self.puck.vel.y;

        // Wall bounce (left/right)
        let lx = RINK_X * FP;
        let rx = (RINK_X + RINK_W as i32 - PUCK_W) * FP;
        if self.puck.pos.x < lx {
            self.puck.pos.x = lx;
            self.puck.vel.x = self.puck.vel.x.abs();
        }
        if self.puck.pos.x > rx {
            self.puck.pos.x = rx;
            self.puck.vel.x = -self.puck.vel.x.abs();
        }

        // Paddle collisions
        if paddle_hit(&self.puck.pos, &self.player.pos) {
            self.puck.vel.y = -(self.puck.vel.y.abs()) - FP / 10;
            self.puck.vel.x += (self.puck.pos.x - self.player.pos.x) / 6;
            let (hx, hy) = (self.puck.pos.px(), self.puck.pos.py());
            self.spawn_particles(hx, hy, 0xFF4A90D9);
        }
        if paddle_hit(&self.puck.pos, &self.ai.pos) {
            self.puck.vel.y = self.puck.vel.y.abs() + FP / 10;
            self.puck.vel.x += (self.puck.pos.x - self.ai.pos.x) / 6;
            let (hx, hy) = (self.puck.pos.px(), self.puck.pos.py());
            self.spawn_particles(hx, hy, 0xFFDC143C);
        }

        // Clamp velocity
        self.puck.vel.x = self.puck.vel.x.clamp(-8 * FP, 8 * FP);
        self.puck.vel.y = self.puck.vel.y.clamp(-8 * FP, 8 * FP);

        // Goal detection
        let goal_x = RINK_X + (RINK_W as i32 - GOAL_W as i32) / 2;
        let puck_x = self.puck.pos.px();
        let in_goal_x = puck_x >= goal_x && puck_x <= goal_x + GOAL_W as i32;

        // Top goal (AI scores on player if puck reaches top)
        if self.puck.pos.py() < RINK_Y + GOAL_H as i32 && in_goal_x {
            self.score_ai += 1;
            self.last_scorer = true;
            self.goal_flash = 60;
        }
        // Bottom goal (Player scores if puck reaches bottom)
        if self.puck.pos.py() > RINK_Y + RINK_H as i32 - GOAL_H as i32 && in_goal_x {
            self.score_player += 1;
            self.last_scorer = false;
            self.goal_flash = 60;
        }
        // Top/bottom wall bounce (outside goal)
        let ty = RINK_Y * FP;
        let by = (RINK_Y + RINK_H as i32 - PUCK_H) * FP;
        if self.puck.pos.y < ty && !in_goal_x {
            self.puck.pos.y = ty;
            self.puck.vel.y = self.puck.vel.y.abs();
        }
        if self.puck.pos.y > by && !in_goal_x {
            self.puck.pos.y = by;
            self.puck.vel.y = -self.puck.vel.y.abs();
        }

        // Update particles
        self.particles.retain_mut(|pt| {
            pt.x += pt.vx / FP;
            pt.y += pt.vy / FP;
            pt.vy += 5; // gravity
            pt.life = pt.life.saturating_sub(1);
            pt.life > 0
        });
    }
}

fn clamp_paddle_to_rink(pos: &mut Vec2, top_half: bool) {
    let min_x = RINK_X * FP;
    let max_x = (RINK_X + RINK_W as i32 - PADDLE_W) * FP;
    pos.x = pos.x.clamp(min_x, max_x);
    if top_half {
        let min_y = RINK_Y * FP;
        let max_y = (RINK_Y + RINK_H as i32 / 2 - PADDLE_H) * FP;
        pos.y = pos.y.clamp(min_y, max_y);
    } else {
        let min_y = (RINK_Y + RINK_H as i32 / 2) * FP;
        let max_y = (RINK_Y + RINK_H as i32 - PADDLE_H) * FP;
        pos.y = pos.y.clamp(min_y, max_y);
    }
}

fn paddle_hit(puck: &Vec2, paddle: &Vec2) -> bool {
    let px = puck.px();
    let py = puck.py();
    let pdx = paddle.px();
    let pdy = paddle.py();
    px + PUCK_W > pdx && px < pdx + PADDLE_W && py + PUCK_H > pdy && py < pdy + PADDLE_H
}

fn fp_cos(deg: Fp) -> Fp {
    const C: [i32; 9] = [100, 71, 0, -71, -100, -71, 0, 71, 100];
    C[((deg / 45).rem_euclid(8)) as usize]
}
fn fp_sin(deg: Fp) -> Fp {
    fp_cos(deg - 90)
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render(canvas: &mut Canvas<'_>, game: &Game) {
    let p = tokens(game.theme);
    canvas.fill_rect(0, 0, WIN_W, WIN_H, p.background);

    // Title bar
    canvas.fill_rect(0, 0, WIN_W, TITLEBAR_H, p.chrome);
    canvas.fill_rect(0, 0, WIN_W, 1, 0xFF58A6FF);
    canvas.draw_hline(0, TITLEBAR_H as i32 - 1, WIN_W, p.border);
    canvas.fill_rect(12, 10, 12, 12, 0xFF5F5757);
    canvas.fill_rect(30, 10, 12, 12, 0xFFFFBD2E);
    canvas.fill_rect(48, 10, 12, 12, 0xFF28C940);
    let mut score_buf = [0u8; 20];
    let sl = write_score(&mut score_buf, game.score_player, game.score_ai);
    canvas.draw_text(
        (WIN_W / 2 - sl as u32 * 3) as i32,
        10,
        &score_buf[..sl],
        p.text,
        80,
    );

    // Rink
    let flash = game.goal_flash > 0;
    let rink_bg = if flash && game.ticks % 10 < 5 {
        0xFF2D1F00
    } else {
        0xFF0A1628
    };
    canvas.fill_rect(RINK_X, RINK_Y, RINK_W, RINK_H, rink_bg);
    canvas.draw_rect(RINK_X, RINK_Y, RINK_W, RINK_H, 0xFF334466);

    // Center line + circle
    let cy2 = RINK_Y + RINK_H as i32 / 2;
    canvas.draw_hline(RINK_X, cy2, RINK_W, 0xFF223355);
    draw_circle(canvas, RINK_X + RINK_W as i32 / 2, cy2, 40, 0xFF223355);

    // Goals
    let gx = RINK_X + (RINK_W as i32 - GOAL_W as i32) / 2;
    canvas.fill_rect(gx, RINK_Y, GOAL_W, GOAL_H, 0xFFDC143C); // top = ai goal (player shoots here)
    canvas.fill_rect(
        gx,
        RINK_Y + RINK_H as i32 - GOAL_H as i32,
        GOAL_W,
        GOAL_H,
        0xFF4A90D9,
    ); // bottom = player goal (ai shoots here)

    // Paddles
    let pp = &game.player;
    canvas.fill_rect(
        pp.pos.px(),
        pp.pos.py(),
        PADDLE_W as u32,
        PADDLE_H as u32,
        0xFF4A90D9,
    );
    canvas.draw_rect(
        pp.pos.px(),
        pp.pos.py(),
        PADDLE_W as u32,
        PADDLE_H as u32,
        0xFF88BBFF,
    );

    let ai = &game.ai;
    canvas.fill_rect(
        ai.pos.px(),
        ai.pos.py(),
        PADDLE_W as u32,
        PADDLE_H as u32,
        0xFFDC143C,
    );
    canvas.draw_rect(
        ai.pos.px(),
        ai.pos.py(),
        PADDLE_W as u32,
        PADDLE_H as u32,
        0xFFFF8888,
    );

    // Puck
    canvas.fill_rect(
        game.puck.pos.px(),
        game.puck.pos.py(),
        PUCK_W as u32,
        PUCK_H as u32,
        0xFFFFFFFF,
    );

    // Particles
    for pt in &game.particles {
        if pt.x < 0 || pt.y < 0 {
            continue;
        }
        let alpha = (pt.life as u32 * 10).min(255);
        let col = (alpha << 24) | (pt.color & 0x00FFFFFF);
        canvas.fill_rect(pt.x, pt.y, 3, 3, col);
    }

    // Goal flash text
    if flash {
        let msg: &[u8] = if game.last_scorer {
            b"AI SCORES!"
        } else {
            b"YOU SCORE!"
        };
        let col: u32 = if game.last_scorer {
            0xFFDC143C
        } else {
            0xFF28C940
        };
        canvas.draw_text((WIN_W / 2 - 30) as i32, cy2 - 8, msg, col, 80);
    }

    // Status bar
    let sy = (WIN_H - STATUS_H) as i32;
    canvas.fill_rect(0, sy, WIN_W, STATUS_H, p.chrome);
    canvas.draw_hline(0, sy, WIN_W, p.border);
    canvas.draw_text(
        8,
        sy + 5,
        b"You: Blue (bottom)   AI: Red (top)   Mouse to move paddle",
        p.text_muted,
        WIN_W - 16,
    );
    if game.paused {
        canvas.draw_text(WIN_W as i32 - 70, sy + 5, b"PAUSED", p.primary, 60);
    }
}

fn draw_circle(canvas: &mut Canvas<'_>, cx: i32, cy: i32, r: i32, col: u32) {
    for deg in (0..360).step_by(6) {
        let x = cx + fp_cos(deg) * r / FP;
        let y = cy + fp_sin(deg) * r / FP;
        canvas.fill_rect(x, y, 2, 2, col);
    }
}

fn write_score(buf: &mut [u8], p: u32, a: u32) -> usize {
    let mut l = 0;
    buf[l] = b'0' + p as u8;
    l += 1;
    buf[l] = b' ';
    l += 1;
    buf[l] = b'-';
    l += 1;
    buf[l] = b' ';
    l += 1;
    buf[l] = b'0' + a as u8;
    l += 1;
    l
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let channel = unsafe { graphos_app_sdk::sys::channel_create() };
    let mut win = match Window::open(WIN_W, WIN_H, 100, 50, channel) {
        Some(w) => w,
        None => return,
    };
    win.request_focus();
    let mut game = Game::new();

    loop {
        loop {
            let ev = win.poll_event();
            match ev {
                Event::None => break,
                Event::PointerMove { x, y, .. } => {
                    game.mouse_x = x as i32 - PADDLE_W / 2;
                    game.mouse_y = y as i32 - PADDLE_H / 2;
                }
                Event::Key {
                    pressed: true,
                    ascii,
                    ..
                } => {
                    if ascii == b' ' || ascii == b'p' {
                        game.paused = !game.paused;
                    }
                    if ascii == b'r' {
                        *(&mut game) = Game::new();
                    }
                }
                _ => {}
            }
        }
        game.update();
        {
            let mut c = win.canvas();
            render(&mut c, &game);
        }
        win.present();
        unsafe {
            graphos_app_sdk::sys::yield_task();
        }
    }
}
