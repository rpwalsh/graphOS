// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Software rasterizer for ring-3 GraphOS applications.
//!
//! `Canvas` wraps a pixel buffer (from a shared surface) and provides
//! primitive drawing operations: clear, fill_rect, draw_text.
//!
//! All coordinates are in pixels relative to (0, 0) = top-left.
//! Pixels are stored as BGRA32 (0xAARRGGBB on little-endian x86).

/// 8×8 bitmap font, ASCII 0x20–0x7E (95 glyphs).
///
/// Each glyph occupies 8 bytes, one per row from top to bottom.
/// Bit 7 of each byte is the leftmost pixel column.
const FONT_FIRST: u8 = 0x20;
const FONT_LAST: u8 = 0x7E;
/// Pixels per glyph column.
pub const FONT_W: u32 = 8;
/// Pixels per glyph row.
pub const FONT_H: u32 = 8;

// 95 glyphs × 8 bytes each = 760 bytes.
static FONT: [[u8; 8]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // 0x20 SPACE
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00], // !
    [0x6C, 0x6C, 0x48, 0x00, 0x00, 0x00, 0x00, 0x00], // "
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00], // #
    [0x0C, 0x3F, 0x03, 0x1E, 0x30, 0x3F, 0x0C, 0x00], // $
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00], // %
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00], // &
    [0x18, 0x18, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00], // '
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00], // (
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00], // )
    [0x00, 0x36, 0x1C, 0x7F, 0x1C, 0x36, 0x00, 0x00], // *
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00], // +
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06], // ,
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00], // -
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00], // .
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00], // /
    [0x1E, 0x33, 0x3B, 0x37, 0x33, 0x33, 0x1E, 0x00], // 0
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00], // 1
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x03, 0x3F, 0x00], // 2
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00], // 3
    [0x18, 0x1C, 0x16, 0x13, 0x3F, 0x10, 0x3C, 0x00], // 4
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00], // 5
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00], // 6
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00], // 7
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00], // 8
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00], // 9
    [0x00, 0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00], // :
    [0x00, 0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x0C], // ;
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00], // <
    [0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00], // =
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00], // >
    [0x1E, 0x33, 0x30, 0x18, 0x18, 0x00, 0x18, 0x00], // ?
    [0x3E, 0x63, 0x7B, 0x7B, 0x7F, 0x03, 0x1E, 0x00], // @
    [0x0C, 0x1E, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00], // A
    [0x1F, 0x33, 0x33, 0x1F, 0x33, 0x33, 0x1F, 0x00], // B
    [0x1E, 0x33, 0x03, 0x03, 0x03, 0x33, 0x1E, 0x00], // C
    [0x0F, 0x1B, 0x33, 0x33, 0x33, 0x1B, 0x0F, 0x00], // D
    [0x3F, 0x03, 0x03, 0x1F, 0x03, 0x03, 0x3F, 0x00], // E
    [0x3F, 0x03, 0x03, 0x1F, 0x03, 0x03, 0x03, 0x00], // F
    [0x1E, 0x33, 0x03, 0x3B, 0x33, 0x33, 0x1E, 0x00], // G
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00], // H
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // I
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00], // J
    [0x33, 0x1B, 0x0F, 0x07, 0x0F, 0x1B, 0x33, 0x00], // K
    [0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x3F, 0x00], // L
    [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00], // M
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00], // N
    [0x1E, 0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x00], // O
    [0x1F, 0x33, 0x33, 0x1F, 0x03, 0x03, 0x03, 0x00], // P
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00], // Q
    [0x1F, 0x33, 0x33, 0x1F, 0x0F, 0x1B, 0x33, 0x00], // R
    [0x1E, 0x33, 0x03, 0x1E, 0x30, 0x33, 0x1E, 0x00], // S
    [0x3F, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x00], // T
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x00], // U
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // V
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00], // W
    [0x33, 0x33, 0x1E, 0x0C, 0x1E, 0x33, 0x33, 0x00], // X
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x0C, 0x00], // Y
    [0x3F, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x3F, 0x00], // Z
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00], // [
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00], // backslash
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00], // ]
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00], // ^
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3F], // _
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00], // `
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00], // a
    [0x03, 0x03, 0x1F, 0x33, 0x33, 0x33, 0x1F, 0x00], // b
    [0x00, 0x00, 0x1E, 0x03, 0x03, 0x33, 0x1E, 0x00], // c
    [0x30, 0x30, 0x3E, 0x33, 0x33, 0x33, 0x6E, 0x00], // d
    [0x00, 0x00, 0x1E, 0x33, 0x3F, 0x03, 0x1E, 0x00], // e
    [0x38, 0x0C, 0x0C, 0x1E, 0x0C, 0x0C, 0x0C, 0x00], // f
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F], // g
    [0x03, 0x03, 0x1B, 0x37, 0x33, 0x33, 0x33, 0x00], // h
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // i
    [0x18, 0x00, 0x1C, 0x18, 0x18, 0x1B, 0x0E, 0x00], // j
    [0x03, 0x03, 0x33, 0x1B, 0x0F, 0x1B, 0x33, 0x00], // k
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // l
    [0x00, 0x00, 0x37, 0x7F, 0x6B, 0x63, 0x63, 0x00], // m
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00], // n
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00], // o
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x1F, 0x03, 0x03], // p
    [0x00, 0x00, 0x3E, 0x33, 0x33, 0x3E, 0x30, 0x30], // q
    [0x00, 0x00, 0x1B, 0x37, 0x03, 0x03, 0x03, 0x00], // r
    [0x00, 0x00, 0x1E, 0x03, 0x1E, 0x30, 0x1F, 0x00], // s
    [0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x0C, 0x38, 0x00], // t
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00], // u
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // v
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00], // w
    [0x00, 0x00, 0x33, 0x1E, 0x0C, 0x1E, 0x33, 0x00], // x
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F], // y
    [0x00, 0x00, 0x3F, 0x18, 0x0C, 0x06, 0x3F, 0x00], // z
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00], // {
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00], // |
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00], // }
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ~
];

/// A drawing canvas backed by a mutable BGRA32 pixel buffer.
///
/// The buffer is typically the shared memory returned by `SYS_SURFACE_CREATE`.
/// `width` and `height` are in pixels; the buffer must be at least
/// `width × height × 4` bytes.
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
}

impl<'a> Canvas<'a> {
    /// Create a canvas over a mutable pixel slice.
    ///
    /// # Safety
    /// The caller must ensure `pixels` is valid for `width × height` u32
    /// values and that no other alias exists while this `Canvas` is live.
    pub unsafe fn from_raw(ptr: *mut u32, width: u32, height: u32) -> Canvas<'a> {
        let len = (width as usize) * (height as usize);
        Canvas {
            pixels: unsafe { core::slice::from_raw_parts_mut(ptr, len) },
            width,
            height,
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Borrow the raw BGRA32 pixel buffer mutably.
    ///
    /// Useful when handing the buffer to a 3D rasterizer (e.g.
    /// [`graphos_gl`](https://docs.rs/graphos-gl)) that wants its own
    /// `Target<'_>`. Length is always `width * height`.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        self.pixels
    }

    /// Fill the entire canvas with `color`.
    pub fn clear(&mut self, color: u32) {
        self.pixels.fill(color);
    }

    /// Set a single pixel if it lies inside the canvas bounds.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let x = x as u32;
        let y = y as u32;
        if x >= self.width || y >= self.height {
            return;
        }
        self.pixels[y as usize * self.width as usize + x as usize] = color;
    }

    /// Fill a rectangle with `color`. Clips to canvas bounds.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + w as i32).max(0) as u32;
        let y1 = (y + h as i32).max(0) as u32;
        let x1 = x1.min(self.width);
        let y1 = y1.min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.width as usize;
        for row in y0 as usize..y1 as usize {
            let start = row * stride + x0 as usize;
            let end = row * stride + x1 as usize;
            self.pixels[start..end].fill(color);
        }
    }

    /// Draw a horizontal line.
    pub fn draw_hline(&mut self, x: i32, y: i32, len: u32, color: u32) {
        self.fill_rect(x, y, len, 1, color);
    }

    /// Draw a vertical line.
    pub fn draw_vline(&mut self, x: i32, y: i32, len: u32, color: u32) {
        self.fill_rect(x, y, 1, len, color);
    }

    /// Draw a rectangle outline.
    pub fn draw_rect(&mut self, x: i32, y: i32, w: u32, h: u32, color: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.draw_hline(x, y, w, color);
        self.draw_hline(x, y + h as i32 - 1, w, color);
        self.draw_vline(x, y, h, color);
        self.draw_vline(x + w as i32 - 1, y, h, color);
    }

    /// Draw a line using Bresenham rasterisation.
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32) {
        let mut x0 = x0;
        let mut y0 = y0;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;

        loop {
            self.set_pixel(x0, y0, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let err2 = err.saturating_mul(2);
            if err2 >= dy {
                err += dy;
                x0 += sx;
            }
            if err2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    /// Draw a single character at `(x, y)` using the built-in bitmap font.
    ///
    /// Characters outside the printable ASCII range are rendered as spaces.
    pub fn draw_char(&mut self, x: i32, y: i32, ch: u8, fg: u32) {
        let idx = if ch >= FONT_FIRST && ch <= FONT_LAST {
            (ch - FONT_FIRST) as usize
        } else {
            0 // render as space
        };
        let glyph = &FONT[idx];
        for row in 0..8usize {
            let bits = glyph[row];
            for col in 0..8u32 {
                // Bit 0 = leftmost column (LSB-first font layout).
                if (bits >> col) & 1 == 1 {
                    let px = x + col as i32;
                    let py = y + row as i32;
                    if px >= 0 && py >= 0 && (px as u32) < self.width && (py as u32) < self.height {
                        self.pixels[py as usize * self.width as usize + px as usize] = fg;
                    }
                }
            }
        }
    }

    /// Draw a UTF-8 string at `(x, y)`, advancing by `FONT_W + 1` per glyph.
    ///
    /// Only printable ASCII (0x20–0x7E) is rendered; others are skipped.
    /// `max_width` limits the number of pixels drawn horizontally (0 = no limit).
    pub fn draw_text(&mut self, x: i32, y: i32, text: &[u8], fg: u32, max_width: u32) {
        let mut cx = x;
        for &byte in text {
            if byte < FONT_FIRST || byte > FONT_LAST {
                continue;
            }
            if max_width != 0 && (cx - x) as u32 + FONT_W > max_width {
                break;
            }
            self.draw_char(cx, y, byte, fg);
            cx += (FONT_W + 1) as i32;
        }
    }

    /// Width of a text string in pixels (using built-in font metrics).
    pub fn text_width(text: &[u8]) -> u32 {
        let printable = text
            .iter()
            .filter(|&&b| b >= FONT_FIRST && b <= FONT_LAST)
            .count();
        if printable == 0 {
            0
        } else {
            printable as u32 * (FONT_W + 1) - 1
        }
    }

    // -----------------------------------------------------------------------
    // GL3 — software-OpenGL-style primitives (alpha blend, triangles, blur).
    //
    // Pixel format is BGRA32 packed as 0xAARRGGBB on little-endian. `blend_*`
    // helpers treat the high byte as straight alpha (0=transparent,255=opaque)
    // and composite "source over destination" onto the existing buffer.
    // -----------------------------------------------------------------------

    /// Read pixel at `(x,y)` if in bounds.
    #[inline]
    pub fn pixel_at(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 {
            return None;
        }
        let (xu, yu) = (x as u32, y as u32);
        if xu >= self.width || yu >= self.height {
            return None;
        }
        Some(self.pixels[yu as usize * self.width as usize + xu as usize])
    }

    /// Source-over alpha blend `src` over the existing pixel.
    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, src: u32) {
        if x < 0 || y < 0 {
            return;
        }
        let (xu, yu) = (x as u32, y as u32);
        if xu >= self.width || yu >= self.height {
            return;
        }
        let idx = yu as usize * self.width as usize + xu as usize;
        let dst = self.pixels[idx];
        self.pixels[idx] = blend_over(dst, src);
    }

    /// Alpha-blended filled rectangle.
    pub fn fill_rect_blend(&mut self, x: i32, y: i32, w: u32, h: u32, src: u32) {
        let alpha = (src >> 24) & 0xFF;
        if alpha == 0 {
            return;
        }
        if alpha == 0xFF {
            self.fill_rect(x, y, w, h, src);
            return;
        }
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + w as i32).max(0) as u32).min(self.width);
        let y1 = ((y + h as i32).max(0) as u32).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.width as usize;
        for row in y0 as usize..y1 as usize {
            let base = row * stride;
            for col in x0 as usize..x1 as usize {
                let dst = self.pixels[base + col];
                self.pixels[base + col] = blend_over(dst, src);
            }
        }
    }

    /// Solid rounded rectangle (corner radius `r`).
    pub fn fill_round_rect(&mut self, x: i32, y: i32, w: u32, h: u32, r: u32, color: u32) {
        let r = r.min(w / 2).min(h / 2);
        if r == 0 {
            self.fill_rect(x, y, w, h, color);
            return;
        }
        // middle band (full width)
        self.fill_rect(x, y + r as i32, w, h - 2 * r, color);
        // top + bottom bands minus rounded corners
        self.fill_rect(x + r as i32, y, w - 2 * r, r, color);
        self.fill_rect(x + r as i32, y + h as i32 - r as i32, w - 2 * r, r, color);
        // 4 quarter-circle corners
        let r_i = r as i32;
        let r2 = (r as i32) * (r as i32);
        for dy in 0..r_i {
            for dx in 0..r_i {
                let dd = (r_i - dx - 1) * (r_i - dx - 1) + (r_i - dy - 1) * (r_i - dy - 1);
                if dd <= r2 {
                    self.set_pixel(x + dx, y + dy, color);
                    self.set_pixel(x + w as i32 - 1 - dx, y + dy, color);
                    self.set_pixel(x + dx, y + h as i32 - 1 - dy, color);
                    self.set_pixel(x + w as i32 - 1 - dx, y + h as i32 - 1 - dy, color);
                }
            }
        }
    }

    /// Alpha-blended rounded rectangle (used as a glass panel).
    pub fn fill_round_rect_blend(&mut self, x: i32, y: i32, w: u32, h: u32, r: u32, src: u32) {
        let r = r.min(w / 2).min(h / 2);
        if r == 0 {
            self.fill_rect_blend(x, y, w, h, src);
            return;
        }
        self.fill_rect_blend(x, y + r as i32, w, h - 2 * r, src);
        self.fill_rect_blend(x + r as i32, y, w - 2 * r, r, src);
        self.fill_rect_blend(x + r as i32, y + h as i32 - r as i32, w - 2 * r, r, src);
        let r_i = r as i32;
        let r2 = (r as i32) * (r as i32);
        for dy in 0..r_i {
            for dx in 0..r_i {
                let dd = (r_i - dx - 1) * (r_i - dx - 1) + (r_i - dy - 1) * (r_i - dy - 1);
                if dd <= r2 {
                    self.blend_pixel(x + dx, y + dy, src);
                    self.blend_pixel(x + w as i32 - 1 - dx, y + dy, src);
                    self.blend_pixel(x + dx, y + h as i32 - 1 - dy, src);
                    self.blend_pixel(x + w as i32 - 1 - dx, y + h as i32 - 1 - dy, src);
                }
            }
        }
    }

    /// Filled triangle with per-vertex Gouraud color and alpha (barycentric
    /// rasterisation). All three colors are source-over composited onto the
    /// destination so it works as the building block for the 3D pipeline.
    pub fn fill_triangle_blend(
        &mut self,
        x0: i32,
        y0: i32,
        c0: u32,
        x1: i32,
        y1: i32,
        c1: u32,
        x2: i32,
        y2: i32,
        c2: u32,
    ) {
        let min_x = x0.min(x1).min(x2).max(0);
        let min_y = y0.min(y1).min(y2).max(0);
        let max_x = x0.max(x1).max(x2).min(self.width as i32 - 1);
        let max_y = y0.max(y1).max(y2).min(self.height as i32 - 1);
        if min_x > max_x || min_y > max_y {
            return;
        }
        // Edge function denominator. Twice the signed area; sign indicates winding.
        let area = ((x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0)) as i64;
        if area == 0 {
            return;
        }
        let abs_area = area.unsigned_abs() as u64;
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let w0 = ((x1 - px) * (y2 - py) - (y1 - py) * (x2 - px)) as i64;
                let w1 = ((x2 - px) * (y0 - py) - (y2 - py) * (x0 - px)) as i64;
                let w2 = ((x0 - px) * (y1 - py) - (y0 - py) * (x1 - px)) as i64;
                // Both windings accepted: take absolute and check same sign.
                let same_sign = (w0 >= 0 && w1 >= 0 && w2 >= 0) || (w0 <= 0 && w1 <= 0 && w2 <= 0);
                if !same_sign {
                    continue;
                }
                let aw0 = w0.unsigned_abs() as u64;
                let aw1 = w1.unsigned_abs() as u64;
                let aw2 = w2.unsigned_abs() as u64;
                let src = lerp3(c0, c1, c2, aw0, aw1, aw2, abs_area);
                let alpha = (src >> 24) & 0xFF;
                if alpha == 0 {
                    continue;
                }
                let idx = py as usize * self.width as usize + px as usize;
                let dst = self.pixels[idx];
                self.pixels[idx] = if alpha == 0xFF {
                    src
                } else {
                    blend_over(dst, src)
                };
            }
        }
    }

    /// Box blur a rectangular region in place. `radius` is the half-width of
    /// the kernel (1 = 3x3 average). This is the frosted-glass primitive.
    pub fn box_blur(&mut self, x: i32, y: i32, w: u32, h: u32, radius: u32) {
        if radius == 0 || w == 0 || h == 0 {
            return;
        }
        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = ((x + w as i32).max(0) as u32).min(self.width);
        let y1 = ((y + h as i32).max(0) as u32).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let bw = (x1 - x0) as usize;
        let bh = (y1 - y0) as usize;
        // Single-pass approximation: 3x3 box for radius=1; for radius>=2 we
        // iterate the 3x3 pass `radius` times. This stays inside fixed
        // stack budgets (one row buffer at a time).
        let stride = self.width as usize;
        let passes = radius.min(4) as usize;
        for _ in 0..passes {
            // Copy of the current band into a tiny scratch (cap to keep stack tidy).
            // We process row by row using a 3-row sliding window in-place; for
            // simplicity (and because the launcher only blurs small panels)
            // we use a stack-bounded scratch limited to 256 px wide.
            const MAX_W: usize = 1280;
            if bw > MAX_W {
                return;
            }
            let mut prev: [u32; MAX_W] = [0; MAX_W];
            let mut curr: [u32; MAX_W] = [0; MAX_W];
            // Prime prev with first row, curr with second row.
            for c in 0..bw {
                prev[c] = self.pixels[y0 as usize * stride + (x0 as usize + c)];
                curr[c] = self.pixels
                    [(y0 as usize + 1).min(y1 as usize - 1) * stride + (x0 as usize + c)];
            }
            for r in 0..bh {
                let next_row = (y0 as usize + r + 1).min(y1 as usize - 1);
                let mut next: [u32; MAX_W] = [0; MAX_W];
                for c in 0..bw {
                    next[c] = self.pixels[next_row * stride + (x0 as usize + c)];
                }
                for c in 0..bw {
                    let l = if c == 0 { c } else { c - 1 };
                    let rgt = if c + 1 >= bw { c } else { c + 1 };
                    let avg = avg9(
                        prev[l], prev[c], prev[rgt], curr[l], curr[c], curr[rgt], next[l], next[c],
                        next[rgt],
                    );
                    self.pixels[(y0 as usize + r) * stride + (x0 as usize + c)] = avg;
                }
                prev = curr;
                curr = next;
            }
        }
    }

    /// Filled, alpha-blended disc centred at `(cx, cy)` with radius `r`.
    /// The boundary is anti-aliased via a 1-pixel coverage falloff.
    pub fn fill_circle_blend(&mut self, cx: i32, cy: i32, r: i32, src: u32) {
        if r <= 0 {
            return;
        }
        let r2 = (r * r) as i64;
        let r_outer2 = ((r + 1) * (r + 1)) as i64;
        for dy in -r..=r {
            let py = cy + dy;
            if py < 0 || py as u32 >= self.height {
                continue;
            }
            for dx in -r..=r {
                let px = cx + dx;
                if px < 0 || px as u32 >= self.width {
                    continue;
                }
                let d2 = (dx * dx + dy * dy) as i64;
                if d2 > r_outer2 {
                    continue;
                }
                let pixel = if d2 <= r2 {
                    src
                } else {
                    // 1-pixel AA falloff at the edge.
                    let cov = 255 - (((d2 - r2) * 255) / (r_outer2 - r2)).clamp(0, 255) as u32;
                    let a = ((src >> 24) & 0xFF) * cov / 255;
                    (a << 24) | (src & 0x00FF_FFFF)
                };
                self.blend_pixel(px, py, pixel);
            }
        }
    }

    /// Soft radial glow disc — alpha decays smoothly from `inner_alpha` at the
    /// centre to 0 at radius `r`. Cheap nebula / node-bloom primitive.
    pub fn fill_glow(&mut self, cx: i32, cy: i32, r: i32, rgb: u32, inner_alpha: u32) {
        if r <= 0 {
            return;
        }
        let r2 = (r * r) as i64;
        for dy in -r..=r {
            let py = cy + dy;
            if py < 0 || py as u32 >= self.height {
                continue;
            }
            for dx in -r..=r {
                let px = cx + dx;
                if px < 0 || px as u32 >= self.width {
                    continue;
                }
                let d2 = (dx * dx + dy * dy) as i64;
                if d2 >= r2 {
                    continue;
                }
                // Quadratic falloff for a soft bloom.
                let t = ((r2 - d2) * (r2 - d2)) as i64 / (r2 * r2 / 256);
                let a = ((inner_alpha as i64 * t) / 256).clamp(0, 255) as u32;
                if a == 0 {
                    continue;
                }
                self.blend_pixel(px, py, (a << 24) | (rgb & 0x00FF_FFFF));
            }
        }
    }

    /// Wu-style anti-aliased line with selectable width, alpha-blended.
    pub fn draw_line_aa(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, src: u32, width: u32) {
        let w = width.max(1) as i32;
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let steps = dx.max(dy).max(1);
        for s in 0..=steps {
            let px = x0 + (x1 - x0) * s / steps;
            let py = y0 + (y1 - y0) * s / steps;
            if w <= 1 {
                self.blend_pixel(px, py, src);
            } else {
                let half = w / 2;
                self.fill_circle_blend(px, py, half, src);
            }
        }
    }
}

/// Source-over alpha composite. ARGB layout 0xAARRGGBB.
#[inline]
fn blend_over(dst: u32, src: u32) -> u32 {
    let a = (src >> 24) & 0xFF;
    if a == 0 {
        return dst;
    }
    if a == 0xFF {
        return src;
    }
    let inv = 255 - a;
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;
    let sr = (src >> 16) & 0xFF;
    let sg = (src >> 8) & 0xFF;
    let sb = src & 0xFF;
    let r = (sr * a + dr * inv + 127) / 255;
    let g = (sg * a + dg * inv + 127) / 255;
    let b = (sb * a + db * inv + 127) / 255;
    let da = (dst >> 24) & 0xFF;
    let oa = a + (da * inv + 127) / 255;
    (oa << 24) | (r << 16) | (g << 8) | b
}

/// Barycentric color lerp across three ARGB endpoints.
#[inline]
fn lerp3(c0: u32, c1: u32, c2: u32, w0: u64, w1: u64, w2: u64, area: u64) -> u32 {
    if area == 0 {
        return c0;
    }
    let mix = |s0: u64, s1: u64, s2: u64| -> u64 { (s0 * w0 + s1 * w1 + s2 * w2) / area };
    let a = mix(
        (c0 >> 24) as u64 & 0xFF,
        (c1 >> 24) as u64 & 0xFF,
        (c2 >> 24) as u64 & 0xFF,
    );
    let r = mix(
        (c0 >> 16) as u64 & 0xFF,
        (c1 >> 16) as u64 & 0xFF,
        (c2 >> 16) as u64 & 0xFF,
    );
    let g = mix(
        (c0 >> 8) as u64 & 0xFF,
        (c1 >> 8) as u64 & 0xFF,
        (c2 >> 8) as u64 & 0xFF,
    );
    let b = mix(c0 as u64 & 0xFF, c1 as u64 & 0xFF, c2 as u64 & 0xFF);
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

#[inline]
fn avg9(p0: u32, p1: u32, p2: u32, p3: u32, p4: u32, p5: u32, p6: u32, p7: u32, p8: u32) -> u32 {
    let acc = |sh: u32| -> u32 {
        let sum = ((p0 >> sh) & 0xFF)
            + ((p1 >> sh) & 0xFF)
            + ((p2 >> sh) & 0xFF)
            + ((p3 >> sh) & 0xFF)
            + ((p4 >> sh) & 0xFF)
            + ((p5 >> sh) & 0xFF)
            + ((p6 >> sh) & 0xFF)
            + ((p7 >> sh) & 0xFF)
            + ((p8 >> sh) & 0xFF);
        sum / 9
    };
    (acc(24) << 24) | (acc(16) << 16) | (acc(8) << 8) | acc(0)
}
