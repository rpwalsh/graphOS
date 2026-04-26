// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graph-first display driver over the boot scanout surface.
//!
//! This is not a compositor or GPU driver yet. It is the kernel-owned display
//! endpoint for the current UEFI/GOP scanout, with explicit telemetry so the
//! graph substrate can reason about scanout activity instead of treating the
//! display surface as an anonymous side effect.

use crate::arch::serial;
use crate::bootinfo::{BootInfo, FramebufferFormat};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use super::ProbeResult;

#[derive(Clone, Copy)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: FramebufferFormat,
}

#[derive(Clone, Copy)]
pub struct DisplayTelemetrySnapshot {
    pub online: bool,
    pub graph_first: bool,
    pub framebuffer_addr: u64,
    pub framebuffer_bytes: u64,
    pub mode: DisplayMode,
    pub backbuffer_bytes: usize,
    pub full_presents: u64,
    pub partial_presents: u64,
    pub native_fills: u64,
    pub presented_pixels: u64,
    pub presented_bytes: u64,
    pub mirrored_serial_bytes: u64,
    pub mirrored_serial_lines: u64,
    pub peak_dirty_pixels: u64,
    pub last_dirty_pixels: u64,
}

struct DisplayState {
    online: bool,
    graph_first: bool,
    framebuffer_addr: u64,
    framebuffer_bytes: u64,
    mode: DisplayMode,
}

impl DisplayState {
    const fn new() -> Self {
        Self {
            online: false,
            graph_first: true,
            framebuffer_addr: 0,
            framebuffer_bytes: 0,
            mode: DisplayMode {
                width: 0,
                height: 0,
                stride: 0,
                format: FramebufferFormat::Unknown,
            },
        }
    }
}

static DISPLAY: Mutex<DisplayState> = Mutex::new(DisplayState::new());
static BACKBUFFER_BYTES: AtomicUsize = AtomicUsize::new(0);
static FULL_PRESENTS: AtomicU64 = AtomicU64::new(0);
static PARTIAL_PRESENTS: AtomicU64 = AtomicU64::new(0);
static NATIVE_FILLS: AtomicU64 = AtomicU64::new(0);
static PRESENTED_PIXELS: AtomicU64 = AtomicU64::new(0);
static PRESENTED_BYTES: AtomicU64 = AtomicU64::new(0);
static MIRRORED_SERIAL_BYTES: AtomicU64 = AtomicU64::new(0);
static MIRRORED_SERIAL_LINES: AtomicU64 = AtomicU64::new(0);
static PEAK_DIRTY_PIXELS: AtomicU64 = AtomicU64::new(0);
static LAST_DIRTY_PIXELS: AtomicU64 = AtomicU64::new(0);

pub fn attach_boot_framebuffer(info: &BootInfo) {
    reset_telemetry();

    let mut display = DISPLAY.lock();
    display.online =
        info.framebuffer_addr != 0 && info.framebuffer_width != 0 && info.framebuffer_height != 0;
    display.graph_first = true;
    display.framebuffer_addr = info.framebuffer_addr;
    display.framebuffer_bytes = info.framebuffer_size_bytes();
    display.mode = DisplayMode {
        width: info.framebuffer_width,
        height: info.framebuffer_height,
        stride: info.framebuffer_stride,
        format: info.framebuffer_format,
    };
}

/// Repoint display writes to a runtime framebuffer (for example virtio-gpu scanout backing).
///
/// This keeps the graph-first display pipeline intact while detaching from the
/// early-boot GOP framebuffer.
pub fn attach_runtime_framebuffer(
    framebuffer_addr: u64,
    width: u32,
    height: u32,
    stride: u32,
    format: FramebufferFormat,
) {
    if framebuffer_addr == 0 || width == 0 || height == 0 || stride == 0 {
        return;
    }

    reset_telemetry();

    let mut display = DISPLAY.lock();
    display.online = true;
    display.graph_first = true;
    display.framebuffer_addr = framebuffer_addr;
    display.framebuffer_bytes = (stride as u64)
        .saturating_mul(height as u64)
        .saturating_mul(core::mem::size_of::<u32>() as u64);
    display.mode = DisplayMode {
        width,
        height,
        stride,
        format,
    };
}

pub fn probe_boot_display() -> ProbeResult {
    if DISPLAY.lock().online {
        ProbeResult::Bound
    } else {
        ProbeResult::NoMatch
    }
}

pub fn set_backbuffer_bytes(bytes: usize) {
    BACKBUFFER_BYTES.store(bytes, Ordering::Release);
}

pub fn note_full_present() {
    FULL_PRESENTS.fetch_add(1, Ordering::Relaxed);
}

pub fn note_present(pixels: u64, full_surface_pixels: u64) {
    PARTIAL_PRESENTS.fetch_add(1, Ordering::Relaxed);
    record_display_write(pixels, full_surface_pixels);
}

pub fn note_native_fill(pixels: u64) {
    NATIVE_FILLS.fetch_add(1, Ordering::Relaxed);
    let full_surface_pixels = visible_pixels();
    record_display_write(pixels, full_surface_pixels);
}

pub fn note_serial_mirror(bytes: &[u8]) {
    MIRRORED_SERIAL_BYTES.fetch_add(bytes.len() as u64, Ordering::Relaxed);

    let mut lines = 0u64;
    for &byte in bytes {
        if byte == b'\n' {
            lines += 1;
        }
    }
    if lines != 0 {
        MIRRORED_SERIAL_LINES.fetch_add(lines, Ordering::Relaxed);
    }
}

pub fn telemetry_snapshot() -> DisplayTelemetrySnapshot {
    let display = DISPLAY.lock();
    DisplayTelemetrySnapshot {
        online: display.online,
        graph_first: display.graph_first,
        framebuffer_addr: display.framebuffer_addr,
        framebuffer_bytes: display.framebuffer_bytes,
        mode: display.mode,
        backbuffer_bytes: BACKBUFFER_BYTES.load(Ordering::Acquire),
        full_presents: FULL_PRESENTS.load(Ordering::Relaxed),
        partial_presents: PARTIAL_PRESENTS.load(Ordering::Relaxed),
        native_fills: NATIVE_FILLS.load(Ordering::Relaxed),
        presented_pixels: PRESENTED_PIXELS.load(Ordering::Relaxed),
        presented_bytes: PRESENTED_BYTES.load(Ordering::Relaxed),
        mirrored_serial_bytes: MIRRORED_SERIAL_BYTES.load(Ordering::Relaxed),
        mirrored_serial_lines: MIRRORED_SERIAL_LINES.load(Ordering::Relaxed),
        peak_dirty_pixels: PEAK_DIRTY_PIXELS.load(Ordering::Relaxed),
        last_dirty_pixels: LAST_DIRTY_PIXELS.load(Ordering::Relaxed),
    }
}

pub fn log_summary() {
    let snapshot = telemetry_snapshot();
    if !snapshot.online {
        serial::write_line(b"[display] graph-display0 offline");
        return;
    }

    serial::write_bytes(b"[display] graph-display0 bound: ");
    serial::write_u64_dec_inline(snapshot.mode.width as u64);
    serial::write_bytes(b"x");
    serial::write_u64_dec_inline(snapshot.mode.height as u64);
    serial::write_bytes(b" stride=");
    serial::write_u64_dec_inline(snapshot.mode.stride as u64);
    serial::write_bytes(b" format=");
    serial::write_bytes(snapshot.mode.format.as_bytes());
    serial::write_bytes(b" graph-first=");
    serial::write_line(if snapshot.graph_first {
        b"yes" as &[u8]
    } else {
        b"no" as &[u8]
    });

    serial::write_bytes(b"[display] framebuffer bytes:    ");
    serial::write_u64_dec(snapshot.framebuffer_bytes);
    serial::write_bytes(b"[display] backbuffer bytes:     ");
    serial::write_u64_dec(snapshot.backbuffer_bytes as u64);
    serial::write_bytes(b"[display] full presents:        ");
    serial::write_u64_dec(snapshot.full_presents);
    serial::write_bytes(b"[display] partial presents:     ");
    serial::write_u64_dec(snapshot.partial_presents);
    serial::write_bytes(b"[display] native fills:         ");
    serial::write_u64_dec(snapshot.native_fills);
    serial::write_bytes(b"[display] presented pixels:     ");
    serial::write_u64_dec(snapshot.presented_pixels);
    serial::write_bytes(b"[display] presented bytes:      ");
    serial::write_u64_dec(snapshot.presented_bytes);
    serial::write_bytes(b"[display] mirrored serial bytes:");
    serial::write_u64_dec(snapshot.mirrored_serial_bytes);
    serial::write_bytes(b"[display] mirrored serial lines:");
    serial::write_u64_dec(snapshot.mirrored_serial_lines);
    serial::write_bytes(b"[display] peak dirty pixels:    ");
    serial::write_u64_dec(snapshot.peak_dirty_pixels);
    serial::write_bytes(b"[display] last dirty pixels:    ");
    serial::write_u64_dec(snapshot.last_dirty_pixels);
}

fn reset_telemetry() {
    BACKBUFFER_BYTES.store(0, Ordering::Release);
    FULL_PRESENTS.store(0, Ordering::Relaxed);
    PARTIAL_PRESENTS.store(0, Ordering::Relaxed);
    NATIVE_FILLS.store(0, Ordering::Relaxed);
    PRESENTED_PIXELS.store(0, Ordering::Relaxed);
    PRESENTED_BYTES.store(0, Ordering::Relaxed);
    MIRRORED_SERIAL_BYTES.store(0, Ordering::Relaxed);
    MIRRORED_SERIAL_LINES.store(0, Ordering::Relaxed);
    PEAK_DIRTY_PIXELS.store(0, Ordering::Relaxed);
    LAST_DIRTY_PIXELS.store(0, Ordering::Relaxed);
}

fn visible_pixels() -> u64 {
    let display = DISPLAY.lock();
    display.mode.width as u64 * display.mode.height as u64
}

fn record_display_write(pixels: u64, full_surface_pixels: u64) {
    if pixels == 0 {
        return;
    }

    let bytes = pixels.saturating_mul(4);
    PRESENTED_PIXELS.fetch_add(pixels, Ordering::Relaxed);
    PRESENTED_BYTES.fetch_add(bytes, Ordering::Relaxed);
    LAST_DIRTY_PIXELS.store(pixels, Ordering::Relaxed);
    update_peak(&PEAK_DIRTY_PIXELS, pixels);

    crate::graph::twin::ingest_display_present(
        crate::arch::timer::ticks(),
        pixels,
        bytes,
        full_surface_pixels,
    );
}

fn update_peak(target: &AtomicU64, value: u64) {
    let mut current = target.load(Ordering::Relaxed);
    while value > current {
        match target.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}
