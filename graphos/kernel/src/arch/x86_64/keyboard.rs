// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal PS/2 keyboard input queue for the desktop shell.
//!
//! This is intentionally tiny: we only decode the keys we currently need for
//! the desktop shell and serial-equivalent prompt control.

use spin::Mutex;
use x86_64::instructions::interrupts;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyInput {
    Char(u8),
    Tab { shift: bool },
    Left,
    Right,
    Up,
    Down,
    Backspace,
    Enter,
}

#[derive(Clone, Copy)]
struct KeyboardQueue {
    buf: [Option<KeyInput>; 32],
    head: usize,
    len: usize,
    extended: bool,
    shift_down: bool,
}

impl KeyboardQueue {
    const fn new() -> Self {
        Self {
            buf: [None; 32],
            head: 0,
            len: 0,
            extended: false,
            shift_down: false,
        }
    }

    fn push(&mut self, input: KeyInput) {
        let tail = (self.head + self.len) % self.buf.len();
        self.buf[tail] = Some(input);
        if self.len == self.buf.len() {
            self.head = (self.head + 1) % self.buf.len();
        } else {
            self.len += 1;
        }
    }

    fn pop(&mut self) -> Option<KeyInput> {
        if self.len == 0 {
            return None;
        }
        let item = self.buf[self.head].take();
        self.head = (self.head + 1) % self.buf.len();
        self.len -= 1;
        item
    }
}

static KEYBOARD: Mutex<KeyboardQueue> = Mutex::new(KeyboardQueue::new());

pub fn push_scancode(scancode: u8) {
    interrupts::without_interrupts(|| {
        let mut queue = KEYBOARD.lock();

        if scancode == 0xE0 {
            queue.extended = true;
            return;
        }

        match scancode {
            0x2A | 0x36 => {
                queue.shift_down = true;
                queue.extended = false;
                return;
            }
            0xAA | 0xB6 => {
                queue.shift_down = false;
                queue.extended = false;
                return;
            }
            _ => {}
        }

        if scancode & 0x80 != 0 {
            queue.extended = false;
            return;
        }

        let key = if queue.extended {
            queue.extended = false;
            match scancode {
                0x4B => Some(KeyInput::Left),
                0x4D => Some(KeyInput::Right),
                0x48 => Some(KeyInput::Up),
                0x50 => Some(KeyInput::Down),
                0x1C => Some(KeyInput::Enter),
                _ => None,
            }
        } else {
            decode_make(scancode, queue.shift_down)
        };

        if let Some(key) = key {
            queue.push(key);
            crate::input::diagnostics::record_key(key);
        }
    });
    crate::sched::notify_interactive_input();
}

pub fn try_read_key() -> Option<KeyInput> {
    interrupts::without_interrupts(|| KEYBOARD.lock().pop())
}

pub fn has_pending_key() -> bool {
    interrupts::without_interrupts(|| KEYBOARD.lock().len != 0)
}

fn decode_make(scancode: u8, shift_down: bool) -> Option<KeyInput> {
    let _ = shift_down;
    match scancode {
        0x0E => Some(KeyInput::Backspace),
        0x0F => Some(KeyInput::Tab { shift: shift_down }),
        0x1C => Some(KeyInput::Enter),
        0x02 => Some(KeyInput::Char(b'1')),
        0x03 => Some(KeyInput::Char(b'2')),
        0x04 => Some(KeyInput::Char(b'3')),
        0x05 => Some(KeyInput::Char(b'4')),
        0x06 => Some(KeyInput::Char(b'5')),
        0x07 => Some(KeyInput::Char(b'6')),
        0x08 => Some(KeyInput::Char(b'7')),
        0x09 => Some(KeyInput::Char(b'8')),
        0x0A => Some(KeyInput::Char(b'9')),
        0x0B => Some(KeyInput::Char(b'0')),
        0x10 => Some(KeyInput::Char(b'q')),
        0x11 => Some(KeyInput::Char(b'w')),
        0x12 => Some(KeyInput::Char(b'e')),
        0x13 => Some(KeyInput::Char(b'r')),
        0x14 => Some(KeyInput::Char(b't')),
        0x15 => Some(KeyInput::Char(b'y')),
        0x16 => Some(KeyInput::Char(b'u')),
        0x17 => Some(KeyInput::Char(b'i')),
        0x18 => Some(KeyInput::Char(b'o')),
        0x19 => Some(KeyInput::Char(b'p')),
        0x1E => Some(KeyInput::Char(b'a')),
        0x1F => Some(KeyInput::Char(b's')),
        0x20 => Some(KeyInput::Char(b'd')),
        0x21 => Some(KeyInput::Char(b'f')),
        0x22 => Some(KeyInput::Char(b'g')),
        0x23 => Some(KeyInput::Char(b'h')),
        0x24 => Some(KeyInput::Char(b'j')),
        0x25 => Some(KeyInput::Char(b'k')),
        0x26 => Some(KeyInput::Char(b'l')),
        0x2C => Some(KeyInput::Char(b'z')),
        0x2D => Some(KeyInput::Char(b'x')),
        0x2E => Some(KeyInput::Char(b'c')),
        0x2F => Some(KeyInput::Char(b'v')),
        0x30 => Some(KeyInput::Char(b'b')),
        0x31 => Some(KeyInput::Char(b'n')),
        0x32 => Some(KeyInput::Char(b'm')),
        0x39 => Some(KeyInput::Char(b' ')),
        _ => None,
    }
}
