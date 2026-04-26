// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use core::cmp::max;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::interrupts;
use spin::Mutex;

use crate::arch::timer;
use crate::arch::x86_64::keyboard::KeyInput;
use crate::graph::arena;
use crate::graph::types::{EdgeKind, NODE_ID_KERNEL, NodeId, NodeKind};
use crate::input::pointer::{MouseButton, PointerEvent};

pub const POINTER_TRAIL_POINTS: usize = 72;
pub const KEY_HISTORY_LINES: usize = 14;
pub const KEY_HISTORY_COLS: usize = 30;
const BACKEND_LABEL_BYTES: usize = 24;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputHealth {
    Stable,
    Watching,
    Unstable,
}

#[derive(Clone, Copy)]
pub struct PointerTrailPoint {
    pub x: i32,
    pub y: i32,
    pub jump: u32,
}

#[derive(Clone, Copy)]
pub struct Snapshot {
    pub online: bool,
    pub backend: [u8; BACKEND_LABEL_BYTES],
    pub backend_len: usize,
    pub display_width: u32,
    pub display_height: u32,
    pub pointer_device_node: NodeId,
    pub keyboard_device_node: NodeId,
    pub latest_anomaly_node: NodeId,
    pub graph_anomaly_count: u64,
    pub enqueued_events: u64,
    pub delivered_events: u64,
    pub abs_events: u64,
    pub relative_events: u64,
    pub button_events: u64,
    pub coalesced_abs_events: u64,
    pub queue_depth: usize,
    pub max_queue_depth: usize,
    pub severe_jump_count: u64,
    pub max_jump: u32,
    pub last_jump: u32,
    pub last_abs_x: i32,
    pub last_abs_y: i32,
    pub have_abs_sample: bool,
    pub rendered_x: i32,
    pub rendered_y: i32,
    pub have_rendered_cursor: bool,
    pub left_down: bool,
    pub render_lag_x: i32,
    pub render_lag_y: i32,
    pub last_input_tick: u64,
    pub trail: [PointerTrailPoint; POINTER_TRAIL_POINTS],
    pub trail_head: usize,
    pub trail_count: usize,
    pub keyboard_events: u64,
    pub key_lines: [[u8; KEY_HISTORY_COLS]; KEY_HISTORY_LINES],
    pub key_lens: [u8; KEY_HISTORY_LINES],
    pub key_head: usize,
    pub key_count: usize,
    pub health: InputHealth,
}

struct DiagnosticsState {
    online: bool,
    backend: [u8; BACKEND_LABEL_BYTES],
    backend_len: usize,
    display_width: u32,
    display_height: u32,
    pointer_device_node: NodeId,
    keyboard_device_node: NodeId,
    latest_anomaly_node: NodeId,
    pending_graph_anomalies: u32,
    graph_anomaly_count: u64,
    enqueued_events: u64,
    delivered_events: u64,
    abs_events: u64,
    relative_events: u64,
    button_events: u64,
    coalesced_abs_events: u64,
    queue_depth: usize,
    max_queue_depth: usize,
    severe_jump_count: u64,
    max_jump: u32,
    last_jump: u32,
    last_abs_x: i32,
    last_abs_y: i32,
    have_abs_sample: bool,
    rendered_x: i32,
    rendered_y: i32,
    have_rendered_cursor: bool,
    left_down: bool,
    last_input_tick: u64,
    trail: [PointerTrailPoint; POINTER_TRAIL_POINTS],
    trail_head: usize,
    trail_count: usize,
    keyboard_events: u64,
    key_lines: [[u8; KEY_HISTORY_COLS]; KEY_HISTORY_LINES],
    key_lens: [u8; KEY_HISTORY_LINES],
    key_head: usize,
    key_count: usize,
}

impl DiagnosticsState {
    const fn new() -> Self {
        Self {
            online: false,
            backend: [0; BACKEND_LABEL_BYTES],
            backend_len: 0,
            display_width: 0,
            display_height: 0,
            pointer_device_node: 0,
            keyboard_device_node: 0,
            latest_anomaly_node: 0,
            pending_graph_anomalies: 0,
            graph_anomaly_count: 0,
            enqueued_events: 0,
            delivered_events: 0,
            abs_events: 0,
            relative_events: 0,
            button_events: 0,
            coalesced_abs_events: 0,
            queue_depth: 0,
            max_queue_depth: 0,
            severe_jump_count: 0,
            max_jump: 0,
            last_jump: 0,
            last_abs_x: 0,
            last_abs_y: 0,
            have_abs_sample: false,
            rendered_x: 0,
            rendered_y: 0,
            have_rendered_cursor: false,
            left_down: false,
            last_input_tick: 0,
            trail: [PointerTrailPoint {
                x: 0,
                y: 0,
                jump: 0,
            }; POINTER_TRAIL_POINTS],
            trail_head: 0,
            trail_count: 0,
            keyboard_events: 0,
            key_lines: [[0; KEY_HISTORY_COLS]; KEY_HISTORY_LINES],
            key_lens: [0; KEY_HISTORY_LINES],
            key_head: 0,
            key_count: 0,
        }
    }

    fn clear_runtime(&mut self) {
        self.enqueued_events = 0;
        self.delivered_events = 0;
        self.abs_events = 0;
        self.relative_events = 0;
        self.button_events = 0;
        self.coalesced_abs_events = 0;
        self.queue_depth = 0;
        self.max_queue_depth = 0;
        self.severe_jump_count = 0;
        self.max_jump = 0;
        self.last_jump = 0;
        self.last_abs_x = 0;
        self.last_abs_y = 0;
        self.have_abs_sample = false;
        self.rendered_x = 0;
        self.rendered_y = 0;
        self.have_rendered_cursor = false;
        self.left_down = false;
        self.last_input_tick = 0;
        self.trail = [PointerTrailPoint {
            x: 0,
            y: 0,
            jump: 0,
        }; POINTER_TRAIL_POINTS];
        self.trail_head = 0;
        self.trail_count = 0;
        self.keyboard_events = 0;
        self.key_lines = [[0; KEY_HISTORY_COLS]; KEY_HISTORY_LINES];
        self.key_lens = [0; KEY_HISTORY_LINES];
        self.key_head = 0;
        self.key_count = 0;
        self.pending_graph_anomalies = 0;
        self.latest_anomaly_node = 0;
        self.graph_anomaly_count = 0;
    }

    fn push_trail_point(&mut self, x: i32, y: i32, jump: u32) {
        let slot = self.trail_head;
        self.trail[slot] = PointerTrailPoint { x, y, jump };
        self.trail_head = (self.trail_head + 1) % POINTER_TRAIL_POINTS;
        self.trail_count = self.trail_count.min(POINTER_TRAIL_POINTS - 1) + 1;
    }

    fn push_key_line(&mut self, bytes: &[u8]) {
        let slot = self.key_head;
        let len = bytes.len().min(KEY_HISTORY_COLS);
        self.key_lines[slot].fill(0);
        self.key_lines[slot][..len].copy_from_slice(&bytes[..len]);
        self.key_lens[slot] = len as u8;
        self.key_head = (self.key_head + 1) % KEY_HISTORY_LINES;
        self.key_count = self.key_count.min(KEY_HISTORY_LINES - 1) + 1;
    }

    fn health(&self) -> InputHealth {
        if self.queue_depth > 8
            || self.last_jump >= severe_jump_threshold(self.display_width, self.display_height)
        {
            InputHealth::Unstable
        } else if self.last_jump >= warning_jump_threshold(self.display_width, self.display_height)
            || self.coalesced_abs_events != 0
        {
            InputHealth::Watching
        } else {
            InputHealth::Stable
        }
    }
}

static STATE: Mutex<DiagnosticsState> = Mutex::new(DiagnosticsState::new());
static EPOCH: AtomicU32 = AtomicU32::new(0);

pub fn init(display_width: u32, display_height: u32) {
    interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        state.display_width = display_width;
        state.display_height = display_height;
        state.clear_runtime();
    });
    bump_epoch();
}

pub fn set_pointer_online(online: bool) {
    interrupts::without_interrupts(|| {
        STATE.lock().online = online;
    });
    bump_epoch();
}

pub fn set_pointer_backend(bytes: &[u8]) {
    interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        let len = bytes.len().min(BACKEND_LABEL_BYTES);
        state.backend_len = len;
        state.backend.fill(0);
        state.backend[..len].copy_from_slice(&bytes[..len]);
    });
    bump_epoch();
}

pub fn record_pointer_event(event: PointerEvent, queue_depth: usize, coalesced: bool) {
    let tick = timer::ticks();
    {
        let mut state = STATE.lock();
        state.last_input_tick = tick;
        state.queue_depth = queue_depth;
        state.max_queue_depth = max(state.max_queue_depth, queue_depth);
        state.enqueued_events = state.enqueued_events.saturating_add(1);
        if coalesced {
            state.coalesced_abs_events = state.coalesced_abs_events.saturating_add(1);
        }
        match event {
            PointerEvent::Absolute { x, y } => {
                state.abs_events = state.abs_events.saturating_add(1);
                let mut jump = 0u32;
                if state.have_abs_sample {
                    let dx = x.abs_diff(state.last_abs_x);
                    let dy = y.abs_diff(state.last_abs_y);
                    jump = max(dx, dy);
                    state.last_jump = jump;
                    state.max_jump = max(state.max_jump, jump);
                    if jump >= severe_jump_threshold(state.display_width, state.display_height) {
                        state.severe_jump_count = state.severe_jump_count.saturating_add(1);
                        state.pending_graph_anomalies =
                            state.pending_graph_anomalies.saturating_add(1);
                    }
                }
                state.last_abs_x = x;
                state.last_abs_y = y;
                state.have_abs_sample = true;
                state.push_trail_point(x, y, jump);
            }
            PointerEvent::Move { .. } => {
                state.relative_events = state.relative_events.saturating_add(1);
            }
            PointerEvent::Button { button, pressed } => {
                state.button_events = state.button_events.saturating_add(1);
                if button == MouseButton::Left {
                    state.left_down = pressed;
                }
            }
        }
    }
    bump_epoch();
}

pub fn record_pointer_delivery(queue_depth: usize) {
    interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        state.delivered_events = state.delivered_events.saturating_add(1);
        state.queue_depth = queue_depth;
    });
    bump_epoch();
}

pub fn record_rendered_cursor(x: i32, y: i32, left_down: bool) {
    interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        state.rendered_x = x;
        state.rendered_y = y;
        state.left_down = left_down;
        state.have_rendered_cursor = true;
    });
}

pub fn record_key(key: KeyInput) {
    let mut line = [0u8; KEY_HISTORY_COLS];
    let len = format_key_line(&mut line, key);
    interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        state.keyboard_events = state.keyboard_events.saturating_add(1);
        state.push_key_line(&line[..len]);
    });
    bump_epoch();
}

pub fn clear_history() {
    interrupts::without_interrupts(|| {
        STATE.lock().clear_runtime();
    });
    bump_epoch();
}

pub fn epoch() -> u32 {
    EPOCH.load(Ordering::Relaxed)
}

pub fn snapshot() -> Snapshot {
    interrupts::without_interrupts(|| {
        let state = STATE.lock();
        let (render_lag_x, render_lag_y) = if state.have_abs_sample && state.have_rendered_cursor {
            (
                state.rendered_x.saturating_sub(state.last_abs_x),
                state.rendered_y.saturating_sub(state.last_abs_y),
            )
        } else {
            (0, 0)
        };
        Snapshot {
            online: state.online,
            backend: state.backend,
            backend_len: state.backend_len,
            display_width: state.display_width,
            display_height: state.display_height,
            pointer_device_node: state.pointer_device_node,
            keyboard_device_node: state.keyboard_device_node,
            latest_anomaly_node: state.latest_anomaly_node,
            graph_anomaly_count: state.graph_anomaly_count,
            enqueued_events: state.enqueued_events,
            delivered_events: state.delivered_events,
            abs_events: state.abs_events,
            relative_events: state.relative_events,
            button_events: state.button_events,
            coalesced_abs_events: state.coalesced_abs_events,
            queue_depth: state.queue_depth,
            max_queue_depth: state.max_queue_depth,
            severe_jump_count: state.severe_jump_count,
            max_jump: state.max_jump,
            last_jump: state.last_jump,
            last_abs_x: state.last_abs_x,
            last_abs_y: state.last_abs_y,
            have_abs_sample: state.have_abs_sample,
            rendered_x: state.rendered_x,
            rendered_y: state.rendered_y,
            have_rendered_cursor: state.have_rendered_cursor,
            left_down: state.left_down,
            render_lag_x,
            render_lag_y,
            last_input_tick: state.last_input_tick,
            trail: state.trail,
            trail_head: state.trail_head,
            trail_count: state.trail_count,
            keyboard_events: state.keyboard_events,
            key_lines: state.key_lines,
            key_lens: state.key_lens,
            key_head: state.key_head,
            key_count: state.key_count,
            health: state.health(),
        }
    })
}

pub fn bind_graph() {
    let need_pointer = interrupts::without_interrupts(|| STATE.lock().pointer_device_node == 0);
    if need_pointer && let Some(node_id) = arena::add_node(NodeKind::Device, 0, NODE_ID_KERNEL) {
        let _ = arena::add_edge(NODE_ID_KERNEL, node_id, EdgeKind::Owns, 0);
        let _ = arena::add_edge(NODE_ID_KERNEL, node_id, EdgeKind::Created, 0);
        interrupts::without_interrupts(|| {
            STATE.lock().pointer_device_node = node_id;
        });
    }

    let need_keyboard = interrupts::without_interrupts(|| STATE.lock().keyboard_device_node == 0);
    if need_keyboard && let Some(node_id) = arena::add_node(NodeKind::Device, 0, NODE_ID_KERNEL) {
        let _ = arena::add_edge(NODE_ID_KERNEL, node_id, EdgeKind::Owns, 0);
        let _ = arena::add_edge(NODE_ID_KERNEL, node_id, EdgeKind::Created, 0);
        interrupts::without_interrupts(|| {
            STATE.lock().keyboard_device_node = node_id;
        });
    }
}

pub fn poll_graph() {
    let pointer_node = interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        if state.pointer_device_node == 0 || state.pending_graph_anomalies == 0 {
            return None;
        }
        state.pending_graph_anomalies -= 1;
        Some(state.pointer_device_node)
    });

    let Some(pointer_node) = pointer_node else {
        return;
    };

    if let Some(anomaly_node) = arena::add_node(NodeKind::Anomaly, 0, NODE_ID_KERNEL) {
        let _ = arena::add_edge(pointer_node, anomaly_node, EdgeKind::Created, 0);
        let _ = arena::add_edge(NODE_ID_KERNEL, anomaly_node, EdgeKind::Created, 0);
        interrupts::without_interrupts(|| {
            let mut state = STATE.lock();
            state.latest_anomaly_node = anomaly_node;
            state.graph_anomaly_count = state.graph_anomaly_count.saturating_add(1);
        });
        bump_epoch();
    }
}

fn bump_epoch() {
    EPOCH.fetch_add(1, Ordering::Relaxed);
}

fn severe_jump_threshold(display_width: u32, display_height: u32) -> u32 {
    let base = max(display_width, display_height) / 12;
    base.max(64)
}

fn warning_jump_threshold(display_width: u32, display_height: u32) -> u32 {
    let base = max(display_width, display_height) / 20;
    base.max(24)
}

fn format_key_line(buf: &mut [u8; KEY_HISTORY_COLS], key: KeyInput) -> usize {
    let mut len = 0usize;
    match key {
        KeyInput::Char(byte) => {
            len += push_bytes(buf, len, b"char ");
            let glyph = match byte {
                0x20..=0x7e => byte,
                _ => b'?',
            };
            len += push_bytes(buf, len, &[glyph]);
        }
        KeyInput::Tab { shift: false } => len += push_bytes(buf, len, b"tab"),
        KeyInput::Tab { shift: true } => len += push_bytes(buf, len, b"shift+tab"),
        KeyInput::Left => len += push_bytes(buf, len, b"left"),
        KeyInput::Right => len += push_bytes(buf, len, b"right"),
        KeyInput::Up => len += push_bytes(buf, len, b"up"),
        KeyInput::Down => len += push_bytes(buf, len, b"down"),
        KeyInput::Backspace => len += push_bytes(buf, len, b"backspace"),
        KeyInput::Enter => len += push_bytes(buf, len, b"enter"),
    }
    len
}

fn push_bytes(buf: &mut [u8], start: usize, bytes: &[u8]) -> usize {
    if start >= buf.len() {
        return 0;
    }
    let len = bytes.len().min(buf.len() - start);
    buf[start..start + len].copy_from_slice(&bytes[..len]);
    len
}
