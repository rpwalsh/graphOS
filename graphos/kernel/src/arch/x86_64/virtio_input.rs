// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use core::mem::size_of;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::arch::x86_64::serial;
use crate::input::pointer::{MouseButton, PointerEvent};

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

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_DEVICE_INPUT: u16 = 18;
const VIRTIO_TRANSITIONAL_DEVICE_BASE: u16 = 0x1000;
const VIRTIO_MODERN_INPUT_DEVICE_ID: u16 = 0x1052;
const ENABLE_MODERN_VIRTIO_INPUT: bool = false;

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
const VIRTIO_PCI_ISR_STATUS: u16 = 0x13;
const VIRTIO_PCI_DEVICE_CONFIG: u16 = 0x14;

const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1 << 0;
const VIRTIO_STATUS_DRIVER: u8 = 1 << 1;
const VIRTIO_STATUS_DRIVER_OK: u8 = 1 << 2;
const VIRTIO_STATUS_FEATURES_OK: u8 = 1 << 3;
const VIRTIO_STATUS_FAILED: u8 = 1 << 7;

const VIRTIO_F_VERSION_1: u32 = 1 << 0;

const VIRTQ_DESC_F_WRITE: u16 = 1 << 1;
const VIRTQ_ALIGNMENT: usize = 4096;
const MAX_QUEUE_SIZE: usize = 256;
const EVENT_QUEUE_CAPACITY: usize = 512;
const DEFAULT_EVENT_QUEUE_SIZE: u16 = 64;
const DEFAULT_STATUS_QUEUE_SIZE: u16 = 8;

const VIRTIO_INPUT_CFG_ABS_INFO: u8 = 0x12;
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const ABS_X: u8 = 0x00;
const ABS_Y: u8 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

const INPUT_EVENTQ_INDEX: u16 = 0;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    Legacy,
    Modern,
}

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

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtioInputEvent {
    ty: u16,
    code: u16,
    value: u32,
}

#[repr(align(4096))]
struct QueueMemory([u8; QUEUE_MEMORY_BYTES]);

const ZERO_INPUT_EVENT: VirtioInputEvent = VirtioInputEvent {
    ty: 0,
    code: 0,
    value: 0,
};

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

const DESC_BYTES_MAX: usize = size_of::<VirtqDesc>() * MAX_QUEUE_SIZE;
const AVAIL_BYTES_MAX: usize = 4 + MAX_QUEUE_SIZE * 2;
const USED_OFFSET_MAX: usize = align_up(DESC_BYTES_MAX + AVAIL_BYTES_MAX, VIRTQ_ALIGNMENT);
const USED_BYTES_MAX: usize = 4 + MAX_QUEUE_SIZE * size_of::<VirtqUsedElem>();
const QUEUE_MEMORY_BYTES: usize = USED_OFFSET_MAX + USED_BYTES_MAX;

static STATE: Mutex<VirtioInputState> = Mutex::new(VirtioInputState::new());
static mut EVENT_QUEUE_MEMORY: QueueMemory = QueueMemory([0; QUEUE_MEMORY_BYTES]);
static mut STATUS_QUEUE_MEMORY: QueueMemory = QueueMemory([0; QUEUE_MEMORY_BYTES]);
static mut EVENT_BUFFERS: [VirtioInputEvent; MAX_QUEUE_SIZE] = [ZERO_INPUT_EVENT; MAX_QUEUE_SIZE];

struct VirtioInputState {
    present: bool,
    transport: TransportMode,
    io_base: u16,
    notify_addr: usize,
    isr_addr: usize,
    irq_line: u8,
    queue_size: u16,
    avail_idx: u16,
    used_idx: u16,
    eventq_primed: bool,
    abs_x_min: u32,
    abs_x_max: u32,
    abs_y_min: u32,
    abs_y_max: u32,
    current_abs_x: u32,
    current_abs_y: u32,
    pending_abs_x: Option<u32>,
    pending_abs_y: Option<u32>,
    pending_rel_x: i32,
    pending_rel_y: i32,
    display_width: u32,
    display_height: u32,
    events: [Option<PointerEvent>; EVENT_QUEUE_CAPACITY],
    head: usize,
    len: usize,
}

impl VirtioInputState {
    const fn new() -> Self {
        Self {
            present: false,
            transport: TransportMode::Legacy,
            io_base: 0,
            notify_addr: 0,
            isr_addr: 0,
            irq_line: u8::MAX,
            queue_size: 0,
            avail_idx: 0,
            used_idx: 0,
            eventq_primed: false,
            abs_x_min: 0,
            abs_x_max: 0,
            abs_y_min: 0,
            abs_y_max: 0,
            current_abs_x: 0,
            current_abs_y: 0,
            pending_abs_x: None,
            pending_abs_y: None,
            pending_rel_x: 0,
            pending_rel_y: 0,
            display_width: 0,
            display_height: 0,
            events: [None; EVENT_QUEUE_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    fn reset_runtime(&mut self) {
        self.avail_idx = 0;
        self.used_idx = 0;
        self.eventq_primed = false;
        self.current_abs_x = 0;
        self.current_abs_y = 0;
        self.pending_abs_x = None;
        self.pending_abs_y = None;
        self.pending_rel_x = 0;
        self.pending_rel_y = 0;
        self.events = [None; EVENT_QUEUE_CAPACITY];
        self.head = 0;
        self.len = 0;
    }

    fn push_event(&mut self, event: PointerEvent) {
        if self.len != 0 {
            let tail = (self.head + self.len - 1) % self.events.len();
            if matches!(self.events[tail], Some(PointerEvent::Absolute { .. }))
                && matches!(event, PointerEvent::Absolute { .. })
            {
                self.events[tail] = Some(event);
                crate::input::diagnostics::record_pointer_event(event, self.len, true);
                return;
            }
        }
        let tail = (self.head + self.len) % self.events.len();
        self.events[tail] = Some(event);
        if self.len == self.events.len() {
            self.head = (self.head + 1) % self.events.len();
        } else {
            self.len += 1;
        }
        crate::input::diagnostics::record_pointer_event(event, self.len, false);
    }

    fn pop_event(&mut self) -> Option<PointerEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.events[self.head].take();
        self.head = (self.head + 1) % self.events.len();
        self.len -= 1;
        event
    }

    fn flush_frame(&mut self) {
        let had_abs = self.pending_abs_x.is_some() || self.pending_abs_y.is_some();
        let had_rel = self.pending_rel_x != 0 || self.pending_rel_y != 0;

        if let Some(x) = self.pending_abs_x.take() {
            self.current_abs_x = x;
        }
        if let Some(y) = self.pending_abs_y.take() {
            self.current_abs_y = y;
        }

        if had_abs
            && self.abs_x_max > self.abs_x_min
            && self.abs_y_max > self.abs_y_min
            && self.display_width != 0
            && self.display_height != 0
        {
            let x = scale_axis(
                self.current_abs_x,
                self.abs_x_min,
                self.abs_x_max,
                self.display_width,
            );
            let y = scale_axis(
                self.current_abs_y,
                self.abs_y_min,
                self.abs_y_max,
                self.display_height,
            );
            self.push_event(PointerEvent::Absolute { x, y });
        } else if had_rel {
            let dx = self.pending_rel_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let dy = self.pending_rel_y.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            self.push_event(PointerEvent::Move { dx, dy });
        }

        self.pending_rel_x = 0;
        self.pending_rel_y = 0;
    }
}

pub fn set_display_bounds(width: u32, height: u32) {
    let mut state = STATE.lock();
    state.display_width = width;
    state.display_height = height;
}

pub fn init() -> bool {
    let Some(device) = find_input_device() else {
        return false;
    };

    let Some((queue_size, notify_addr, abs_x, abs_y)) = interrupts::without_interrupts(|| {
        let (queue_size, notify_addr) = configure_input_queue(device)?;
        let abs_x = read_abs_info(device.device_cfg_addr, device.transport, ABS_X);
        let abs_y = read_abs_info(device.device_cfg_addr, device.transport, ABS_Y);
        Some((queue_size, notify_addr, abs_x, abs_y))
    }) else {
        serial::write_line(b"[virtio-input] failed to configure event queue");
        return false;
    };

    let mut state = STATE.lock();
    state.transport = device.transport;
    state.io_base = device.io_base;
    state.notify_addr = notify_addr;
    state.isr_addr = device.isr_cfg_addr;
    state.irq_line = device.irq_line;
    state.queue_size = queue_size;
    state.reset_runtime();
    state.avail_idx = if matches!(device.transport, TransportMode::Modern) {
        0
    } else {
        queue_size
    };
    state.eventq_primed = !matches!(device.transport, TransportMode::Modern);
    if let Some(abs) = abs_x {
        state.abs_x_min = abs.min;
        state.abs_x_max = abs.max;
    }
    if let Some(abs) = abs_y {
        state.abs_y_min = abs.min;
        state.abs_y_max = abs.max;
    }
    state.present = true;

    serial::write_bytes(b"[virtio-input] online transport=");
    serial::write_bytes(match state.transport {
        TransportMode::Legacy => b"legacy",
        TransportMode::Modern => b"modern",
    });
    serial::write_bytes(b" irq=");
    serial::write_u64_dec_inline(state.irq_line as u64);
    serial::write_bytes(b" mode=");
    if state.abs_x_max > state.abs_x_min && state.abs_y_max > state.abs_y_min {
        serial::write_line(b"absolute");
    } else {
        serial::write_line(b"relative");
    }
    true
}

pub fn try_read_event() -> Option<PointerEvent> {
    interrupts::without_interrupts(|| {
        let mut state = STATE.lock();
        if !state.present {
            return None;
        }
        let event = state.pop_event();
        if event.is_some() {
            crate::input::diagnostics::record_pointer_delivery(state.len);
        }
        event
    })
}

pub fn has_pending_event() -> bool {
    interrupts::without_interrupts(|| {
        let state = STATE.lock();
        state.present && state.len != 0
    })
}

pub fn irq_line() -> Option<u8> {
    let state = STATE.lock();
    if !state.present || state.irq_line == u8::MAX {
        None
    } else {
        Some(state.irq_line)
    }
}

/// Poll the virtio event queue without waiting for an IRQ.
/// Uses try_lock so it is safe to call from timer ISR context.
pub fn poll_input() {
    if let Some(mut state) = STATE.try_lock() {
        if !state.present || state.queue_size == 0 {
            return;
        }
        if !state.eventq_primed {
            prime_event_queue(&mut state);
            return;
        }
        if process_used_events(&mut state) {
            crate::sched::notify_interactive_input();
        }
    }
}

pub fn handle_irq(irq: u8) -> bool {
    let mut state = STATE.lock();
    if !state.present || state.irq_line != irq {
        return false;
    }

    let is_ours = match state.transport {
        TransportMode::Legacy => {
            let status = read_u8(state.io_base + VIRTIO_PCI_ISR_STATUS);
            status & 0x1 != 0
        }
        TransportMode::Modern => {
            if state.isr_addr == 0 {
                false
            } else {
                mmio_read_u8(state.isr_addr) & 0x1 != 0
            }
        }
    };
    if !is_ours {
        return false;
    }

    if !state.eventq_primed {
        prime_event_queue(&mut state);
        return true;
    }

    if process_used_events(&mut state) || state.len != 0 {
        crate::sched::notify_interactive_input();
    }
    true
}

#[derive(Clone, Copy)]
struct PciDevice {
    transport: TransportMode,
    io_base: u16,
    common_cfg_addr: usize,
    notify_cfg_addr: usize,
    notify_off_multiplier: u32,
    isr_cfg_addr: usize,
    device_cfg_addr: usize,
    irq_line: u8,
}

fn find_input_device() -> Option<PciDevice> {
    let transitional_input_device_id = VIRTIO_TRANSITIONAL_DEVICE_BASE + VIRTIO_DEVICE_INPUT;
    let mut modern_candidate = None;
    let mut legacy_candidate = None;
    let mut seen = [(0u8, 0u8, 0u8, 0u16, 0u16); 8];
    let mut seen_len = 0usize;

    crate::arch::x86_64::pci::for_each_device(|info| {
        if info.vendor_id != VIRTIO_VENDOR_ID {
            return;
        }

        let bus = info.location.bus;
        let slot = info.location.slot;
        let func = info.location.func;
        let device = info.device_id;
        let subsystem = pci_read_u32(bus, slot, func, PCI_SUBSYSTEM_OFFSET);
        let subsystem_device = (subsystem >> 16) as u16;

        if seen_len < seen.len() {
            seen[seen_len] = (bus, slot, func, device, subsystem_device);
            seen_len += 1;
        }

        let mut command = pci_read_u16(bus, slot, func, PCI_COMMAND_OFFSET);
        command |= PCI_COMMAND_IO_SPACE | PCI_COMMAND_MEMORY_SPACE | PCI_COMMAND_BUS_MASTER;
        pci_write_u16(bus, slot, func, PCI_COMMAND_OFFSET, command);

        if ENABLE_MODERN_VIRTIO_INPUT
            && modern_candidate.is_none()
            && device == VIRTIO_MODERN_INPUT_DEVICE_ID
        {
            modern_candidate = probe_modern_device(bus, slot, func);
        }

        if legacy_candidate.is_none()
            && (subsystem_device == VIRTIO_DEVICE_INPUT || device == transitional_input_device_id)
        {
            let bar0 = pci_read_u32(bus, slot, func, PCI_BAR0_OFFSET);
            if bar0 & 0x1 != 0 {
                legacy_candidate = Some(PciDevice {
                    transport: TransportMode::Legacy,
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
    });

    let candidate = modern_candidate.or(legacy_candidate);
    if candidate.is_none() {
        serial::write_line(b"[virtio-input] no PCI input candidate matched");
        for &(bus, slot, func, device, subsystem_device) in &seen[..seen_len] {
            serial::write_bytes(b"[virtio-input] saw virtio pci bus=");
            serial::write_u64_dec_inline(bus as u64);
            serial::write_bytes(b" slot=");
            serial::write_u64_dec_inline(slot as u64);
            serial::write_bytes(b" func=");
            serial::write_u64_dec_inline(func as u64);
            serial::write_bytes(b" did=");
            serial::write_hex_inline(device as u64);
            serial::write_bytes(b" subdev=");
            serial::write_hex(subsystem_device as u64);
        }
    }
    candidate
}

fn probe_modern_device(bus: u8, slot: u8, func: u8) -> Option<PciDevice> {
    if pci_read_u16(bus, slot, func, PCI_STATUS_OFFSET) & PCI_STATUS_CAPABILITIES == 0 {
        return None;
    }

    let mut common_cfg_addr = 0usize;
    let mut common_cfg_len = 0usize;
    let mut notify_cfg_addr = 0usize;
    let mut notify_cfg_len = 0usize;
    let mut notify_off_multiplier = 0u32;
    let mut isr_cfg_addr = 0usize;
    let mut isr_cfg_len = 0usize;
    let mut device_cfg_addr = 0usize;
    let mut device_cfg_len = 0usize;

    let mut cap_ptr = pci_read_u8(bus, slot, func, PCI_CAP_PTR_OFFSET) & !0x3;
    let mut hops = 0u8;
    while cap_ptr >= 0x40 && cap_ptr != 0 && hops < 32 {
        let cap_id = pci_read_u8(bus, slot, func, cap_ptr);
        let cap_len = pci_read_u8(bus, slot, func, cap_ptr + 2);
        let next = pci_read_u8(bus, slot, func, cap_ptr + 1) & !0x3;
        if cap_id == PCI_CAP_ID_VENDOR_SPECIFIC {
            if cap_len < 16 {
                cap_ptr = next;
                hops = hops.saturating_add(1);
                continue;
            }
            let cfg_type = pci_read_u8(bus, slot, func, cap_ptr + 3);
            let bar = pci_read_u8(bus, slot, func, cap_ptr + 4);
            let offset = pci_read_u32(bus, slot, func, cap_ptr + 8) as usize;
            let length = pci_read_u32(bus, slot, func, cap_ptr + 12) as usize;
            if let Some(bar_addr) = pci_read_bar_address(bus, slot, func, bar) {
                if length == 0 {
                    cap_ptr = next;
                    hops = hops.saturating_add(1);
                    continue;
                }
                let Some(addr_u64) = bar_addr.checked_add(offset as u64) else {
                    cap_ptr = next;
                    hops = hops.saturating_add(1);
                    continue;
                };
                let addr = addr_u64 as usize;
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => {
                        common_cfg_addr = addr;
                        common_cfg_len = length;
                    }
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        if cap_len < 20 {
                            cap_ptr = next;
                            hops = hops.saturating_add(1);
                            continue;
                        }
                        notify_cfg_addr = addr;
                        notify_cfg_len = length;
                        notify_off_multiplier = pci_read_u32(bus, slot, func, cap_ptr + 16);
                    }
                    VIRTIO_PCI_CAP_ISR_CFG => {
                        isr_cfg_addr = addr;
                        isr_cfg_len = length;
                    }
                    VIRTIO_PCI_CAP_DEVICE_CFG => {
                        device_cfg_addr = addr;
                        device_cfg_len = length;
                    }
                    _ => {}
                }
            }
        }
        cap_ptr = next;
        hops = hops.saturating_add(1);
    }

    if common_cfg_addr == 0
        || notify_cfg_addr == 0
        || isr_cfg_addr == 0
        || device_cfg_addr == 0
        || common_cfg_len == 0
        || notify_cfg_len == 0
        || isr_cfg_len == 0
        || device_cfg_len == 0
        || notify_off_multiplier == 0
    {
        return None;
    }

    let common_cfg_end = common_cfg_addr.saturating_add(common_cfg_len.saturating_sub(1));
    let notify_cfg_end = notify_cfg_addr.saturating_add(notify_cfg_len.saturating_sub(1));
    let isr_cfg_end = isr_cfg_addr.saturating_add(isr_cfg_len.saturating_sub(1));
    let device_cfg_end = device_cfg_addr.saturating_add(device_cfg_len.saturating_sub(1));

    if !crate::mm::page_table::ensure_identity_mapped_2m(common_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(common_cfg_end as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(notify_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(notify_cfg_end as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(isr_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(isr_cfg_end as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(device_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(device_cfg_end as u64)
    {
        serial::write_line(b"[virtio-input] failed to map virtio MMIO window");
        return None;
    }

    serial::write_bytes(b"[virtio-input] modern pci ");
    serial::write_u64_dec_inline(slot as u64);
    serial::write_bytes(b".");
    serial::write_u64_dec_inline(func as u64);
    serial::write_bytes(b" common=");
    serial::write_hex_inline(common_cfg_addr as u64);
    serial::write_bytes(b" notify=");
    serial::write_hex_inline(notify_cfg_addr as u64);
    serial::write_bytes(b" isr=");
    serial::write_hex_inline(isr_cfg_addr as u64);
    serial::write_bytes(b" device=");
    serial::write_hex(device_cfg_addr as u64);

    Some(PciDevice {
        transport: TransportMode::Modern,
        io_base: 0,
        common_cfg_addr,
        notify_cfg_addr,
        notify_off_multiplier,
        isr_cfg_addr,
        device_cfg_addr,
        irq_line: pci_read_u8(bus, slot, func, PCI_INTERRUPT_LINE_OFFSET),
    })
}

fn configure_input_queue(device: PciDevice) -> Option<(u16, usize)> {
    match device.transport {
        TransportMode::Legacy => {
            let queue_size = write_input_queue_legacy(device.io_base);
            if queue_size == 0 {
                None
            } else {
                Some((queue_size, 0))
            }
        }
        TransportMode::Modern => write_input_queue_modern(device),
    }
}

fn write_input_queue_legacy(io_base: u16) -> u16 {
    write_u8(io_base + VIRTIO_PCI_DEVICE_STATUS, 0);
    let _ = read_u8(io_base + VIRTIO_PCI_ISR_STATUS);

    write_u8(
        io_base + VIRTIO_PCI_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    write_u8(
        io_base + VIRTIO_PCI_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    let _ = read_u32(io_base + VIRTIO_PCI_HOST_FEATURES);
    write_u32(io_base + VIRTIO_PCI_GUEST_FEATURES, 0);

    write_u16(io_base + VIRTIO_PCI_QUEUE_SELECT, INPUT_EVENTQ_INDEX);
    let queue_size = read_u16(io_base + VIRTIO_PCI_QUEUE_SIZE);
    if queue_size == 0 || queue_size as usize > MAX_QUEUE_SIZE {
        return 0;
    }

    let layout = prepare_event_queue_memory(queue_size, true);

    fence(Ordering::SeqCst);
    write_u32(io_base + VIRTIO_PCI_QUEUE_PFN, layout.queue_pfn);
    write_u8(
        io_base + VIRTIO_PCI_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK,
    );
    write_u16(io_base + VIRTIO_PCI_QUEUE_NOTIFY, INPUT_EVENTQ_INDEX);
    queue_size
}

/// Short ISA-bus dummy write used as a deterministic post-MMIO settle
/// delay; equivalent to the classic Linux `io_wait()`.
#[inline(always)]
fn io_delay() {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") 0x80u16,
            in("al") 0u8,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[inline(never)]
fn settle() {
    fence(Ordering::SeqCst);
    // QEMU's modern virtio status machine needs a meaningful gap between
    // back-to-back MMIO writes; in earlier sessions the only thing keeping
    // boot alive was the implicit delay from a serial::write_line call
    // (~hundreds of microseconds while the UART THR drained). 256 ISA-bus
    // dummy writes give a comparable ~256 µs settle window without spamming
    // the console.
    let mut i = 0u32;
    while i < 256 {
        io_delay();
        i += 1;
    }
    fence(Ordering::SeqCst);
}

fn write_input_queue_modern(device: PciDevice) -> Option<(u16, usize)> {
    let common = device.common_cfg_addr;
    serial::write_line(b"[virtio-input] modern init reset");

    // Reset the device and step status: ACK -> DRIVER. settle() between
    // each write provides a generous I/O-bus delay window so QEMU's modern
    // virtio model has time to publish status transitions.
    mmio_write_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS, 0);
    settle();
    // Spin until the device clears status to 0 (per virtio 1.0 spec the
    // driver MUST wait for status to read back as 0 after a reset).
    let mut reset_spins: u32 = 0;
    while mmio_read_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS) != 0 {
        io_delay();
        reset_spins = reset_spins.saturating_add(1);
        if reset_spins > 100_000 {
            serial::write_line(b"[virtio-input] reset timeout");
            return None;
        }
    }
    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    settle();
    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );
    settle();
    serial::write_line(b"[virtio-input] modern init features");

    // Feature negotiation: read device feature bits 0-31 then 32-63.
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 0);
    settle();
    let _ = mmio_read_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE);
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 0);
    settle();
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE, 0);
    settle();

    mmio_write_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 1);
    settle();
    let device_features_hi = mmio_read_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE);
    if device_features_hi & VIRTIO_F_VERSION_1 == 0 {
        serial::write_line(b"[virtio-input] device lacks VERSION_1");
        return None;
    }
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 1);
    settle();
    mmio_write_u32(
        common + VIRTIO_PCI_COMMON_DRIVER_FEATURE,
        VIRTIO_F_VERSION_1,
    );
    settle();

    let status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
    mmio_write_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS, status);
    settle();
    // Poll for FEATURES_OK acknowledgement (spec says the device may take
    // some time to validate the offered feature bits).
    let mut features_spins: u32 = 0;
    loop {
        let s = mmio_read_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS);
        if s & VIRTIO_STATUS_FEATURES_OK != 0 {
            break;
        }
        features_spins = features_spins.saturating_add(1);
        if features_spins > 100_000 {
            serial::write_line(b"[virtio-input] FEATURES_OK timeout");
            return None;
        }
        io_delay();
    }
    serial::write_line(b"[virtio-input] modern init queue-select");

    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SELECT, INPUT_EVENTQ_INDEX);
    settle();
    let device_queue_size = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE);
    if device_queue_size == 0 {
        serial::write_line(b"[virtio-input] bad queue size");
        return None;
    }
    let queue_size = device_queue_size
        .min(DEFAULT_EVENT_QUEUE_SIZE)
        .min(MAX_QUEUE_SIZE as u16);

    let layout = prepare_event_queue_memory(queue_size, false);
    let notify_off = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF) as usize;
    let notify_delta = notify_off.checked_mul(device.notify_off_multiplier as usize)?;
    let notify_addr = device.notify_cfg_addr.checked_add(notify_delta)?;

    serial::write_line(b"[virtio-input] modern init queue-addr");
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE, queue_size);
    settle();
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_DESC, layout.desc_addr);
    settle();
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_AVAIL, layout.avail_addr);
    settle();
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_USED, layout.used_addr);
    settle();
    serial::write_line(b"[virtio-input] modern init enable");
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE, 1);
    settle();
    let queue_enabled = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE);
    if queue_enabled != 1 {
        serial::write_bytes(b"[virtio-input] queue-enable readback=");
        serial::write_hex(queue_enabled as u64);
        return None;
    }
    if !configure_status_queue_modern(common) {
        serial::write_line(b"[virtio-input] statusq setup failed");
        return None;
    }
    serial::write_line(b"[virtio-input] modern init driver-ok");
    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        status | VIRTIO_STATUS_DRIVER_OK,
    );
    settle();
    let final_status = mmio_read_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS);
    if final_status & VIRTIO_STATUS_FAILED != 0 {
        serial::write_bytes(b"[virtio-input] device entered FAILED state status=");
        serial::write_hex(final_status as u64);
        return None;
    }
    serial::write_line(b"[virtio-input] modern init ready");
    Some((queue_size, notify_addr))
}

struct QueueLayout {
    queue_pfn: u32,
    desc_addr: u64,
    avail_addr: u64,
    used_addr: u64,
}

#[allow(clippy::needless_range_loop)]
fn prepare_event_queue_memory(queue_size: u16, publish_available: bool) -> QueueLayout {
    unsafe {
        let queue_base = core::ptr::addr_of_mut!(EVENT_QUEUE_MEMORY.0).cast::<u8>();
        core::ptr::write_bytes(queue_base, 0, QUEUE_MEMORY_BYTES);

        let qsize = queue_size as usize;
        let desc_bytes = size_of::<VirtqDesc>() * qsize;
        let avail_bytes = 4 + qsize * 2;
        let used_offset = align_up(desc_bytes + avail_bytes, VIRTQ_ALIGNMENT);

        let desc_ptr = queue_base.cast::<VirtqDesc>();
        for id in 0..qsize {
            write_volatile(
                desc_ptr.add(id),
                VirtqDesc {
                    addr: core::ptr::addr_of!(EVENT_BUFFERS[id]) as u64,
                    len: size_of::<VirtioInputEvent>() as u32,
                    flags: VIRTQ_DESC_F_WRITE,
                    next: 0,
                },
            );
        }

        let avail_ring = queue_base.add(desc_bytes).cast::<u16>();
        write_volatile(avail_ring, 0);
        write_volatile(avail_ring.add(1), if publish_available { queue_size } else { 0 });
        if publish_available {
            for id in 0..qsize {
                write_volatile(avail_ring.add(2 + id), id as u16);
            }
        }

        let used_ring = queue_base.add(used_offset).cast::<u16>();
        write_volatile(used_ring, 0);
        write_volatile(used_ring.add(1), 0);

        QueueLayout {
            queue_pfn: (queue_base as usize >> 12) as u32,
            desc_addr: desc_ptr as u64,
            avail_addr: avail_ring as u64,
            used_addr: used_ring as u64,
        }
    }
}

fn configure_status_queue_modern(common: usize) -> bool {
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SELECT, 1);
    settle();
    let device_queue_size = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE);
    if device_queue_size == 0 {
        return true;
    }
    let queue_size = device_queue_size
        .min(DEFAULT_STATUS_QUEUE_SIZE)
        .min(MAX_QUEUE_SIZE as u16);
    let layout = prepare_status_queue_memory(queue_size);
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE, queue_size);
    settle();
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_DESC, layout.desc_addr);
    settle();
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_AVAIL, layout.avail_addr);
    settle();
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_USED, layout.used_addr);
    settle();
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE, 1);
    settle();
    mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE) == 1
}

fn prepare_status_queue_memory(queue_size: u16) -> QueueLayout {
    unsafe {
        let queue_base = core::ptr::addr_of_mut!(STATUS_QUEUE_MEMORY.0).cast::<u8>();
        core::ptr::write_bytes(queue_base, 0, QUEUE_MEMORY_BYTES);

        let qsize = queue_size as usize;
        let desc_bytes = size_of::<VirtqDesc>() * qsize;
        let avail_bytes = 4 + qsize * 2;
        let used_offset = align_up(desc_bytes + avail_bytes, VIRTQ_ALIGNMENT);
        let desc_ptr = queue_base.cast::<VirtqDesc>();
        let avail_ring = queue_base.add(desc_bytes).cast::<u16>();
        let used_ring = queue_base.add(used_offset).cast::<u16>();
        write_volatile(avail_ring, 0);
        write_volatile(avail_ring.add(1), 0);
        write_volatile(used_ring, 0);
        write_volatile(used_ring.add(1), 0);

        QueueLayout {
            queue_pfn: (queue_base as usize >> 12) as u32,
            desc_addr: desc_ptr as u64,
            avail_addr: avail_ring as u64,
            used_addr: used_ring as u64,
        }
    }
}

fn prime_event_queue(state: &mut VirtioInputState) {
    if state.eventq_primed || state.queue_size == 0 {
        return;
    }

    let qsize = state.queue_size as usize;
    let desc_bytes = size_of::<VirtqDesc>() * qsize;
    unsafe {
        let queue_base = core::ptr::addr_of_mut!(EVENT_QUEUE_MEMORY.0).cast::<u8>();
        let avail_ring = queue_base.add(desc_bytes).cast::<u16>();
        for id in 0..qsize {
            write_volatile(avail_ring.add(2 + id), id as u16);
        }
        fence(Ordering::SeqCst);
        write_volatile(avail_ring.add(1), state.queue_size);
    }

    state.avail_idx = state.queue_size;
    state.eventq_primed = true;
    serial::write_line(b"[virtio-input] eventq primed");
    notify_queue(state);
}

fn process_used_events(state: &mut VirtioInputState) -> bool {
    if state.queue_size == 0 {
        return false;
    }

    let queue_size = state.queue_size as usize;
    let desc_bytes = size_of::<VirtqDesc>() * queue_size;
    let avail_bytes = 4 + queue_size * 2;
    let used_offset = align_up(desc_bytes + avail_bytes, VIRTQ_ALIGNMENT);
    let mut replenished = false;
    let start_len = state.len;

    unsafe {
        let queue_base = core::ptr::addr_of_mut!(EVENT_QUEUE_MEMORY.0).cast::<u8>();
        let avail_ring = queue_base.add(desc_bytes).cast::<u16>();
        let used_ring = queue_base.add(used_offset).cast::<u16>();
        let device_used_idx = read_volatile(used_ring.add(1));

        while state.used_idx != device_used_idx {
            let slot = (state.used_idx as usize) % queue_size;
            let elem_ptr = used_ring.add(2).cast::<VirtqUsedElem>().add(slot);
            let elem = read_volatile(elem_ptr);
            let head = elem.id as usize;
            if head < queue_size {
                let event = read_volatile(core::ptr::addr_of!(EVENT_BUFFERS[head]));
                handle_input_event(state, event);

                let ring_slot = 2 + (state.avail_idx as usize % queue_size);
                write_volatile(avail_ring.add(ring_slot), head as u16);
                state.avail_idx = state.avail_idx.wrapping_add(1);
                replenished = true;
            }
            state.used_idx = state.used_idx.wrapping_add(1);
        }

        if replenished {
            fence(Ordering::SeqCst);
            write_volatile(avail_ring.add(1), state.avail_idx);
        }
    }

    if replenished {
        notify_queue(state);
    }
    state.len != start_len
}

fn handle_input_event(state: &mut VirtioInputState, event: VirtioInputEvent) {
    match event.ty {
        EV_REL => match event.code {
            REL_X => state.pending_rel_x = state.pending_rel_x.saturating_add(event.value as i32),
            REL_Y => state.pending_rel_y = state.pending_rel_y.saturating_add(event.value as i32),
            _ => {}
        },
        EV_ABS => match event.code as u8 {
            ABS_X => {
                state.pending_abs_x = Some(event.value);
            }
            ABS_Y => state.pending_abs_y = Some(event.value),
            _ => {}
        },
        EV_KEY => {
            if let Some(button) = button_from_code(event.code) {
                state.push_event(PointerEvent::Button {
                    button,
                    pressed: event.value != 0,
                });
            }
        }
        EV_SYN if event.code == SYN_REPORT => state.flush_frame(),
        _ => {}
    }
}

fn button_from_code(code: u16) -> Option<MouseButton> {
    match code {
        BTN_LEFT => Some(MouseButton::Left),
        BTN_RIGHT => Some(MouseButton::Right),
        BTN_MIDDLE => Some(MouseButton::Middle),
        _ => None,
    }
}

#[derive(Clone, Copy)]
struct AbsInfo {
    min: u32,
    max: u32,
}

fn read_abs_info(device_cfg_addr: usize, transport: TransportMode, axis: u8) -> Option<AbsInfo> {
    match transport {
        TransportMode::Legacy => read_abs_info_legacy(device_cfg_addr as u16, axis),
        TransportMode::Modern => read_abs_info_modern(device_cfg_addr, axis),
    }
}

fn read_abs_info_legacy(io_base: u16, axis: u8) -> Option<AbsInfo> {
    write_u8(
        io_base + VIRTIO_PCI_DEVICE_CONFIG,
        VIRTIO_INPUT_CFG_ABS_INFO,
    );
    write_u8(io_base + VIRTIO_PCI_DEVICE_CONFIG + 1, axis);
    let size = read_u8(io_base + VIRTIO_PCI_DEVICE_CONFIG + 2);
    if size < 8 {
        return None;
    }

    let min = read_u32(io_base + VIRTIO_PCI_DEVICE_CONFIG + 8);
    let max = read_u32(io_base + VIRTIO_PCI_DEVICE_CONFIG + 12);
    Some(AbsInfo { min, max })
}

fn read_abs_info_modern(device_cfg_addr: usize, axis: u8) -> Option<AbsInfo> {
    mmio_write_u8(device_cfg_addr, VIRTIO_INPUT_CFG_ABS_INFO);
    mmio_write_u8(device_cfg_addr + 1, axis);
    let size = mmio_read_u8(device_cfg_addr + 2);
    if size < 8 {
        return None;
    }

    let min = mmio_read_u32(device_cfg_addr + 8);
    let max = mmio_read_u32(device_cfg_addr + 12);
    Some(AbsInfo { min, max })
}

fn notify_queue(state: &VirtioInputState) {
    match state.transport {
        TransportMode::Legacy => {
            write_u16(state.io_base + VIRTIO_PCI_QUEUE_NOTIFY, INPUT_EVENTQ_INDEX);
        }
        TransportMode::Modern => {
            mmio_write_u16(state.notify_addr, INPUT_EVENTQ_INDEX);
        }
    }
}

fn scale_axis(value: u32, min: u32, max: u32, pixels: u32) -> i32 {
    if pixels <= 1 || max <= min {
        return 0;
    }
    let clamped = value.clamp(min, max);
    let span = (max - min) as u64;
    let offset = (clamped - min) as u64;
    ((offset * (pixels - 1) as u64) / span) as i32
}

fn pci_read_u8(bus: u8, slot: u8, func: u8, offset: u8) -> u8 {
    let value = pci_read_u32(bus, slot, func, offset);
    ((value >> (((offset & 0x3) as u32) * 8)) & 0xFF) as u8
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

fn pci_read_u16(bus: u8, slot: u8, func: u8, offset: u8) -> u16 {
    let value = pci_read_u32(bus, slot, func, offset);
    ((value >> (((offset & 0x2) as u32) * 8)) & 0xFFFF) as u16
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
    unsafe { outb(port, value) };
}

fn write_u16(port: u16, value: u16) {
    unsafe { outw(port, value) };
}

fn write_u32(port: u16, value: u32) {
    unsafe { outl(port, value) };
}

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
    unsafe { write_volatile(addr as *mut u8, value) };
}

fn mmio_write_u16(addr: usize, value: u16) {
    unsafe { write_volatile(addr as *mut u16, value) };
}

fn mmio_write_u32(addr: usize, value: u32) {
    unsafe { write_volatile(addr as *mut u32, value) };
}

fn mmio_write_u64(addr: usize, value: u64) {
    mmio_write_u32(addr, value as u32);
    mmio_write_u32(addr + 4, (value >> 32) as u32);
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        core::arch::asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
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
