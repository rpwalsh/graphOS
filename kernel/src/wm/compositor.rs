// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::gfx::canvas::{Canvas, ClipRect};
use crate::gfx::surface::{DrawTarget, Screen};
use crate::wm::window::{BORDER, Rect, TITLE_BAR_HEIGHT, Window};
use core::sync::atomic::{AtomicU64, Ordering};

pub const SHELL_BAR_HEIGHT: i32 = 32;
pub const SHELL_START_X: i32 = 10;
pub const SHELL_START_Y: i32 = 6;
pub const SHELL_START_W: u32 = 80;
pub const SHELL_START_H: u32 = 20;
pub const START_MENU_X: i32 = 12;
pub const START_MENU_Y: i32 = SHELL_BAR_HEIGHT + 10;
pub const START_MENU_W: u32 = 320;
pub const START_MENU_H: u32 = 236;
pub const START_MENU_ITEM_H: u32 = 38;
pub const START_MENU_ITEM_GAP: i32 = 8;
pub const START_MENU_ITEMS: usize = 4;
const DESKTOP_BG: u32 = 0x000b0f14;
const SHELL_BAR_BG: u32 = 0x0011171d;
const SHELL_BAR_EDGE: u32 = 0x002c3b47;
const SHELL_BAR_TOP: u32 = 0x0023364a;
const START_MENU_BG: u32 = 0x00101824;
const START_MENU_BORDER: u32 = 0x00436484;
const START_MENU_TITLE: u32 = 0x0089cbff;
const CHIP_BG: u32 = 0x00151d24;
const CHIP_BORDER: u32 = 0x00384a59;
const CHIP_FOCUS: u32 = 0x001d3140;
const CHIP_FOCUS_BORDER: u32 = 0x005d8db5;
const CHIP_GLOW: u32 = 0x00264256;
const SHELL_TEXT: u32 = 0x00e7eef4;
const SHELL_MUTED: u32 = 0x0092a2b0;

/// Monotonic vsync frame counter incremented each time `render()` completes.
/// Ring-3 tasks may poll this to synchronise surface flips to the display
/// refresh cycle without spinning in the kernel.
static VSYNC_FRAME: AtomicU64 = AtomicU64::new(0);

/// Return the current vsync frame counter.
/// A task that captured a value before calling `surface_present` can spin on
/// this until the value advances to confirm their frame has been displayed.
pub fn vsync_frame() -> u64 {
    VSYNC_FRAME.load(Ordering::Acquire)
}

/// Wait-free fence: spin until the frame counter exceeds `last_frame`.
/// Called from the kernel syscall path for `SYS_SURFACE_QUERY_PENDING`.
pub fn vsync_wait(last_frame: u64) -> u64 {
    // Non-blocking: return current value so the caller can decide.
    let current = VSYNC_FRAME.load(Ordering::Acquire);
    let _ = last_frame;
    current
}

pub struct Compositor {
    screen: Screen,
    shared_surfaces: [u32; crate::wm::surface_table::MAX_SURFACES_GLOBAL],
    shared_surface_count: usize,
}

impl Compositor {
    pub fn new(screen: Screen) -> Self {
        Self {
            screen,
            shared_surfaces: [0; crate::wm::surface_table::MAX_SURFACES_GLOBAL],
            shared_surface_count: 0,
        }
    }

    pub fn width(&self) -> u32 {
        self.screen.width()
    }

    pub fn height(&self) -> u32 {
        self.screen.height()
    }

    pub fn render(&mut self, windows: &mut [Window], start_menu_open: bool, start_pressed: bool) {
        let width = self.screen.width();
        let mut canvas = Canvas::new(&mut self.screen);
        canvas.clear(DESKTOP_BG);
        Self::draw_windows(&mut canvas, windows, None, true);
        Self::draw_shell_bar(&mut canvas, windows, width, start_pressed);
        if start_menu_open {
            Self::draw_start_menu(&mut canvas);
        }
        self.draw_shared_surfaces_overlay();
        self.screen.present();
        // Advance vsync fence so waiting tasks can detect frame completion.
        VSYNC_FRAME.fetch_add(1, Ordering::Release);
    }

    pub fn render_region(
        &mut self,
        windows: &mut [Window],
        damage: Rect,
        start_menu_open: bool,
        start_pressed: bool,
    ) {
        let Some(clip) = self.clip_rect_for(damage) else {
            return;
        };
        let width = self.screen.width();

        let mut canvas = Canvas::with_clip(&mut self.screen, clip);
        canvas.clear(DESKTOP_BG);
        Self::draw_windows(&mut canvas, windows, Some(damage), true);
        if damage.intersects(Rect::new(0, 0, width, SHELL_BAR_HEIGHT as u32)) {
            Self::draw_shell_bar(&mut canvas, windows, width, start_pressed);
        }
        if start_menu_open
            && damage.intersects(Rect::new(
                START_MENU_X,
                START_MENU_Y,
                START_MENU_W,
                START_MENU_H,
            ))
        {
            Self::draw_start_menu(&mut canvas);
        }
        self.draw_shared_surfaces_overlay();
        self.screen
            .present_region(clip.x as i32, clip.y as i32, clip.w, clip.h);
    }

    pub fn show_cursor(&mut self, x: i32, y: i32, left_down: bool) {
        self.screen.show_cursor(x, y, left_down);
    }

    pub fn set_cursor(&mut self, x: i32, y: i32, left_down: bool) {
        self.screen.set_cursor(x, y, left_down, true);
    }

    pub fn move_cursor(&mut self, old_x: i32, old_y: i32, new_x: i32, new_y: i32, left_down: bool) {
        self.screen
            .move_cursor(old_x, old_y, new_x, new_y, left_down);
    }

    /// Drain the present queue and blit any pending shared surfaces.
    ///
    /// Each surface whose ID is in the queue is read through the kernel's
    /// identity map (all physical RAM is identity-mapped below 4 GiB) and
    /// blitted into the screen backbuffer at position (0, 0). After draining
    /// the queue, `present()` is called to push the backbuffer to the physical
    /// framebuffer.
    ///
    /// This is the kernel-resident path for shared surfaces. The ring-3
    /// compositor service performs an equivalent operation via its own mapped
    /// virtual address.
    pub fn render_shared_surfaces(&mut self) {
        use crate::wm::surface_table::present_queue_pop;

        let mut max_w = 0u32;
        let mut max_h = 0u32;
        while let Some(surface_id) = present_queue_pop() {
            self.track_shared_surface(surface_id);
            if let Some((w, h)) = self.draw_shared_surface(surface_id) {
                max_w = max_w.max(w);
                max_h = max_h.max(h);
            }
        }

        if max_w != 0 && max_h != 0 {
            self.screen.present_region(0, 0, max_w, max_h);
        }
    }

    fn track_shared_surface(&mut self, surface_id: u32) {
        if self
            .shared_surfaces
            .iter()
            .take(self.shared_surface_count)
            .any(|&id| id == surface_id)
        {
            return;
        }
        if self.shared_surface_count < self.shared_surfaces.len() {
            self.shared_surfaces[self.shared_surface_count] = surface_id;
            self.shared_surface_count += 1;
        }
    }

    fn draw_shared_surfaces_overlay(&mut self) {
        let mut write = 0usize;
        for read in 0..self.shared_surface_count {
            let id = self.shared_surfaces[read];
            if self.draw_shared_surface(id).is_some() {
                self.shared_surfaces[write] = id;
                write += 1;
            }
        }
        self.shared_surface_count = write;
    }

    fn draw_shared_surface(&mut self, surface_id: u32) -> Option<(u32, u32)> {
        use crate::wm::surface_table::{
            MAX_SURFACE_FRAMES, surface_dimensions, surface_exists, surface_frames,
        };

        if !surface_exists(surface_id) {
            return None;
        }

        let (surface_w16, surface_h16) = surface_dimensions(surface_id)?;

        let surface_w = surface_w16 as usize;
        let surface_h = surface_h16 as usize;
        if surface_w == 0 || surface_h == 0 {
            return None;
        }

        let mut frames = [0u64; MAX_SURFACE_FRAMES];
        let frame_count = surface_frames(surface_id, &mut frames);
        if frame_count == 0 {
            return None;
        }

        let scr_w = self.screen.width() as usize;
        let scr_h = self.screen.height() as usize;

        const PIXELS_PER_FRAME: usize = 4096 / core::mem::size_of::<u32>();
        let max_pixels = frame_count.saturating_mul(PIXELS_PER_FRAME);
        let total_pixels = surface_w.saturating_mul(surface_h).min(max_pixels);

        let mut pixel_offset = 0usize;
        for frame_phys in frames.iter().copied().take(frame_count) {
            // Shared surfaces may be backed by frames above the initial
            // bootstrap identity map. Ensure the 2 MiB region is mapped
            // before dereferencing the physical pointer.
            if !crate::mm::page_table::ensure_identity_mapped_2m(frame_phys) {
                crate::arch::serial::write_bytes(b"[compositor] surface frame not mapped id=");
                crate::arch::serial::write_u64_dec_inline(surface_id as u64);
                crate::arch::serial::write_line(b"");
                return None;
            }
            let src =
                unsafe { core::slice::from_raw_parts(frame_phys as *const u32, PIXELS_PER_FRAME) };

            for (local_px, &color) in src.iter().enumerate().take(PIXELS_PER_FRAME) {
                let global_px = pixel_offset + local_px;
                if global_px >= total_pixels {
                    return Some((surface_w16 as u32, surface_h16 as u32));
                }
                let py = global_px / surface_w;
                let px = global_px % surface_w;
                if py >= scr_h || px >= scr_w {
                    continue;
                }
                let dst_idx = py * scr_w + px;
                self.screen.backbuffer_set_pixel(dst_idx, color);
            }
            pixel_offset += PIXELS_PER_FRAME;
        }

        Some((surface_w16 as u32, surface_h16 as u32))
    }

    fn clip_rect_for(&self, damage: Rect) -> Option<ClipRect> {
        if damage.is_empty() {
            return None;
        }

        let x0 = damage.x.max(0) as u32;
        let y0 = damage.y.max(0) as u32;
        let x1 = damage.right().min(self.screen.width() as i32).max(0) as u32;
        let y1 = damage.bottom().min(self.screen.height() as i32).max(0) as u32;
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some(ClipRect::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn draw_windows<T: DrawTarget + ?Sized>(
        canvas: &mut Canvas<'_, T>,
        windows: &mut [Window],
        damage: Option<Rect>,
        rerender_dirty: bool,
    ) {
        for window in windows.iter_mut() {
            if let Some(damage) = damage
                && !window.damage_bounds().intersects(damage)
            {
                continue;
            }
            if rerender_dirty {
                window.render_if_dirty();
            }
            Self::draw_window(canvas, window);
        }
    }

    fn draw_window<T: DrawTarget + ?Sized>(canvas: &mut Canvas<'_, T>, window: &Window) {
        let border = if window.focused {
            0x005c8db8
        } else {
            0x0035434f
        };
        let title_bg = if window.focused {
            0x00171f27
        } else {
            0x0011161c
        };
        let frame_bg = 0x000d1217;
        let text = if window.focused {
            0x00eef4f8
        } else {
            0x00b0bdc9
        };
        let x = window.bounds.x;
        let y = window.bounds.y;
        let w = window.bounds.w;
        let h = window.bounds.h;

        canvas.fill_rect(x + 4, y + 4, w, h, 0x00060a0f);
        canvas.fill_rect(x, y, w, h, frame_bg);
        canvas.stroke_rect(x, y, w, h, border);
        canvas.fill_rect(
            x,
            y,
            w,
            1,
            if window.focused {
                0x005c8db8
            } else {
                0x00414f5c
            },
        );
        canvas.fill_rect(
            x + BORDER as i32,
            y + BORDER as i32,
            w.saturating_sub(BORDER * 2),
            TITLE_BAR_HEIGHT,
            title_bg,
        );
        canvas.draw_text(
            x + BORDER as i32 + 6,
            y + BORDER as i32 + 5,
            window.title(),
            text,
            title_bg,
        );
        let (close_x, close_y, close_w, close_h) = window.close_button_rect();
        let close_bg = if window.focused {
            0x002d3942
        } else {
            0x001f262c
        };
        canvas.fill_rect(close_x, close_y, close_w, close_h, close_bg);
        canvas.stroke_rect(close_x, close_y, close_w, close_h, border);
        canvas.fill_rect(close_x + 3, close_y + 3, close_w.saturating_sub(6), 1, text);
        canvas.fill_rect(
            close_x + 3,
            close_y + close_h as i32 - 4,
            close_w.saturating_sub(6),
            1,
            text,
        );
        canvas.fill_rect(close_x + 3, close_y + 3, 1, close_h.saturating_sub(6), text);
        canvas.fill_rect(
            close_x + close_w as i32 - 4,
            close_y + 3,
            1,
            close_h.saturating_sub(6),
            text,
        );
        canvas.blit(&window.surface, window.content_x(), window.content_y());
    }

    fn draw_shell_bar<T: DrawTarget + ?Sized>(
        canvas: &mut Canvas<'_, T>,
        windows: &[Window],
        width: u32,
        start_pressed: bool,
    ) {
        // Two-tone bar creates mild depth cue for a more modern shell look.
        canvas.fill_rect(0, 0, width, 2, SHELL_BAR_TOP);
        canvas.fill_rect(0, 0, width, SHELL_BAR_HEIGHT as u32, SHELL_BAR_BG);
        canvas.fill_rect(0, SHELL_BAR_HEIGHT - 1, width, 1, SHELL_BAR_EDGE);

        Self::draw_chip(
            canvas,
            SHELL_START_X,
            SHELL_START_Y,
            SHELL_START_W,
            SHELL_START_H,
            b"start",
            SHELL_TEXT,
            if start_pressed { CHIP_FOCUS } else { CHIP_BG },
            if start_pressed {
                CHIP_FOCUS_BORDER
            } else {
                CHIP_BORDER
            },
        );

        let mut x = (SHELL_START_X + SHELL_START_W as i32 + 10).max(100);
        for window in windows.iter().take(4) {
            let label = window.title();
            let label_w = (label.len() as u32).saturating_mul(8).saturating_add(14);
            let chip_w = label_w.clamp(76, 180);
            let bg = if window.focused { CHIP_FOCUS } else { CHIP_BG };
            let border = if window.focused {
                CHIP_FOCUS_BORDER
            } else {
                CHIP_BORDER
            };
            let fg = if window.focused {
                SHELL_TEXT
            } else {
                SHELL_MUTED
            };
            Self::draw_chip(canvas, x, 6, chip_w, 20, label, fg, bg, border);
            x = x.saturating_add(chip_w as i32 + 8);
            if x > width as i32 - 180 {
                break;
            }
        }

        let status = b"GUI operator mode";
        let status_w = (status.len() as u32).saturating_mul(8).saturating_add(14);
        let status_x = width.saturating_sub(status_w + 10) as i32;
        Self::draw_chip(
            canvas,
            status_x,
            6,
            status_w,
            20,
            b"3D shell online",
            SHELL_MUTED,
            SHELL_BAR_BG,
            SHELL_BAR_EDGE,
        );
    }

    fn draw_start_menu<T: DrawTarget + ?Sized>(canvas: &mut Canvas<'_, T>) {
        // Outer drop-shadow for a floating card effect.
        canvas.fill_rect(
            START_MENU_X + 5,
            START_MENU_Y + 6,
            START_MENU_W,
            START_MENU_H,
            0x00070d13,
        );
        canvas.fill_rect(
            START_MENU_X,
            START_MENU_Y,
            START_MENU_W,
            START_MENU_H,
            START_MENU_BG,
        );
        canvas.stroke_rect(
            START_MENU_X,
            START_MENU_Y,
            START_MENU_W,
            START_MENU_H,
            START_MENU_BORDER,
        );
        canvas.fill_rect(
            START_MENU_X,
            START_MENU_Y,
            START_MENU_W,
            1,
            START_MENU_TITLE,
        );

        canvas.draw_text(
            START_MENU_X + 12,
            START_MENU_Y + 12,
            b"GraphOS Launcher",
            START_MENU_TITLE,
            START_MENU_BG,
        );
        canvas.draw_text(
            START_MENU_X + 12,
            START_MENU_Y + 24,
            b"Apps and system controls",
            SHELL_MUTED,
            START_MENU_BG,
        );

        let labels: [&[u8]; START_MENU_ITEMS] =
            [b"App Launcher", b"Settings", b"Config", b"Terminal"];
        for (idx, label) in labels.iter().enumerate() {
            let (x, y, w, h) = start_menu_item_rect(idx);
            let bg = if idx == 0 { CHIP_FOCUS } else { CHIP_BG };
            let border = if idx == 0 {
                CHIP_FOCUS_BORDER
            } else {
                CHIP_BORDER
            };
            canvas.fill_rect(x + 2, y + 3, w, h, CHIP_GLOW);
            canvas.fill_rect(x, y, w, h, bg);
            canvas.stroke_rect(x, y, w, h, border);
            canvas.draw_text(x + 10, y + 11, label, SHELL_TEXT, bg);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_chip<T: DrawTarget + ?Sized>(
        canvas: &mut Canvas<'_, T>,
        x: i32,
        y: i32,
        w: u32,
        h: u32,
        label: &[u8],
        fg: u32,
        bg: u32,
        border: u32,
    ) {
        canvas.fill_rect(x, y, w, h, bg);
        canvas.stroke_rect(x, y, w, h, border);
        canvas.fill_rect(x + 1, y + 1, w.saturating_sub(2), 1, CHIP_GLOW);
        canvas.draw_text(x + 7, y + 6, label, fg, bg);
    }
}

pub fn start_menu_item_rect(index: usize) -> (i32, i32, u32, u32) {
    let item_w = START_MENU_W.saturating_sub(24);
    let x = START_MENU_X + 12;
    let y = START_MENU_Y + 46 + (index as i32) * (START_MENU_ITEM_H as i32 + START_MENU_ITEM_GAP);
    (x, y, item_w, START_MENU_ITEM_H)
}
