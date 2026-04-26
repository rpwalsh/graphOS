// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AArch64 serial output via PL011 UART.

/// Base physical/virtual address of the PL011 UART on QEMU `virt` machine.
const PL011_BASE: u64 = 0x0900_0000;

const UARTDR: *mut u32 = PL011_BASE as *mut u32;
const UARTFR: *const u32 = (PL011_BASE + 0x18) as *const u32;
const TXFF: u32 = 1 << 5;
const RXFE: u32 = 1 << 4;

pub fn init() {
    // PL011 is enabled by firmware in the QEMU virt flow.
}

pub fn write_byte(b: u8) {
    unsafe {
        while core::ptr::read_volatile(UARTFR) & TXFF != 0 {}
        core::ptr::write_volatile(UARTDR, b as u32);
    }
}

pub fn write_bytes(msg: &[u8]) {
    for &b in msg {
        write_byte(b);
    }
    crate::apps::logview::record_bytes(msg);
    crate::drivers::display::note_serial_mirror(msg);
}

pub fn write_line(msg: &[u8]) {
    write_bytes_raw(msg);
    write_bytes_raw(b"\r\n");
    crate::apps::logview::record_bytes(msg);
    crate::apps::logview::record_bytes(b"\n");
    crate::drivers::display::note_serial_mirror(msg);
    crate::drivers::display::note_serial_mirror(b"\n");
}

pub fn write_bytes_raw(msg: &[u8]) {
    for &b in msg {
        write_byte(b);
    }
}

pub fn write_line_raw(msg: &[u8]) {
    write_bytes_raw(msg);
    write_bytes_raw(b"\r\n");
}

pub fn write_hex(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_hex(val, true, &mut buf);
    write_bytes(&buf[..len]);
}

pub fn write_hex_inline(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_hex(val, false, &mut buf);
    write_bytes(&buf[..len]);
}

pub fn write_u64_dec(val: u64) {
    let mut buf = [0u8; 22];
    let len = format_decimal_line(val, &mut buf);
    write_bytes(&buf[..len]);
}

pub fn write_u64_dec_inline(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_decimal(val, &mut buf);
    write_bytes(&buf[..len]);
}

pub fn write_u64_dec_inline_raw(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_decimal(val, &mut buf);
    write_bytes_raw(&buf[..len]);
}

pub fn try_read_byte() -> Option<u8> {
    unsafe {
        if core::ptr::read_volatile(UARTFR) & RXFE != 0 {
            return None;
        }
        Some((core::ptr::read_volatile(UARTDR) & 0xFF) as u8)
    }
}

fn format_hex(val: u64, newline: bool, buf: &mut [u8; 20]) -> usize {
    let hex = b"0123456789abcdef";
    let mut pos = 0usize;
    buf[pos] = b'0';
    pos += 1;
    buf[pos] = b'x';
    pos += 1;
    if newline {
        for i in (0..16).rev() {
            let nibble = ((val >> (i * 4)) & 0xF) as usize;
            buf[pos] = hex[nibble];
            pos += 1;
        }
        buf[pos] = b'\r';
        pos += 1;
        buf[pos] = b'\n';
        pos + 1
    } else {
        let mut started = false;
        for i in (0..16).rev() {
            let nibble = ((val >> (i * 4)) & 0xF) as usize;
            if nibble != 0 {
                started = true;
            }
            if started || i == 0 {
                buf[pos] = hex[nibble];
                pos += 1;
            }
        }
        pos
    }
}

fn format_decimal(val: u64, buf: &mut [u8; 20]) -> usize {
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut pos = 0usize;
    let mut v = val;
    while v > 0 {
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
        pos += 1;
    }
    reverse_in_place(&mut buf[..pos]);
    pos
}

fn format_decimal_line(val: u64, buf: &mut [u8; 22]) -> usize {
    let mut digits = [0u8; 20];
    let len = format_decimal(val, &mut digits);
    buf[..len].copy_from_slice(&digits[..len]);
    buf[len] = b'\r';
    buf[len + 1] = b'\n';
    len + 2
}

fn reverse_in_place(slice: &mut [u8]) {
    let mut i = 0usize;
    let mut j = slice.len().saturating_sub(1);
    while i < j {
        slice.swap(i, j);
        i += 1;
        j -= 1;
    }
}
