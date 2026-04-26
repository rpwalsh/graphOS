// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! virtio-blk PCI driver — legacy I/O-port transport.
//!
//! Implements a simple synchronous request queue (queue 0) for block I/O.
//! Each request uses three descriptors: header + data buffer + status byte.
//! Sectors are 512 bytes.  A single global static ring of 16 entries is used.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::arch::x86_64::serial;
use crate::drivers::ProbeResult;

// ── PCI / virtio constants ──────────────────────────────────────────

const PCI_BAR0_OFFSET: u8 = 0x10;
const PCI_INTERRUPT_LINE_OFFSET: u8 = 0x3C;

const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_STATUS: u16 = 0x12;
const VIRTIO_PCI_ISR: u16 = 0x13;

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;

/// PCI vendor / device IDs for virtio-blk (legacy and modern subsystem).
const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE_LEGACY: u16 = 0x1001;
const VIRTIO_BLK_DEVICE_MODERN: u16 = 0x1042;
const VIRTIO_SUBSYSTEM_BLK: u16 = 0x0002;

/// Request type constants.
const VIRTIO_BLK_T_IN: u32 = 0; // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write

/// Queue ring size.
const QUEUE_SIZE: usize = 16;

/// Descriptor flags.
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2; // device-writable (for host→guest buffers)

const SECTOR_SIZE: usize = 512;

// ── Ring layout ─────────────────────────────────────────────────────
//
// Layout (all in one page-aligned array):
//   descriptors : QUEUE_SIZE * 16 bytes
//   avail ring  : 6 + QUEUE_SIZE * 2 bytes
//   padding to 4-KiB alignment
//   used ring   : 6 + QUEUE_SIZE * 8 bytes

const DESC_BYTES: usize = QUEUE_SIZE * 16;
const AVAIL_BYTES: usize = 6 + QUEUE_SIZE * 2;
const USED_ELEM_BYTES: usize = 8; // id: u32, len: u32
const USED_BYTES: usize = 6 + QUEUE_SIZE * USED_ELEM_BYTES;
const QUEUE_BYTES: usize = DESC_BYTES + AVAIL_BYTES + 4096 + USED_BYTES;
// Align used ring to 4096-byte boundary.
const USED_OFFSET: usize = (DESC_BYTES + AVAIL_BYTES + 0xFFF) & !0xFFF;

// ── Structs ──────────────────────────────────────────────────────────

#[repr(C)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

/// virtio_blk_req header (type + reserved + sector).
#[repr(C)]
struct BlkReqHeader {
    typ: u32,
    reserved: u32,
    sector: u64,
}

// ── Static storage ───────────────────────────────────────────────────

/// Aligned queue memory.
#[repr(align(4096))]
struct QueueBuf([u8; QUEUE_BYTES]);

/// I/O header for each request (re-used per request, under lock).
#[repr(align(8))]
struct ReqHeader(BlkReqHeader);

/// Data buffer used for a single sector transfer.
#[repr(align(512))]
struct DataBuf([u8; SECTOR_SIZE]);

/// Status byte written by device.
struct StatusBuf(u8);

static mut QUEUE_MEM: QueueBuf = QueueBuf([0u8; QUEUE_BYTES]);
static mut REQ_HDR: ReqHeader = ReqHeader(BlkReqHeader {
    typ: 0,
    reserved: 0,
    sector: 0,
});
static mut DATA_BUF: DataBuf = DataBuf([0u8; SECTOR_SIZE]);
static mut STATUS_BYTE: StatusBuf = StatusBuf(0xFF);

// ── Driver state ─────────────────────────────────────────────────────

struct VirtioBlkState {
    present: bool,
    io_base: u16,
    irq_line: u8,
    queue_size: u16,
    avail_idx: u16,
    used_idx: u16,
    /// Total number of sectors on the device (from device config).
    capacity_sectors: u64,
}

impl VirtioBlkState {
    const fn new() -> Self {
        Self {
            present: false,
            io_base: 0,
            irq_line: u8::MAX,
            queue_size: 0,
            avail_idx: 0,
            used_idx: 0,
            capacity_sectors: 0,
        }
    }
}

static STATE: Mutex<VirtioBlkState> = Mutex::new(VirtioBlkState::new());

// ── I/O port helpers ─────────────────────────────────────────────────

fn read_u8(io: u16, off: u16) -> u8 {
    unsafe { inb(io + off) }
}
fn read_u16(io: u16, off: u16) -> u16 {
    unsafe { inw(io + off) }
}
fn read_u32(io: u16, off: u16) -> u32 {
    unsafe { inl(io + off) }
}
fn write_u8(io: u16, off: u16, v: u8) {
    unsafe { outb(io + off, v) }
}
fn write_u16(io: u16, off: u16, v: u16) {
    unsafe { outw(io + off, v) }
}
fn write_u32(io: u16, off: u16, v: u32) {
    unsafe { outl(io + off, v) }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack));
    }
    v
}
#[inline]
unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack));
    }
    v
}
#[inline]
unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe {
        core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack));
    }
    v
}
#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
    }
}
#[inline]
unsafe fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
    }
}
#[inline]
unsafe fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
    }
}

// ── PCI scan ─────────────────────────────────────────────────────────

struct PciBlkDevice {
    io_base: u16,
    irq_line: u8,
}

fn find_blk_device() -> Option<PciBlkDevice> {
    // Try modern device ID first, then legacy.
    for &did in &[VIRTIO_BLK_DEVICE_MODERN, VIRTIO_BLK_DEVICE_LEGACY] {
        if let Some(info) = crate::arch::x86_64::pci::find_device(VIRTIO_VENDOR, did) {
            crate::arch::x86_64::pci::enable_bus_master(info.location);
            let bar0 = crate::arch::x86_64::pci::read_u32(
                info.location.bus,
                info.location.slot,
                info.location.func,
                PCI_BAR0_OFFSET,
            );
            if bar0 & 1 != 0 {
                let io_base = (bar0 & !0x3) as u16;
                return Some(PciBlkDevice {
                    io_base,
                    irq_line: info.irq_line,
                });
            }
        }
    }
    // Also probe legacy device with subsystem check.
    if let Some(info) = crate::arch::x86_64::pci::find_device(VIRTIO_VENDOR, 0x1001) {
        crate::arch::x86_64::pci::enable_bus_master(info.location);
        let bar0 = crate::arch::x86_64::pci::read_u32(
            info.location.bus,
            info.location.slot,
            info.location.func,
            PCI_BAR0_OFFSET,
        );
        if bar0 & 1 != 0 {
            return Some(PciBlkDevice {
                io_base: (bar0 & !0x3) as u16,
                irq_line: info.irq_line,
            });
        }
    }
    None
}

// ── Queue setup ──────────────────────────────────────────────────────

fn setup_queue(io: u16) -> u16 {
    write_u16(io, VIRTIO_PCI_QUEUE_SELECT, 0);
    let qsize = read_u16(io, VIRTIO_PCI_QUEUE_SIZE) as usize;
    if qsize == 0 {
        return 0;
    }
    let actual = qsize.min(QUEUE_SIZE) as u16;

    unsafe {
        let base = core::ptr::addr_of_mut!(QUEUE_MEM.0).cast::<u8>();
        core::ptr::write_bytes(base, 0, QUEUE_BYTES);
        let pfn = (base as usize >> 12) as u32;
        write_u32(io, VIRTIO_PCI_QUEUE_PFN, pfn);
    }
    actual
}

// ── Probe / init ─────────────────────────────────────────────────────

/// Called by `drivers::probe_all()`.
pub fn probe_driver() -> ProbeResult {
    let Some(dev) = find_blk_device() else {
        return ProbeResult::NoMatch;
    };
    let mut state = STATE.lock();
    state.io_base = dev.io_base;
    state.irq_line = dev.irq_line;
    state.present = true;

    // Read capacity (at offset 0x14 in legacy I/O space, after the common virtio header).
    // Legacy layout: common header is 20 bytes, device-specific config starts at offset 20.
    // Capacity (u64) is at offset 0x14 = 20.
    let cap_lo = read_u32(dev.io_base, 0x14);
    let cap_hi = read_u32(dev.io_base, 0x18);
    state.capacity_sectors = ((cap_hi as u64) << 32) | (cap_lo as u64);

    serial::write_bytes(b"[virtio-blk] probe: io=0x");
    serial::write_hex(dev.io_base as u64);
    serial::write_bytes(b" irq=");
    serial::write_u64_dec(dev.irq_line as u64);
    serial::write_bytes(b" sectors=");
    serial::write_u64_dec(state.capacity_sectors);
    serial::write_line(b"");
    ProbeResult::Bound
}

/// Full initialisation: negotiate features, set up queue, DRIVER_OK.
pub fn init() -> bool {
    let state = STATE.lock();
    if !state.present {
        return false;
    }
    let io = state.io_base;
    drop(state);

    // Reset device.
    write_u8(io, VIRTIO_PCI_STATUS, 0);
    // Acknowledge + Driver.
    write_u8(
        io,
        VIRTIO_PCI_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );
    // Accept all offered features.
    let _features = read_u32(io, 0x00);
    write_u32(io, 0x04, 0);
    // Set up request queue.
    let qsize = setup_queue(io);
    if qsize == 0 {
        serial::write_line(b"[virtio-blk] queue size 0 -- init failed");
        return false;
    }
    let mut state = STATE.lock();
    state.queue_size = qsize;
    drop(state);
    // DRIVER_OK.
    write_u8(
        io,
        VIRTIO_PCI_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
    );
    serial::write_line(b"[virtio-blk] init complete");
    true
}

pub fn is_present() -> bool {
    STATE.lock().present
}
pub fn capacity_sectors() -> u64 {
    STATE.lock().capacity_sectors
}

// ── Block I/O ────────────────────────────────────────────────────────

/// Read one 512-byte sector from `sector_lba` into `buf`.
pub fn read_sector(sector_lba: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
    do_request(VIRTIO_BLK_T_IN, sector_lba, buf)
}

/// Write one 512-byte sector to `sector_lba` from `buf`.
pub fn write_sector(sector_lba: u64, buf: &[u8; SECTOR_SIZE]) -> bool {
    // Temporarily copy to the mutable DATA_BUF; do_request reads from it.
    unsafe {
        core::ptr::addr_of_mut!(DATA_BUF.0)
            .cast::<u8>()
            .copy_from_nonoverlapping(buf.as_ptr(), SECTOR_SIZE)
    };
    // SAFETY: DATA_BUF now holds buf's content; we cast it to &mut for do_request.
    let tmp: &mut [u8; SECTOR_SIZE] = unsafe { &mut *core::ptr::addr_of_mut!(DATA_BUF.0) };
    do_request(VIRTIO_BLK_T_OUT, sector_lba, tmp)
}

fn do_request(typ: u32, sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
    interrupts::without_interrupts(|| do_request_inner(typ, sector, buf))
}

fn do_request_inner(typ: u32, sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
    let mut state = STATE.lock();
    if !state.present || state.queue_size == 0 {
        return false;
    }
    let io = state.io_base;
    let qsize = state.queue_size as usize;

    unsafe {
        // Set up request header.
        let hdr_ptr = core::ptr::addr_of_mut!(REQ_HDR.0);
        write_volatile(
            hdr_ptr,
            BlkReqHeader {
                typ,
                reserved: 0,
                sector,
            },
        );

        // Clear status byte (device will write 0=OK, 1=ERR, 2=UNSUPP).
        let status_ptr = core::ptr::addr_of_mut!(STATUS_BYTE.0);
        write_volatile(status_ptr, 0xFF);

        // Copy data to/from DATA_BUF depending on direction.
        let data_ptr = core::ptr::addr_of_mut!(DATA_BUF.0).cast::<u8>();
        if typ == VIRTIO_BLK_T_OUT {
            // For writes, data is already in DATA_BUF (set by write_sector).
        } else {
            // For reads, zero the buffer so stale data is visible.
            core::ptr::write_bytes(data_ptr, 0, SECTOR_SIZE);
        }

        // Build descriptor chain: [hdr] → [data] → [status].
        let base = core::ptr::addr_of_mut!(QUEUE_MEM.0).cast::<u8>();
        let avail_ring = base.add(DESC_BYTES).cast::<u16>();
        let used_ring = base.add(USED_OFFSET).cast::<u16>();

        let d0 = (state.avail_idx as usize) % qsize;
        let d1 = (d0 + 1) % qsize;
        let d2 = (d1 + 1) % qsize;

        let desc_base = base.cast::<VirtqDesc>();

        write_volatile(
            desc_base.add(d0),
            VirtqDesc {
                addr: hdr_ptr as u64,
                len: core::mem::size_of::<BlkReqHeader>() as u32,
                flags: VIRTQ_DESC_F_NEXT,
                next: d1 as u16,
            },
        );
        let data_flags = if typ == VIRTIO_BLK_T_IN {
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE
        } else {
            VIRTQ_DESC_F_NEXT
        };
        write_volatile(
            desc_base.add(d1),
            VirtqDesc {
                addr: data_ptr as u64,
                len: SECTOR_SIZE as u32,
                flags: data_flags,
                next: d2 as u16,
            },
        );
        write_volatile(
            desc_base.add(d2),
            VirtqDesc {
                addr: status_ptr as u64,
                len: 1,
                flags: VIRTQ_DESC_F_WRITE, // device writes status
                next: 0,
            },
        );

        // Post to avail ring.
        let avail_slot = 2 + (state.avail_idx as usize % qsize);
        write_volatile(avail_ring.add(avail_slot), d0 as u16);
        state.avail_idx = state.avail_idx.wrapping_add(1);
        fence(Ordering::SeqCst);
        write_volatile(avail_ring.add(1), state.avail_idx);

        // Notify device (queue 0).
        write_u16(io, VIRTIO_PCI_QUEUE_NOTIFY, 0);

        // Spin-wait for device to consume the request (up to ~5 ms).
        let deadline = crate::arch::x86_64::timer::ticks().saturating_add(5);
        loop {
            fence(Ordering::SeqCst);
            let dev_used = read_volatile(used_ring.add(1));
            if dev_used == state.avail_idx {
                state.used_idx = dev_used;
                break;
            }
            if crate::arch::x86_64::timer::ticks() >= deadline {
                serial::write_line(b"[virtio-blk] request timeout");
                return false;
            }
            core::hint::spin_loop();
        }

        let status = read_volatile(status_ptr);
        if status != 0 {
            serial::write_bytes(b"[virtio-blk] request failed status=");
            serial::write_u64_dec(status as u64);
            serial::write_line(b"");
            return false;
        }

        // For reads, copy data out of DATA_BUF to caller's buf.
        if typ == VIRTIO_BLK_T_IN {
            buf.as_mut_ptr()
                .copy_from_nonoverlapping(data_ptr, SECTOR_SIZE);
        }
    }
    true
}
