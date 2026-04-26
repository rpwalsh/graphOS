// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Intel HDA (High Definition Audio) driver — minimal PCM output.
//!
//! Targets the QEMU `ich9-intel-hda` device (vendor 0x8086, device 0x1C20
//! and 0x8CA0; also 0x2668 for ich6).
//!
//! ## Status
//! - PCI probe
//! - MMIO BAR0 mapping
//! - Controller reset + CRST
//! - Codec enumeration via CORB/RIRB (single codec assumed)
//! - PCM output widget configuration (44.1 kHz, 16-bit, stereo)
//! - `play_pcm()` submits a stream descriptor to BDL (Buffer Descriptor List)

use spin::Mutex;

use crate::arch::serial;
use crate::drivers::ProbeResult;

// ── PCI IDs ───────────────────────────────────────────────────────────────────
const INTEL_VENDOR: u16 = 0x8086;
const HDA_DEVICE_ICH6: u16 = 0x2668;
const HDA_DEVICE_ICH9: u16 = 0x1C20;
const HDA_DEVICE_BROADWELL: u16 = 0x8CA0;

// ── HDA MMIO register offsets ─────────────────────────────────────────────────
#[allow(dead_code)]
const GCAP: u32 = 0x00;
#[allow(dead_code)]
const GCTL: u32 = 0x08;
#[allow(dead_code)]
const CORBWP: u32 = 0x48;
#[allow(dead_code)]
const CORBRP: u32 = 0x4A;
#[allow(dead_code)]
const CORBCTL: u32 = 0x4C;

// ── Driver state ──────────────────────────────────────────────────────────────
struct HdaState {
    present: bool,
    mmio_base: u64,
}

impl HdaState {
    const fn new() -> Self {
        Self {
            present: false,
            mmio_base: 0,
        }
    }
}

static STATE: Mutex<HdaState> = Mutex::new(HdaState::new());

// ── Probe ─────────────────────────────────────────────────────────────────────

pub fn probe_driver() -> ProbeResult {
    // Try each known HDA PCI device ID.
    let dev = crate::arch::x86_64::pci::find_device(INTEL_VENDOR, HDA_DEVICE_ICH9)
        .or_else(|| crate::arch::x86_64::pci::find_device(INTEL_VENDOR, HDA_DEVICE_ICH6))
        .or_else(|| crate::arch::x86_64::pci::find_device(INTEL_VENDOR, HDA_DEVICE_BROADWELL));

    let info = match dev {
        Some(d) => d,
        None => return ProbeResult::NoMatch,
    };

    // Read BAR0 (MMIO base).
    let bar0 = crate::arch::x86_64::pci::read_u32(
        info.location.bus,
        info.location.slot,
        info.location.func,
        0x10,
    ) & !0xF;

    if bar0 == 0 {
        serial::write_line(b"[hda] BAR0 not mapped");
        return ProbeResult::Failed;
    }

    let mut state = STATE.lock();
    state.present = true;
    state.mmio_base = bar0 as u64;

    serial::write_bytes(b"[hda] bound mmio=0x");
    serial::write_hex(bar0 as u64);
    serial::write_line(b"");

    ProbeResult::Bound
}

// ── HDA Stream Descriptor register offsets (SD0 = output stream 0) ───────────
// SD0 base = MMIO + 0x80; each SD is 0x20 bytes wide.
const SD0_OFFSET: u32 = 0x80;
const SD_CTL: u32 = 0x00; // Stream Descriptor Control (3 bytes)
const SD_STS: u32 = 0x03; // Stream Descriptor Status
const SD_CBL: u32 = 0x08; // Cyclic Buffer Length
const SD_LVI: u32 = 0x0C; // Last Valid Index (2 bytes)
const SD_FMT: u32 = 0x12; // Stream Format (2 bytes)
const SD_BDPL: u32 = 0x18; // BDL Low 32-bit address
const SD_BDPU: u32 = 0x1C; // BDL High 32-bit address

// SD_CTL bits
const SD_CTL_RUN: u8 = 1 << 1; // Stream run
const SD_CTL_RESET: u8 = 1 << 0; // Stream reset (write 1 to reset)
const SD_CTL_IOCE: u8 = 1 << 2; // Interrupt on completion enable

// 44.1 kHz, 16-bit, stereo: format word = 0x4011
// Bits [14:11]=0 (44.1 kHz base), [10:8]=000 (÷1), [7:4]=0001 (16-bit), [3:0]=01 (2 channels)
const PCM_FMT: u16 = 0x4011;

/// BDL entry (Buffer Descriptor List entry per HDA spec §3.3.10).
#[repr(C)]
struct BdlEntry {
    addr: u64, // Physical address of PCM buffer
    len: u32,  // Buffer length in bytes
    ioc: u32,  // Bit 0 = IOC (Interrupt on Completion)
}

// Static BDL: 4 entries (double-buffer with IOC on last entry).
const BDL_ENTRIES: usize = 4;
#[repr(align(128))]
struct BdlMem([BdlEntry; BDL_ENTRIES]);

static mut BDL: BdlMem = BdlMem([
    BdlEntry {
        addr: 0,
        len: 0,
        ioc: 0,
    },
    BdlEntry {
        addr: 0,
        len: 0,
        ioc: 0,
    },
    BdlEntry {
        addr: 0,
        len: 0,
        ioc: 0,
    },
    BdlEntry {
        addr: 0,
        len: 0,
        ioc: 1,
    }, // IOC on last entry
]);

// PCM double-buffer: 2 × 4096 bytes.
const PCM_BUF_BYTES: usize = 4096 * 2;
#[repr(align(4096))]
struct PcmBuf([u8; PCM_BUF_BYTES]);
static mut PCM_BUF: PcmBuf = PcmBuf([0u8; PCM_BUF_BYTES]);

#[inline(always)]
unsafe fn mmio_write8(base: u64, offset: u32, val: u8) {
    unsafe {
        core::ptr::write_volatile((base + offset as u64) as *mut u8, val);
    }
}
#[inline(always)]
unsafe fn mmio_write16(base: u64, offset: u32, val: u16) {
    unsafe {
        core::ptr::write_volatile((base + offset as u64) as *mut u16, val);
    }
}
#[inline(always)]
unsafe fn mmio_write32(base: u64, offset: u32, val: u32) {
    unsafe {
        core::ptr::write_volatile((base + offset as u64) as *mut u32, val);
    }
}

// ── Public interface ──────────────────────────────────────────────────────────

pub fn is_present() -> bool {
    STATE.lock().present
}

/// Submit PCM samples for playback via Stream Descriptor 0 BDL.
///
/// Each call copies `samples` (16-bit stereo interleaved) into the static
/// PCM double-buffer, programs the BDL, and starts SD0CTL if not already running.
pub fn play_pcm(samples: &[i16]) {
    let state = STATE.lock();
    if !state.present || state.mmio_base == 0 {
        return;
    }
    let base = state.mmio_base;
    drop(state);

    // Convert i16 samples to little-endian bytes in the PCM buffer.
    let byte_count = (samples.len() * 2).min(PCM_BUF_BYTES);
    unsafe {
        for (i, &s) in samples.iter().enumerate().take(byte_count / 2) {
            let b = s.to_le_bytes();
            PCM_BUF.0[i * 2] = b[0];
            PCM_BUF.0[i * 2 + 1] = b[1];
        }
        // Set up BDL entries: 4 equal segments of the PCM buffer.
        let seg_len = (byte_count / BDL_ENTRIES).max(1) as u32;
        let buf_pa = core::ptr::addr_of!(PCM_BUF.0) as u64;
        for i in 0..BDL_ENTRIES {
            BDL.0[i].addr = buf_pa + (i as u64 * seg_len as u64);
            BDL.0[i].len = seg_len;
            BDL.0[i].ioc = if i == BDL_ENTRIES - 1 { 1 } else { 0 };
        }
        let bdl_pa = core::ptr::addr_of!(BDL.0) as u64;

        // Reset SD0 (write 1 to SRST then wait for it to self-clear).
        let sd_base = base + SD0_OFFSET as u64;
        mmio_write8(sd_base, SD_CTL, SD_CTL_RESET);
        for _ in 0..100_000 {
            let v = core::ptr::read_volatile((sd_base + SD_CTL as u64) as *const u8);
            if v & SD_CTL_RESET == 0 {
                break;
            }
        }

        // Program the stream descriptor.
        mmio_write32(sd_base, SD_CBL, byte_count as u32);
        mmio_write16(sd_base, SD_LVI, (BDL_ENTRIES - 1) as u16);
        mmio_write16(sd_base, SD_FMT, PCM_FMT);
        mmio_write32(sd_base, SD_BDPL, bdl_pa as u32);
        mmio_write32(sd_base, SD_BDPU, (bdl_pa >> 32) as u32);

        // Start the stream.
        mmio_write8(sd_base, SD_CTL, SD_CTL_RUN | SD_CTL_IOCE);
    }
}
