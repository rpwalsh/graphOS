// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::apps::inputharness::InputHarnessApp;
use crate::apps::logview::LogViewApp;
use crate::apps::operator::OperatorApp;
use crate::apps::sceneview::SceneViewApp;
use crate::apps::terminal::TerminalApp;
use crate::gfx::surface::Surface;
use crate::input::event::InputEvent;

pub const TITLE_BAR_HEIGHT: u32 = 18;
pub const BORDER: u32 = 2;
pub const CLOSE_SIZE: u32 = 10;

#[derive(Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> i32 {
        self.x.saturating_add(self.w as i32)
    }

    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.h as i32)
    }

    pub fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(
            x0,
            y0,
            x1.saturating_sub(x0).max(0) as u32,
            y1.saturating_sub(y0).max(0) as u32,
        )
    }

    pub fn intersects(self, other: Self) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }

    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
}

pub enum WindowApp {
    Operator(OperatorApp),
    InputHarness(InputHarnessApp),
    Terminal(TerminalApp),
    Log(LogViewApp),
    Scene(SceneViewApp),
}

pub struct Window {
    pub id: u32,
    pub bounds: Rect,
    pub focused: bool,
    title: [u8; 32],
    title_len: usize,
    pub surface: Surface,
    pub app: WindowApp,
    content_dirty: bool,
}

impl Window {
    pub fn new_operator(id: u32, x: i32, y: i32, test_failures: u32, protected_ok: bool) -> Self {
        let content_w = 632u32;
        let content_h = 404u32;
        let mut window = Self {
            id,
            bounds: Rect::new(
                x,
                y,
                content_w.saturating_add(BORDER * 2),
                content_h
                    .saturating_add(TITLE_BAR_HEIGHT)
                    .saturating_add(BORDER * 2),
            ),
            focused: false,
            title: [0; 32],
            title_len: 0,
            surface: Surface::new(content_w, content_h, 0x000b1016),
            app: WindowApp::Operator(OperatorApp::new(test_failures, protected_ok)),
            content_dirty: true,
        };
        window.set_title(b"Operator");
        window
    }

    pub fn new_terminal(id: u32, x: i32, y: i32, status: &[u8]) -> Self {
        let content_w = 472u32;
        let content_h = 232u32;
        let mut window = Self {
            id,
            bounds: Rect::new(
                x,
                y,
                content_w.saturating_add(BORDER * 2),
                content_h
                    .saturating_add(TITLE_BAR_HEIGHT)
                    .saturating_add(BORDER * 2),
            ),
            focused: false,
            title: [0; 32],
            title_len: 0,
            surface: Surface::new(content_w, content_h, 0x0010171b),
            app: WindowApp::Terminal(TerminalApp::new(status)),
            content_dirty: true,
        };
        window.set_title(b"Terminal");
        window
    }

    pub fn new_input_harness(id: u32, x: i32, y: i32) -> Self {
        let content_w = 560u32;
        let content_h = 352u32;
        let mut window = Self {
            id,
            bounds: Rect::new(
                x,
                y,
                content_w.saturating_add(BORDER * 2),
                content_h
                    .saturating_add(TITLE_BAR_HEIGHT)
                    .saturating_add(BORDER * 2),
            ),
            focused: false,
            title: [0; 32],
            title_len: 0,
            surface: Surface::new(content_w, content_h, 0x000c1117),
            app: WindowApp::InputHarness(InputHarnessApp::new()),
            content_dirty: true,
        };
        window.set_title(b"Input Harness");
        window
    }

    pub fn new_log(id: u32, x: i32, y: i32) -> Self {
        let content_w = 520u32;
        let content_h = 286u32;
        let mut window = Self {
            id,
            bounds: Rect::new(
                x,
                y,
                content_w.saturating_add(BORDER * 2),
                content_h
                    .saturating_add(TITLE_BAR_HEIGHT)
                    .saturating_add(BORDER * 2),
            ),
            focused: false,
            title: [0; 32],
            title_len: 0,
            surface: Surface::new(content_w, content_h, 0x000f1418),
            app: WindowApp::Log(LogViewApp::new()),
            content_dirty: true,
        };
        window.set_title(b"Log");
        window
    }

    pub fn new_scene(id: u32, x: i32, y: i32) -> Self {
        let content_w = 456u32;
        let content_h = 232u32;
        let mut window = Self {
            id,
            bounds: Rect::new(
                x,
                y,
                content_w.saturating_add(BORDER * 2),
                content_h
                    .saturating_add(TITLE_BAR_HEIGHT)
                    .saturating_add(BORDER * 2),
            ),
            focused: false,
            title: [0; 32],
            title_len: 0,
            surface: Surface::new(content_w, content_h, 0x0010151d),
            app: WindowApp::Scene(SceneViewApp::new()),
            content_dirty: true,
        };
        window.set_title(b"Protected Scene");
        window
    }

    pub fn title(&self) -> &[u8] {
        &self.title[..self.title_len]
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.bounds.x
            && y >= self.bounds.y
            && x < self.bounds.x.saturating_add(self.bounds.w as i32)
            && y < self.bounds.y.saturating_add(self.bounds.h as i32)
    }

    pub fn title_bar_contains(&self, x: i32, y: i32) -> bool {
        self.contains(x, y)
            && y < self
                .bounds
                .y
                .saturating_add(BORDER as i32)
                .saturating_add(TITLE_BAR_HEIGHT as i32)
    }

    pub fn close_button_contains(&self, x: i32, y: i32) -> bool {
        let (bx, by, bw, bh) = self.close_button_rect();
        x >= bx && y >= by && x < bx.saturating_add(bw as i32) && y < by.saturating_add(bh as i32)
    }

    pub fn close_button_rect(&self) -> (i32, i32, u32, u32) {
        let x = self
            .bounds
            .x
            .saturating_add(self.bounds.w as i32)
            .saturating_sub(BORDER as i32)
            .saturating_sub(CLOSE_SIZE as i32)
            .saturating_sub(4);
        let y = self
            .bounds
            .y
            .saturating_add(BORDER as i32)
            .saturating_add(4);
        (x, y, CLOSE_SIZE, CLOSE_SIZE)
    }

    pub fn content_x(&self) -> i32 {
        self.bounds.x + BORDER as i32
    }

    pub fn content_y(&self) -> i32 {
        self.bounds.y + BORDER as i32 + TITLE_BAR_HEIGHT as i32
    }

    pub fn handle_event(&mut self, event: InputEvent) {
        match &mut self.app {
            WindowApp::Operator(app) => {
                app.handle_event(event);
                self.content_dirty = true;
            }
            WindowApp::InputHarness(app) => {
                app.handle_event(event);
                self.content_dirty = true;
            }
            WindowApp::Terminal(app) => {
                app.handle_event(event);
                self.content_dirty = true;
            }
            WindowApp::Log(_) | WindowApp::Scene(_) => {}
        }
    }

    pub fn render_if_dirty(&mut self) {
        if !self.content_dirty {
            return;
        }

        match &mut self.app {
            WindowApp::Operator(app) => app.render(&mut self.surface),
            WindowApp::InputHarness(app) => app.render(&mut self.surface),
            WindowApp::Terminal(app) => app.render(&mut self.surface),
            WindowApp::Log(app) => app.render(&mut self.surface),
            WindowApp::Scene(app) => app.render(&mut self.surface),
        }
        self.content_dirty = false;
    }

    pub fn handle_primary_click(&mut self, x: i32, y: i32) -> bool {
        let local_x = x.saturating_sub(self.content_x());
        let local_y = y.saturating_sub(self.content_y());
        if local_x < 0 || local_y < 0 {
            return false;
        }

        let handled = match &mut self.app {
            WindowApp::Operator(app) => app.handle_click(local_x, local_y),
            WindowApp::InputHarness(app) => app.handle_click(local_x, local_y),
            WindowApp::Terminal(_) | WindowApp::Log(_) | WindowApp::Scene(_) => false,
        };
        if handled {
            self.content_dirty = true;
        }
        handled
    }

    pub fn scene_app_mut(&mut self) -> Option<&mut SceneViewApp> {
        match &mut self.app {
            WindowApp::Scene(app) => Some(app),
            _ => None,
        }
    }

    pub fn is_log(&self) -> bool {
        matches!(&self.app, WindowApp::Log(_))
    }

    pub fn mark_content_dirty(&mut self) {
        self.content_dirty = true;
    }

    pub fn is_content_dirty(&self) -> bool {
        self.content_dirty
    }

    pub fn damage_bounds(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.w.saturating_add(4),
            self.bounds.h.saturating_add(4),
        )
    }

    fn set_title(&mut self, title: &[u8]) {
        self.title_len = title.len().min(self.title.len());
        self.title[..self.title_len].copy_from_slice(&title[..self.title_len]);
        self.title[self.title_len..].fill(0);
    }
}
