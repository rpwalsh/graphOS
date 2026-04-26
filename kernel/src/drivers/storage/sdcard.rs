// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! SD/SDHC/SDXC card driver (SDHCI 3.0).
//!
//! Probes the SDHCI host controller via memory-mapped registers and supports
//! card identification, sector read/write with the built-in DMA engine.
//!
//! Limitations:
//! - Only single-block reads/writes in 512-byte sectors for now.
//! - 4-bit data bus, HS (high-speed) timing mode.
//! - ADMA2 is stubbed; real DMA requires a physically-contiguous buffer.

use spin::Mutex;

// ---------------------------------------------------------------------------
// SDHCI register offsets (relative to BAR0)
// ---------------------------------------------------------------------------

const SDHCI_DMA_ADDR: u64 = 0x00;
const SDHCI_BLK_SIZE: u64 = 0x04;
const SDHCI_BLK_COUNT: u64 = 0x06;
const SDHCI_ARG: u64 = 0x08;
const SDHCI_TRANS_MODE: u64 = 0x0C;
const SDHCI_CMD: u64 = 0x0E;
const SDHCI_RESP0: u64 = 0x10;
const SDHCI_BUF_DATA: u64 = 0x20;
const SDHCI_PRESENT_STATE: u64 = 0x24;
const SDHCI_HOST_CTRL: u64 = 0x28;
const SDHCI_POWER_CTRL: u64 = 0x29;
const SDHCI_CLK_CTRL: u64 = 0x2C;
const SDHCI_RESET: u64 = 0x2F;
const SDHCI_INT_STATUS: u64 = 0x30;
const SDHCI_INT_ENA: u64 = 0x34;
const SDHCI_SLOT_INT: u64 = 0xFC;

const SDHCI_RESET_ALL: u8 = 0x01;
const SDHCI_INT_CMD_CMP: u32 = 1 << 0;
const SDHCI_INT_XFER_CMP: u32 = 1 << 1;
const SDHCI_INT_BUF_RD: u32 = 1 << 5;
const SDHCI_INT_BUF_WR: u32 = 1 << 4;
const SDHCI_INT_ERR: u32 = 1 << 15;

const TRANS_READ: u16 = 1 << 4;
const TRANS_WRITE: u16 = 0;
const TRANS_BLK_CNT_ENA: u16 = 1 << 1;

/// Sector size (fixed at 512 for SDHC/SDXC).
pub const SECTOR_SIZE: usize = 512;

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

struct SdhciState {
    bar0: u64,
    /// Card address (RCA, set after card identification).
    rca: u16,
    /// `true` if an SDHC/SDXC card is present (uses block-addressed LBA).
    high_capacity: bool,
    /// Total sector count.
    sector_count: u64,
}

static STATE: Mutex<Option<SdhciState>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Register accessors
// ---------------------------------------------------------------------------

#[inline]
fn rd8(base: u64, off: u64) -> u8 {
    unsafe { core::ptr::read_volatile((base + off) as *const u8) }
}
#[inline]
fn rd16(base: u64, off: u64) -> u16 {
    unsafe { core::ptr::read_volatile((base + off) as *const u16) }
}
#[inline]
fn rd32(base: u64, off: u64) -> u32 {
    unsafe { core::ptr::read_volatile((base + off) as *const u32) }
}
#[inline]
fn wr8(base: u64, off: u64, val: u8) {
    unsafe {
        core::ptr::write_volatile((base + off) as *mut u8, val);
    }
}
#[inline]
fn wr16(base: u64, off: u64, val: u16) {
    unsafe {
        core::ptr::write_volatile((base + off) as *mut u16, val);
    }
}
#[inline]
fn wr32(base: u64, off: u64, val: u32) {
    unsafe {
        core::ptr::write_volatile((base + off) as *mut u32, val);
    }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Probe for an SDHCI controller at `bar0` and initialise it.
///
/// Returns `true` if a card was detected and initialised successfully.
pub fn probe(bar0: u64) -> bool {
    // Software-reset all.
    wr8(bar0, SDHCI_RESET, SDHCI_RESET_ALL);
    let mut i = 0u32;
    while rd8(bar0, SDHCI_RESET) & SDHCI_RESET_ALL != 0 {
        i += 1;
        if i > 100_000 {
            return false;
        }
    }

    // Enable interrupts (CMD complete, transfer complete, error).
    wr32(
        bar0,
        SDHCI_INT_ENA,
        SDHCI_INT_CMD_CMP
            | SDHCI_INT_XFER_CMP
            | SDHCI_INT_BUF_RD
            | SDHCI_INT_BUF_WR
            | SDHCI_INT_ERR,
    );

    // Power on: 3.3 V.
    wr8(bar0, SDHCI_POWER_CTRL, 0x0F); // 3.3V, bus power on

    // Set clock to 400 kHz for identification phase.
    // With a 48 MHz base clock, divider = 120.
    wr16(bar0, SDHCI_CLK_CTRL, 0x7800 | 0x01); // divider=60 → ~400 kHz

    // CMD0: GO_IDLE_STATE
    if !send_cmd(bar0, 0, 0, 0) {
        return false;
    }
    // CMD8: SEND_IF_COND (voltage + check pattern)
    let _ = send_cmd(bar0, 8, 0x000001AA, 0b01); // R1 response
    // ACMD41: SD_SEND_OP_COND — loop until ready.
    let mut ocr = 0u32;
    for _ in 0..1000 {
        // CMD55 + ACMD41
        send_cmd(bar0, 55, 0, 0b01);
        let r = send_cmd_resp(bar0, 41, 0x40FF8000, 0b10); // R3
        ocr = rd32(bar0, SDHCI_RESP0);
        if ocr & (1 << 31) != 0 {
            break;
        }
        let _ = r;
    }
    if ocr & (1 << 31) == 0 {
        return false;
    }
    let high_capacity = (ocr >> 30) & 1 != 0;

    // CMD2: ALL_SEND_CID, then CMD3: SEND_RELATIVE_ADDR
    send_cmd(bar0, 2, 0, 0b11); // R2
    send_cmd(bar0, 3, 0, 0b01); // R6 — sets RCA
    let rca = (rd32(bar0, SDHCI_RESP0) >> 16) as u16;

    // CMD7: SELECT_CARD
    send_cmd(bar0, 7, (rca as u32) << 16, 0b01);

    // Set 4-bit bus: ACMD6
    send_cmd(bar0, 55, (rca as u32) << 16, 0b01);
    send_cmd(bar0, 6, 0x02, 0b01);
    let hc = rd8(bar0, SDHCI_HOST_CTRL);
    wr8(bar0, SDHCI_HOST_CTRL, hc | 0x02); // 4-bit data width

    *STATE.lock() = Some(SdhciState {
        bar0,
        rca,
        high_capacity,
        sector_count: 0, // Would be read from CSD/CSDV2 register
    });
    crate::arch::serial::write_line(b"[sdcard] card ready");
    true
}

fn send_cmd(base: u64, index: u8, arg: u32, resp_type: u8) -> bool {
    let _ = send_cmd_resp(base, index, arg, resp_type);
    true
}

fn send_cmd_resp(base: u64, index: u8, arg: u32, resp_type: u8) -> u32 {
    wr32(base, SDHCI_ARG, arg);
    let cmd = ((index as u16) << 8) | ((resp_type as u16) & 0x3);
    wr16(base, SDHCI_CMD, cmd);
    // Wait for CMD complete.
    let mut tries = 0u32;
    loop {
        let st = rd32(base, SDHCI_INT_STATUS);
        if st & SDHCI_INT_CMD_CMP != 0 {
            wr32(base, SDHCI_INT_STATUS, SDHCI_INT_CMD_CMP);
            break;
        }
        if st & SDHCI_INT_ERR != 0 {
            return 0;
        }
        tries += 1;
        if tries > 1_000_000 {
            return 0;
        }
    }
    rd32(base, SDHCI_RESP0)
}

// ---------------------------------------------------------------------------
// Public I/O
// ---------------------------------------------------------------------------

/// Read one 512-byte sector from LBA `lba` into `buf`.
///
/// Returns `true` on success.
pub fn read_sector(lba: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
    let guard = STATE.lock();
    let Some(ref s) = *guard else { return false };
    let bar0 = s.bar0;
    let addr = if s.high_capacity {
        lba as u32
    } else {
        (lba * 512) as u32
    };

    wr16(bar0, SDHCI_BLK_SIZE, 512);
    wr16(bar0, SDHCI_BLK_COUNT, 1);
    wr16(bar0, SDHCI_TRANS_MODE, TRANS_READ | TRANS_BLK_CNT_ENA);
    // CMD17: READ_SINGLE_BLOCK
    send_cmd_resp(bar0, 17, addr, 0b01);
    // Wait for buffer read ready.
    let mut tries = 0u32;
    loop {
        let st = rd32(bar0, SDHCI_INT_STATUS);
        if st & SDHCI_INT_BUF_RD != 0 {
            wr32(bar0, SDHCI_INT_STATUS, SDHCI_INT_BUF_RD);
            break;
        }
        if st & SDHCI_INT_ERR != 0 {
            return false;
        }
        tries += 1;
        if tries > 1_000_000 {
            return false;
        }
    }
    // Read 512 bytes from the data port (32-bit at a time).
    for i in (0..SECTOR_SIZE).step_by(4) {
        let word = rd32(bar0, SDHCI_BUF_DATA);
        buf[i] = word as u8;
        buf[i + 1] = (word >> 8) as u8;
        buf[i + 2] = (word >> 16) as u8;
        buf[i + 3] = (word >> 24) as u8;
    }
    // Wait for transfer complete.
    let mut tries = 0u32;
    loop {
        let st = rd32(bar0, SDHCI_INT_STATUS);
        if st & SDHCI_INT_XFER_CMP != 0 {
            wr32(bar0, SDHCI_INT_STATUS, SDHCI_INT_XFER_CMP);
            return true;
        }
        tries += 1;
        if tries > 100_000 {
            return false;
        }
    }
}

/// Write one 512-byte sector to LBA `lba` from `buf`.
///
/// Returns `true` on success.
pub fn write_sector(lba: u64, buf: &[u8; SECTOR_SIZE]) -> bool {
    let guard = STATE.lock();
    let Some(ref s) = *guard else { return false };
    let bar0 = s.bar0;
    let addr = if s.high_capacity {
        lba as u32
    } else {
        (lba * 512) as u32
    };

    wr16(bar0, SDHCI_BLK_SIZE, 512);
    wr16(bar0, SDHCI_BLK_COUNT, 1);
    wr16(bar0, SDHCI_TRANS_MODE, TRANS_WRITE | TRANS_BLK_CNT_ENA);
    // CMD24: WRITE_BLOCK
    send_cmd_resp(bar0, 24, addr, 0b01);
    // Wait for buffer write ready.
    let mut tries = 0u32;
    loop {
        let st = rd32(bar0, SDHCI_INT_STATUS);
        if st & SDHCI_INT_BUF_WR != 0 {
            wr32(bar0, SDHCI_INT_STATUS, SDHCI_INT_BUF_WR);
            break;
        }
        if st & SDHCI_INT_ERR != 0 {
            return false;
        }
        tries += 1;
        if tries > 1_000_000 {
            return false;
        }
    }
    for i in (0..SECTOR_SIZE).step_by(4) {
        let word = (buf[i] as u32)
            | ((buf[i + 1] as u32) << 8)
            | ((buf[i + 2] as u32) << 16)
            | ((buf[i + 3] as u32) << 24);
        wr32(bar0, SDHCI_BUF_DATA, word);
    }
    let mut tries = 0u32;
    loop {
        let st = rd32(bar0, SDHCI_INT_STATUS);
        if st & SDHCI_INT_XFER_CMP != 0 {
            wr32(bar0, SDHCI_INT_STATUS, SDHCI_INT_XFER_CMP);
            return true;
        }
        tries += 1;
        if tries > 100_000 {
            return false;
        }
    }
}
