// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! virtio-net PCI driver — legacy I/O-port and modern MMIO transports.
//!
//! Implements RX queue (0) + TX queue (1) with static 16-entry rings.
//! Delivers received Ethernet frames to `net::receive_raw_frame()`.
//! Exports `transmit(frame)` for use by the network layer.

use core::mem::size_of;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering, fence};

use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::arch::x86_64::serial;
use crate::drivers::ProbeResult;

// One-time diagnostics: log first IRQ and first received frame.
static FIRST_IRQ_LOGGED: AtomicU32 = AtomicU32::new(0);
static FIRST_FRAME_LOGGED: AtomicU32 = AtomicU32::new(0);
static LAST_RX_DEVICE_USED_LOGGED: AtomicU32 = AtomicU32::new(u32::MAX);

// Lock-free copies of isr_addr and irq_line for use inside the IRQ handler,
// avoiding the STATE spinlock which would deadlock on single-core.
static IRQ_ISR_ADDR: AtomicUsize = AtomicUsize::new(0);
static IRQ_LINE_CACHED: AtomicU8 = AtomicU8::new(u8::MAX);

// ────────────────────────────────────────────────────────────────────
// PCI config-space constants
// ────────────────────────────────────────────────────────────────────

const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

const PCI_COMMAND_OFFSET: u8 = 0x04;
const PCI_BAR0_OFFSET: u8 = 0x10;
const PCI_STATUS_OFFSET: u8 = 0x06;
const PCI_CAP_PTR_OFFSET: u8 = 0x34;
const PCI_INTERRUPT_LINE_OFFSET: u8 = 0x3C;
const PCI_SUBSYSTEM_OFFSET: u8 = 0x2C;

const PCI_COMMAND_IO_SPACE: u16 = 1 << 0;
const PCI_COMMAND_MEMORY_SPACE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;
const PCI_STATUS_CAPABILITIES: u16 = 1 << 4;

// ────────────────────────────────────────────────────────────────────
// Virtio PCI capability constants
// ────────────────────────────────────────────────────────────────────

const PCI_CAP_ID_VENDOR_SPECIFIC: u8 = 0x09;
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

const VIRTIO_PCI_HOST_FEATURES: u16 = 0x00;
const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_DEVICE_STATUS: u16 = 0x12;

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1 << 0;
const VIRTIO_STATUS_DRIVER: u8 = 1 << 1;
const VIRTIO_STATUS_DRIVER_OK: u8 = 1 << 2;
const VIRTIO_STATUS_FEATURES_OK: u8 = 1 << 3;

const VIRTIO_F_VERSION_1: u32 = 1 << 0;

const VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT: usize = 0x00;
const VIRTIO_PCI_COMMON_DEVICE_FEATURE: usize = 0x04;
const VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT: usize = 0x08;
const VIRTIO_PCI_COMMON_DRIVER_FEATURE: usize = 0x0C;
const VIRTIO_PCI_COMMON_DEVICE_STATUS: usize = 0x14;
const VIRTIO_PCI_COMMON_QUEUE_SELECT: usize = 0x16;
const VIRTIO_PCI_COMMON_QUEUE_SIZE: usize = 0x18;
const VIRTIO_PCI_COMMON_QUEUE_ENABLE: usize = 0x1C;
const VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF: usize = 0x1E;
const VIRTIO_PCI_COMMON_QUEUE_DESC: usize = 0x20;
const VIRTIO_PCI_COMMON_QUEUE_AVAIL: usize = 0x28;
const VIRTIO_PCI_COMMON_QUEUE_USED: usize = 0x30;

// ────────────────────────────────────────────────────────────────────
// virtio-net specific
// ────────────────────────────────────────────────────────────────────

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_MODERN_NET_DEVICE_ID: u16 = 0x1041;
const VIRTIO_NET_SUBSYSTEM_DEVICE_ID: u16 = 1;

const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_HDR_SIZE: usize = 10;
const MAX_ETHERNET_FRAME: usize = 1514;
const NET_BUF_SIZE: usize = VIRTIO_NET_HDR_SIZE + MAX_ETHERNET_FRAME;

const NETQ_RX: u16 = 0;
const NETQ_TX: u16 = 1;
const NETQ_SIZE: usize = 256;

const VIRTQ_DESC_F_WRITE: u16 = 1 << 1;
const VIRTQ_ALIGNMENT: usize = 4096;

// ────────────────────────────────────────────────────────────────────
// Queue layout maths
// ────────────────────────────────────────────────────────────────────

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

const DESC_BYTES: usize = size_of::<VirtqDesc>() * NETQ_SIZE;
const AVAIL_BYTES: usize = 4 + NETQ_SIZE * 2;
const USED_OFFSET: usize = align_up(DESC_BYTES + AVAIL_BYTES, VIRTQ_ALIGNMENT);
const USED_BYTES: usize = 4 + NETQ_SIZE * size_of::<VirtqUsedElem>();
const QUEUE_BYTES: usize = USED_OFFSET + USED_BYTES;

// ────────────────────────────────────────────────────────────────────
// Virtqueue structures
// ────────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

#[repr(align(4096))]
struct QueueMemory([u8; QUEUE_BYTES]);

// ────────────────────────────────────────────────────────────────────
// Static storage
// ────────────────────────────────────────────────────────────────────

static mut RX_QUEUE_MEMORY: QueueMemory = QueueMemory([0; QUEUE_BYTES]);
static mut TX_QUEUE_MEMORY: QueueMemory = QueueMemory([0; QUEUE_BYTES]);
static mut RX_BUFFERS: [[u8; NET_BUF_SIZE]; NETQ_SIZE] = [[0; NET_BUF_SIZE]; NETQ_SIZE];
static mut TX_BUFFER: [u8; NET_BUF_SIZE] = [0; NET_BUF_SIZE];

// ────────────────────────────────────────────────────────────────────
// Driver state
// ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    Legacy,
    Modern,
}

#[derive(Clone, Copy)]
struct PciDevice {
    transport: Transport,
    bus: u8,
    slot: u8,
    func: u8,
    io_base: u16,
    common_cfg_addr: usize,
    notify_cfg_addr: usize,
    notify_off_multiplier: u32,
    isr_cfg_addr: usize,
    device_cfg_addr: usize,
    irq_line: u8,
}

struct VirtioNetState {
    present: bool,
    transport: Transport,
    io_base: u16,
    isr_addr: usize,
    notify_rx_addr: usize,
    notify_tx_addr: usize,
    rx_queue_size: u16,
    tx_queue_size: u16,
    rx_avail_idx: u16,
    rx_used_idx: u16,
    tx_avail_idx: u16,
    tx_used_idx: u16,
    irq_line: u8,
    mac: [u8; 6],
}

impl VirtioNetState {
    const fn new() -> Self {
        Self {
            present: false,
            transport: Transport::Legacy,
            io_base: 0,
            isr_addr: 0,
            notify_rx_addr: 0,
            notify_tx_addr: 0,
            rx_queue_size: 0,
            tx_queue_size: 0,
            rx_avail_idx: 0,
            rx_used_idx: 0,
            tx_avail_idx: 0,
            tx_used_idx: 0,
            irq_line: u8::MAX,
            mac: [0; 6],
        }
    }
}

static STATE: Mutex<VirtioNetState> = Mutex::new(VirtioNetState::new());

// ────────────────────────────────────────────────────────────────────
// Public interface
// ────────────────────────────────────────────────────────────────────

/// Called by `drivers::probe_all()`. Discovers the device and updates STATE.
pub fn probe_driver() -> ProbeResult {
    let Some(dev) = find_net_device() else {
        return ProbeResult::NoMatch;
    };

    let mac = read_mac(dev);

    // Publish isr_addr and irq_line to lock-free atomics so the IRQ handler
    // can read/clear the ISR without acquiring STATE (avoids single-core deadlock).
    IRQ_ISR_ADDR.store(dev.isr_cfg_addr, Ordering::Release);
    IRQ_LINE_CACHED.store(dev.irq_line, Ordering::Release);

    let mut state = STATE.lock();
    state.transport = dev.transport;
    state.io_base = dev.io_base;
    state.isr_addr = dev.isr_cfg_addr;
    state.irq_line = dev.irq_line;
    state.mac = mac;
    state.present = true;

    serial::write_bytes(b"[virtio-net] probe ok irq=");
    serial::write_u64_dec_inline(dev.irq_line as u64);
    serial::write_bytes(b" mac=");
    for (i, b) in mac.iter().enumerate() {
        serial::write_hex_inline(*b as u64);
        if i < 5 {
            serial::write_bytes(b":");
        }
    }
    serial::write_line(b"");
    ProbeResult::Bound
}

/// Full driver init — negotiate features, configure RX + TX queues.
/// Called from `main.rs` after `probe_all()`.
pub fn init() -> bool {
    let dev = match find_net_device() {
        Some(d) => d,
        None => return false,
    };
    if !STATE.lock().present {
        return false;
    }
    if !configure_queues(dev) {
        serial::write_line(b"[virtio-net] queue configure failed");
        return false;
    }
    let mac = STATE.lock().mac;
    crate::net::set_our_mac(mac);
    crate::net::set_tx_hook(transmit);
    crate::net::set_link_ready(true);
    // Unmask the PIC IRQ line so the kernel receives virtio-net interrupts.
    let irq = STATE.lock().irq_line;
    if irq != u8::MAX {
        unsafe { crate::arch::x86_64::pic::unmask(irq) };
    }
    serial::write_line(b"[virtio-net] init complete -- rx+tx online");
    serial::write_line(b"[virtio] calling dhcp::start");
    crate::net::dhcp::start();
    serial::write_bytes(b"[virtio] dhcp::start returned. set_hook=");
    serial::write_u64_dec_inline(crate::net::SET_TX_HOOK_COUNT.load(Ordering::Relaxed) as u64);
    serial::write_bytes(b" tx_calls=");
    serial::write_u64_dec_inline(crate::net::TX_HOOK_CALL_COUNT.load(Ordering::Relaxed) as u64);
    serial::write_bytes(b" no_hook=");
    serial::write_u64_dec_inline(crate::net::TX_NO_HOOK_COUNT.load(Ordering::Relaxed) as u64);
    serial::write_line(b"");
    true
}

static TX_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Transmit a single Ethernet frame (no virtio header; we prepend it).
pub fn transmit(frame: &[u8]) -> bool {
    if frame.len() > MAX_ETHERNET_FRAME {
        return false;
    }
    let n = TX_COUNT.fetch_add(1, Ordering::Relaxed);
    if n == 0 {
        serial::write_line(b"[virtio] first TX");
    }
    interrupts::without_interrupts(|| transmit_inner(frame))
}

/// IRQ handler — reads ISR, drains RX ring.
///
/// Uses lock-free atomics for isr_addr / irq_line to avoid acquiring
/// STATE.lock() inside the IRQ handler.  On a single-core system,
/// taking a blocking spinlock here deadlocks whenever the orchestrator
/// task holds STATE when the interrupt fires.
pub fn handle_irq(irq: u8) -> bool {
    let cached_irq = IRQ_LINE_CACHED.load(Ordering::Relaxed);
    if cached_irq == u8::MAX || cached_irq != irq {
        return false;
    }
    let isr_addr = IRQ_ISR_ADDR.load(Ordering::Relaxed);
    // For legacy transport isr_addr is 0; fall through to poll_rx anyway
    // since the legacy ISR is read via I/O port — skip the check.
    let is_ours = if isr_addr != 0 {
        // Read-to-clear: clears the ISR bit so QEMU can fire the next IRQ.
        (mmio_read_u8(isr_addr) & 0x1u8) != 0
    } else {
        // Legacy: always proceed (poll_rx will no-op if nothing to drain).
        true
    };
    if !is_ours {
        return false;
    }
    if FIRST_IRQ_LOGGED.fetch_add(1, Ordering::Relaxed) == 0 {
        serial::write_line(b"[virtio] first IRQ");
    }
    poll_rx();
    true
}

static POLL_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Drain the RX used-ring and deliver frames to the network layer.
/// Try to drain the RX ring once. Uses try_lock so it is safe to call from
/// ISR / timer context without risk of spinning on STATE.
pub fn poll_rx() {
    let mut state = match STATE.try_lock() {
        Some(s) => s,
        None => return, // lock contended — skip this cycle
    };
    if !state.present || state.rx_queue_size == 0 {
        return;
    }
    let qsize = state.rx_queue_size as usize;
    let mut replenished = false;

    unsafe {
        let queue_base = core::ptr::addr_of_mut!(RX_QUEUE_MEMORY.0).cast::<u8>();
        let avail_ring = queue_base.add(DESC_BYTES).cast::<u16>();
        let used_ring = queue_base.add(USED_OFFSET).cast::<u16>();
        let device_used_idx = read_volatile(used_ring.add(1));
        let prev_logged = LAST_RX_DEVICE_USED_LOGGED.load(Ordering::Relaxed);
        if prev_logged != device_used_idx as u32 {
            LAST_RX_DEVICE_USED_LOGGED.store(device_used_idx as u32, Ordering::Relaxed);
            serial::write_bytes(b"[virtio] rx used advanced to ");
            serial::write_u64_dec(device_used_idx as u64);
        }
        // Log once every 100 polls to confirm poll_rx is running.
        let pc = POLL_COUNT.fetch_add(1, Ordering::Relaxed);
        if pc == 0 || pc == 99 {
            serial::write_bytes(b"[virtio] poll_rx: rx_used=");
            serial::write_u64_dec_inline(state.rx_used_idx as u64);
            serial::write_bytes(b" dev_used=");
            serial::write_u64_dec_inline(device_used_idx as u64);
            serial::write_line(b"");
        }

        while state.rx_used_idx != device_used_idx {
            let slot = (state.rx_used_idx as usize) % qsize;
            let elem = read_volatile(used_ring.add(2).cast::<VirtqUsedElem>().add(slot));
            let head = elem.id as usize;
            let rx_len = elem.len as usize;
            if head < qsize && rx_len > VIRTIO_NET_HDR_SIZE {
                let frame_len = rx_len - VIRTIO_NET_HDR_SIZE;
                // Copy frame out so we can release the lock while dispatching.
                let mut frame_copy = [0u8; MAX_ETHERNET_FRAME];
                let copy_len = frame_len.min(MAX_ETHERNET_FRAME);
                frame_copy[..copy_len].copy_from_slice(
                    &RX_BUFFERS[head][VIRTIO_NET_HDR_SIZE..VIRTIO_NET_HDR_SIZE + copy_len],
                );
                if FIRST_FRAME_LOGGED.fetch_add(1, Ordering::Relaxed) == 0 {
                    serial::write_line(b"[virtio] first RX frame");
                }
                // Log inbound TCP segments destined for port 22 (SSH).
                if copy_len >= 36
                    && frame_copy[12] == 0x08 && frame_copy[13] == 0x00  // IPv4
                    && frame_copy[23] == 0x06
                // TCP
                {
                    let src_port = u16::from_be_bytes([frame_copy[34], frame_copy[35]]);
                    let dst_port = u16::from_be_bytes([frame_copy[36], frame_copy[37]]);
                    serial::write_bytes(b"[virtio] RX TCP src=");
                    serial::write_u64_dec_inline(src_port as u64);
                    serial::write_bytes(b" dst=");
                    serial::write_u64_dec(dst_port as u64);
                    if dst_port == 22 {
                        serial::write_line(b"[virtio] RX TCP->22");
                    }
                }
                drop(state);
                crate::net::receive_raw_frame(&frame_copy[..copy_len]);
                // Re-acquire lock (ok to spin here — outside ISR hot path).
                state = loop {
                    if let Some(s) = STATE.try_lock() {
                        break s;
                    }
                    core::hint::spin_loop();
                };
                // Re-queue descriptor.
                let avail_slot = 2 + (state.rx_avail_idx as usize % qsize);
                write_volatile(avail_ring.add(avail_slot), head as u16);
                state.rx_avail_idx = state.rx_avail_idx.wrapping_add(1);
                replenished = true;
            }
            state.rx_used_idx = state.rx_used_idx.wrapping_add(1);
        }

        if replenished {
            fence(Ordering::SeqCst);
            write_volatile(avail_ring.add(1), state.rx_avail_idx);
            notify_queue(&state, NETQ_RX);
        }
    }
}

pub fn is_present() -> bool {
    STATE.lock().present
}

pub fn irq_line() -> Option<u8> {
    let state = STATE.lock();
    if state.present && state.irq_line != u8::MAX {
        Some(state.irq_line)
    } else {
        None
    }
}

// ────────────────────────────────────────────────────────────────────
// Transmit inner (interrupts already disabled)
// ────────────────────────────────────────────────────────────────────

fn transmit_inner(frame: &[u8]) -> bool {
    let mut state = STATE.lock();
    if !state.present || state.tx_queue_size == 0 {
        return false;
    }
    let qsize = state.tx_queue_size as usize;

    unsafe {
        // Prepend zeroed virtio_net_hdr (10 bytes) before the Ethernet frame.
        let tx_buf_ptr = core::ptr::addr_of_mut!(TX_BUFFER).cast::<u8>();
        core::ptr::write_bytes(tx_buf_ptr, 0, VIRTIO_NET_HDR_SIZE);
        let total_len = VIRTIO_NET_HDR_SIZE + frame.len();
        core::ptr::copy_nonoverlapping(
            frame.as_ptr(),
            tx_buf_ptr.add(VIRTIO_NET_HDR_SIZE),
            frame.len(),
        );

        let queue_base = core::ptr::addr_of_mut!(TX_QUEUE_MEMORY.0).cast::<u8>();
        let avail_ring = queue_base.add(DESC_BYTES).cast::<u16>();
        let used_ring = queue_base.add(USED_OFFSET).cast::<u16>();

        let desc_idx = (state.tx_avail_idx as usize) % qsize;
        let desc_ptr = queue_base.cast::<VirtqDesc>().add(desc_idx);
        write_volatile(
            desc_ptr,
            VirtqDesc {
                addr: core::ptr::addr_of!(TX_BUFFER) as u64,
                len: total_len as u32,
                flags: 0,
                next: 0,
            },
        );

        let avail_slot = 2 + (state.tx_avail_idx as usize % qsize);
        write_volatile(avail_ring.add(avail_slot), desc_idx as u16);
        state.tx_avail_idx = state.tx_avail_idx.wrapping_add(1);
        fence(Ordering::SeqCst);
        write_volatile(avail_ring.add(1), state.tx_avail_idx);
        notify_queue(&state, NETQ_TX);

        // Spin-wait up to 5 timer ticks for TX completion.
        let deadline = crate::arch::x86_64::timer::ticks().saturating_add(5);
        loop {
            let dev_used = read_volatile(used_ring.add(1));
            if dev_used == state.tx_avail_idx {
                state.tx_used_idx = dev_used;
                break;
            }
            if crate::arch::x86_64::timer::ticks() >= deadline {
                serial::write_line(b"[virtio] TX timeout: dev did not consume buf");
                break;
            }
            core::hint::spin_loop();
        }
    }
    true
}

// ────────────────────────────────────────────────────────────────────
// Queue notification
// ────────────────────────────────────────────────────────────────────

fn notify_queue(state: &VirtioNetState, queue_idx: u16) {
    match state.transport {
        Transport::Legacy => {
            write_u16(state.io_base + VIRTIO_PCI_QUEUE_NOTIFY, queue_idx);
        }
        Transport::Modern => {
            let addr = if queue_idx == NETQ_RX {
                state.notify_rx_addr
            } else {
                state.notify_tx_addr
            };
            if addr != 0 {
                mmio_write_u16(addr, queue_idx);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Device discovery
// ────────────────────────────────────────────────────────────────────

fn find_net_device() -> Option<PciDevice> {
    for bus in 0u8..=255 {
        for slot in 0u8..32 {
            for func in 0u8..8 {
                let id = pci_read_u32(bus, slot, func, 0x00);
                let vendor = (id & 0xFFFF) as u16;
                if vendor == 0xFFFF {
                    continue;
                }
                let device = (id >> 16) as u16;
                if vendor != VIRTIO_VENDOR_ID {
                    continue;
                }

                let mut command = pci_read_u16(bus, slot, func, PCI_COMMAND_OFFSET);
                command |= PCI_COMMAND_IO_SPACE | PCI_COMMAND_MEMORY_SPACE | PCI_COMMAND_BUS_MASTER;
                pci_write_u16(bus, slot, func, PCI_COMMAND_OFFSET, command);

                if device == VIRTIO_MODERN_NET_DEVICE_ID
                    && let Some(dev) = probe_modern(bus, slot, func)
                {
                    return Some(dev);
                }

                let subsys = pci_read_u32(bus, slot, func, PCI_SUBSYSTEM_OFFSET);
                let sub_device = (subsys >> 16) as u16;
                if sub_device == VIRTIO_NET_SUBSYSTEM_DEVICE_ID {
                    let bar0 = pci_read_u32(bus, slot, func, PCI_BAR0_OFFSET);
                    if bar0 & 0x1 != 0 {
                        return Some(PciDevice {
                            transport: Transport::Legacy,
                            bus,
                            slot,
                            func,
                            io_base: (bar0 & !0x3) as u16,
                            common_cfg_addr: 0,
                            notify_cfg_addr: 0,
                            notify_off_multiplier: 0,
                            isr_cfg_addr: 0,
                            device_cfg_addr: (bar0 & !0x3) as usize,
                            irq_line: pci_read_u8(bus, slot, func, PCI_INTERRUPT_LINE_OFFSET),
                        });
                    }
                }
            }
        }
    }
    None
}

fn probe_modern(bus: u8, slot: u8, func: u8) -> Option<PciDevice> {
    if pci_read_u16(bus, slot, func, PCI_STATUS_OFFSET) & PCI_STATUS_CAPABILITIES == 0 {
        return None;
    }

    let mut common_cfg_addr = 0usize;
    let mut notify_cfg_addr = 0usize;
    let mut notify_off_multiplier = 0u32;
    let mut isr_cfg_addr = 0usize;
    let mut device_cfg_addr = 0usize;

    let mut cap_ptr = pci_read_u8(bus, slot, func, PCI_CAP_PTR_OFFSET) & !0x3;
    let mut hops = 0u8;
    while cap_ptr >= 0x40 && cap_ptr != 0 && hops < 32 {
        let cap_id = pci_read_u8(bus, slot, func, cap_ptr);
        let next = pci_read_u8(bus, slot, func, cap_ptr + 1) & !0x3;
        if cap_id == PCI_CAP_ID_VENDOR_SPECIFIC {
            let cfg_type = pci_read_u8(bus, slot, func, cap_ptr + 3);
            let bar = pci_read_u8(bus, slot, func, cap_ptr + 4);
            let offset = pci_read_u32(bus, slot, func, cap_ptr + 8) as usize;
            if let Some(bar_addr) = pci_read_bar_address(bus, slot, func, bar) {
                let addr = bar_addr.saturating_add(offset as u64) as usize;
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => common_cfg_addr = addr,
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        notify_cfg_addr = addr;
                        notify_off_multiplier = pci_read_u32(bus, slot, func, cap_ptr + 16);
                    }
                    VIRTIO_PCI_CAP_ISR_CFG => isr_cfg_addr = addr,
                    VIRTIO_PCI_CAP_DEVICE_CFG => device_cfg_addr = addr,
                    _ => {}
                }
            }
        }
        cap_ptr = next;
        hops = hops.saturating_add(1);
    }

    if common_cfg_addr == 0 || notify_cfg_addr == 0 || isr_cfg_addr == 0 {
        return None;
    }

    if !crate::mm::page_table::ensure_identity_mapped_2m(common_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(notify_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(isr_cfg_addr as u64)
    {
        return None;
    }
    if device_cfg_addr != 0 {
        let _ = crate::mm::page_table::ensure_identity_mapped_2m(device_cfg_addr as u64);
    }

    Some(PciDevice {
        transport: Transport::Modern,
        bus,
        slot,
        func,
        io_base: 0,
        common_cfg_addr,
        notify_cfg_addr,
        notify_off_multiplier,
        isr_cfg_addr,
        device_cfg_addr,
        irq_line: pci_read_u8(bus, slot, func, PCI_INTERRUPT_LINE_OFFSET),
    })
}

// ────────────────────────────────────────────────────────────────────
// Queue configuration
// ────────────────────────────────────────────────────────────────────

fn configure_queues(device: PciDevice) -> bool {
    match device.transport {
        Transport::Legacy => configure_queues_legacy(device),
        Transport::Modern => configure_queues_modern(device),
    }
}

fn configure_queues_legacy(device: PciDevice) -> bool {
    let io = device.io_base;
    write_u8(io + VIRTIO_PCI_DEVICE_STATUS, 0);
    let _ = read_u8(io + 0x13);
    write_u8(io + VIRTIO_PCI_DEVICE_STATUS, VIRTIO_STATUS_ACKNOWLEDGE);
    write_u8(
        io + VIRTIO_PCI_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );
    let host_feat = read_u32(io + VIRTIO_PCI_HOST_FEATURES);
    write_u32(io + VIRTIO_PCI_GUEST_FEATURES, host_feat & VIRTIO_NET_F_MAC);

    write_u16(io + VIRTIO_PCI_QUEUE_SELECT, NETQ_RX);
    let rx_size = read_u16(io + VIRTIO_PCI_QUEUE_SIZE);
    if rx_size == 0 || rx_size as usize > NETQ_SIZE {
        return false;
    }
    let rx_layout = prepare_rx_queue_memory(rx_size);
    write_u32(io + VIRTIO_PCI_QUEUE_PFN, rx_layout.pfn);

    write_u16(io + VIRTIO_PCI_QUEUE_SELECT, NETQ_TX);
    let tx_size = read_u16(io + VIRTIO_PCI_QUEUE_SIZE);
    if tx_size == 0 || tx_size as usize > NETQ_SIZE {
        return false;
    }
    let tx_layout = prepare_tx_queue_memory(tx_size);
    write_u32(io + VIRTIO_PCI_QUEUE_PFN, tx_layout.pfn);

    write_u8(
        io + VIRTIO_PCI_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
    );
    write_u16(io + VIRTIO_PCI_QUEUE_NOTIFY, NETQ_RX);
    fence(Ordering::SeqCst);

    let mut state = STATE.lock();
    state.rx_queue_size = rx_size;
    state.tx_queue_size = tx_size;
    state.rx_avail_idx = rx_size;
    state.tx_avail_idx = 0;
    state.rx_used_idx = 0;
    state.tx_used_idx = 0;
    true
}

fn configure_queues_modern(device: PciDevice) -> bool {
    let common = device.common_cfg_addr;
    mmio_write_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS, 0);
    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    mmio_write_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 0);
    let feat_lo = mmio_read_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE);
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 0);
    mmio_write_u32(
        common + VIRTIO_PCI_COMMON_DRIVER_FEATURE,
        feat_lo & VIRTIO_NET_F_MAC,
    );

    mmio_write_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 1);
    let feat_hi = mmio_read_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE);
    if feat_hi & VIRTIO_F_VERSION_1 == 0 {
        return false;
    }
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 1);
    mmio_write_u32(
        common + VIRTIO_PCI_COMMON_DRIVER_FEATURE,
        VIRTIO_F_VERSION_1,
    );

    let status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
    mmio_write_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS, status);
    if mmio_read_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS) & VIRTIO_STATUS_FEATURES_OK == 0 {
        return false;
    }

    // RX queue.
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SELECT, NETQ_RX);
    let rx_size = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE);
    if rx_size == 0 || rx_size as usize > NETQ_SIZE {
        return false;
    }
    let rx_notify_off = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF) as usize;
    let rx_notify = device.notify_cfg_addr
        + rx_notify_off.saturating_mul(device.notify_off_multiplier as usize);
    let rx_layout = prepare_rx_queue_memory(rx_size);
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE, rx_size);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_DESC, rx_layout.desc_addr);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_AVAIL, rx_layout.avail_addr);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_USED, rx_layout.used_addr);
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE, 1);

    // TX queue.
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SELECT, NETQ_TX);
    let tx_size = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE);
    if tx_size == 0 || tx_size as usize > NETQ_SIZE {
        return false;
    }
    let tx_notify_off = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF) as usize;
    let tx_notify = device.notify_cfg_addr
        + tx_notify_off.saturating_mul(device.notify_off_multiplier as usize);
    let tx_layout = prepare_tx_queue_memory(tx_size);
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE, tx_size);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_DESC, tx_layout.desc_addr);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_AVAIL, tx_layout.avail_addr);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_USED, tx_layout.used_addr);
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE, 1);

    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        status | VIRTIO_STATUS_DRIVER_OK,
    );
    mmio_write_u16(rx_notify, NETQ_RX);
    fence(Ordering::SeqCst);

    let mut state = STATE.lock();
    state.isr_addr = device.isr_cfg_addr;
    state.notify_rx_addr = rx_notify;
    state.notify_tx_addr = tx_notify;
    state.rx_queue_size = rx_size;
    state.tx_queue_size = tx_size;
    state.rx_avail_idx = rx_size;
    state.tx_avail_idx = 0;
    state.rx_used_idx = 0;
    state.tx_used_idx = 0;
    true
}

// ────────────────────────────────────────────────────────────────────
// Queue memory preparation
// ────────────────────────────────────────────────────────────────────

struct QueueLayout {
    pfn: u32,
    desc_addr: u64,
    avail_addr: u64,
    used_addr: u64,
}

fn prepare_rx_queue_memory(queue_size: u16) -> QueueLayout {
    unsafe {
        let base = core::ptr::addr_of_mut!(RX_QUEUE_MEMORY.0).cast::<u8>();
        core::ptr::write_bytes(base, 0, QUEUE_BYTES);
        let qsize = queue_size as usize;

        let desc_ptr = base.cast::<VirtqDesc>();
        let bufs_ptr = core::ptr::addr_of!(RX_BUFFERS).cast::<[u8; NET_BUF_SIZE]>();
        let mut i = 0usize;
        while i < qsize {
            let buf = bufs_ptr.add(i);
            write_volatile(
                desc_ptr.add(i),
                VirtqDesc {
                    addr: (*buf).as_ptr() as u64,
                    len: NET_BUF_SIZE as u32,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );
            i += 1;
        }

        let avail = base.add(DESC_BYTES).cast::<u16>();
        write_volatile(avail, 0);
        write_volatile(avail.add(1), queue_size);
        for i in 0..qsize {
            write_volatile(avail.add(2 + i), i as u16);
        }

        let used = base.add(USED_OFFSET).cast::<u16>();
        write_volatile(used, 0);
        write_volatile(used.add(1), 0);

        QueueLayout {
            pfn: (base as usize >> 12) as u32,
            desc_addr: desc_ptr as u64,
            avail_addr: avail as u64,
            used_addr: used as u64,
        }
    }
}

fn prepare_tx_queue_memory(queue_size: u16) -> QueueLayout {
    unsafe {
        let base = core::ptr::addr_of_mut!(TX_QUEUE_MEMORY.0).cast::<u8>();
        core::ptr::write_bytes(base, 0, QUEUE_BYTES);
        let qsize = queue_size as usize;

        let desc_ptr = base.cast::<VirtqDesc>();
        for i in 0..qsize {
            write_volatile(
                desc_ptr.add(i),
                VirtqDesc {
                    addr: core::ptr::addr_of!(TX_BUFFER) as u64,
                    len: NET_BUF_SIZE as u32,
                    flags: 0,
                    next: 0,
                },
            );
        }

        let avail = base.add(DESC_BYTES).cast::<u16>();
        write_volatile(avail, 0);
        write_volatile(avail.add(1), 0);

        let used = base.add(USED_OFFSET).cast::<u16>();
        write_volatile(used, 0);
        write_volatile(used.add(1), 0);

        QueueLayout {
            pfn: (base as usize >> 12) as u32,
            desc_addr: desc_ptr as u64,
            avail_addr: avail as u64,
            used_addr: used as u64,
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// MAC address read from device config space
// ────────────────────────────────────────────────────────────────────

fn read_mac(device: PciDevice) -> [u8; 6] {
    let mut mac = [0u8; 6];
    match device.transport {
        Transport::Legacy => {
            for (i, byte) in mac.iter_mut().enumerate() {
                *byte = read_u8(device.io_base + 0x14 + i as u16);
            }
        }
        Transport::Modern => {
            if device.device_cfg_addr != 0 {
                for (i, byte) in mac.iter_mut().enumerate() {
                    *byte = mmio_read_u8(device.device_cfg_addr + i);
                }
            }
        }
    }
    mac
}

// ────────────────────────────────────────────────────────────────────
// PCI config-space helpers
// ────────────────────────────────────────────────────────────────────

fn pci_read_u8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let v = pci_read_u32(bus, slot, func, offset);
    ((v >> (((offset & 0x3) as u32) * 8)) & 0xFF) as u8
}

fn pci_read_u16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let v = pci_read_u32(bus, slot, func, offset);
    ((v >> (((offset & 0x2) as u32) * 8)) & 0xFFFF) as u16
}

fn pci_read_u32(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, address);
        inl(PCI_CONFIG_DATA)
    }
}

fn pci_write_u16(bus: u8, slot: u8, func: u8, offset: u8, value: u16) {
    let address = 0x8000_0000u32
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, address);
        let shift = ((offset & 0x2) as u32) * 8;
        let current = inl(PCI_CONFIG_DATA);
        let mask = !(0xFFFFu32 << shift);
        outl(
            PCI_CONFIG_DATA,
            (current & mask) | ((value as u32) << shift),
        );
    }
}

fn pci_read_bar_address(bus: u8, slot: u8, func: u8, bar: u8) -> Option<u64> {
    if bar >= 6 {
        return None;
    }
    let offset = PCI_BAR0_OFFSET + bar * 4;
    let low = pci_read_u32(bus, slot, func, offset);
    if low == 0 {
        return None;
    }
    if low & 0x1 != 0 {
        return Some((low & !0x3) as u64);
    }
    let addr = match (low >> 1) & 0x3 {
        0x0 => (low & !0xF) as u64,
        0x2 if bar + 1 < 6 => {
            let high = pci_read_u32(bus, slot, func, offset + 4);
            ((high as u64) << 32) | (low & !0xF) as u64
        }
        _ => return None,
    };
    Some(addr)
}

// ────────────────────────────────────────────────────────────────────
// MMIO helpers
// ────────────────────────────────────────────────────────────────────

fn mmio_read_u8(addr: usize) -> u8 {
    unsafe { read_volatile(addr as *const u8) }
}

fn mmio_read_u16(addr: usize) -> u16 {
    unsafe { read_volatile(addr as *const u16) }
}

fn mmio_read_u32(addr: usize) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

fn mmio_write_u8(addr: usize, value: u8) {
    unsafe { write_volatile(addr as *mut u8, value) }
}

fn mmio_write_u16(addr: usize, value: u16) {
    unsafe { write_volatile(addr as *mut u16, value) }
}

fn mmio_write_u32(addr: usize, value: u32) {
    unsafe { write_volatile(addr as *mut u32, value) }
}

fn mmio_write_u64(addr: usize, value: u64) {
    mmio_write_u32(addr, value as u32);
    mmio_write_u32(addr + 4, (value >> 32) as u32);
}

// ────────────────────────────────────────────────────────────────────
// I/O port helpers
// ────────────────────────────────────────────────────────────────────

fn read_u8(port: u16) -> u8 {
    unsafe { inb(port) }
}
fn read_u16(port: u16) -> u16 {
    unsafe { inw(port) }
}
fn read_u32(port: u16) -> u32 {
    unsafe { inl(port) }
}
fn write_u8(port: u16, value: u8) {
    unsafe { outb(port, value) }
}
fn write_u16(port: u16, value: u16) {
    unsafe { outw(port, value) }
}
fn write_u32(port: u16, value: u32) {
    unsafe { outl(port, value) }
}

unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe {
        core::arch::asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") v,
            options(nomem, nostack, preserves_flags),
        );
    }
    v
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

unsafe fn outw(port: u16, value: u16) {
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

unsafe fn outl(port: u16, value: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
}
