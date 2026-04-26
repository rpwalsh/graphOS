// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! COM1 serial port driver (16550 UART).
//!
//! Provides the earliest possible diagnostic output channel.

use spin::Mutex;
use uart_16550::SerialPort;
use x86_64::instructions::interrupts;

/// COM1 base I/O port.
const COM1: u16 = 0x3F8;
const COM1_DATA: u16 = COM1;
const COM1_LINE_STATUS: u16 = COM1 + 5;
const LSR_DATA_READY: u8 = 1 << 0;

static SERIAL: Mutex<Option<SerialPort>> = Mutex::new(None);

/// Initialise the COM1 serial port.
///
/// Must be called exactly once, before any other serial function.
/// If not called, all output functions silently discard their data.
pub fn init() {
    let port = unsafe {
        // SAFETY: COM1 (0x3F8) is the standard first serial port on x86 PCs.
        // `SerialPort::new` stores the base address; `init()` writes the UART
        // divisor, FIFO, and modem-control registers to configure 115200 8N1.
        // This is a required hardware-init step and is non-destructive to any
        // other device because 0x3F8-0x3FF is exclusively the 16550 range.
        let mut p = SerialPort::new(COM1);
        p.init();
        p
    };
    interrupts::without_interrupts(|| {
        *SERIAL.lock() = Some(port);
    });
}

/// Write raw bytes to serial without any line termination.
pub fn write_byte(byte: u8) {
    write_bytes(&[byte]);
}

/// Write raw bytes to serial without any line termination.
pub fn write_bytes(msg: &[u8]) {
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in msg {
                port.send(b);
            }
        }
    });
    crate::apps::logview::record_bytes(msg);
    crate::drivers::display::note_serial_mirror(msg);
}

/// Write a line of bytes followed by `\r\n` to the serial port.
pub fn write_line(msg: &[u8]) {
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in msg {
                port.send(b);
            }
            port.send(b'\r');
            port.send(b'\n');
        }
    });
    crate::apps::logview::record_bytes(msg);
    crate::apps::logview::record_bytes(b"\n");
    crate::drivers::display::note_serial_mirror(msg);
    crate::drivers::display::note_serial_mirror(b"\n");
}

/// Write raw bytes directly to the UART without mirroring them into the log surfaces.
pub fn write_bytes_raw(msg: &[u8]) {
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in msg {
                port.send(b);
            }
        }
    });
}

/// Write a raw line directly to the UART without log mirroring.
pub fn write_line_raw(msg: &[u8]) {
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in msg {
                port.send(b);
            }
            port.send(b'\r');
            port.send(b'\n');
        }
    });
}

/// Write a 64-bit value as `0x` hex to the serial port, followed by `\r\n`.
pub fn write_hex(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_hex(val, true, &mut buf);
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in &buf[..len] {
                port.send(b);
            }
        }
    });
    crate::apps::logview::record_bytes(&buf[..len]);
    crate::drivers::display::note_serial_mirror(&buf[..len]);
}

/// Write a 64-bit value as `0x` hex inline (no newline).
pub fn write_hex_inline(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_hex(val, false, &mut buf);
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in &buf[..len] {
                port.send(b);
            }
        }
    });
    crate::apps::logview::record_bytes(&buf[..len]);
    crate::drivers::display::note_serial_mirror(&buf[..len]);
}

/// Write a u64 as a decimal string with no trailing newline.
pub fn write_u64_dec_inline(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_decimal(val, &mut buf);
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in &buf[..len] {
                port.send(b);
            }
        }
    });
    crate::apps::logview::record_bytes(&buf[..len]);
    crate::drivers::display::note_serial_mirror(&buf[..len]);
}

/// Write a u64 as a decimal string with no trailing newline and no mirroring.
pub fn write_u64_dec_inline_raw(val: u64) {
    let mut buf = [0u8; 20];
    let len = format_decimal(val, &mut buf);
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in &buf[..len] {
                port.send(b);
            }
        }
    });
}

/// Write a u64 as a decimal string followed by `\r\n`.
///
/// Holds the serial lock for the entire emission to prevent interleaving.
pub fn write_u64_dec(val: u64) {
    let mut buf = [0u8; 22];
    let len = format_decimal_line(val, &mut buf);
    interrupts::without_interrupts(|| {
        if let Some(ref mut port) = *SERIAL.lock() {
            for &b in &buf[..len] {
                port.send(b);
            }
        }
    });
    crate::apps::logview::record_bytes(&buf[..len]);
    crate::drivers::display::note_serial_mirror(&buf[..len]);
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
    // Emit most-significant digit first.
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

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
    val
}

/// Poll for one received byte from COM1.
///
/// Returns `None` if the UART has no pending input.
pub fn try_read_byte() -> Option<u8> {
    interrupts::without_interrupts(|| {
        if SERIAL.lock().is_none() {
            return None;
        }

        let status = unsafe { inb(COM1_LINE_STATUS) };
        if status & LSR_DATA_READY == 0 {
            return None;
        }

        Some(unsafe { inb(COM1_DATA) })
    })
}
