// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::gfx::canvas::Canvas;
use crate::gfx::surface::Surface;

const SCENE_COLS: usize = 56;
const SCENE_LINES: usize = 20;

pub struct SceneViewApp {
    lines: [[u8; SCENE_COLS]; SCENE_LINES],
    lens: [u8; SCENE_LINES],
    count: usize,
    online: bool,
}

impl SceneViewApp {
    pub fn new() -> Self {
        let mut app = Self {
            lines: [[0; SCENE_COLS]; SCENE_LINES],
            lens: [0; SCENE_LINES],
            count: 0,
            online: false,
        };
        app.set_offline();
        app
    }

    pub fn apply_summary(&mut self, bytes: &[u8]) {
        self.clear_lines();
        self.online = true;
        for raw_line in bytes.split(|&byte| byte == b'\n') {
            if self.count >= SCENE_LINES {
                break;
            }
            let line = trim_line(raw_line);
            if line.is_empty() {
                continue;
            }
            self.push_line(line);
        }
        if self.count == 0 {
            self.push_line(b"compositor reply empty");
        }
    }

    pub fn set_offline(&mut self) {
        self.clear_lines();
        self.online = false;
        self.push_line(b"protected compositor offline");
        self.push_line(b"desktop running local surfaces");
        self.push_line(b"waiting for scene-summary");
    }

    pub fn render(&self, surface: &mut Surface) {
        let width = surface.width();
        let height = surface.height();
        let mut canvas = Canvas::new(surface);
        let bg = if self.online { 0x0010151d } else { 0x00191212 };
        canvas.clear(bg);
        canvas.fill_rect(0, 0, width, height, bg);
        canvas.draw_text(8, 8, b"protected scene", 0x00d7e3f0, bg);

        let status = if self.online {
            b"service link active" as &[u8]
        } else {
            b"service link offline" as &[u8]
        };
        canvas.draw_text(8, 18, status, 0x006fd1ff, bg);

        let visible = self.count.min(SCENE_LINES);
        let mut y = 34i32;
        for idx in 0..visible {
            let line = &self.lines[idx][..self.lens[idx] as usize];
            canvas.draw_text(8, y, line, line_color(line), bg);
            y += 10;
        }
    }

    fn clear_lines(&mut self) {
        self.lines = [[0; SCENE_COLS]; SCENE_LINES];
        self.lens = [0; SCENE_LINES];
        self.count = 0;
    }

    fn push_line(&mut self, bytes: &[u8]) {
        if self.count >= SCENE_LINES {
            return;
        }
        let len = bytes.len().min(SCENE_COLS);
        self.lines[self.count][..len].copy_from_slice(&bytes[..len]);
        self.lens[self.count] = len as u8;
        self.count += 1;
    }
}

fn trim_line(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < bytes.len() && matches!(bytes[start], b' ' | b'\t' | b'\r' | 0) {
        start += 1;
    }

    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], b' ' | b'\t' | b'\r' | 0) {
        end -= 1;
    }

    &bytes[start..end]
}

fn line_color(line: &[u8]) -> u32 {
    if line.starts_with(b"scene ") {
        0x00e5eef8
    } else if line.starts_with(b"surfaces=") {
        0x006fd1ff
    } else if line.starts_with(b"s") {
        0x00b9c8d9
    } else {
        0x00d7e3f0
    }
}
