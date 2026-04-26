#![no_std]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#![no_main]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    runtime::panic(info)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    runtime::leaf_service(b"[sysd] protected observatory online\n", b"sysd")
}
