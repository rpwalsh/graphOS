// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! NVMe driver scaffold.
//!
//! This module provides a concrete integration point for Phase F/I storage
//! gates. It currently probes PCI for NVMe-class devices and exposes
//! a stable API surface for upcoming admin/io queue bring-up.

use crate::drivers::ProbeResult;

/// Runtime NVMe status exported for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvmeStatus {
    /// No matching PCI class/device was discovered.
    Missing,
    /// Device present and minimally enumerated.
    Present,
}

static mut STATUS: NvmeStatus = NvmeStatus::Missing;

/// Probe NVMe controller presence.
///
/// Returns `ProbeResult::Bound` when an NVMe PCI function is found,
/// otherwise `ProbeResult::NoMatch`.
pub fn probe() -> ProbeResult {
    if pci_nvme_present() {
        unsafe {
            STATUS = NvmeStatus::Present;
        }
        ProbeResult::Bound
    } else {
        unsafe {
            STATUS = NvmeStatus::Missing;
        }
        ProbeResult::NoMatch
    }
}

/// Current probe status.
pub fn status() -> NvmeStatus {
    unsafe { STATUS }
}

/// Returns `true` if the I/O paths (`read_lba`/`write_lba`) are stubs.
///
/// **v1.1 gate** — call this before issuing storage I/O; if it returns `true`
/// the operation will silently succeed without touching hardware.  Full NVMe
/// submission-queue bring-up is tracked in docs/OPEN_WORK.md §NVMe-IO.
pub fn is_io_stub() -> bool {
    true
}

/// Stub read path: returns `false` (I/O not performed) when the driver is
/// a stub.  Callers must treat a `false` return as a hard I/O error.
/// **Deferred to v1.1** — see `is_io_stub()`.
pub fn read_lba(_lba: u64, _out: &mut [u8]) -> bool {
    if is_io_stub() {
        return false;
    }
    matches!(status(), NvmeStatus::Present)
}

/// Stub write path: returns `false` (I/O not performed) when the driver is
/// a stub.  Callers must treat a `false` return as a hard I/O error.
/// **Deferred to v1.1** — see `is_io_stub()`.
pub fn write_lba(_lba: u64, _data: &[u8]) -> bool {
    if is_io_stub() {
        return false;
    }
    matches!(status(), NvmeStatus::Present)
}

fn pci_nvme_present() -> bool {
    // PCI class code 0x01, subclass 0x08 indicates NVM controller.
    let mut found = false;
    crate::arch::x86_64::pci::for_each_device(|info| {
        if !found && info.class_code == 0x01 && info.subclass == 0x08 {
            found = true;
        }
    });
    found
}
