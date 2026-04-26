// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Shared pixel blit helpers used by scanout paths.
//!
//! This module is pure CPU-side logic so it can be validated in host tests
//! without booting the kernel or touching virtio hardware.

#[allow(clippy::too_many_arguments)]
pub fn blit_pixels_into(
    dst: &mut [u32],
    dst_stride: usize,
    dst_w: usize,
    dst_h: usize,
    src: &[u32],
    src_w: usize,
    src_h: usize,
    dst_x: i32,
    dst_y: i32,
    scale_fp: u16,
    opacity: u8,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 || opacity == 0 {
        return;
    }
    if src.len() < src_w.saturating_mul(src_h) || dst.len() < dst_stride.saturating_mul(dst_h) {
        return;
    }

    let scale = scale_fp.max(1) as u32;
    let out_w = ((src_w as u64).saturating_mul(scale as u64) / 1024) as i32;
    let out_h = ((src_h as u64).saturating_mul(scale as u64) / 1024) as i32;
    if out_w <= 0 || out_h <= 0 {
        return;
    }

    let clip_w = dst_w as i32;
    let clip_h = dst_h as i32;
    let x0 = dst_x.clamp(0, clip_w);
    let y0 = dst_y.clamp(0, clip_h);
    let x1 = dst_x.saturating_add(out_w).clamp(0, clip_w);
    let y1 = dst_y.saturating_add(out_h).clamp(0, clip_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let a = opacity as u32;
    let ia = 255u32.saturating_sub(a);

    for dy in y0..y1 {
        let sy = (((dy - dst_y) as i64).saturating_mul(1024) / scale as i64) as usize;
        if sy >= src_h {
            continue;
        }
        let row_base = dy as usize * dst_stride;
        for dx in x0..x1 {
            let sx = (((dx - dst_x) as i64).saturating_mul(1024) / scale as i64) as usize;
            if sx >= src_w {
                continue;
            }

            let src_px = src[sy * src_w + sx];
            let out_px = if opacity < 255 {
                let dst_idx = row_base + dx as usize;
                let dst_px = dst[dst_idx];
                let sr = (src_px >> 16) & 0xFF;
                let sg = (src_px >> 8) & 0xFF;
                let sb = src_px & 0xFF;
                let dr = (dst_px >> 16) & 0xFF;
                let dg = (dst_px >> 8) & 0xFF;
                let db = dst_px & 0xFF;
                let r = ((dr * ia + sr * a) / 255) & 0xFF;
                let g = ((dg * ia + sg * a) / 255) & 0xFF;
                let b = ((db * ia + sb * a) / 255) & 0xFF;
                0xFF00_0000 | (r << 16) | (g << 8) | b
            } else {
                src_px
            };

            dst[row_base + dx as usize] = out_px;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::blit_pixels_into;

    #[test]
    fn copies_pixels_without_booting() {
        let src = [0xFF11_2233, 0xFF44_5566, 0xFF77_8899, 0xFFAA_BBCC];
        let mut dst = [0u32; 8 * 6];

        blit_pixels_into(&mut dst, 8, 8, 6, &src, 2, 2, 2, 1, 1024, 255);

        let idx = |x: usize, y: usize| y * 8 + x;
        assert_eq!(dst[idx(2, 1)], 0xFF11_2233);
        assert_eq!(dst[idx(3, 1)], 0xFF44_5566);
        assert_eq!(dst[idx(2, 2)], 0xFF77_8899);
        assert_eq!(dst[idx(3, 2)], 0xFFAA_BBCC);
        assert_eq!(dst[idx(1, 1)], 0);
    }

    #[test]
    fn clips_offscreen_writes_without_booting() {
        let src = [0xFF00_00FF, 0xFF00_FF00, 0xFFFF_0000, 0xFFFF_FF00];
        let mut dst = [0u32; 4 * 3];

        blit_pixels_into(&mut dst, 4, 4, 3, &src, 2, 2, -1, -1, 1024, 255);

        let idx = |x: usize, y: usize| y * 4 + x;
        assert_eq!(dst[idx(0, 0)], 0xFFFF_FF00);
        assert_eq!(dst[idx(1, 0)], 0);
        assert_eq!(dst[idx(0, 1)], 0);
    }
}
