// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Intel Xe/Arc PCIe GPU driver — bare-metal bring-up (Phase 1).
//!
//! This module performs real hardware discovery and MMIO bring-up for
//! Intel display controllers (including Arc-era Xe devices):
//! - PCI scan for vendor 0x8086 + display class
//! - BAR0 decode (MMIO aperture)
//! - Bus-master + MMIO decode enable
//! - Identity map BAR0 and validate MMIO responsiveness
//!
//! Phase 1 intentionally does not submit command streams yet; it establishes
//! a reliable, auditable hardware bring-up path that later phases can build on.

use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use crate::arch::serial;
use crate::drivers::ProbeResult;

const INTEL_VENDOR_ID: u16 = 0x8086;
const PCI_CLASS_DISPLAY: u8 = 0x03;
const PCI_SUBCLASS_VGA: u8 = 0x00;
const PCI_SUBCLASS_3D: u8 = 0x02;

const PCI_BAR0_OFFSET: u8 = 0x10;
const PCI_STATUS_CMD_OFFSET: u8 = 0x04;
const PCI_COMMAND_IO_SPACE: u16 = 1 << 0;
const PCI_COMMAND_MEMORY_SPACE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;

static PRESENT: AtomicBool = AtomicBool::new(false);
static DEVICE_ID: AtomicU16 = AtomicU16::new(0);
static BAR0: AtomicU64 = AtomicU64::new(0);

#[inline]
fn mmio_read_u32(addr: u64, off: u64) -> u32 {
    // SAFETY: caller ensures `addr + off` is identity-mapped and points to
    // device MMIO space. Access is volatile to preserve ordering/side effects.
    unsafe { core::ptr::read_volatile((addr + off) as *const u32) }
}

#[inline]
fn read_bar0_64(bus: u8, slot: u8, func: u8) -> Option<u64> {
    let low = crate::arch::x86_64::pci::read_u32(bus, slot, func, PCI_BAR0_OFFSET);
    if low == 0 || low == 0xFFFF_FFFF {
        return None;
    }
    // Reject I/O BAR; we require MMIO.
    if (low & 0x1) != 0 {
        return None;
    }

    let bar_type = (low >> 1) & 0x3;
    let base_low = (low & !0xF) as u64;
    match bar_type {
        0x0 => Some(base_low),
        0x2 => {
            let high = crate::arch::x86_64::pci::read_u32(bus, slot, func, PCI_BAR0_OFFSET + 4);
            Some(((high as u64) << 32) | base_low)
        }
        _ => None,
    }
}

#[inline]
fn is_intel_display(info: &crate::arch::x86_64::pci::PciDeviceInfo) -> bool {
    info.vendor_id == INTEL_VENDOR_ID
        && info.class_code == PCI_CLASS_DISPLAY
        && (info.subclass == PCI_SUBCLASS_VGA || info.subclass == PCI_SUBCLASS_3D)
}

pub fn probe_driver() -> ProbeResult {
    let mut candidate: Option<crate::arch::x86_64::pci::PciDeviceInfo> = None;

    crate::arch::x86_64::pci::for_each_device(|info| {
        if candidate.is_none() && is_intel_display(&info) {
            candidate = Some(info);
        }
    });

    let Some(dev) = candidate else {
        return ProbeResult::NoMatch;
    };

    crate::arch::x86_64::pci::enable_bus_master(dev.location);

    // Force memory decode + bus mastering on (defensive for firmware states).
    let mut cmd = crate::arch::x86_64::pci::read_u16(
        dev.location.bus,
        dev.location.slot,
        dev.location.func,
        PCI_STATUS_CMD_OFFSET,
    );
    cmd |= PCI_COMMAND_MEMORY_SPACE | PCI_COMMAND_BUS_MASTER;
    cmd &= !PCI_COMMAND_IO_SPACE;
    crate::arch::x86_64::pci::write_u16(
        dev.location.bus,
        dev.location.slot,
        dev.location.func,
        PCI_STATUS_CMD_OFFSET,
        cmd,
    );

    let Some(bar0) = read_bar0_64(dev.location.bus, dev.location.slot, dev.location.func) else {
        serial::write_line(b"[intel-xe] BAR0 missing or invalid");
        return ProbeResult::Failed;
    };

    // Map at least first 2 MiB of MMIO aperture (BAR0 usually much larger).
    if !crate::mm::page_table::ensure_identity_mapped_2m(bar0) {
        serial::write_line(b"[intel-xe] failed to identity-map BAR0");
        return ProbeResult::Failed;
    }

    // Validate MMIO responsiveness with a pair of reads from low offsets.
    let reg0 = mmio_read_u32(bar0, 0x0);
    let reg4 = mmio_read_u32(bar0, 0x4);
    if reg0 == 0xFFFF_FFFF && reg4 == 0xFFFF_FFFF {
        serial::write_line(b"[intel-xe] MMIO not responding");
        return ProbeResult::Failed;
    }

    BAR0.store(bar0, Ordering::Release);
    DEVICE_ID.store(dev.device_id, Ordering::Release);
    PRESENT.store(true, Ordering::Release);

    serial::write_bytes(b"[intel-xe] bound vid=0x");
    serial::write_hex_inline(dev.vendor_id as u64);
    serial::write_bytes(b" did=0x");
    serial::write_hex_inline(dev.device_id as u64);
    serial::write_bytes(b" bar0=0x");
    serial::write_hex_inline(bar0);
    serial::write_bytes(b" irq=");
    serial::write_u64_dec(dev.irq_line as u64);

    ProbeResult::Bound
}

pub fn is_present() -> bool {
    PRESENT.load(Ordering::Acquire)
}

pub fn device_id() -> u16 {
    DEVICE_ID.load(Ordering::Acquire)
}

pub fn bar0() -> u64 {
    BAR0.load(Ordering::Acquire)
}
