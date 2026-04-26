// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use core::sync::atomic::{AtomicU32, Ordering};

use spin::Mutex;

use crate::gfx::canvas::Canvas;
use crate::gfx::surface::Surface;

const LOG_COLS: usize = 72;
const LOG_LINES: usize = 28;

#[derive(Clone)]
struct LogStore {
    lines: [[u8; LOG_COLS]; LOG_LINES],
    lens: [u8; LOG_LINES],
    head: usize,
    count: usize,
    current: [u8; LOG_COLS],
    current_len: usize,
}

impl LogStore {
    const fn new() -> Self {
        Self {
            lines: [[0; LOG_COLS]; LOG_LINES],
            lens: [0; LOG_LINES],
            head: 0,
            count: 0,
            current: [0; LOG_COLS],
            current_len: 0,
        }
    }

    fn push_line(&mut self, bytes: &[u8]) {
        let len = bytes.len().min(LOG_COLS);
        let slot = self.head;
        self.lines[slot].fill(0);
        self.lines[slot][..len].copy_from_slice(&bytes[..len]);
        self.lens[slot] = len as u8;
        self.head = (self.head + 1) % LOG_LINES;
        self.count = self.count.min(LOG_LINES - 1) + 1;
    }
}

static LOGS: Mutex<LogStore> = Mutex::new(LogStore::new());
static LOG_EPOCH: AtomicU32 = AtomicU32::new(0);

pub struct LogViewApp;

impl LogViewApp {
    pub fn new() -> Self {
        Self
    }

    pub fn render(&self, surface: &mut Surface) {
        let store = LOGS.lock().clone();
        let mut canvas = Canvas::new(surface);
        canvas.clear(0x000f1418);
        canvas.draw_text(8, 8, b"runtime log", 0x00d7e3f0, 0x000f1418);

        let visible = store.count.min(LOG_LINES);
        let first = store.count.saturating_sub(visible);
        let mut y = 24i32;
        for row in 0..visible {
            let logical = first + row;
            let slot = if store.count < LOG_LINES {
                logical
            } else {
                (store.head + logical) % LOG_LINES
            };
            canvas.draw_text(
                8,
                y,
                &store.lines[slot][..store.lens[slot] as usize],
                0x00b8c6d6,
                0x000f1418,
            );
            y += 10;
        }

        if store.current_len != 0 {
            canvas.draw_text(
                8,
                y + 4,
                &store.current[..store.current_len],
                0x006fd1ff,
                0x000f1418,
            );
        }
    }
}

pub fn record_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    // Acquire the LOGS lock with interrupts disabled.  This prevents a
    // deadlock where a kernel task is preempted while holding LOGS.lock()
    // and the new task's INT-0x80 handler (IF=0) tries to acquire LOGS too.
    crate::arch::interrupts::without_interrupts(|| {
        let mut store = LOGS.lock();
        let mut changed = false;
        for &byte in bytes {
            match byte {
                b'\r' => {}
                b'\n' => {
                    if store.current_len != 0 {
                        let current_len = store.current_len;
                        let mut line = [0u8; LOG_COLS];
                        line[..current_len].copy_from_slice(&store.current[..current_len]);
                        store.push_line(&line[..current_len]);
                        store.current.fill(0);
                        store.current_len = 0;
                        changed = true;
                    }
                }
                _ => {
                    if store.current_len < LOG_COLS {
                        let current_len = store.current_len;
                        store.current[current_len] = byte;
                        store.current_len += 1;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            LOG_EPOCH.fetch_add(1, Ordering::Relaxed);
        }
    });
}

pub fn epoch() -> u32 {
    LOG_EPOCH.load(Ordering::Relaxed)
}
