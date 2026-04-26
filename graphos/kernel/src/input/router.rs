// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Kernel input router.
//!
//! Routes keyboard and pointer events from kernel ISR handlers to ring-3
//! tasks via IPC channels.
//!
//! ## Focus model
//! One task at a time holds keyboard focus; all keyboard events are delivered
//! only to the focused task. Pointer events are broadcast to the compositor
//! (channel 7) and also delivered to the task whose registered window region
//! contains the pointer, if any.
//!
//! ## IPC wire format
//! All input messages are delivered as tagged IPC payloads.
//!
//! ### Keyboard message (tag `INPUT_TAG_KEY = 0x60`)
//! Byte 0: `1` = key-down, `0` = key-up  
//! Byte 1: USB HID usage page (0 = ASCII shortcut path)  
//! Byte 2: USB HID usage / ASCII codepoint  
//! Bytes 3–7: reserved (zeroed)
//!
//! ### Pointer message (tag `INPUT_TAG_POINTER = 0x61`)
//! Bytes 0–1: absolute x (i16, little-endian)  
//! Bytes 2–3: absolute y (i16, little-endian)  
//! Byte 4:    button mask (bit 0 = left, bit 1 = right, bit 2 = middle)  
//! Bytes 5–7: reserved (zeroed)

use crate::arch::interrupts;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// IPC message tag for keyboard events sent to ring-3 tasks.
pub const INPUT_TAG_KEY: u8 = 0x60;

/// IPC message tag for pointer events sent to ring-3 tasks.
pub const INPUT_TAG_POINTER: u8 = 0x61;

/// Sentinel: no task currently holds focus.
const NO_FOCUS: usize = usize::MAX;

/// Maximum simultaneously registered windows.
const MAX_WINDOWS: usize = 16;

/// A registered window rectangle for hit-testing.
#[derive(Clone, Copy)]
struct WindowEntry {
    /// Task table index.
    task_index: usize,
    /// IPC channel to deliver input to.
    channel: u32,
    /// Window bounds for pointer hit-testing.
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    /// Whether this slot is occupied.
    active: bool,
}

impl WindowEntry {
    const EMPTY: Self = Self {
        task_index: 0,
        channel: 0,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        active: false,
    };

    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x as i32
            && py >= self.y as i32
            && px < (self.x as i32 + self.w as i32)
            && py < (self.y as i32 + self.h as i32)
    }
}

struct InputRouterState {
    /// Task table index of the focused task, or `NO_FOCUS`.
    focused_task: usize,
    /// IPC channel used to deliver input to the focused task.
    focused_channel: u32,
    /// Dedicated compositor event channel for raw seat/input delivery.
    compositor_channel: u32,
    /// Registered windows for pointer hit-testing.
    windows: [WindowEntry; MAX_WINDOWS],
    /// Current pointer position (absolute, pixels).
    pointer_x: i32,
    pointer_y: i32,
    /// Current button mask (bit 0 = left).
    pointer_buttons: u8,
}

impl InputRouterState {
    const fn new() -> Self {
        Self {
            focused_task: NO_FOCUS,
            focused_channel: 0,
            compositor_channel: 0,
            windows: [WindowEntry::EMPTY; MAX_WINDOWS],
            pointer_x: 0,
            pointer_y: 0,
            pointer_buttons: 0,
        }
    }
}

static ROUTER: Mutex<InputRouterState> = Mutex::new(InputRouterState::new());
static POINTER_EVENTS_TOTAL: AtomicUsize = AtomicUsize::new(0);
static POINTER_EVENTS_TO_COMPOSITOR: AtomicUsize = AtomicUsize::new(0);
static POINTER_EVENTS_TO_WINDOW: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
pub struct PointerRouteStats {
    pub total: usize,
    pub to_compositor: usize,
    pub to_window: usize,
}

pub fn pointer_route_stats_snapshot() -> PointerRouteStats {
    PointerRouteStats {
        total: POINTER_EVENTS_TOTAL.load(Ordering::Relaxed),
        to_compositor: POINTER_EVENTS_TO_COMPOSITOR.load(Ordering::Relaxed),
        to_window: POINTER_EVENTS_TO_WINDOW.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Set the keyboard focus to `task_index`, delivering events on `channel`.
///
/// Pass `task_index = usize::MAX` to release focus.
pub fn set_focus(task_index: usize, channel: u32) {
    interrupts::without_interrupts(|| {
        let mut r = ROUTER.lock();
        r.focused_task = task_index;
        r.focused_channel = channel;
    });
}

/// Register a window region for pointer hit-testing.
///
/// When a pointer event lands inside this rectangle the event is also sent
/// to `channel`. Returns `false` if the window table is full.
pub fn register_window(task_index: usize, channel: u32, x: i16, y: i16, w: u16, h: u16) -> bool {
    interrupts::without_interrupts(|| {
        let mut r = ROUTER.lock();
        // Update existing entry if task already registered.
        for slot in r.windows.iter_mut() {
            if slot.active && slot.task_index == task_index {
                slot.channel = channel;
                slot.x = x;
                slot.y = y;
                slot.w = w;
                slot.h = h;
                return true;
            }
        }
        // Allocate a new slot.
        for slot in r.windows.iter_mut() {
            if !slot.active {
                *slot = WindowEntry {
                    task_index,
                    channel,
                    x,
                    y,
                    w,
                    h,
                    active: true,
                };
                return true;
            }
        }
        false
    })
}

pub fn set_compositor_channel(channel: u32) {
    interrupts::without_interrupts(|| {
        ROUTER.lock().compositor_channel = channel;
    });
}

/// Unregister a window region for `task_index`.
pub fn unregister_window(task_index: usize) {
    interrupts::without_interrupts(|| {
        let mut r = ROUTER.lock();
        for slot in r.windows.iter_mut() {
            if slot.active && slot.task_index == task_index {
                *slot = WindowEntry::EMPTY;
            }
        }
    });
}

/// Route a keyboard event.
///
/// `pressed` — true for key-down, false for key-up.  
/// `ascii` — ASCII character code (0 for non-printable keys).  
/// `hid_usage` — USB HID usage code (0 when routing via ASCII path).
///
/// Delivers a `INPUT_TAG_KEY` IPC message to the focused task's channel and
/// also forwards events to the compositor when available.
pub fn route_key_event(pressed: bool, ascii: u8, hid_usage: u8) {
    let (focused_task, channel, compositor_channel) = interrupts::without_interrupts(|| {
        let r = ROUTER.lock();
        (r.focused_task, r.focused_channel, r.compositor_channel)
    });

    let payload: [u8; 8] = [
        pressed as u8,
        0, // HID page (0 = ASCII path)
        if ascii != 0 { ascii } else { hid_usage },
        0,
        0,
        0,
        0,
        0,
    ];

    if let Some(tag) = crate::ipc::msg::MsgTag::from_u8(INPUT_TAG_KEY) {
        let compositor_channel = if compositor_channel != 0 {
            Some(compositor_channel)
        } else {
            crate::registry::channel_alias_by_name(b"compositor")
        };
        if let Some(compositor_channel) = compositor_channel {
            let comp_uuid = crate::ipc::channel::uuid_for_alias(compositor_channel);
            if crate::ipc::channel::is_active(comp_uuid) && compositor_channel != channel {
                crate::ipc::channel_send_tagged(comp_uuid, tag, &payload);
            }
        }

        if focused_task != NO_FOCUS && channel != 0 {
            let ch_uuid = crate::ipc::channel::uuid_for_alias(channel);
            crate::ipc::channel_send_tagged(ch_uuid, tag, &payload);
        }
    }
}

/// Route a pointer event.
///
/// Updates the internal pointer state, then sends a `INPUT_TAG_POINTER` IPC
/// message to:
/// 1. The compositor service inbox — always when present in the registry.
/// 2. The task whose registered window contains the new pointer position.
///
/// Hit-testing is O(MAX_WINDOWS) and runs under interrupts disabled.
pub fn route_pointer_event(abs_x: i32, abs_y: i32, buttons: u8) {
    POINTER_EVENTS_TOTAL.fetch_add(1, Ordering::Relaxed);

    // Clamp to non-negative screen coordinates.
    let abs_x = abs_x.max(0).min(i16::MAX as i32);
    let abs_y = abs_y.max(0).min(i16::MAX as i32);

    let payload: [u8; 8] = {
        let x_bytes = (abs_x as i16).to_le_bytes();
        let y_bytes = (abs_y as i16).to_le_bytes();
        [
            x_bytes[0], x_bytes[1], y_bytes[0], y_bytes[1], buttons, 0, 0, 0,
        ]
    };

    // Find the window under the pointer (if any).
    // Prefer the focused task's window first, then fall back to most-recently
    // registered matching window so top-most apps receive pointer events.
    let (hit_channel, compositor_channel) = interrupts::without_interrupts(|| {
        let mut r = ROUTER.lock();
        r.pointer_x = abs_x;
        r.pointer_y = abs_y;
        r.pointer_buttons = buttons;
        let compositor_channel = r.compositor_channel;

        if r.focused_task != NO_FOCUS {
            for slot in r.windows.iter() {
                if slot.active && slot.task_index == r.focused_task && slot.contains(abs_x, abs_y) {
                    return (slot.channel, compositor_channel);
                }
            }
        }

        for slot in r.windows.iter().rev() {
            if slot.active && slot.contains(abs_x, abs_y) {
                return (slot.channel, compositor_channel);
            }
        }
        (0, compositor_channel)
    });

    if let Some(tag) = crate::ipc::msg::MsgTag::from_u8(INPUT_TAG_POINTER) {
        let compositor_channel = if compositor_channel != 0 {
            Some(compositor_channel)
        } else {
            crate::registry::channel_alias_by_name(b"compositor")
        };
        if let Some(channel) = compositor_channel {
            let ch_uuid = crate::ipc::channel::uuid_for_alias(channel);
            crate::ipc::channel_send_tagged(ch_uuid, tag, &payload);
            POINTER_EVENTS_TO_COMPOSITOR.fetch_add(1, Ordering::Relaxed);
        }

        // Also deliver to the hit window if different from compositor.
        if hit_channel != 0 && Some(hit_channel) != compositor_channel {
            let hit_uuid = crate::ipc::channel::uuid_for_alias(hit_channel);
            crate::ipc::channel_send_tagged(hit_uuid, tag, &payload);
            POINTER_EVENTS_TO_WINDOW.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Query the current pointer position and button state.
pub fn pointer_state() -> (i32, i32, u8) {
    interrupts::without_interrupts(|| {
        let r = ROUTER.lock();
        (r.pointer_x, r.pointer_y, r.pointer_buttons)
    })
}

/// Query the currently focused task index. Returns `usize::MAX` if none.
pub fn focused_task() -> usize {
    interrupts::without_interrupts(|| ROUTER.lock().focused_task)
}
