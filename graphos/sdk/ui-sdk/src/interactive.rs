// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Input-aware widget state for ring3 surfaces.
//!
//! All types are `Copy` and heap-free. Each carries a minimal state
//! struct plus an `update()` method that accepts an `Event` and returns
//! whether a redraw is needed.

use graphos_app_sdk::event::Event;

// ---------------------------------------------------------------------------
// Focus ring
// ---------------------------------------------------------------------------

/// Tracks keyboard focus across an ordered list of focusable elements.
///
/// `capacity` controls the slot count (statically determined at construction
/// time from the length of your widget array). Focus wraps at both ends.
#[derive(Clone, Copy, Debug)]
pub struct FocusRing {
    /// Currently focused slot index.
    pub focused: usize,
    /// Total number of focusable slots.
    pub count: usize,
}

impl FocusRing {
    /// Construct with a given number of focusable slots.
    pub const fn new(count: usize) -> Self {
        Self { focused: 0, count }
    }

    /// Advance focus by one (wraps). Returns `true` if focus changed.
    pub fn next(&mut self) -> bool {
        if self.count == 0 {
            return false;
        }
        self.focused = (self.focused + 1) % self.count;
        true
    }

    /// Retreat focus by one (wraps). Returns `true` if focus changed.
    pub fn prev(&mut self) -> bool {
        if self.count == 0 {
            return false;
        }
        self.focused = if self.focused == 0 {
            self.count - 1
        } else {
            self.focused - 1
        };
        true
    }

    /// Returns `true` if slot `i` currently holds focus.
    pub fn is_focused(&self, i: usize) -> bool {
        self.focused == i
    }

    /// Handle Tab and arrow keys; return whether a redraw is needed.
    /// HID usage 0x4B = PageUp / Up, 0x4E = PageDn / Down (approximate).
    /// Tab (ascii 0x09) advances; no shift-tab discrimination without modifier state.
    pub fn update(&mut self, event: &Event) -> bool {
        match event {
            Event::Key {
                pressed: true,
                ascii: b'\t',
                ..
            } => self.next(),
            // HID Up arrow = 0x52, Down arrow = 0x51
            Event::Key {
                pressed: true,
                hid_usage: 0x52,
                ..
            } => self.prev(),
            Event::Key {
                pressed: true,
                hid_usage: 0x51,
                ..
            } => self.next(),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Button state
// ---------------------------------------------------------------------------

/// Tracks hover, press, and focus state for a single button.
#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonState {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
}

impl ButtonState {
    pub const fn new() -> Self {
        Self {
            hovered: false,
            pressed: false,
            focused: false,
        }
    }

    /// Update from a pointer event given the button's bounding box.
    /// Returns `true` if any state changed.
    pub fn update_pointer(&mut self, event: &Event, bx: i16, by: i16, bw: i16, bh: i16) -> bool {
        match *event {
            Event::PointerMove { x, y, buttons } => {
                let hit = x >= bx && x < bx + bw && y >= by && y < by + bh;
                let old_hov = self.hovered;
                let old_press = self.pressed;
                self.hovered = hit;
                self.pressed = hit && (buttons & 1 != 0);
                (self.hovered != old_hov) || (self.pressed != old_press)
            }
            _ => false,
        }
    }

    /// Return `true` if a key activation (Enter/Space) fires while focused.
    pub fn update_key(&self, event: &Event) -> bool {
        matches!(
            event,
            Event::Key {
                pressed: true,
                ascii: b' ' | b'\r',
                ..
            }
        )
    }
}

// ---------------------------------------------------------------------------
// Toggle (checkbox / switch)
// ---------------------------------------------------------------------------

/// A boolean toggle that can be flipped via pointer click or keyboard.
#[derive(Clone, Copy, Debug)]
pub struct ToggleState {
    pub checked: bool,
    pub hovered: bool,
    pub focused: bool,
}

impl ToggleState {
    pub const fn new(initial: bool) -> Self {
        Self {
            checked: initial,
            hovered: false,
            focused: false,
        }
    }

    /// Update from pointer event within bounding box. Returns `true` on toggle flip.
    pub fn update_pointer(&mut self, event: &Event, bx: i16, by: i16, bw: i16, bh: i16) -> bool {
        match *event {
            Event::PointerMove { x, y, buttons } => {
                let hit = x >= bx && x < bx + bw && y >= by && y < by + bh;
                let was_pressed = self.hovered && (buttons & 1 != 0);
                let now_pressed = hit && (buttons & 1 != 0);
                let flip = was_pressed && !now_pressed && hit;
                self.hovered = hit;
                if flip {
                    self.checked = !self.checked;
                }
                flip
            }
            _ => false,
        }
    }

    /// Flip on Space/Enter when focused.
    pub fn update_key(&mut self, event: &Event) -> bool {
        if matches!(
            event,
            Event::Key {
                pressed: true,
                ascii: b' ' | b'\r',
                ..
            }
        ) {
            self.checked = !self.checked;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// List / menu selection
// ---------------------------------------------------------------------------

/// Scrollable list selection state. `count` is the total number of items.
/// Scroll offset and visual page size are left to the caller to maintain;
/// this type tracks logical selection only.
#[derive(Clone, Copy, Debug)]
pub struct ListState {
    /// Currently highlighted item.
    pub selected: usize,
    /// Total number of items.
    pub count: usize,
}

impl ListState {
    pub const fn new(count: usize) -> Self {
        Self { selected: 0, count }
    }

    /// Move selection up. Returns `true` if changed.
    pub fn up(&mut self) -> bool {
        if self.selected > 0 {
            self.selected -= 1;
            true
        } else {
            false
        }
    }

    /// Move selection down. Returns `true` if changed.
    pub fn down(&mut self) -> bool {
        if self.count > 0 && self.selected + 1 < self.count {
            self.selected += 1;
            true
        } else {
            false
        }
    }

    /// Handle Up/Down arrow keys and PageUp/PageDown. Returns redraw needed.
    /// HID usages: Up=0x52, Down=0x51, PageUp=0x4B, PageDn=0x4E.
    pub fn update_key(&mut self, event: &Event, page_size: usize) -> bool {
        match event {
            Event::Key {
                pressed: true,
                hid_usage: 0x52,
                ..
            } => self.up(),
            Event::Key {
                pressed: true,
                hid_usage: 0x51,
                ..
            } => self.down(),
            Event::Key {
                pressed: true,
                hid_usage: 0x4B,
                ..
            } => {
                let steps = page_size.min(self.selected);
                self.selected -= steps;
                steps > 0
            }
            Event::Key {
                pressed: true,
                hid_usage: 0x4E,
                ..
            } => {
                let end = self.count.saturating_sub(1);
                let steps = page_size.min(end.saturating_sub(self.selected));
                self.selected += steps;
                steps > 0
            }
            _ => false,
        }
    }

    /// Click hit-test: given list origin `ly`, item height `item_h`, determine
    /// which item was clicked. Returns `true` if selection changed.
    pub fn update_pointer(
        &mut self,
        event: &Event,
        ly: i16,
        item_h: i16,
        visible_items: usize,
    ) -> bool {
        match *event {
            Event::PointerMove { y, buttons: 1, .. } => {
                let rel = (y - ly).max(0);
                let idx = (rel / item_h.max(1)) as usize;
                let idx = idx
                    .min(visible_items.saturating_sub(1))
                    .min(self.count.saturating_sub(1));
                if self.selected != idx {
                    self.selected = idx;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Dialog/modal state
// ---------------------------------------------------------------------------

/// Two-option dialog (primary vs. cancel) with keyboard and pointer support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogChoice {
    None,
    Primary,
    Cancel,
}

#[derive(Clone, Copy, Debug)]
pub struct DialogState {
    /// Which button is currently highlighted (`false` = primary, `true` = cancel).
    pub cancel_focused: bool,
    pub dismissed: bool,
}

impl DialogState {
    pub const fn new() -> Self {
        Self {
            cancel_focused: false,
            dismissed: false,
        }
    }

    /// Handle keyboard navigation within the dialog.
    /// Returns the user's choice when a button is activated, otherwise `None`.
    pub fn update_key(&mut self, event: &Event) -> DialogChoice {
        match event {
            Event::Key {
                pressed: true,
                ascii: b'\t',
                ..
            } => {
                self.cancel_focused = !self.cancel_focused;
                DialogChoice::None
            }
            Event::Key {
                pressed: true,
                ascii: 0x1B,
                ..
            } => {
                // Escape cancels
                self.dismissed = true;
                DialogChoice::Cancel
            }
            Event::Key {
                pressed: true,
                ascii: b'\r' | b' ',
                ..
            } => {
                self.dismissed = true;
                if self.cancel_focused {
                    DialogChoice::Cancel
                } else {
                    DialogChoice::Primary
                }
            }
            _ => DialogChoice::None,
        }
    }
}
