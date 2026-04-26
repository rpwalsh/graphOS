// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! SwapChain — manages the display back buffer and present logic.

use crate::command::ResourceId;

/// The compositor's display swap chain.
///
/// Wraps the kernel-allocated back buffer resource. `present()` submits a
/// `GpuCmd::Present` which triggers the current scanout flush path now and can
/// map to a native display flip once the hardware backend exists.
pub struct SwapChain {
    pub(crate) back_buffer: ResourceId,
    pub width: u32,
    pub height: u32,
}

impl SwapChain {
    /// The kernel resource ID for the single back buffer.
    pub fn back_buffer(&self) -> ResourceId {
        self.back_buffer
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// Screen-space rect covering the full display.
    pub fn full_rect(&self) -> crate::command::Rect {
        crate::command::Rect::screen(self.width, self.height)
    }
}
