// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal UI substrate (GraphOS GDK-equivalent).
//!
//! This module intentionally stays small and hard-edged. It defines only
//! platform interaction primitives required by higher-level toolkit widgets.

use graphos_app_sdk::event::Event;

use crate::geom::Rect;

/// Frame sequence number.
pub type FrameId = u64;

/// Unique identifier for a composited surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceId(pub u32);

/// Normalized pointer buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerButtons {
    /// Left button state.
    pub left: bool,
    /// Right button state.
    pub right: bool,
    /// Middle button state.
    pub middle: bool,
}

impl PointerButtons {
    /// Decode from the kernel/app-sdk button bitmask.
    pub const fn from_mask(mask: u8) -> Self {
        Self {
            left: (mask & 0b001) != 0,
            right: (mask & 0b010) != 0,
            middle: (mask & 0b100) != 0,
        }
    }
}

/// Pointer event payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    /// Absolute screen-space X coordinate.
    pub x: i16,
    /// Absolute screen-space Y coordinate.
    pub y: i16,
    /// Current button state.
    pub buttons: PointerButtons,
}

/// Keyboard event payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Whether this is a key-down event.
    pub pressed: bool,
    /// ASCII code (0 for non-printable).
    pub ascii: u8,
    /// HID usage code.
    pub hid_usage: u8,
}

/// Toolkit-facing normalized event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiEvent {
    /// Keyboard event.
    Key(KeyEvent),
    /// Pointer event.
    Pointer(PointerEvent),
    /// Frame-tick callback.
    FrameTick { frame_id: FrameId, now_ms: u64 },
    /// No event.
    None,
}

impl UiEvent {
    /// Convert from `graphos_app_sdk::event::Event`.
    pub const fn from_app_event(event: Event) -> Self {
        match event {
            Event::Key {
                pressed,
                ascii,
                hid_usage,
            } => Self::Key(KeyEvent {
                pressed,
                ascii,
                hid_usage,
            }),
            Event::PointerMove { x, y, buttons } => Self::Pointer(PointerEvent {
                x,
                y,
                buttons: PointerButtons::from_mask(buttons),
            }),
            Event::FrameTick { now_ms } => Self::FrameTick {
                frame_id: 0,
                now_ms,
            },
            Event::None => Self::None,
        }
    }
}

/// Explicit frame pacing helper.
#[derive(Clone, Copy, Debug)]
pub struct FrameClock {
    target_frame_ms: u32,
    last_frame_ms: u64,
    frame_id: FrameId,
}

impl FrameClock {
    /// Create a frame clock targeting `fps` frames per second.
    pub const fn new(fps: u32) -> Self {
        let frame_ms = if fps == 0 {
            16
        } else {
            if (1000 / fps) == 0 { 1 } else { 1000 / fps }
        };
        Self {
            target_frame_ms: frame_ms,
            last_frame_ms: 0,
            frame_id: 0,
        }
    }

    /// Check whether a new frame should be emitted at `now_ms`.
    pub fn tick_due(&mut self, now_ms: u64) -> Option<UiEvent> {
        if now_ms.saturating_sub(self.last_frame_ms) < self.target_frame_ms as u64 {
            return None;
        }
        self.last_frame_ms = now_ms;
        self.frame_id = self.frame_id.saturating_add(1);
        Some(UiEvent::FrameTick {
            frame_id: self.frame_id,
            now_ms,
        })
    }
}

/// Damage tracking primitive with explicit invalidation.
#[derive(Clone, Copy, Debug)]
pub struct DamageTracker {
    dirty: bool,
    region: Rect,
}

impl DamageTracker {
    /// Construct an empty tracker.
    pub const fn new() -> Self {
        Self {
            dirty: false,
            region: Rect::new(0, 0, 0, 0),
        }
    }

    /// Mark the given rectangle as dirty.
    pub fn invalidate(&mut self, rect: Rect) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        if !self.dirty {
            self.region = rect;
            self.dirty = true;
            return;
        }

        let left = self.region.x.min(rect.x);
        let top = self.region.y.min(rect.y);
        let right = (self.region.x + self.region.w as i32).max(rect.x + rect.w as i32);
        let bottom = (self.region.y + self.region.h as i32).max(rect.y + rect.h as i32);
        self.region = Rect::new(
            left,
            top,
            right.saturating_sub(left) as u32,
            bottom.saturating_sub(top) as u32,
        );
    }

    /// Consume and return current damage region.
    pub fn take(&mut self) -> Option<Rect> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        Some(self.region)
    }
}

/// Surface/compositor boundary contract.
///
/// Implemented by application runtime glue that owns the present path.
pub trait Substrate {
    /// Surface identifier represented by this substrate.
    fn surface_id(&self) -> SurfaceId;

    /// Surface bounds in pixels.
    fn bounds(&self) -> Rect;

    /// Submit explicit damage and request composition.
    fn submit_damage(&mut self, damage: Rect) -> bool;

    /// Submit a full frame.
    fn present(&mut self) -> bool;
}
