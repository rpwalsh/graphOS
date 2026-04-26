// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.w)
    }

    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.h)
    }

    pub const fn inset(self, pad: u16) -> Self {
        let doubled = pad.saturating_mul(2);
        Self {
            x: self.x.saturating_add(pad),
            y: self.y.saturating_add(pad),
            w: self.w.saturating_sub(doubled),
            h: self.h.saturating_sub(doubled),
        }
    }
}
