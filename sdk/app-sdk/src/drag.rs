// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Drag-resize helpers for ring-3 GraphOS application windows.
//!
//! GraphOS uses **server-side decorations**: the kernel compositor draws the
//! title bar and resize handles.  Ring-3 apps interact via the `surface_move`
//! and `surface_resize` syscalls wrapped here.
//!
//! Typical usage pattern:
//! 1. Receive a `PointerMove` event with `buttons & 1` while the pointer is
//!    inside the title-bar strip (y < 18 from the window top).
//! 2. Call `DragState::begin_move(surface_id, pointer_x, pointer_y, win_x, win_y)`.
//! 3. On each subsequent `PointerMove` with button held, call `DragState::update(ptr_x, ptr_y)`.
//! 4. On button-release, call `DragState::finish()`.

use crate::sys;

/// Tracks an in-progress window move or resize drag.
pub struct DragState {
    surface_id: u32,
    /// Pointer offset from window origin at the moment the drag started.
    anchor_dx: i32,
    anchor_dy: i32,
    /// Current window position.
    win_x: i32,
    win_y: i32,
    active: bool,
}

impl DragState {
    /// Create an idle drag state.
    pub const fn idle() -> Self {
        Self {
            surface_id: 0,
            anchor_dx: 0,
            anchor_dy: 0,
            win_x: 0,
            win_y: 0,
            active: false,
        }
    }

    /// Begin a window-move drag.
    ///
    /// `ptr_x`, `ptr_y` — pointer position at drag start (screen coords).
    /// `win_x`, `win_y` — current window position (screen coords).
    pub fn begin_move(&mut self, surface_id: u32, ptr_x: i32, ptr_y: i32, win_x: i32, win_y: i32) {
        self.surface_id = surface_id;
        self.anchor_dx = ptr_x - win_x;
        self.anchor_dy = ptr_y - win_y;
        self.win_x = win_x;
        self.win_y = win_y;
        self.active = true;
    }

    /// Update the window position to follow the pointer.
    ///
    /// Returns `true` if the syscall succeeded.
    pub fn update(&mut self, ptr_x: i32, ptr_y: i32) -> bool {
        if !self.active {
            return false;
        }
        let new_x = ptr_x - self.anchor_dx;
        let new_y = ptr_y - self.anchor_dy;
        self.win_x = new_x;
        self.win_y = new_y;
        sys::surface_move(self.surface_id, new_x, new_y)
    }

    /// End the drag.
    pub fn finish(&mut self) {
        self.active = false;
    }

    /// Whether a drag is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }
}
