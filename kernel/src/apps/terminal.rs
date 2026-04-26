// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::gfx::canvas::Canvas;
use crate::gfx::surface::Surface;
use crate::input::event::InputEvent;

const TERM_COLS: usize = 56;
const TERM_LINES: usize = 20;

pub struct TerminalApp {
    lines: [[u8; TERM_COLS]; TERM_LINES],
    lens: [u8; TERM_LINES],
    head: usize,
    count: usize,
    current: [u8; TERM_COLS],
    current_len: usize,
}

impl TerminalApp {
    pub fn new(status: &[u8]) -> Self {
        let mut app = Self {
            lines: [[0; TERM_COLS]; TERM_LINES],
            lens: [0; TERM_LINES],
            head: 0,
            count: 0,
            current: [0; TERM_COLS],
            current_len: 0,
        };
        app.push_line(b"GraphOS Terminal");
        app.push_line(status);
        app.push_line(b"type keys, enter commits");
        app
    }

    pub fn handle_event(&mut self, event: InputEvent) {
        match event {
            InputEvent::Text(byte) if self.current_len < TERM_COLS => {
                self.current[self.current_len] = byte;
                self.current_len += 1;
            }
            InputEvent::Backspace if self.current_len > 0 => {
                self.current_len -= 1;
                self.current[self.current_len] = 0;
            }
            InputEvent::Text(_) | InputEvent::Backspace => {}
            InputEvent::Enter => {
                if self.current_len > 0 {
                    let mut line = [0u8; TERM_COLS];
                    line[..self.current_len].copy_from_slice(&self.current[..self.current_len]);
                    self.push_line(&line[..self.current_len]);
                    self.current.fill(0);
                    self.current_len = 0;
                } else {
                    self.push_line(b"");
                }
            }
            _ => {}
        }
    }

    pub fn render(&self, surface: &mut Surface) {
        let width = surface.width();
        let height = surface.height();
        let mut canvas = Canvas::new(surface);
        canvas.clear(0x0010171b);
        canvas.fill_rect(0, 0, width, height, 0x0010171b);
        canvas.draw_text(8, 8, b"terminal", 0x00d7e3f0, 0x0010171b);

        let visible = self.count.min(TERM_LINES.saturating_sub(1));
        let first = self.count.saturating_sub(visible);
        let mut row_y = 24i32;
        for row in 0..visible {
            let logical = first + row;
            let slot = if self.count < TERM_LINES {
                logical
            } else {
                (self.head + logical) % TERM_LINES
            };
            canvas.draw_text(
                8,
                row_y,
                &self.lines[slot][..self.lens[slot] as usize],
                0x00dce8f5,
                0x0010171b,
            );
            row_y += 10;
        }

        let mut prompt = [0u8; TERM_COLS + 2];
        prompt[0] = b'>';
        prompt[1] = b' ';
        prompt[2..2 + self.current_len].copy_from_slice(&self.current[..self.current_len]);
        canvas.draw_text(
            8,
            row_y + 4,
            &prompt[..self.current_len + 2],
            0x006fd1ff,
            0x0010171b,
        );
    }

    fn push_line(&mut self, text: &[u8]) {
        let len = text.len().min(TERM_COLS);
        let slot = self.head;
        self.lines[slot].fill(0);
        self.lines[slot][..len].copy_from_slice(&text[..len]);
        self.lens[slot] = len as u8;
        self.head = (self.head + 1) % TERM_LINES;
        self.count = self.count.min(TERM_LINES - 1) + 1;
    }
}
