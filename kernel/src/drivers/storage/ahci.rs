// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! AHCI SATA driver — bare metal path.
//!
//! Implements the Advanced Host Controller Interface (AHCI) 1.3.1 spec over PCI.
//! QEMU exposes SATA controllers as PCI class 0x01/0x06 (AHCI variant prog-if 0x01).
//! Real hardware: Intel 6-series+ SATA controller, AMD Fusion/FCH.
//!
//! ## Design
//! - Discovers AHCI controller via `arch::x86_64::pci::for_each_device`.
//! - Maps ABAR (BAR5) MMIO; enumerates implemented ports (HBA.PI bitmask).
//! - Issues 48-bit LBA DMA READ/WRITE commands via CFIS (FIS_TYPE_REG_H2D).
//! - Interrupt-less spin-poll for boot-time use; IRQ hook added once APIC is live.
//! - No `alloc`; static command-list / FIS-receive buffers per port (max 4 ports).

use spin::Mutex;

// ─── PCI class codes ────────────────────────────────────────────────────────
const AHCI_CLASS: u8 = 0x01; // Mass Storage
const AHCI_SUBCLASS: u8 = 0x06; // SATA
const AHCI_PROG_IF: u8 = 0x01; // AHCI 1.0

// ─── BAR offsets ────────────────────────────────────────────────────────────
const PCI_BAR5_OFFSET: u8 = 0x24;

// ─── HBA memory-mapped registers ────────────────────────────────────────────
const HBA_GHC: u64 = 0x04; // Global Host Control
const HBA_IS: u64 = 0x08; // Interrupt Status
const HBA_PI: u64 = 0x0C; // Ports Implemented
const HBA_CAP: u64 = 0x00; // Host Capabilities

const HBA_GHC_AHCI_ENABLE: u32 = 1 << 31;
const HBA_GHC_HBA_RESET: u32 = 1 << 0;

// Per-port register offsets (port N base = abar + 0x100 + N * 0x80)
const PORT_CLB: u64 = 0x00; // Command List Base Address (low 32)
const PORT_CLBU: u64 = 0x04; // Command List Base Address (high 32)
const PORT_FB: u64 = 0x08; // FIS Base Address (low 32)
const PORT_FBU: u64 = 0x0C; // FIS Base Address (high 32)
const PORT_IS: u64 = 0x10; // Interrupt Status
const PORT_IE: u64 = 0x14; // Interrupt Enable
const PORT_CMD: u64 = 0x18; // Command and Status
const PORT_TFD: u64 = 0x20; // Task File Data
const PORT_SIG: u64 = 0x24; // Signature
const PORT_SSTS: u64 = 0x28; // Serial ATA Status
const PORT_SERR: u64 = 0x30; // Serial ATA Error
const PORT_CI: u64 = 0x38; // Command Issue

const PORT_CMD_ST: u32 = 1 << 0; // Start
const PORT_CMD_FRE: u32 = 1 << 4; // FIS Receive Enable
const PORT_CMD_FR: u32 = 1 << 14; // FIS Receive Running
const PORT_CMD_CR: u32 = 1 << 15; // Command List Running

const SATA_SIG_ATA: u32 = 0x0000_0101; // SATA drive
const SSTS_DET_PRESENT: u32 = 0x3;

// ─── FIS types ───────────────────────────────────────────────────────────────
const FIS_TYPE_REG_H2D: u8 = 0x27;

// ─── ATA commands ────────────────────────────────────────────────────────────
const ATA_CMD_READ_DMA_EX: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EX: u8 = 0x35;
const ATA_CMD_IDENTIFY: u8 = 0xEC;

// ─── Static DMA buffers (no heap) ────────────────────────────────────────────

/// Maximum simultaneous ports we track.
const MAX_PORTS: usize = 4;

/// Each port needs a 1 KiB command list (32 command headers × 32 bytes each).
#[repr(C, align(1024))]
#[derive(Copy, Clone)]
struct CommandList {
    headers: [[u32; 8]; 32],
}

/// Each port needs a 256-byte FIS receive area.
#[repr(C, align(256))]
#[derive(Copy, Clone)]
struct FisReceive {
    raw: [u8; 256],
}

/// Each command needs a Physical Region Descriptor Table entry.
#[repr(C, align(128))]
#[derive(Copy, Clone)]
struct CmdTable {
    cfis: [u8; 64],
    acmd: [u8; 16],
    _rsvd: [u8; 48],
    prdt: [[u32; 4]; 8], // 8 PRD entries max per command
}

static mut CMD_LISTS: [CommandList; MAX_PORTS] = [CommandList {
    headers: [[0u32; 8]; 32],
}; MAX_PORTS];
static mut FIS_AREAS: [FisReceive; MAX_PORTS] = [FisReceive { raw: [0u8; 256] }; MAX_PORTS];
static mut CMD_TABLES: [CmdTable; MAX_PORTS] = [CmdTable {
    cfis: [0u8; 64],
    acmd: [0u8; 16],
    _rsvd: [0u8; 48],
    prdt: [[0u32; 4]; 8],
}; MAX_PORTS];

// ─── Driver state ────────────────────────────────────────────────────────────

struct PortState {
    base: u64,         // Port MMIO base
    abar: u64,         // HBA ABAR
    sector_count: u64, // Total LBA sectors
}

struct AhciState {
    abar: u64,
    ports: [Option<PortState>; MAX_PORTS],
    port_count: usize,
}

static STATE: Mutex<Option<AhciState>> = Mutex::new(None);

// ─── MMIO helpers ────────────────────────────────────────────────────────────

#[inline]
fn mmio_r32(base: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}

#[inline]
fn mmio_w32(base: u64, off: u64, val: u32) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u32, val) }
}

// ─── Port helpers ─────────────────────────────────────────────────────────────

fn port_base(abar: u64, port: usize) -> u64 {
    abar + 0x100 + (port as u64) * 0x80
}

fn port_start(pb: u64) {
    // Clear ST and FRE; wait for CR and FR to clear.
    let mut cmd = mmio_r32(pb, PORT_CMD);
    cmd &= !(PORT_CMD_ST | PORT_CMD_FRE);
    mmio_w32(pb, PORT_CMD, cmd);
    let mut spin = 0u32;
    while mmio_r32(pb, PORT_CMD) & (PORT_CMD_CR | PORT_CMD_FR) != 0 {
        spin += 1;
        if spin > 500_000 {
            break;
        }
    }
    // Start again.
    let mut cmd = mmio_r32(pb, PORT_CMD);
    cmd |= PORT_CMD_FRE | PORT_CMD_ST;
    mmio_w32(pb, PORT_CMD, cmd);
}

fn port_stop(pb: u64) {
    let mut cmd = mmio_r32(pb, PORT_CMD);
    cmd &= !(PORT_CMD_ST | PORT_CMD_FRE);
    mmio_w32(pb, PORT_CMD, cmd);
}

// ─── Initialisation ──────────────────────────────────────────────────────────

/// Probe for an AHCI controller on the PCI bus and initialise it.
/// Returns the number of SATA drives found.
pub fn init() -> usize {
    use crate::arch::x86_64::{pci, serial};

    let mut found_dev = None;
    pci::for_each_device(|info| {
        if found_dev.is_none()
            && info.class_code == AHCI_CLASS
            && info.subclass == AHCI_SUBCLASS
            && info.prog_if == AHCI_PROG_IF
        {
            found_dev = Some(info);
        }
    });

    let dev = match found_dev {
        Some(d) => d,
        None => {
            serial::write_bytes(b"[ahci] no controller found\n");
            return 0;
        }
    };

    // Read BAR5 (ABAR) — the 32-bit MMIO aperture.
    let bar5_raw = pci::read_u32(
        dev.location.bus,
        dev.location.slot,
        dev.location.func,
        PCI_BAR5_OFFSET,
    );
    let abar = (bar5_raw & !0xFu32) as u64;
    if abar == 0 {
        serial::write_bytes(b"[ahci] ABAR is 0; skipping\n");
        return 0;
    }

    pci::enable_bus_master(dev.location);

    // Enable AHCI mode.
    let mut ghc = mmio_r32(abar, HBA_GHC);
    ghc |= HBA_GHC_AHCI_ENABLE;
    mmio_w32(abar, HBA_GHC, ghc);

    // Clear pending interrupts.
    let is = mmio_r32(abar, HBA_IS);
    mmio_w32(abar, HBA_IS, is);

    let pi = mmio_r32(abar, HBA_PI);

    let mut port_count = 0usize;
    let mut ports: [Option<PortState>; MAX_PORTS] = [None, None, None, None];

    for port_idx in 0..32usize {
        if port_count >= MAX_PORTS {
            break;
        }
        if pi & (1 << port_idx) == 0 {
            continue;
        }

        let pb = port_base(abar, port_idx);

        // Check device presence.
        let ssts = mmio_r32(pb, PORT_SSTS);
        if (ssts & 0xF) != SSTS_DET_PRESENT {
            continue;
        }

        // Check signature — we only handle plain SATA (not ATAPI).
        let sig = mmio_r32(pb, PORT_SIG);
        if sig != SATA_SIG_ATA {
            continue;
        }

        // Stop the port cleanly before reconfiguring DMA buffers.
        port_stop(pb);

        // Wire up command list and FIS receive areas.
        let cl_addr = unsafe { CMD_LISTS[port_count].headers.as_ptr() as u64 };
        let fb_addr = unsafe { FIS_AREAS[port_count].raw.as_ptr() as u64 };
        let ct_addr = unsafe { CMD_TABLES[port_count].cfis.as_ptr() as u64 };

        mmio_w32(pb, PORT_CLB, cl_addr as u32);
        mmio_w32(pb, PORT_CLBU, (cl_addr >> 32) as u32);
        mmio_w32(pb, PORT_FB, fb_addr as u32);
        mmio_w32(pb, PORT_FBU, (fb_addr >> 32) as u32);

        // Set command table address in command header 0.
        unsafe {
            CMD_LISTS[port_count].headers[0][2] = ct_addr as u32;
            CMD_LISTS[port_count].headers[0][3] = (ct_addr >> 32) as u32;
        }

        // Clear errors.
        let serr = mmio_r32(pb, PORT_SERR);
        mmio_w32(pb, PORT_SERR, serr);
        let pis = mmio_r32(pb, PORT_IS);
        mmio_w32(pb, PORT_IS, pis);

        // Restart the port.
        port_start(pb);

        // Issue IDENTIFY to get sector count.
        let sector_count = identify(port_count, pb) as u64;

        serial::write_bytes(b"[ahci] port=");
        serial::write_u64_dec_inline(port_idx as u64);
        serial::write_bytes(b" sectors=");
        serial::write_u64_dec(sector_count);

        ports[port_count] = Some(PortState {
            base: pb,
            abar,
            sector_count,
        });
        port_count += 1;
    }

    *STATE.lock() = Some(AhciState {
        abar,
        ports,
        port_count,
    });

    serial::write_bytes(b"[ahci] init complete drives=");
    serial::write_u64_dec(port_count as u64);
    port_count
}

// ─── IDENTIFY ────────────────────────────────────────────────────────────────

fn identify(port_slot: usize, pb: u64) -> u32 {
    static mut IDENTIFY_BUF: [u8; 512] = [0u8; 512];
    let buf_addr = core::ptr::addr_of!(IDENTIFY_BUF) as u64;

    build_cfis(port_slot, ATA_CMD_IDENTIFY, 0, 0, 0);
    build_prdt(port_slot, buf_addr, 512);
    set_cmd_header(port_slot, 5 /* cfis_length in dwords */, false, 1);

    if issue_command(pb) {
        // sectors is at words 100-103 (48-bit LBA).
        let words = unsafe {
            core::slice::from_raw_parts(core::ptr::addr_of!(IDENTIFY_BUF) as *const u16, 256)
        };
        let low = words[100] as u32;
        let high = words[101] as u32;
        low | (high << 16)
    } else {
        0
    }
}

// ─── Command building ─────────────────────────────────────────────────────────

fn build_cfis(slot: usize, cmd: u8, lba: u64, count: u16, device: u8) {
    unsafe {
        let cfis = &mut CMD_TABLES[slot].cfis;
        cfis[0] = FIS_TYPE_REG_H2D;
        cfis[1] = 0x80; // C bit — command register
        cfis[2] = cmd;
        cfis[3] = 0; // features (low)
        cfis[4] = (lba & 0xFF) as u8; // LBA 0
        cfis[5] = ((lba >> 8) & 0xFF) as u8; // LBA 1
        cfis[6] = ((lba >> 16) & 0xFF) as u8; // LBA 2
        cfis[7] = device | 0x40; // LBA mode
        cfis[8] = ((lba >> 24) & 0xFF) as u8; // LBA 3
        cfis[9] = ((lba >> 32) & 0xFF) as u8; // LBA 4
        cfis[10] = ((lba >> 40) & 0xFF) as u8; // LBA 5
        cfis[11] = 0; // features (high)
        cfis[12] = (count & 0xFF) as u8; // count (low)
        cfis[13] = ((count >> 8) & 0xFF) as u8; // count (high)
        cfis[14] = 0;
        cfis[15] = 0;
    }
}

fn build_prdt(slot: usize, buf: u64, byte_count: u32) {
    unsafe {
        CMD_TABLES[slot].prdt[0][0] = buf as u32;
        CMD_TABLES[slot].prdt[0][1] = (buf >> 32) as u32;
        CMD_TABLES[slot].prdt[0][2] = 0;
        CMD_TABLES[slot].prdt[0][3] = (byte_count - 1) | 0x8000_0000; // interrupt on completion
    }
}

fn set_cmd_header(slot: usize, cfis_len_dwords: u32, write: bool, prdt_count: u16) {
    unsafe {
        let h = &mut CMD_LISTS[slot].headers[0];
        // DW0: flags | write | CFIS length
        h[0] = cfis_len_dwords | (if write { 1 << 6 } else { 0 }) | ((prdt_count as u32) << 16);
        h[1] = 0; // PRD byte count (filled by HBA)
    }
}

fn issue_command(pb: u64) -> bool {
    // Clear port error/interrupt state before issuing.
    mmio_w32(pb, PORT_SERR, mmio_r32(pb, PORT_SERR));
    mmio_w32(pb, PORT_IS, mmio_r32(pb, PORT_IS));

    // Issue command slot 0.
    mmio_w32(pb, PORT_CI, 1);

    // Spin until slot 0 is cleared (command complete) or BSY/ERR.
    let mut spin = 0u32;
    loop {
        let tfd = mmio_r32(pb, PORT_TFD);
        if tfd & 0x88 != 0 {
            return false;
        } // ERR or DF
        if mmio_r32(pb, PORT_CI) & 1 == 0 {
            return true;
        } // done
        spin += 1;
        if spin > 1_000_000 {
            return false;
        }
    }
}

// ─── Public read/write API ────────────────────────────────────────────────────

/// Read `count` 512-byte sectors starting at `lba` from the first AHCI drive
/// into `buf`. Returns `true` on success.
pub fn read_sectors(lba: u64, count: u16, buf: &mut [u8]) -> bool {
    debug_assert!(buf.len() >= (count as usize) * 512);
    let lock = STATE.lock();
    let state = match lock.as_ref() {
        Some(s) => s,
        None => return false,
    };
    let port_slot = 0;
    let port = match state.ports[port_slot].as_ref() {
        Some(p) => p,
        None => return false,
    };

    build_cfis(port_slot, ATA_CMD_READ_DMA_EX, lba, count, 0);
    build_prdt(port_slot, buf.as_ptr() as u64, count as u32 * 512);
    set_cmd_header(port_slot, 5, false, 1);
    issue_command(port.base)
}

/// Write `count` 512-byte sectors starting at `lba` from `buf` to the first
/// AHCI drive. Returns `true` on success.
pub fn write_sectors(lba: u64, count: u16, buf: &[u8]) -> bool {
    debug_assert!(buf.len() >= (count as usize) * 512);
    let lock = STATE.lock();
    let state = match lock.as_ref() {
        Some(s) => s,
        None => return false,
    };
    let port_slot = 0;
    let port = match state.ports[port_slot].as_ref() {
        Some(p) => p,
        None => return false,
    };

    build_cfis(port_slot, ATA_CMD_WRITE_DMA_EX, lba, count, 0);
    build_prdt(port_slot, buf.as_ptr() as u64, count as u32 * 512);
    set_cmd_header(port_slot, 5, true, 1);
    issue_command(port.base)
}

/// Returns total sector count for the first AHCI drive, or 0 if none.
pub fn sector_count() -> u64 {
    let lock = STATE.lock();
    match lock.as_ref() {
        Some(s) => s.ports[0].as_ref().map(|p| p.sector_count).unwrap_or(0),
        None => 0,
    }
}
