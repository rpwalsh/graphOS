// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! virtio-input driver — keyboard and mouse events from QEMU.
//!
//! virtio-input (vendor 0x1AF4, device 0x1052) provides evdev-like events.
//! Each event is an 8-byte `InputEvent { type, code, value }` struct delivered
//! via a single eventq virtqueue.  This driver initialises the device and
//! routes received events to the kernel input router.

use spin::Mutex;

use crate::arch::serial;
use crate::drivers::ProbeResult;

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_INPUT_DEVICE: u16 = 0x1052;

/// evdev event types.
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;

/// A decoded input event.
#[derive(Clone, Copy, Debug)]
pub struct InputEvent {
    pub typ: u16,
    pub code: u16,
    pub value: i32,
}

// ── Virtio PCI legacy I/O port register offsets ───────────────────────────────
const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const PCI_INTERRUPT_LINE_OFFSET: u8 = 0x3C;

// ── Virtqueue layout for eventq (queue 0) ─────────────────────────────────────
// 16 descriptors, each pointing to an 8-byte event buffer (device-writeable).
const INQ_SIZE: usize = 16;
const INQ_DESC_BYTES: usize = 16 * INQ_SIZE; // 256
const INQ_AVAIL_BYTES: usize = 4 + 2 * INQ_SIZE + 2; // flags+idx+ring[16]+used_event = 38
const INQ_PAGE_BYTES: usize = 4096;
const INQ_USED_OFF: usize = INQ_PAGE_BYTES; // page boundary
const INQ_USED_BYTES: usize = 4 + 8 * INQ_SIZE; // flags+idx+ring[16] = 132
const INQ_QUEUE_BYTES: usize = INQ_USED_OFF + INQ_USED_BYTES;

#[repr(align(4096))]
struct InqMem([u8; INQ_QUEUE_BYTES]);
static mut INQ_MEM: InqMem = InqMem([0u8; INQ_QUEUE_BYTES]);

/// 16 × 8-byte event receive buffers (device fills them).
#[repr(align(8))]
struct InqBufs([[u8; 8]; INQ_SIZE]);
static mut INQ_BUFS: InqBufs = InqBufs([[0u8; 8]; INQ_SIZE]);

// ── Driver state ──────────────────────────────────────────────────────────────

struct VirtioInputState {
    present: bool,
    io_base: u16,
    irq_line: u8,
    used_idx: u16,
}

impl VirtioInputState {
    const fn new() -> Self {
        Self {
            present: false,
            io_base: 0,
            irq_line: u8::MAX,
            used_idx: 0,
        }
    }
}

static STATE: Mutex<VirtioInputState> = Mutex::new(VirtioInputState::new());

/// Pending event ring (lock-protected).
const EVENT_RING_SIZE: usize = 64;
struct EventRing {
    buf: [InputEvent; EVENT_RING_SIZE],
    head: usize,
    tail: usize,
}

impl EventRing {
    const fn new() -> Self {
        Self {
            buf: [InputEvent {
                typ: 0,
                code: 0,
                value: 0,
            }; EVENT_RING_SIZE],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, ev: InputEvent) {
        let next = (self.tail + 1) % EVENT_RING_SIZE;
        if next == self.head {
            return;
        } // full, drop
        self.buf[self.tail] = ev;
        self.tail = next;
    }

    fn pop(&mut self) -> Option<InputEvent> {
        if self.head == self.tail {
            return None;
        }
        let ev = self.buf[self.head];
        self.head = (self.head + 1) % EVENT_RING_SIZE;
        Some(ev)
    }
}

static EVENTS: Mutex<EventRing> = Mutex::new(EventRing::new());

// ── I/O port helpers ──────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn vio_write16(base: u16, off: u16, val: u16) {
    unsafe {
        x86_64::instructions::port::PortWrite::write_to_port(base + off, val);
    }
}
#[inline(always)]
unsafe fn vio_write32(base: u16, off: u16, val: u32) {
    unsafe {
        x86_64::instructions::port::PortWrite::write_to_port(base + off, val);
    }
}
#[inline(always)]
unsafe fn vio_read16(base: u16, off: u16) -> u16 {
    unsafe { x86_64::instructions::port::PortRead::read_from_port(base + off) }
}

// ── Probe ─────────────────────────────────────────────────────────────────────

pub fn probe_driver() -> ProbeResult {
    let dev = match crate::arch::x86_64::pci::find_device(VIRTIO_VENDOR, VIRTIO_INPUT_DEVICE) {
        Some(d) => d,
        None => return ProbeResult::NoMatch,
    };

    let bar0_raw = crate::arch::x86_64::pci::read_u32(
        dev.location.bus,
        dev.location.slot,
        dev.location.func,
        0x10,
    );
    let io_base = (bar0_raw & !0x3) as u16;
    if bar0_raw & 0x1 == 0 || io_base == 0 {
        // This fallback driver only supports the legacy I/O transport.
        // Modern virtio-input (MMIO cfg caps) is handled by
        // arch::x86_64::virtio_input.
        return ProbeResult::NoMatch;
    }
    let irq_line = crate::arch::x86_64::pci::read_u8(
        dev.location.bus,
        dev.location.slot,
        dev.location.func,
        PCI_INTERRUPT_LINE_OFFSET,
    );

    {
        let mut st = STATE.lock();
        st.present = true;
        st.io_base = io_base;
        st.irq_line = irq_line;
    }

    // Set up eventq (queue 0) with pre-filled writable descriptors.
    unsafe {
        setup_eventq(io_base);
    }

    serial::write_line(b"[virtio-input] bound");
    ProbeResult::Bound
}

/// Pre-fill eventq descriptors with writable 8-byte event buffers.
///
/// # Safety
/// `io_base` must be the virtio-input legacy I/O port base.
unsafe fn setup_eventq(io_base: u16) {
    unsafe {
        vio_write16(io_base, VIRTIO_PCI_QUEUE_SELECT, 0);
        let qsize = vio_read16(io_base, VIRTIO_PCI_QUEUE_SIZE) as usize;
        if qsize == 0 {
            return;
        }
        let qsize = qsize.min(INQ_SIZE);

        let q = core::ptr::addr_of_mut!(INQ_MEM.0).cast::<u8>();
        let buf_base = core::ptr::addr_of!(INQ_BUFS.0) as u64;

        // Build descriptor table: each entry points to one 8-byte event buffer.
        // flags = VIRTQ_DESC_F_WRITE(1<<1) = 2 (device writes to buffer).
        for i in 0..qsize {
            let desc = q.add(16 * i);
            let buf_pa: u64 = buf_base + (i as u64 * 8);
            core::ptr::write_unaligned(desc as *mut u64, buf_pa); // addr
            core::ptr::write_unaligned(desc.add(8) as *mut u32, 8u32); // len
            core::ptr::write_unaligned(desc.add(12) as *mut u16, 2u16); // flags = WRITE
            core::ptr::write_unaligned(desc.add(14) as *mut u16, 0u16); // next
        }

        // Available ring: flags=0, idx=qsize, ring[0..qsize]=0..qsize-1.
        let avail = q.add(INQ_DESC_BYTES);
        core::ptr::write_unaligned(avail as *mut u16, 0u16); // flags
        core::ptr::write_unaligned(avail.add(2) as *mut u16, qsize as u16); // idx
        for i in 0..qsize {
            core::ptr::write_unaligned(avail.add(4 + i * 2) as *mut u16, i as u16);
        }

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Register queue PFN.
        let queue_pa = core::ptr::addr_of!(INQ_MEM.0) as u64;
        vio_write32(io_base, VIRTIO_PCI_QUEUE_PFN, (queue_pa >> 12) as u32);

        // Kick the device to start delivering events.
        vio_write16(io_base, VIRTIO_PCI_QUEUE_NOTIFY, 0);
    }
}

// ── IRQ / event injection ─────────────────────────────────────────────────────

/// Called from the IRQ handler when the eventq has available buffers.
/// Drains the eventq used ring and pushes events to the EVENTS ring.
pub fn handle_irq() {
    let (io_base, mut used_idx) = {
        let st = STATE.lock();
        if !st.present {
            return;
        }
        (st.io_base, st.used_idx)
    };
    let _ = io_base;

    unsafe {
        // Used ring is at INQ_USED_OFF in the queue memory page.
        let used_ptr = core::ptr::addr_of!(INQ_MEM.0)
            .cast::<u8>()
            .add(INQ_USED_OFF);
        // used.idx is at offset 2 of the used ring (flags at 0, idx at 2).
        let device_used_idx = core::ptr::read_volatile(used_ptr.add(2) as *const u16);

        while used_idx != device_used_idx {
            // used.ring[used_idx % INQ_SIZE]: id(4) + len(4) at offset (4 + 8*i).
            let ring_idx = (used_idx as usize) % INQ_SIZE;
            let elem = used_ptr.add(4 + 8 * ring_idx);
            let desc_id = core::ptr::read_volatile(elem as *const u32) as usize;
            // desc_id indexes into INQ_BUFS.
            if desc_id < INQ_SIZE {
                let ev_bytes: [u8; 8] = core::ptr::read_volatile(
                    (core::ptr::addr_of!(INQ_BUFS.0) as *const [u8; 8]).add(desc_id),
                );
                // virtio_input_event: type(u16) + code(u16) + value(i32)
                let typ = u16::from_le_bytes([ev_bytes[0], ev_bytes[1]]);
                let code = u16::from_le_bytes([ev_bytes[2], ev_bytes[3]]);
                let value =
                    i32::from_le_bytes([ev_bytes[4], ev_bytes[5], ev_bytes[6], ev_bytes[7]]);
                EVENTS.lock().push(InputEvent { typ, code, value });

                // Recycle descriptor: put it back in the available ring.
                let q = core::ptr::addr_of_mut!(INQ_MEM.0).cast::<u8>();
                let avail = q.add(INQ_DESC_BYTES);
                let cur_avail_idx = core::ptr::read_volatile(avail.add(2) as *const u16);
                let slot = (cur_avail_idx as usize) % INQ_SIZE;
                core::ptr::write_unaligned(avail.add(4 + slot * 2) as *mut u16, desc_id as u16);
                core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
                core::ptr::write_volatile(avail.add(2) as *mut u16, cur_avail_idx.wrapping_add(1));
                if io_base != 0 {
                    vio_write16(io_base, VIRTIO_PCI_QUEUE_NOTIFY, 0);
                }
            }
            used_idx = used_idx.wrapping_add(1);
        }
    }

    STATE.lock().used_idx = used_idx;
}

/// Inject an event (used by PS/2 keyboard glue and future virtqueue drain).
pub fn inject_event(ev: InputEvent) {
    EVENTS.lock().push(ev);
}

/// Poll the next pending event, if any.
pub fn poll_event() -> Option<InputEvent> {
    EVENTS.lock().pop()
}

pub fn is_present() -> bool {
    STATE.lock().present
}

pub fn irq_line() -> Option<u8> {
    let state = STATE.lock();
    if !state.present || state.irq_line == u8::MAX {
        None
    } else {
        Some(state.irq_line)
    }
}

pub fn has_pending_event() -> bool {
    let events = EVENTS.lock();
    events.head != events.tail
}

/// Poll path used by timer fallback when shared IRQ delivery is delayed/lost.
/// This reuses the regular used-ring drain logic.
pub fn poll_input() {
    handle_irq();
}
