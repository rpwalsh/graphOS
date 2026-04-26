// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use alloc::vec;
use alloc::vec::Vec;

use crate::bootinfo::FramebufferFormat;
use crate::drivers::display;

const CURSOR_WIDTH: u32 = 16;
const CURSOR_HEIGHT: u32 = 16;
const CURSOR_ROWS: [u16; CURSOR_HEIGHT as usize] = [
    0b0000000000000011,
    0b0000000000000111,
    0b0000000000001111,
    0b0000000000011111,
    0b0000000000110111,
    0b0000000001100011,
    0b0000000011000011,
    0b0000000110000011,
    0b0000001100000011,
    0b0000011000000011,
    0b0000110000001111,
    0b0001100000011110,
    0b0001000000111100,
    0b0000000001111000,
    0b0000000011110000,
    0b0000000001100000,
];

pub trait DrawTarget {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn set_pixel(&mut self, x: u32, y: u32, color: u32);
    fn fill_rect_region(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        for row in y..y.saturating_add(h) {
            for col in x..x.saturating_add(w) {
                self.set_pixel(col, row, color);
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn blit_surface_region(
        &mut self,
        src: &Surface,
        src_x: u32,
        src_y: u32,
        w: u32,
        h: u32,
        dst_x: u32,
        dst_y: u32,
    ) {
        for row in 0..h {
            for col in 0..w {
                if let Some(color) = src.pixel(src_x + col, src_y + row) {
                    self.set_pixel(dst_x + col, dst_y + row, color);
                }
            }
        }
    }

    fn clear(&mut self, color: u32) {
        let width = self.width();
        let height = self.height();
        self.fill_rect_region(0, 0, width, height, color);
    }

    /// Read back a pixel (default: returns transparent black if not overridden).
    fn get_pixel(&self, _x: u32, _y: u32) -> u32 {
        0x00000000
    }
}

pub struct Surface {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Surface {
    pub fn new(width: u32, height: u32, fill: u32) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        Self {
            width,
            height,
            pixels: vec![fill; len],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    pub fn clear(&mut self, color: u32) {
        self.pixels.fill(color);
    }

    pub fn fill_rect_region(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        if w == 0 || h == 0 || x >= self.width || y >= self.height {
            return;
        }

        let x1 = x.saturating_add(w).min(self.width);
        let y1 = y.saturating_add(h).min(self.height);
        let width = self.width as usize;
        for row in y as usize..y1 as usize {
            let start = row.saturating_mul(width).saturating_add(x as usize);
            let end = row.saturating_mul(width).saturating_add(x1 as usize);
            if start < end && end <= self.pixels.len() {
                self.pixels[start..end].fill(color);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blit_surface_region(
        &mut self,
        src: &Surface,
        src_x: u32,
        src_y: u32,
        w: u32,
        h: u32,
        dst_x: u32,
        dst_y: u32,
    ) {
        if w == 0 || h == 0 || dst_x >= self.width || dst_y >= self.height {
            return;
        }
        if src_x >= src.width || src_y >= src.height {
            return;
        }

        let copy_w = w
            .min(src.width.saturating_sub(src_x))
            .min(self.width.saturating_sub(dst_x));
        let copy_h = h
            .min(src.height.saturating_sub(src_y))
            .min(self.height.saturating_sub(dst_y));
        if copy_w == 0 || copy_h == 0 {
            return;
        }

        let dst_width = self.width as usize;
        let src_width = src.width as usize;
        for row in 0..copy_h as usize {
            let src_start = (src_y as usize + row)
                .saturating_mul(src_width)
                .saturating_add(src_x as usize);
            let dst_start = (dst_y as usize + row)
                .saturating_mul(dst_width)
                .saturating_add(dst_x as usize);
            let len = copy_w as usize;
            self.pixels[dst_start..dst_start + len]
                .copy_from_slice(&src.pixels[src_start..src_start + len]);
        }
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = (y as usize)
            .saturating_mul(self.width as usize)
            .saturating_add(x as usize);
        if let Some(pixel) = self.pixels.get_mut(idx) {
            *pixel = color;
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = (y as usize)
            .saturating_mul(self.width as usize)
            .saturating_add(x as usize);
        self.pixels.get(idx).copied()
    }
}

impl DrawTarget for Surface {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn set_pixel(&mut self, x: u32, y: u32, color: u32) {
        Surface::set_pixel(self, x, y, color);
    }

    fn get_pixel(&self, x: u32, y: u32) -> u32 {
        self.pixel(x, y).unwrap_or(0)
    }

    fn fill_rect_region(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        Surface::fill_rect_region(self, x, y, w, h, color);
    }

    fn blit_surface_region(
        &mut self,
        src: &Surface,
        src_x: u32,
        src_y: u32,
        w: u32,
        h: u32,
        dst_x: u32,
        dst_y: u32,
    ) {
        Surface::blit_surface_region(self, src, src_x, src_y, w, h, dst_x, dst_y);
    }

    fn clear(&mut self, color: u32) {
        Surface::clear(self, color);
    }
}

#[derive(Clone, Copy)]
pub struct FramebufferConfig {
    pub addr: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: FramebufferFormat,
}

pub struct Screen {
    config: FramebufferConfig,
    backbuffer: Surface,
    cursor: CursorState,
}

#[derive(Clone, Copy)]
struct CursorState {
    x: i32,
    y: i32,
    left_down: bool,
    visible: bool,
}

impl Screen {
    pub fn new(config: FramebufferConfig) -> Self {
        let backbuffer = Surface::new(config.width, config.height, 0);
        display::set_backbuffer_bytes(backbuffer.pixels().len().saturating_mul(4));
        Self {
            backbuffer,
            config,
            cursor: CursorState {
                x: 0,
                y: 0,
                left_down: false,
                visible: false,
            },
        }
    }

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn present(&mut self) {
        let width = self.config.width as usize;
        let height = self.config.height as usize;
        let stride = self.config.stride as usize;
        let visible_width = width.min(stride);
        if visible_width == 0 {
            return;
        }
        let base = self.config.addr as *mut u32;
        let cursor_bounds = self.cursor_bounds();

        crate::mm::page_table::with_kernel_address_space(|| unsafe {
            match self.config.format {
                FramebufferFormat::Rgb | FramebufferFormat::BltOnly => {
                    for y in 0..height {
                        let src_row = &self.backbuffer.pixels[y * width..y * width + visible_width];
                        let dst_row = base.add(y * stride);
                        if let Some((_, cy0, _, cy1)) = cursor_bounds
                            && y >= cy0 as usize
                            && y < cy1 as usize
                        {
                            for (x, &color) in src_row.iter().enumerate() {
                                dst_row.add(x).write_volatile(
                                    self.composited_color(x as u32, y as u32, color),
                                );
                            }
                            continue;
                        }
                        for (x, &color) in src_row.iter().enumerate() {
                            dst_row.add(x).write_volatile(color);
                        }
                    }
                }
                _ => {
                    for y in 0..height {
                        let src_row = &self.backbuffer.pixels[y * width..y * width + visible_width];
                        let dst_row = base.add(y * stride);
                        for (x, &color) in src_row.iter().enumerate() {
                            let color = self.composited_color(x as u32, y as u32, color);
                            dst_row
                                .add(x)
                                .write_volatile(native_color(self.config.format, color));
                        }
                    }
                }
            }
        });
        display::note_full_present();
    }

    pub fn present_region(&mut self, x: i32, y: i32, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }

        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let max_x = self.config.width.min(self.config.stride) as i32;
        let x1 = x.saturating_add(w as i32).min(max_x).max(0) as u32;
        let y1 = y
            .saturating_add(h as i32)
            .min(self.config.height as i32)
            .max(0) as u32;

        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let width = self.config.width as usize;
        let stride = self.config.stride as usize;
        let cursor_bounds = self.cursor_bounds();
        let base = self.config.addr as *mut u32;

        crate::mm::page_table::with_kernel_address_space(|| unsafe {
            match self.config.format {
                FramebufferFormat::Rgb | FramebufferFormat::BltOnly => {
                    for row in y0 as usize..y1 as usize {
                        let src_row = &self.backbuffer.pixels
                            [row * width + x0 as usize..row * width + x1 as usize];
                        let dst_row = base.add(row * stride + x0 as usize);
                        let row_intersects_cursor =
                            cursor_bounds.is_some_and(|(cx0, cy0, cx1, cy1)| {
                                row >= cy0.max(0) as usize
                                    && row < cy1.max(0) as usize
                                    && x0 < cx1.max(0) as u32
                                    && x1 > cx0.max(0) as u32
                            });

                        for (offset, &color) in src_row.iter().enumerate() {
                            let col = x0 + offset as u32;
                            let color = if row_intersects_cursor {
                                self.composited_color(col, row as u32, color)
                            } else {
                                color
                            };
                            dst_row.add(offset).write_volatile(color);
                        }
                    }
                }
                _ => {
                    for row in y0 as usize..y1 as usize {
                        let src_row = &self.backbuffer.pixels
                            [row * width + x0 as usize..row * width + x1 as usize];
                        let dst_row = base.add(row * stride + x0 as usize);
                        for (offset, &color) in src_row.iter().enumerate() {
                            let col = x0 + offset as u32;
                            let color = self.composited_color(col, row as u32, color);
                            dst_row
                                .add(offset)
                                .write_volatile(native_color(self.config.format, color));
                        }
                    }
                }
            }
        });
        let dirty_pixels = (x1 - x0) as u64 * (y1 - y0) as u64;
        let full_surface_pixels = self.config.width as u64 * self.config.height as u64;
        display::note_present(dirty_pixels, full_surface_pixels);
    }

    /// Write a single pixel into the backbuffer by linear index.
    ///
    /// Does nothing if `idx` is out of bounds. Used by the compositor's
    /// frame-by-frame blit path for non-contiguous shared surface frames.
    #[inline(always)]
    pub fn backbuffer_set_pixel(&mut self, idx: usize, color: u32) {
        if let Some(p) = self.backbuffer.pixels.get_mut(idx) {
            *p = color;
        }
    }

    pub fn show_cursor(&mut self, x: i32, y: i32, left_down: bool) {
        self.set_cursor(x, y, left_down, true);
        let (rx, ry, rw, rh) = cursor_rect(x, y);
        self.present_region(rx, ry, rw, rh);
    }

    pub fn move_cursor(&mut self, old_x: i32, old_y: i32, new_x: i32, new_y: i32, left_down: bool) {
        self.set_cursor(new_x, new_y, left_down, true);
        let (old_rx, old_ry, old_rw, old_rh) = cursor_rect(old_x, old_y);
        self.present_region(old_rx, old_ry, old_rw, old_rh);
        if old_x != new_x || old_y != new_y {
            let (new_rx, new_ry, new_rw, new_rh) = cursor_rect(new_x, new_y);
            self.present_region(new_rx, new_ry, new_rw, new_rh);
        }
    }

    pub fn set_cursor(&mut self, x: i32, y: i32, left_down: bool, visible: bool) {
        self.cursor = CursorState {
            x,
            y,
            left_down,
            visible,
        };
    }

    fn write_hw_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x >= self.config.width || y >= self.config.height {
            return;
        }
        crate::mm::page_table::with_kernel_address_space(|| unsafe {
            let base = self.config.addr as *mut u32;
            base.add((y * self.config.stride + x) as usize)
                .write_volatile(native_color(self.config.format, color));
        });
    }

    fn composited_color(&self, x: u32, y: u32, base: u32) -> u32 {
        if let Some(cursor) = self.cursor_color_at(x as i32, y as i32) {
            cursor
        } else {
            base
        }
    }

    fn cursor_bounds(&self) -> Option<(i32, i32, i32, i32)> {
        if !self.cursor.visible {
            return None;
        }
        Some((
            self.cursor.x,
            self.cursor.y,
            self.cursor.x.saturating_add(CURSOR_WIDTH as i32),
            self.cursor.y.saturating_add(CURSOR_HEIGHT as i32),
        ))
    }

    fn cursor_color_at(&self, x: i32, y: i32) -> Option<u32> {
        if !self.cursor.visible {
            return None;
        }

        let local_x = x.saturating_sub(self.cursor.x);
        let local_y = y.saturating_sub(self.cursor.y);
        if local_x < 0
            || local_y < 0
            || local_x >= CURSOR_WIDTH as i32
            || local_y >= CURSOR_HEIGHT as i32
        {
            return None;
        }

        let row_mask = CURSOR_ROWS[local_y as usize] as u32;
        let bit = 1u32 << local_x;
        let shadow_mask = if local_y > 0 {
            (CURSOR_ROWS[(local_y - 1) as usize] as u32) << 1
        } else {
            0
        };

        if row_mask & bit != 0 {
            return Some(if self.cursor.left_down {
                0x00ffd27a
            } else {
                0x00e8eef2
            });
        }

        if shadow_mask & bit != 0 {
            return Some(0x0003080c);
        }

        None
    }
}

impl DrawTarget for Screen {
    fn width(&self) -> u32 {
        self.config.width
    }

    fn height(&self) -> u32 {
        self.config.height
    }

    fn set_pixel(&mut self, x: u32, y: u32, color: u32) {
        self.backbuffer.set_pixel(x, y, color);
    }

    fn fill_rect_region(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        self.backbuffer.fill_rect_region(x, y, w, h, color);
    }

    fn blit_surface_region(
        &mut self,
        src: &Surface,
        src_x: u32,
        src_y: u32,
        w: u32,
        h: u32,
        dst_x: u32,
        dst_y: u32,
    ) {
        self.backbuffer
            .blit_surface_region(src, src_x, src_y, w, h, dst_x, dst_y);
    }

    fn clear(&mut self, color: u32) {
        self.backbuffer.clear(color);
    }
}

fn native_color(format: FramebufferFormat, color: u32) -> u32 {
    match format {
        FramebufferFormat::Rgb | FramebufferFormat::BltOnly => color,
        FramebufferFormat::Bgr | FramebufferFormat::Bitmask | FramebufferFormat::Unknown => {
            let r = (color >> 16) & 0xff;
            let g = (color >> 8) & 0xff;
            let b = color & 0xff;
            (b << 16) | (g << 8) | r
        }
    }
}

fn cursor_rect(x: i32, y: i32) -> (i32, i32, u32, u32) {
    (x, y, CURSOR_WIDTH, CURSOR_HEIGHT)
}
