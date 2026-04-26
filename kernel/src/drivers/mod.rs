// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Device driver subsystem — trait-based driver model.
//!
//! Provides a lightweight, no_alloc driver registration and discovery
//! framework. Each driver implements the [`Driver`] trait, registers via
//! [`register()`], and is discovered by [`probe_all()`] at boot.
//!
//! ## Design
//! - Static table of driver descriptors (no heap, no `Box<dyn>`).
//! - Drivers are identified by a `(bus, vendor, device)` triple.
//! - Probe returns a fixed-size status struct, not a dynamic object.
//! - Interrupt routing via the driver's `irq()` hint — the PIC/APIC
//!   layer calls [`dispatch_irq()`] on the matching driver.
//! - Future: VirtIO bring-up, MMIO mapping, DMA buffer reservation.

pub mod audio;
pub mod battery;
pub mod bt;
pub mod display;
pub mod gpu;
pub mod input;
pub mod installer;
pub mod net;
pub mod nvme;
pub mod storage;
pub mod thermal;
pub mod wifi;

use crate::arch::serial;
use crate::graph::types::NodeId;
use crate::uuid::DeviceUuid;
use spin::Mutex;

// ════════════════════════════════════════════════════════════════════
// Driver descriptor
// ════════════════════════════════════════════════════════════════════

/// Bus type for device matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BusType {
    /// Platform / built-in (serial, PIT, PIC).
    Platform = 0,
    /// PCI / PCIe.
    Pci = 1,
    /// VirtIO over MMIO.
    VirtioMmio = 2,
    /// VirtIO over PCI.
    VirtioPci = 3,
}

/// Result of a driver probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProbeResult {
    /// Driver successfully bound to hardware.
    Bound = 0,
    /// Hardware is present and already owned by boot code or a fixed platform path.
    Managed = 3,
    /// Hardware not present or not matching.
    NoMatch = 1,
    /// Hardware present but initialisation failed.
    Failed = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSummary {
    pub bound: usize,
    pub managed: usize,
    pub no_match: usize,
    pub failed: usize,
    pub total: usize,
}

/// Static driver descriptor.
///
/// Each driver is a `const` descriptor with function pointers for
/// lifecycle operations. No dynamic dispatch, no vtable allocation.
#[derive(Clone, Copy)]
pub struct DriverDesc {
    /// Human-readable name (null-terminated, max 31 bytes).
    pub name: &'static [u8],
    /// Bus type this driver binds to.
    pub bus: BusType,
    /// Vendor ID (0 = wildcard / platform).
    pub vendor: u16,
    /// Device ID (0 = wildcard / platform).
    pub device: u16,
    /// IRQ number this driver expects (0 = no IRQ / polled).
    pub irq: u8,
    /// Probe function — detect hardware, initialise, return status.
    pub probe: fn() -> ProbeResult,
    /// IRQ handler — called from the IDT dispatch path.
    pub handle_irq: fn(),
}

// ════════════════════════════════════════════════════════════════════
// Driver table (static, no heap)
// ════════════════════════════════════════════════════════════════════

/// Maximum registered drivers.
const MAX_DRIVERS: usize = 32;

/// Registration slot.
struct DriverSlot {
    desc: DriverDesc,
    state: ProbeResult,
    registered: bool,
    /// Graph arena node ID for this device (0 = not yet registered).
    device_graph_node: u64,
    /// Typed UUID for this device (NIL until probe succeeds).
    device_uuid: DeviceUuid,
}

impl DriverSlot {
    const EMPTY: Self = Self {
        desc: DriverDesc {
            name: b"",
            bus: BusType::Platform,
            vendor: 0,
            device: 0,
            irq: 0,
            probe: noop_probe,
            handle_irq: noop_irq,
        },
        state: ProbeResult::NoMatch,
        registered: false,
        device_graph_node: 0,
        device_uuid: DeviceUuid(crate::uuid::Uuid128::NIL),
    };
}

fn noop_probe() -> ProbeResult {
    ProbeResult::NoMatch
}
fn noop_irq() {}

struct DriverTable {
    slots: [DriverSlot; MAX_DRIVERS],
    count: usize,
}

impl DriverTable {
    const fn new() -> Self {
        Self {
            slots: [DriverSlot::EMPTY; MAX_DRIVERS],
            count: 0,
        }
    }
}

static TABLE: Mutex<DriverTable> = Mutex::new(DriverTable::new());

fn probe_boot_managed_platform() -> ProbeResult {
    ProbeResult::Managed
}

fn noop_platform_irq() {}
fn virtio_net_irq_dispatch() {
    // The DriverDesc irq field is 0 (dynamic); forward any possible IRQ line.
    if let Some(irq) = crate::arch::x86_64::virtio_net::irq_line() {
        crate::arch::x86_64::virtio_net::handle_irq(irq);
    }
}

fn virtio_blk_irq_dispatch() {}
fn virtio_gpu_irq_dispatch() {}
fn hda_irq_dispatch() {}
fn virtio_input_irq_dispatch() {
    crate::drivers::input::virtio_input::handle_irq();
}
fn usb_hid_irq_dispatch() {}

fn nvme_irq_dispatch() {}

const BUILTIN_DRIVERS: [DriverDesc; 14] = [
    DriverDesc {
        name: b"serial0",
        bus: BusType::Platform,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: probe_boot_managed_platform,
        handle_irq: noop_platform_irq,
    },
    DriverDesc {
        name: b"pic8259",
        bus: BusType::Platform,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: probe_boot_managed_platform,
        handle_irq: noop_platform_irq,
    },
    DriverDesc {
        name: b"pit8254",
        bus: BusType::Platform,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: probe_boot_managed_platform,
        handle_irq: noop_platform_irq,
    },
    DriverDesc {
        name: b"ps2-kbd",
        bus: BusType::Platform,
        vendor: 0,
        device: 0,
        irq: 1,
        probe: probe_boot_managed_platform,
        handle_irq: noop_platform_irq,
    },
    DriverDesc {
        name: b"graph-display0",
        bus: BusType::Platform,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: display::probe_boot_display,
        handle_irq: noop_platform_irq,
    },
    DriverDesc {
        name: b"virtio-net0",
        bus: BusType::VirtioPci,
        vendor: 0x1AF4,
        device: 0x1041,
        irq: 0,
        probe: crate::arch::x86_64::virtio_net::probe_driver,
        handle_irq: virtio_net_irq_dispatch,
    },
    DriverDesc {
        name: b"virtio-blk0",
        bus: BusType::VirtioPci,
        vendor: 0x1AF4,
        device: 0x1001,
        irq: 0,
        probe: crate::arch::x86_64::virtio_blk::probe_driver,
        handle_irq: virtio_blk_irq_dispatch,
    },
    DriverDesc {
        name: b"virtio-gpu0",
        bus: BusType::VirtioPci,
        vendor: 0x1AF4,
        device: 0x1050,
        irq: 0,
        probe: gpu::virtio_gpu::probe_driver,
        handle_irq: virtio_gpu_irq_dispatch,
    },
    DriverDesc {
        name: b"intel-hda0",
        bus: BusType::Pci,
        vendor: 0x8086,
        device: 0x1C20,
        irq: 0,
        probe: audio::hda::probe_driver,
        handle_irq: hda_irq_dispatch,
    },
    DriverDesc {
        name: b"virtio-input0",
        bus: BusType::VirtioPci,
        vendor: 0x1AF4,
        device: 0x1052,
        irq: 0,
        probe: input::virtio_input::probe_driver,
        handle_irq: virtio_input_irq_dispatch,
    },
    DriverDesc {
        name: b"usb-hid0",
        bus: BusType::Pci,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: input::usb_hid::probe_driver,
        handle_irq: usb_hid_irq_dispatch,
    },
    DriverDesc {
        name: b"nvme0",
        bus: BusType::Pci,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: storage::nvme::probe_driver,
        handle_irq: nvme_irq_dispatch,
    },
    DriverDesc {
        name: b"wifi0",
        bus: BusType::Pci,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: net::wifi::probe_driver,
        handle_irq: noop_platform_irq,
    },
    DriverDesc {
        name: b"bluetooth0",
        bus: BusType::Pci,
        vendor: 0,
        device: 0,
        irq: 0,
        probe: bt::probe_driver,
        handle_irq: noop_platform_irq,
    },
];

// ════════════════════════════════════════════════════════════════════
// Public API
// ════════════════════════════════════════════════════════════════════

/// Register a driver descriptor. Returns `false` if the table is full.
pub fn register(desc: DriverDesc) -> bool {
    let mut table = TABLE.lock();
    if table.count >= MAX_DRIVERS {
        serial::write_line(b"[drivers] ERROR: driver table full");
        return false;
    }
    for slot in table.slots.iter_mut() {
        if !slot.registered {
            slot.desc = desc;
            slot.registered = true;
            slot.state = ProbeResult::NoMatch;
            table.count += 1;

            serial::write_bytes(b"[drivers] registered: ");
            serial::write_bytes(desc.name);
            serial::write_bytes(b" bus=");
            serial::write_u64_dec(desc.bus as u64);
            return true;
        }
    }
    false
}

/// Probe all registered drivers and return an honest status breakdown.
pub fn probe_all() -> ProbeSummary {
    let mut table = TABLE.lock();
    let mut summary = ProbeSummary {
        bound: 0,
        managed: 0,
        no_match: 0,
        failed: 0,
        total: table.count,
    };
    for slot in table.slots.iter_mut() {
        if !slot.registered {
            continue;
        }
        let result = (slot.desc.probe)();
        slot.state = result;
        serial::write_bytes(b"[drivers] probe ");
        serial::write_bytes(slot.desc.name);
        serial::write_bytes(b" => ");
        match result {
            ProbeResult::Bound => {
                serial::write_line(b"BOUND");
                summary.bound += 1;
                // Register a Device node in the graph arena.
                let handle = crate::graph::handles::register_device(0);
                slot.device_graph_node = handle.0;
                slot.device_uuid = DeviceUuid::from_device_id(
                    (slot.desc.vendor as u64) << 16 | slot.desc.device as u64,
                );
            }
            ProbeResult::Managed => {
                serial::write_line(b"managed-by-boot");
                summary.managed += 1;
                let handle = crate::graph::handles::register_device(0);
                slot.device_graph_node = handle.0;
                slot.device_uuid = DeviceUuid::from_device_id(
                    (slot.desc.vendor as u64) << 16 | slot.desc.device as u64,
                );
            }
            ProbeResult::NoMatch => {
                serial::write_line(b"no-match");
                summary.no_match += 1;
            }
            ProbeResult::Failed => {
                serial::write_line(b"FAILED");
                summary.failed += 1;
            }
        }
    }
    serial::write_bytes(b"[drivers] probe complete: bound=");
    serial::write_u64_dec_inline(summary.bound as u64);
    serial::write_bytes(b" managed=");
    serial::write_u64_dec_inline(summary.managed as u64);
    serial::write_bytes(b" failed=");
    serial::write_u64_dec_inline(summary.failed as u64);
    serial::write_bytes(b" absent=");
    serial::write_u64_dec_inline(summary.no_match as u64);
    serial::write_bytes(b" total=");
    serial::write_u64_dec(summary.total as u64);
    summary
}

/// Dispatch an IRQ to the driver that owns it.
///
/// Called from the IDT interrupt handler. Scans the driver table for
/// a bound driver matching the given IRQ vector. Returns `true` if
/// a handler was invoked.
pub fn dispatch_irq(irq: u8) -> bool {
    let table = TABLE.lock();
    let mut handled = false;
    for slot in table.slots.iter() {
        if !slot.registered || slot.state != ProbeResult::Bound || irq == 0 {
            continue;
        }
        // Some drivers discover their IRQ line dynamically at probe time and
        // keep `desc.irq == 0`. Their dispatch shims validate the real IRQ
        // internally, so they must still be called for shared PCI IRQ lines.
        if slot.desc.irq == irq || slot.desc.irq == 0 {
            (slot.desc.handle_irq)();
            handled = true;
        }
    }
    handled
}

/// Return the number of registered drivers.
pub fn driver_count() -> usize {
    TABLE.lock().count
}

/// Return the number of bound (successfully probed) drivers.
pub fn bound_count() -> usize {
    let table = TABLE.lock();
    table
        .slots
        .iter()
        .filter(|s| s.registered && s.state == ProbeResult::Bound)
        .count()
}

/// Return the number of boot-managed devices that are present but not dynamically bound.
pub fn managed_count() -> usize {
    let table = TABLE.lock();
    table
        .slots
        .iter()
        .filter(|s| s.registered && s.state == ProbeResult::Managed)
        .count()
}

/// Look up the graph NodeId for a device by its UUID.
///
/// Returns `None` if no probed device matches `uuid`, or if the device
/// has not yet been registered in the graph arena.
pub fn device_node_for_uuid(uuid: DeviceUuid) -> Option<NodeId> {
    let table = TABLE.lock();
    table.slots.iter().find_map(|s| {
        if s.registered && s.device_uuid == uuid && s.device_graph_node != 0 {
            Some(s.device_graph_node)
        } else {
            None
        }
    })
}

/// Initialise the driver subsystem.
pub fn init() {
    if driver_count() != 0 {
        serial::write_line(b"[drivers] subsystem already initialised");
        return;
    }

    for desc in BUILTIN_DRIVERS {
        let _ = register(desc);
    }

    serial::write_line(b"[drivers] subsystem initialised with boot-managed platform drivers");
}
