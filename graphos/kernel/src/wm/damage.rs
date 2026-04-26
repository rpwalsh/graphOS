// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Damage tracking for the event-driven desktop compositor.
//!
//! ## Model
//!
//! Every subsystem that changes visible state posts a `DamageRect` before
//! returning control to the main dispatch loop.  The compositor reads and
//! clears the accumulator exactly once per compose pass, then flushes only
//! the changed pixels to virtio-gpu.
//!
//! Two independent damage channels are tracked:
//!
//! - **Scene damage** — windows, panels, background, overlays.  Triggers a
//!   full blit-list rebuild for the damaged region.
//! - **Cursor damage** — the small sprite region around the cursor.  Can be
//!   satisfied by a narrow cursor-region blit without touching scene surfaces.
//!
//! Neither channel drives time.  Time is injected externally by the animation
//! timer source when springs are active (see `gpu_compositor::tick_springs_if_active`).

// ── DamageRect ────────────────────────────────────────────────────────────────

/// An axis-aligned screen-space damage rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl DamageRect {
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    #[inline]
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Compute the bounding box of `self` and `other`.
    pub fn union(self, other: DamageRect) -> DamageRect {
        if other.is_empty() {
            return self;
        }
        if self.is_empty() {
            return other;
        }
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.w as i32).max(other.x + other.w as i32);
        let y2 = (self.y + self.h as i32).max(other.y + other.h as i32);
        DamageRect {
            x: x1,
            y: y1,
            w: (x2 - x1).max(0) as u32,
            h: (y2 - y1).max(0) as u32,
        }
    }

    /// Clip to screen bounds `(0, 0, screen_w, screen_h)`.
    pub fn clip(self, screen_w: u32, screen_h: u32) -> DamageRect {
        let x = self.x.max(0);
        let y = self.y.max(0);
        let x2 = (self.x + self.w as i32).min(screen_w as i32);
        let y2 = (self.y + self.h as i32).min(screen_h as i32);
        if x2 <= x || y2 <= y {
            return DamageRect::EMPTY;
        }
        DamageRect {
            x,
            y,
            w: (x2 - x) as u32,
            h: (y2 - y) as u32,
        }
    }

    /// True iff `self` and `other` have a non-empty intersection.
    pub fn intersects(self, other: DamageRect) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        self.x < other.x + other.w as i32
            && self.x + self.w as i32 > other.x
            && self.y < other.y + other.h as i32
            && self.y + self.h as i32 > other.y
    }

    /// Expand rect by `margin` pixels on each side.
    pub fn expand(self, margin: u32) -> DamageRect {
        let m = margin as i32;
        DamageRect {
            x: self.x - m,
            y: self.y - m,
            w: self.w.saturating_add(margin * 2),
            h: self.h.saturating_add(margin * 2),
        }
    }
}

// ── DamageAccumulator ─────────────────────────────────────────────────────────

/// Per-desktop damage accumulator.
///
/// Posted damage is merged (union) into a single dirty rect per channel.
/// Call `take_scene()` / `take_cursor()` to consume and clear.
pub struct DamageAccumulator {
    scene: DamageRect,
    scene_dirty: bool,
    cursor: DamageRect,
    cursor_dirty: bool,
}

impl DamageAccumulator {
    pub const fn new() -> Self {
        Self {
            scene: DamageRect::EMPTY,
            scene_dirty: false,
            cursor: DamageRect::EMPTY,
            cursor_dirty: false,
        }
    }

    // ── Posting ───────────────────────────────────────────────────────────────

    /// Post an arbitrary scene damage rectangle.
    pub fn post_scene(&mut self, rect: DamageRect) {
        if rect.is_empty() {
            return;
        }
        self.scene = if self.scene_dirty {
            self.scene.union(rect)
        } else {
            rect
        };
        self.scene_dirty = true;
    }

    /// Post full-screen scene damage (wallpaper change, Z-order flip, etc.).
    pub fn post_full(&mut self, screen_w: u32, screen_h: u32) {
        self.post_scene(DamageRect::new(0, 0, screen_w, screen_h));
    }

    /// Post cursor damage (small sprite region).
    ///
    /// Both the old and new cursor positions should be posted so the previous
    /// cursor ghost is erased and the new position is drawn.
    pub fn post_cursor(&mut self, x: i32, y: i32, sprite_w: u32, sprite_h: u32) {
        let rect = DamageRect::new(x - 2, y - 2, sprite_w + 4, sprite_h + 4);
        self.cursor = if self.cursor_dirty {
            self.cursor.union(rect)
        } else {
            rect
        };
        self.cursor_dirty = true;
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// True if any damage is pending (scene or cursor).
    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.scene_dirty || self.cursor_dirty
    }

    pub fn has_scene_damage(&self) -> bool {
        self.scene_dirty
    }

    pub fn has_cursor_damage(&self) -> bool {
        self.cursor_dirty
    }

    // ── Consuming ─────────────────────────────────────────────────────────────

    /// Consume and return the accumulated scene damage.
    /// Returns `None` if no scene damage is pending.
    pub fn take_scene(&mut self) -> Option<DamageRect> {
        if !self.scene_dirty {
            return None;
        }
        self.scene_dirty = false;
        Some(self.scene)
    }

    /// Consume and return the accumulated cursor damage.
    /// Returns `None` if no cursor damage is pending.
    pub fn take_cursor(&mut self) -> Option<DamageRect> {
        if !self.cursor_dirty {
            return None;
        }
        self.cursor_dirty = false;
        Some(self.cursor)
    }

    /// Consume both channels, returning the union (for full-compose paths).
    pub fn take_all(&mut self) -> Option<DamageRect> {
        let scene = self.take_scene();
        let cursor = self.take_cursor();
        match (scene, cursor) {
            (Some(s), Some(c)) => Some(s.union(c)),
            (Some(s), None) => Some(s),
            (None, Some(c)) => Some(c),
            (None, None) => None,
        }
    }

    /// Clear all pending damage without compositing (e.g. on mode switch).
    pub fn clear(&mut self) {
        self.scene_dirty = false;
        self.cursor_dirty = false;
    }
}
