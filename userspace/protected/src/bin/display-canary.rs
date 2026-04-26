// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal 2D display canary app.
//!
//! Purpose: validate the compositor + window present path independently of 3D submit.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;
use graphos_app_sdk::window::Window;

const WIN_W: u32 = 640;
const WIN_H: u32 = 360;
const STALL_FRAME_LIMIT: u32 = 120;

const PALETTE: [u32; 6] = [
    0xFF101420,
    0xFF16324A,
    0xFF1D4F55,
    0xFF3C6E3A,
    0xFF7A6A2C,
    0xFF7A3B2C,
];

fn append_u32_dec(out: &mut [u8], len: &mut usize, mut value: u32) {
    let mut tmp = [0u8; 10];
    let mut n = 0usize;
    if value == 0 {
        if *len < out.len() {
            out[*len] = b'0';
            *len += 1;
        }
        return;
    }
    while value != 0 {
        tmp[n] = b'0' + (value % 10) as u8;
        value /= 10;
        n += 1;
    }
    while n != 0 {
        n -= 1;
        if *len < out.len() {
            out[*len] = tmp[n];
            *len += 1;
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::write_line(b"[display-canary] starting\n");

    let input_channel = runtime::channel_create(64).unwrap_or(0);
    if input_channel == 0 {
        runtime::write_line(b"[display-canary] input channel create failed\n");
        runtime::exit(0xE0)
    }

    let mut win = match Window::open(WIN_W, WIN_H, 40, 40, input_channel) {
        Some(win) => win,
        None => {
            runtime::write_line(b"[display-canary] window open failed\n");
            runtime::exit(0xE1)
        }
    };
    win.request_focus();

    let mut frame = 0u32;
    let mut present_ok = 0u32;
    let mut present_fail = 0u32;
    let mut last_present_ok_frame = 0u32;
    let mut stall_reported = false;

    loop {
        let color = PALETTE[(frame as usize) % PALETTE.len()];
        {
            let mut canvas = win.canvas();
            canvas.clear(color);
            canvas.fill_rect(24, 24, 120, 18, 0xFFFFFFFF);
            canvas.fill_rect(24, 48, ((frame % 240) + 1), 14, 0xFF000000);
        }

        if win.present() {
            present_ok = present_ok.wrapping_add(1);
            last_present_ok_frame = frame;
            stall_reported = false;
            if present_ok == 1 {
                runtime::write_line(b"[display-canary] first present ok\n");
            }
        } else {
            present_fail = present_fail.wrapping_add(1);
            if present_fail <= 8 || present_fail % 60 == 0 {
                runtime::write_line(b"[display-canary] present failed\n");
            }
        }

        if frame <= 16 || frame % 60 == 0 {
            let mut line = [0u8; 96];
            let mut len = 0usize;
            const PREFIX: &[u8] = b"[display-canary] frame_count=";
            line[len..len + PREFIX.len()].copy_from_slice(PREFIX);
            len += PREFIX.len();
            append_u32_dec(&mut line, &mut len, frame);

            const MID_OK: &[u8] = b" present_ok=";
            line[len..len + MID_OK.len()].copy_from_slice(MID_OK);
            len += MID_OK.len();
            append_u32_dec(&mut line, &mut len, present_ok);

            line[len] = b'\n';
            len += 1;
            runtime::write_line(&line[..len]);
        }

        if frame.wrapping_sub(last_present_ok_frame) >= STALL_FRAME_LIMIT && !stall_reported {
            runtime::write_line(b"[display-canary] stall: no successful present in 120 frames\n");
            stall_reported = true;
        }

        frame = frame.wrapping_add(1);
        graphos_app_sdk::sys::sleep_ticks(8);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}
