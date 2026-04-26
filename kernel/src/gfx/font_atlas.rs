// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Multi-size bitmap font atlas for the GraphOS kernel graphics layer.
//!
//! Extends the base `font8x8` 8×8 glyphs to larger pixel sizes by
//! integer-scaling each row/column.  No heap allocation is required: the
//! scaled glyph is produced directly into a caller-supplied pixel buffer.
//!
//! # Supported sizes
//! | `FontSize`   | Cell W×H | Scale factor |
//! |-------------|----------|--------------|
//! | `Px8`       | 8×8      | 1×           |
//! | `Px16`      | 8×16     | 1×2 (rows doubled) |
//! | `Px16sq`    | 16×16    | 2×2 (square)       |
//! | `Px32`      | 32×32    | 4×4 (display title) |

/// Font rendering size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSize {
    /// 8×8 — standard terminal / debug text.
    Px8,
    /// 8×16 — comfortable reading size (rows doubled, width preserved).
    Px16,
    /// 16×16 — square medium size.
    Px16sq,
    /// 32×32 — large display / heading size.
    Px32,
}

#[derive(Clone, Copy)]
pub struct FontColors {
    pub fg: u32,
    pub bg: u32,
}

pub struct FontTarget<'a> {
    pub buf: &'a mut [u32],
    pub stride: usize,
    pub buf_h: usize,
}

impl FontSize {
    /// Width in pixels of one character cell.
    pub const fn cell_w(self) -> u32 {
        match self {
            Self::Px8 | Self::Px16 => 8,
            Self::Px16sq => 16,
            Self::Px32 => 32,
        }
    }

    /// Height in pixels of one character cell.
    pub const fn cell_h(self) -> u32 {
        match self {
            Self::Px8 => 8,
            Self::Px16 | Self::Px16sq => 16,
            Self::Px32 => 32,
        }
    }

    /// Row scale factor (how many output rows per font8x8 row).
    const fn row_scale(self) -> u32 {
        match self {
            Self::Px8 => 1,
            Self::Px16 | Self::Px16sq => 2,
            Self::Px32 => 4,
        }
    }

    /// Column scale factor.
    const fn col_scale(self) -> u32 {
        match self {
            Self::Px8 | Self::Px16 => 1,
            Self::Px16sq => 2,
            Self::Px32 => 4,
        }
    }
}

/// Render a single ASCII character at `(x, y)` in a pixel buffer.
///
/// `buf` must be `stride * height` pixels (u32 ARGB values).
/// Pixels outside the buffer bounds are silently clipped.
pub fn draw_char(
    target: &mut FontTarget<'_>,
    x: i32,
    y: i32,
    ch: u8,
    colors: FontColors,
    size: FontSize,
) {
    use font8x8::UnicodeFonts;
    let printable = if (0x20..=0x7e).contains(&ch) {
        ch as char
    } else {
        '?'
    };
    let glyph = font8x8::BASIC_FONTS
        .get(printable)
        .or_else(|| font8x8::BASIC_FONTS.get('?'))
        .unwrap_or([0u8; 8]);

    let rs = size.row_scale() as usize;
    let cs = size.col_scale() as usize;

    for (src_row, &bits) in glyph.iter().enumerate() {
        for rr in 0..rs {
            let py = y + (src_row * rs + rr) as i32;
            if py < 0 || py as usize >= target.buf_h {
                continue;
            }
            for src_col in 0..8usize {
                let lit = (bits >> src_col) & 1 != 0;
                let color = if lit { colors.fg } else { colors.bg };
                for cc in 0..cs {
                    let px = x + (src_col * cs + cc) as i32;
                    if px < 0 || px as usize >= target.stride {
                        continue;
                    }
                    let idx = py as usize * target.stride + px as usize;
                    if idx < target.buf.len() {
                        target.buf[idx] = color;
                    }
                }
            }
        }
    }
}

/// Render a byte string at `(x, y)`.
///
/// Returns the x position after the last character.
pub fn draw_text(
    target: &mut FontTarget<'_>,
    x: i32,
    y: i32,
    text: &[u8],
    colors: FontColors,
    size: FontSize,
) -> i32 {
    let advance = size.cell_w() as i32;
    let mut cx = x;
    for &b in text {
        draw_char(target, cx, y, b, colors, size);
        cx += advance;
    }
    cx
}
