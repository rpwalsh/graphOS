// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Application window abstraction for ring-3 GraphOS applications.
//!
//! `Window` combines a shared pixel surface with an input channel and
//! provides a high-level `open / present / poll_event` API.

use crate::canvas::Canvas;
use crate::event::{self, Event};
use crate::sys;

/// A window backed by a shared surface.
///
/// Creating a `Window` allocates a shared surface via `SYS_SURFACE_CREATE`
/// and registers the window for input routing. Dropping a `Window` releases
/// the surface and unregisters the input channel.
pub struct Window {
    surface_id: u32,
    /// User virtual address of the pixel buffer.
    vaddr: u64,
    width: u32,
    height: u32,
    /// IPC channel used to receive input events.
    input_channel: u32,
    /// Scratch buffer for `poll_event`.
    recv_buf: [u8; 16],
}

impl Window {
    /// Open a window of the given pixel dimensions at position `(x, y)`.
    ///
    /// `input_channel` must be a channel already claimed by this task that
    /// the kernel can use to deliver keyboard and pointer events.
    ///
    /// This call does **not** steal keyboard focus.  Call `request_focus()`
    /// explicitly once the window is ready to receive keyboard input.
    ///
    /// Returns `None` if the surface could not be created (e.g. OOM or
    /// dimensions too large).
    pub fn open(width: u32, height: u32, x: i32, y: i32, input_channel: u32) -> Option<Self> {
        // Dimensions must fit in u16.
        if width > 0xFFFF || height > 0xFFFF || input_channel == 0 {
            return None;
        }

        let (surface_id, vaddr) = sys::surface_create(width as u16, height as u16)?;
        // Clamp to the wire format used by SYS_INPUT_REGISTER_WINDOW.
        let reg_x = x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let reg_y = y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        // Register hit-test rectangle at the caller-supplied position.
        if !sys::input_register_window(reg_x, reg_y, width as u16, height as u16, input_channel) {
            let _ = sys::surface_destroy(surface_id);
            return None;
        }

        // Subscribe the input channel to display-system frame-tick broadcasts
        // so the app can pace rendering on FrameTick events instead of spinning.
        sys::subscribe_frame_tick(input_channel);

        Some(Self {
            surface_id,
            vaddr,
            width,
            height,
            input_channel,
            recv_buf: [0u8; 16],
        })
    }

    /// Request keyboard focus for this window's input channel.
    ///
    /// Should be called after the first frame is drawn so the compositor can
    /// deliver key events to this window.  The caller is responsible for
    /// deciding when to request focus (e.g. not on every event).
    pub fn request_focus(&self) {
        sys::input_set_focus(self.input_channel);
    }

    /// Borrow the pixel buffer as a `Canvas`.
    ///
    /// # Safety
    /// The surface pixel buffer is shared with the kernel compositor.
    /// The caller must not mutate pixels while the compositor is reading
    /// (in practice the compositor reads on a per-frame cadence and double
    /// buffering is the caller's responsibility for tear-free rendering).
    pub fn canvas(&mut self) -> Canvas<'_> {
        // SAFETY: vaddr is a valid user mapping of the surface pixel buffer
        // created by the kernel. The surface is owned by this Window which
        // holds &mut self, so no other alias exists within this task.
        unsafe { Canvas::from_raw(self.vaddr as *mut u32, self.width, self.height) }
    }

    /// Tell the compositor this frame is ready to display.
    pub fn present(&self) -> bool {
        sys::surface_present(self.surface_id)
    }

    /// Poll for the next input event (non-blocking).
    ///
    /// Returns `Event::None` if no events are pending.
    pub fn poll_event(&mut self) -> Event {
        let raw = sys::channel_recv_nonblock(self.input_channel, &mut self.recv_buf);
        event::decode_event(raw, &self.recv_buf)
    }

    /// Return the surface ID.
    pub fn surface_id(&self) -> u32 {
        self.surface_id
    }

    /// Return the pixel buffer user virtual address.
    pub fn pixel_addr(&self) -> u64 {
        self.vaddr
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        sys::input_unregister_window();
        sys::input_set_focus(0);
        // The kernel does NOT automatically unmap VMAs on surface_destroy;
        // the mapping remains valid until the task exits (kernel cleans up
        // VMAs on task exit) or until the caller calls munmap explicitly.
        // Calling surface_destroy here is safe because the frames are only
        // returned to the allocator after the kernel removes the surface
        // record — the VMA remains in the page-tables but accesses to it
        // after destroy are the caller's bug (Window is being dropped).
        sys::surface_destroy(self.surface_id);
    }
}
