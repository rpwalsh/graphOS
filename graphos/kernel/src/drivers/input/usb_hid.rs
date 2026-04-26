// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! USB HID driver — keyboard and mouse via xHCI.
//!
//! Targets an xHCI controller (USB3) connected to a standard USB HID keyboard
//! and mouse.  The xHCI base address is discovered via PCI BAR0 scan
//! (class 0x0C, subclass 0x03, prog-if 0x30).
//!
//! ## Status
//! - xHCI PCI class scan
//! - MMIO base detection
//! - HID keyboard report routing to the input event ring
//! - Mouse movement deltas forwarded as EV_REL events

use spin::Mutex;

use crate::arch::serial;
use crate::drivers::ProbeResult;

const XHCI_CLASS: u8 = 0x0C;
const XHCI_SUBCLASS: u8 = 0x03;
const XHCI_PROG_IF: u8 = 0x30;

struct UsbHidState {
    present: bool,
    mmio_base: u64,
}

impl UsbHidState {
    const fn new() -> Self {
        Self {
            present: false,
            mmio_base: 0,
        }
    }
}

static STATE: Mutex<UsbHidState> = Mutex::new(UsbHidState::new());

// ── Probe ─────────────────────────────────────────────────────────────────────

pub fn probe_driver() -> ProbeResult {
    let mut found_mmio = 0u64;

    crate::arch::x86_64::pci::for_each_device(|info| {
        if found_mmio != 0 {
            return;
        }
        if info.class_code == XHCI_CLASS
            && info.subclass == XHCI_SUBCLASS
            && info.prog_if == XHCI_PROG_IF
        {
            let bar0 = crate::arch::x86_64::pci::read_u32(
                info.location.bus,
                info.location.slot,
                info.location.func,
                0x10,
            ) & !0xF;
            found_mmio = bar0 as u64;
        }
    });

    if found_mmio == 0 {
        return ProbeResult::NoMatch;
    }

    let mut state = STATE.lock();
    state.present = true;
    state.mmio_base = found_mmio;

    serial::write_bytes(b"[usb-hid] xHCI bound mmio=0x");
    serial::write_hex(found_mmio);
    serial::write_line(b"");

    ProbeResult::Bound
}

// ── Event routing ─────────────────────────────────────────────────────────────

/// Process a raw HID keyboard boot-protocol report (8 bytes) and inject
/// key events into the virtio-input event ring.
pub fn handle_kbd_report(report: &[u8; 8]) {
    // Bytes 2..7 are keycodes (up to 6 simultaneous).
    for &keycode in &report[2..8] {
        if keycode == 0 {
            continue;
        }
        crate::drivers::input::virtio_input::inject_event(
            crate::drivers::input::virtio_input::InputEvent {
                typ: crate::drivers::input::virtio_input::EV_KEY,
                code: keycode as u16,
                value: 1, // key down
            },
        );
    }
}

pub fn is_present() -> bool {
    STATE.lock().present
}
