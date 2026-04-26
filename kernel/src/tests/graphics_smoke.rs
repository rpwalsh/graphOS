// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Regression tests for desktop rendering primitives.

use crate::diag;
use crate::gfx::canvas::{Canvas, ClipRect};
use crate::gfx::surface::Surface;
use crate::input::event::InputEvent;
use crate::wm::window::{Rect, Window};

pub fn run_tests() -> u32 {
    let mut failures = 0;

    if !test_rect_damage_math() {
        failures += 1;
    }
    if !test_canvas_clipped_rendering() {
        failures += 1;
    }
    if !test_window_content_cache() {
        failures += 1;
    }
    if !test_unit_cube_program_resolution() {
        failures += 1;
    }
    if !test_e2e_compositor_not_declared_for_cube_boot() {
        failures += 1;
    }

    failures
}

fn test_rect_damage_math() -> bool {
    let a = Rect::new(10, 10, 20, 15);
    let b = Rect::new(24, 4, 12, 18);
    let union = a.union(b);

    if union.x != 10 || union.y != 4 || union.w != 26 || union.h != 21 {
        diag::test_fail(b"gfx: rect union math incorrect");
        return false;
    }
    if !a.intersects(b) || !union.intersects(a) || !union.intersects(b) {
        diag::test_fail(b"gfx: rect intersection math incorrect");
        return false;
    }

    diag::test_pass(b"gfx: rect damage math");
    true
}

fn test_canvas_clipped_rendering() -> bool {
    let mut target = Surface::new(8, 8, 0);
    let source = Surface::new(4, 4, 0x00334455);

    {
        let mut canvas = Canvas::with_clip(&mut target, ClipRect::new(2, 2, 3, 3));
        canvas.clear(0x00101010);
        canvas.fill_rect(0, 0, 8, 8, 0x00202020);
        canvas.blit(&source, 1, 1);
    }

    for y in 0..8 {
        for x in 0..8 {
            let pixel = target.pixel(x, y).unwrap_or(0);
            let in_clip = (2..5).contains(&x) && (2..5).contains(&y);
            let expected = if in_clip { 0x00334455 } else { 0 };
            if pixel != expected {
                diag::test_fail(b"gfx: clip/blit region leaked outside damage");
                return false;
            }
        }
    }

    diag::test_pass(b"gfx: clipped fill + blit");
    true
}

fn test_window_content_cache() -> bool {
    let mut window = Window::new_terminal(1, 32, 48, b"ok");
    if !window.is_content_dirty() {
        diag::test_fail(b"gfx: new window should begin dirty");
        return false;
    }

    window.render_if_dirty();
    if window.is_content_dirty() {
        diag::test_fail(b"gfx: render should clear content dirty flag");
        return false;
    }

    let first_checksum = checksum(window.surface.pixels());
    window.render_if_dirty();
    let second_checksum = checksum(window.surface.pixels());
    if first_checksum != second_checksum {
        diag::test_fail(b"gfx: cached window rerender mutated clean surface");
        return false;
    }

    window.handle_event(InputEvent::Text(b'x'));
    if !window.is_content_dirty() {
        diag::test_fail(b"gfx: input should invalidate window content");
        return false;
    }

    window.render_if_dirty();
    if window.is_content_dirty() {
        diag::test_fail(b"gfx: rerender should clear dirty after input");
        return false;
    }

    diag::test_pass(b"gfx: window content cache");
    true
}

fn checksum(pixels: &[u32]) -> u64 {
    pixels.iter().fold(0u64, |acc, &pixel| {
        acc.wrapping_mul(16_777_619)
            .wrapping_add(pixel as u64 ^ 0x9e37_79b9)
    })
}

fn test_unit_cube_program_resolution() -> bool {
    if crate::userland::image_for_named_service(b"cube").is_none() {
        diag::test_fail(b"gfx: cube program resolution failed");
        return false;
    }

    diag::test_pass(b"gfx: cube program resolution");
    true
}

fn test_e2e_compositor_not_declared_for_cube_boot() -> bool {
    if crate::userland::manifest_declares_service(b"compositor") {
        diag::test_fail(b"gfx: compositor unexpectedly declared in cube boot manifest");
        return false;
    }

    diag::test_pass(b"gfx: cube boot manifest leaves compositor undeclared");
    true
}
