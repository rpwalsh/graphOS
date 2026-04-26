// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Event decoding for ring-3 GraphOS applications.
//!
//! Decodes raw IPC payloads received on an input channel into typed `Event`s.

/// Input event delivered to a ring-3 application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A key was pressed or released.
    Key {
        /// `true` = key-down, `false` = key-up.
        pressed: bool,
        /// ASCII character (0 for non-printable keys).
        ascii: u8,
        /// HID usage code (0 when routing via ASCII path).
        hid_usage: u8,
    },
    /// Pointer moved to an absolute screen position.
    PointerMove {
        /// Absolute x in screen pixels.
        x: i16,
        /// Absolute y in screen pixels.
        y: i16,
        /// Button mask: bit 0 = left, bit 1 = right, bit 2 = middle.
        buttons: u8,
    },
    /// Display-system frame-tick: render a new frame. `now_ms` is the
    /// desktop monotonic tick counter (1 tick ≈ 1 ms).
    FrameTick {
        /// Display monotonic tick count.
        now_ms: u64,
    },
    /// No event was available (non-blocking poll returned empty).
    None,
}

/// IPC message tag for keyboard events (matches kernel `INPUT_TAG_KEY`).
pub const TAG_KEY: u8 = 0x60;

/// IPC message tag for pointer events (matches kernel `INPUT_TAG_POINTER`).
pub const TAG_POINTER: u8 = 0x61;

/// IPC message tag for display-system frame-tick broadcasts.
pub const TAG_FRAME_TICK: u8 = 0x65;

/// Decode an IPC receive result into an `Event`.
///
/// `raw` is the packed u64 returned by `SYS_CHANNEL_RECV`.  
/// `buf` must be at least 8 bytes and contain the raw payload.
///
/// Returns `Event::None` if `raw` is 0 or `u64::MAX` (no message / error).
pub fn decode_event(raw: u64, buf: &[u8]) -> Event {
    if raw == 0 || raw == u64::MAX {
        return Event::None;
    }

    let payload_len = (raw & 0xFFFF) as usize;
    let tag = ((raw >> 16) & 0xFF) as u8;

    match tag {
        TAG_KEY if payload_len >= 3 => {
            let pressed = buf[0] != 0;
            let ascii = buf[2];
            let hid_usage = buf[1];
            Event::Key {
                pressed,
                ascii,
                hid_usage,
            }
        }
        TAG_POINTER if payload_len >= 5 => {
            let x = i16::from_le_bytes([buf[0], buf[1]]);
            let y = i16::from_le_bytes([buf[2], buf[3]]);
            let buttons = buf[4];
            Event::PointerMove { x, y, buttons }
        }
        TAG_FRAME_TICK if payload_len >= 8 => {
            let now_ms = u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            Event::FrameTick { now_ms }
        }
        _ => Event::None,
    }
}
