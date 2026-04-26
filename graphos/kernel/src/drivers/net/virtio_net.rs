// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! virtio-net PCI driver — Rx/Tx ring setup and packet I/O.
//!
//! Conforms to virtio spec 1.2 (legacy and modern device).
//! - Device ID: 0x1000 (legacy virtio-net), Vendor 0x1AF4
//! - Queues: 0 = RX, 1 = TX
//!
//! This driver hands received frames directly to `net::ethernet::handle_frame()`
//! and exposes `transmit()` for the upper networking stack.

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Mutex;

use crate::arch::x86_64::pci;

/// PCI vendor/device IDs for virtio-net.
const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_NET_DEVICE: u16 = 0x1000;

/// virtio-net legacy I/O port offsets.
const VIRTIO_PCI_HOST_FEATURES: u16 = 0x00;
const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_PCI_QUEUE_ADDR: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_STATUS: u16 = 0x12;
const VIRTIO_PCI_ISR: u16 = 0x13;
/// Offset 0x14–0x17 = Device-specific config (MAC address for net device).
const VIRTIO_NET_MAC_OFFSET: u16 = 0x14;

/// virtio status register bits.
const VIRTIO_STATUS_ACK: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FAILED: u8 = 128;

/// virtio feature bits (legacy).
const VIRTIO_NET_F_MAC: u32 = 1 << 5;

/// Queue entries (must be power of 2, ≤ 32768).
const QUEUE_SIZE: usize = 16;

/// Maximum frame size.
const MAX_FRAME: usize = 1514;

/// virtio-net header (legacy, no GSO).
const VIRT_HDR_LEN: usize = 10;

// ── Virtqueue layout ─────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct VirtqAvail {
    flags: u16,
    idx: u16,
    ring: [u16; QUEUE_SIZE],
    used_event: u16,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
struct VirtqUsed {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; QUEUE_SIZE],
    avail_event: u16,
}

// 4096-aligned static DMA region: descriptor table + available ring.
#[repr(C, align(4096))]
struct TxDescPage {
    desc: [VirtqDesc; QUEUE_SIZE],
    avail: VirtqAvail,
}

// Second page: used ring (device writes here).
#[repr(C, align(4096))]
struct TxUsedPage {
    used: VirtqUsed,
}

// TX frame buffer (virtio-net header + Ethernet frame).
#[repr(C, align(16))]
struct TxFrameBuf {
    hdr: [u8; VIRT_HDR_LEN],
    data: [u8; MAX_FRAME + 14], // +14 for Ethernet header
}

static mut TX_DESC_PAGE: TxDescPage = TxDescPage {
    desc: [VirtqDesc {
        addr: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; QUEUE_SIZE],
    avail: VirtqAvail {
        flags: 0,
        idx: 0,
        ring: [0; QUEUE_SIZE],
        used_event: 0,
    },
};
static mut TX_USED_PAGE: TxUsedPage = TxUsedPage {
    used: VirtqUsed {
        flags: 0,
        idx: 0,
        ring: [VirtqUsedElem { id: 0, len: 0 }; QUEUE_SIZE],
        avail_event: 0,
    },
};
static mut TX_FRAME_BUF: TxFrameBuf = TxFrameBuf {
    hdr: [0u8; VIRT_HDR_LEN],
    data: [0u8; MAX_FRAME + 14],
};

// ── TX ring state (static allocation, no heap) ───────────────────────────────

struct TxState {
    /// I/O base port for this device.
    io_base: u16,
    /// Our MAC address.
    mac: [u8; 6],
    ready: bool,
    /// Next descriptor index to use (wraps at QUEUE_SIZE).
    desc_idx: u16,
    /// Last avail->idx value we wrote (tracks pending TX).
    avail_idx: u16,
}

impl TxState {
    const fn new() -> Self {
        Self {
            io_base: 0,
            mac: [0u8; 6],
            ready: false,
            desc_idx: 0,
            avail_idx: 0,
        }
    }
}

static TX: Mutex<TxState> = Mutex::new(TxState::new());
static DRIVER_READY: AtomicBool = AtomicBool::new(false);

// ── Init ─────────────────────────────────────────────────────────────────────

/// Probe PCI bus for virtio-net and initialise if found.
///
/// Returns `true` if a device was found and initialised.
pub fn init() -> bool {
    let io_base = match pci::find_device(VIRTIO_VENDOR, VIRTIO_NET_DEVICE) {
        Some(dev) => {
            // BAR0 contains the I/O port base for legacy virtio.
            let bar0 = pci::read_u32(dev.location.bus, dev.location.slot, dev.location.func, 0x10);
            if bar0 & 1 == 0 {
                crate::arch::serial::write_line(b"[virtio-net] BAR0 is not I/O space");
                return false;
            }
            (bar0 & !0x3) as u16
        }
        None => {
            crate::arch::serial::write_line(b"[virtio-net] no virtio-net device found");
            return false;
        }
    };

    // Legacy virtio init sequence.
    unsafe {
        // 1. Reset device.
        io_write8(io_base + VIRTIO_PCI_STATUS, 0);
        // 2. ACK.
        io_write8(io_base + VIRTIO_PCI_STATUS, VIRTIO_STATUS_ACK);
        // 3. Driver.
        io_write8(
            io_base + VIRTIO_PCI_STATUS,
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER,
        );

        // 4. Negotiate features (request MAC feature).
        let host_features = io_read32(io_base + VIRTIO_PCI_HOST_FEATURES);
        let guest_features = host_features & VIRTIO_NET_F_MAC;
        io_write32(io_base + VIRTIO_PCI_GUEST_FEATURES, guest_features);

        // 5. Read MAC address from device config.
        let mut mac = [0u8; 6];
        for (i, b) in mac.iter_mut().enumerate() {
            *b = io_read8(io_base + VIRTIO_NET_MAC_OFFSET + i as u16);
        }
        crate::arch::serial::write_line(b"[virtio-net] MAC read from device");

        // 6. Driver OK.
        io_write8(
            io_base + VIRTIO_PCI_STATUS,
            VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
        );

        // 7. Set up TX virtqueue (queue 1).
        //    The queue address sent to the device is the page number of the descriptor table.
        io_write16(io_base + VIRTIO_PCI_QUEUE_SELECT, 1);
        let _qsize = io_read16(io_base + VIRTIO_PCI_QUEUE_SIZE); // read max; we use QUEUE_SIZE
        let desc_phys = core::ptr::addr_of!(TX_DESC_PAGE) as u32;
        io_write32(io_base + VIRTIO_PCI_QUEUE_ADDR, desc_phys / 4096);

        // Store state.
        let mut tx = TX.lock();
        tx.io_base = io_base;
        tx.mac = mac;
        tx.ready = true;
        tx.desc_idx = 0;
        tx.avail_idx = 0;

        // Publish our IPv6 link-local address and initialise grid.
        let ll = crate::net::ipv6::Ipv6Addr::link_local_from_mac(mac);
        crate::net::set_our_ipv6(ll.0);
        if !crate::grid::is_active() {
            crate::grid::init(mac);
        }
    }

    DRIVER_READY.store(true, Ordering::Release);
    crate::arch::serial::write_line(b"[virtio-net] initialised");
    true
}

/// Transmit `frame` (raw Ethernet frame, including header).
///
/// Builds a virtio-net header + frame into the static TX buffer, posts it to
/// the TX virtqueue, notifies the device, and spins until the used ring advances.
///
/// Returns `true` if the device accepted the packet.
pub fn transmit(frame: &[u8]) -> bool {
    if !DRIVER_READY.load(Ordering::Acquire) {
        return false;
    }
    if frame.is_empty() || frame.len() > MAX_FRAME + 14 {
        return false;
    }

    let mut tx = TX.lock();
    if !tx.ready {
        return false;
    }

    let io_base = tx.io_base;
    let desc_idx = tx.desc_idx as usize;
    let avail_idx = tx.avail_idx;

    unsafe {
        // Copy frame into the static TX buffer (after the 10-byte virtio-net header).
        // The header is already zeroed (flags=0, gso_type=0 = no offload).
        let frame_buf = core::ptr::addr_of_mut!(TX_FRAME_BUF);
        (*frame_buf).hdr = [0u8; VIRT_HDR_LEN];
        (&mut (*frame_buf).data)[..frame.len()].copy_from_slice(frame);

        let hdr_addr = core::ptr::addr_of!((*frame_buf).hdr) as u64;
        let data_addr = core::ptr::addr_of!((*frame_buf).data) as u64;

        // Descriptor 0: virtio-net header (VIRT_HDR_LEN bytes), NEXT flag set.
        let next_idx = ((desc_idx + 1) % QUEUE_SIZE) as u16;
        let dp = core::ptr::addr_of_mut!(TX_DESC_PAGE);
        (*dp).desc[desc_idx] = VirtqDesc {
            addr: hdr_addr,
            len: VIRT_HDR_LEN as u32,
            flags: 1, // VRING_DESC_F_NEXT
            next: next_idx,
        };
        // Descriptor 1: frame data.
        (*dp).desc[next_idx as usize] = VirtqDesc {
            addr: data_addr,
            len: frame.len() as u32,
            flags: 0,
            next: 0,
        };

        // Add the head descriptor index to the available ring.
        let avail_ring_idx = (avail_idx as usize) % QUEUE_SIZE;
        (*dp).avail.ring[avail_ring_idx] = desc_idx as u16;
        core::sync::atomic::fence(Ordering::Release);
        (*dp).avail.idx = avail_idx.wrapping_add(1);
        core::sync::atomic::fence(Ordering::Release);

        // Notify device: queue 1 = TX.
        io_write16(io_base + VIRTIO_PCI_QUEUE_NOTIFY, 1);

        // Spin until the used ring advances (device consumed the descriptor).
        let used_ptr = core::ptr::addr_of!(TX_USED_PAGE);
        let mut spins = 0u32;
        loop {
            core::sync::atomic::fence(Ordering::Acquire);
            if (*used_ptr).used.idx == avail_idx.wrapping_add(1) {
                break;
            }
            spins += 1;
            if spins > 500_000 {
                return false;
            } // device timeout
            core::hint::spin_loop();
        }

        // Advance state.
        tx.desc_idx = ((desc_idx + 2) % QUEUE_SIZE) as u16;
        tx.avail_idx = avail_idx.wrapping_add(1);
    }
    true
}

/// Returns the MAC address of the virtio-net device, or all-zeros if not init.
pub fn our_mac() -> [u8; 6] {
    TX.lock().mac
}

// ── Low-level PCI I/O helpers (x86 in/out instructions) ─────────────────────

#[inline]
unsafe fn io_read8(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    }
    val
}

#[inline]
unsafe fn io_read16(port: u16) -> u16 {
    let val: u16;
    unsafe {
        core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack));
    }
    val
}

#[inline]
unsafe fn io_write8(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
    }
}

#[inline]
unsafe fn io_read32(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack));
    }
    val
}

#[inline]
unsafe fn io_write32(port: u16, val: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
    }
}

#[inline]
unsafe fn io_write16(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
    }
}
