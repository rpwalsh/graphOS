// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Geometry helpers for deterministic ring3 layouts.

/// A pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// Left position.
    pub x: i32,
    /// Top position.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// Create a new rectangle.
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// Return an inset version of the rectangle.
    pub const fn inset(self, pad: u32) -> Self {
        let double = pad.saturating_mul(2);
        Self {
            x: self.x + pad as i32,
            y: self.y + pad as i32,
            w: self.w.saturating_sub(double),
            h: self.h.saturating_sub(double),
        }
    }

    /// Split into top and remaining bottom region.
    pub const fn split_top(self, top_h: u32) -> (Self, Self) {
        let top_h = if top_h < self.h { top_h } else { self.h };
        (
            Self::new(self.x, self.y, self.w, top_h),
            Self::new(
                self.x,
                self.y + top_h as i32,
                self.w,
                self.h.saturating_sub(top_h),
            ),
        )
    }

    /// Split into left and remaining right region.
    pub const fn split_left(self, left_w: u32) -> (Self, Self) {
        let left_w = if left_w < self.w { left_w } else { self.w };
        (
            Self::new(self.x, self.y, left_w, self.h),
            Self::new(
                self.x + left_w as i32,
                self.y,
                self.w.saturating_sub(left_w),
                self.h,
            ),
        )
    }
}
