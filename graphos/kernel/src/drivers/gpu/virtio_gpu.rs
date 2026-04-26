// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! virtio-gpu driver — modesetting and 2-D scanout for QEMU.
//!
//! Implements the virtio-gpu device specification (virtio 1.2, §5.7).
//! On probe, detects the virtio-gpu PCI device (vendor 0x1AF4, modern 0x1050
//! or legacy transitional 0x1010),
//! queries display info, and sets up a single 2-D scanout backed by a
//! host-visible resource.  Once initialised, the compositor may call
//! `flush_rect()` to push damaged regions to the QEMU display.
//!
//! ## Status
//! - PCI probe + feature negotiation
//! - DISPLAY_INFO query
//! - CREATE_RESOURCE_2D (RGBA8, 1 resource ID)
//! - ATTACH_BACKING / SET_SCANOUT / TRANSFER / RESOURCE_FLUSH
//! - Framebuffer handed to the compositor via `framebuffer_addr()`

use spin::Mutex;

use crate::arch::serial;
use crate::drivers::ProbeResult;

// ── Virtio PCI IDs ────────────────────────────────────────────────────────────
const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_GPU_DEVICE_MODERN: u16 = 0x1050;
const VIRTIO_GPU_DEVICE_LEGACY: u16 = 0x1010;

// ── Virtio-gpu command types ──────────────────────────────────────────────────
const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
const VIRTIO_GPU_CMD_GET_CAPSET_INFO: u32 = 0x0108;
// VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM
const VIRTIO_GPU_FORMAT_BGRX8888: u32 = 2;
const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1102;
const VIRTIO_GPU_CAPSET_VIRGL: u32 = 1;

// ── Virtio PCI legacy I/O-port register offsets ───────────────────────────────
const VIRTIO_PCI_QUEUE_PFN: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_STATUS: u16 = 0x12;
const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 0x01;
const VIRTIO_STATUS_DRIVER: u8 = 0x02;
const VIRTIO_STATUS_DRIVER_OK: u8 = 0x04;
const VIRTIO_STATUS_FEATURES_OK: u8 = 0x08;

const PCI_STATUS_OFFSET: u8 = 0x06;
const PCI_CAP_PTR_OFFSET: u8 = 0x34;
const PCI_STATUS_CAPABILITIES: u16 = 1 << 4;
const PCI_CAP_ID_VENDOR_SPECIFIC: u8 = 0x09;
const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_F_VERSION_1: u32 = 1 << 0;
// virtio-gpu device feature bits (low 32-bit feature word).
const VIRTIO_GPU_F_VIRGL: u32 = 1 << 0;
const VIRTIO_GPU_F_CONTEXT_INIT: u32 = 1 << 4;

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

// ── Virtqueue layout ──────────────────────────────────────────────────────────
// Single-entry control queue (queue 0) for GPU commands.
// Descriptor table: 1 × 16 = 16 bytes
// Available ring: 4 + 1×2 + 2 = 8 bytes  → padded to 4096 boundary
// Used ring: 4 + 1×8 = 12 bytes
const GPU_QUEUE_SIZE: usize = 2;
const GPU_DESC_BYTES: usize = 16 * GPU_QUEUE_SIZE;
const GPU_AVAIL_BYTES: usize = 4 + 2 * GPU_QUEUE_SIZE + 2;
const GPU_PAGE_BYTES: usize = 4096;
// Second page starts at 4096 (after desc + avail padded to 4096)
const GPU_USED_OFF: usize = GPU_PAGE_BYTES;
const GPU_USED_BYTES: usize = 4 + 8 * GPU_QUEUE_SIZE;
const GPU_QUEUE_BYTES: usize = GPU_USED_OFF + GPU_USED_BYTES;

#[repr(align(4096))]
struct GpuQueueMem([u8; GPU_QUEUE_BYTES]);
static mut GPU_QUEUE_MEM: GpuQueueMem = GpuQueueMem([0u8; GPU_QUEUE_BYTES]);

// ── Command buffer for GPU control messages ───────────────────────────────────
// VIRTIO_GPU_CMD_RESOURCE_FLUSH = 36 bytes (hdr 24 + rect 16 + resource_id 4 + padding 4 = 48)
const GPU_CMD_BUF_SIZE: usize = 64;
#[repr(align(64))]
struct GpuCmdBuf([u8; GPU_CMD_BUF_SIZE]);
static mut GPU_CMD_BUF: GpuCmdBuf = GpuCmdBuf([0u8; GPU_CMD_BUF_SIZE]);
#[repr(align(64))]
struct GpuRespBuf([u8; GPU_CMD_BUF_SIZE]);
static mut GPU_RESP_BUF: GpuRespBuf = GpuRespBuf([0u8; GPU_CMD_BUF_SIZE]);

// ── Driver state ──────────────────────────────────────────────────────────────
struct GpuState {
    present: bool,
    modern: bool,
    width: u32,
    height: u32,
    /// Physical address of the host-visible framebuffer backing store.
    fb_phys: u64,
    /// Resource ID allocated for the 2-D scanout.
    resource_id: u32,
    /// Virtio legacy I/O port base.
    io_base: u16,
    /// Virtio modern common cfg MMIO base (0 when legacy transport in use).
    common_cfg_addr: usize,
    /// Virtio modern notify cfg MMIO base (0 when legacy transport in use).
    notify_cfg_addr: usize,
    /// Virtio modern queue notify address for queue 0.
    notify_addr: usize,
    /// Queue avail ring index (next slot to write).
    avail_idx: u16,
    /// Queue used ring index (last seen used index).
    used_idx: u16,
    /// True if device accepted virgl/3D feature negotiation.
    virgl_3d: bool,
    /// True if context-init feature is available/negotiated.
    context_init: bool,
    /// Selected capset ID (0 when none discovered).
    capset_id: u32,
    /// Selected capset version (0 when none discovered).
    capset_version: u32,
    /// Selected capset payload size in bytes (0 when none discovered).
    capset_size: u32,
}

impl GpuState {
    const fn new() -> Self {
        Self {
            present: false,
            modern: false,
            width: 0,
            height: 0,
            fb_phys: 0,
            resource_id: 0,
            io_base: 0,
            common_cfg_addr: 0,
            notify_cfg_addr: 0,
            notify_addr: 0,
            avail_idx: 0,
            used_idx: 0,
            virgl_3d: false,
            context_init: false,
            capset_id: 0,
            capset_version: 0,
            capset_size: 0,
        }
    }
}

static STATE: Mutex<GpuState> = Mutex::new(GpuState::new());

// ── Legacy virtio I/O port helpers ────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn vio_write8(base: u16, off: u16, val: u8) {
    unsafe {
        x86_64::instructions::port::PortWrite::write_to_port(base + off, val);
    }
}
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn vio_write8(_base: u16, _off: u16, _val: u8) {}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn vio_read8(base: u16, off: u16) -> u8 {
    unsafe { x86_64::instructions::port::PortRead::read_from_port(base + off) }
}
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn vio_read8(_base: u16, _off: u16) -> u8 {
    0
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn vio_write16(base: u16, off: u16, val: u16) {
    unsafe {
        x86_64::instructions::port::PortWrite::write_to_port(base + off, val);
    }
}
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn vio_write16(_base: u16, _off: u16, _val: u16) {}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn vio_read16(base: u16, off: u16) -> u16 {
    unsafe { x86_64::instructions::port::PortRead::read_from_port(base + off) }
}
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn vio_read16(_base: u16, _off: u16) -> u16 {
    0
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn vio_write32(base: u16, off: u16, val: u32) {
    unsafe {
        x86_64::instructions::port::PortWrite::write_to_port(base + off, val);
    }
}
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn vio_write32(_base: u16, _off: u16, _val: u32) {}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn vio_read32(base: u16, off: u16) -> u32 {
    unsafe { x86_64::instructions::port::PortRead::read_from_port(base + off) }
}
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
unsafe fn vio_read32(_base: u16, _off: u16) -> u32 {
    0
}

// ── Probe ─────────────────────────────────────────────────────────────────────

/// Detect virtio-gpu, query display geometry, set up scanout.
pub fn probe_driver() -> ProbeResult {
    // Look for virtio-gpu on PCI bus.
    let dev = match crate::arch::x86_64::pci::find_device(VIRTIO_VENDOR, VIRTIO_GPU_DEVICE_MODERN)
        .or_else(|| crate::arch::x86_64::pci::find_device(VIRTIO_VENDOR, VIRTIO_GPU_DEVICE_LEGACY))
    {
        Some(d) => d,
        None => return ProbeResult::NoMatch,
    };

    // Required for both legacy and modern virtio transports: enable I/O,
    // MMIO decode, and DMA bus-mastering on the PCI function.
    crate::arch::x86_64::pci::enable_bus_master(dev.location);

    // Find the legacy virtio I/O BAR. On virtio-vga this is not guaranteed to
    // be BAR0, so scan standard PCI BAR slots for an I/O-mapped BAR.
    let io_base = find_legacy_io_base(dev.location.bus, dev.location.slot, dev.location.func);
    serial::write_bytes(b"[virtio-gpu] legacy io base=0x");
    serial::write_hex(io_base as u64);

    let mut state = STATE.lock();
    state.present = false;
    state.modern = false;
    state.io_base = io_base;
    state.common_cfg_addr = 0;
    state.notify_cfg_addr = 0;
    state.notify_addr = 0;
    state.virgl_3d = false;
    state.context_init = false;
    state.capset_id = 0;
    state.capset_version = 0;
    state.capset_size = 0;

    // Pick the largest scanout mode we can back with contiguous pages.
    state.width = 0;
    state.height = 0;
    state.resource_id = 1;

    const CANDIDATE_MODES: &[(u32, u32)] = &[(1280, 800), (1024, 768), (800, 600), (640, 480)];
    for &(w, h) in CANDIDATE_MODES {
        let fb_size = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        let fb_pages = fb_size.div_ceil(4096);
        if let Some(phys) = crate::mm::frame_alloc::alloc_contiguous_run(fb_pages) {
            state.width = w;
            state.height = h;
            state.fb_phys = phys;
            let _ = crate::mm::page_table::ensure_identity_mapped_2m(phys);
            let fb_last = phys.saturating_add(fb_size.saturating_sub(1) as u64);
            let _ = crate::mm::page_table::ensure_identity_mapped_2m(fb_last);
            unsafe {
                core::ptr::write_bytes(phys as *mut u8, 0, fb_size);
            }
            serial::write_bytes(b"[virtio-gpu] backing mode ");
            serial::write_u64_dec_inline(w as u64);
            serial::write_bytes(b"x");
            serial::write_u64_dec(h as u64);
            break;
        }
    }
    if state.fb_phys == 0 {
        serial::write_line(
            b"[virtio-gpu] WARNING: could not allocate any backing framebuffer mode",
        );
    }

    // Set up control virtqueue (queue 0) using legacy I/O if available,
    // otherwise use modern virtio-pci common/notify capabilities.
    let ready = if io_base != 0 {
        state.modern = false;
        let controlq_ready = unsafe { setup_controlq(io_base) };
        controlq_ready && init_scanout_locked(&mut state)
    } else if let Some((common_cfg_addr, notify_cfg_addr, notify_off_multiplier)) =
        probe_modern_cfg(dev.location.bus, dev.location.slot, dev.location.func)
    {
        state.modern = true;
        state.common_cfg_addr = common_cfg_addr;
        state.notify_cfg_addr = notify_cfg_addr;
        setup_controlq_modern(&mut state, notify_off_multiplier) && init_scanout_locked(&mut state)
    } else {
        false
    };
    if !ready {
        serial::write_line(b"[virtio-gpu] ERROR: scanout init failed");
        return ProbeResult::Failed;
    }
    if state.virgl_3d {
        query_capset_info_locked(&mut state);
    }
    state.present = true;

    serial::write_bytes(b"[virtio-gpu] bound ");
    serial::write_u64_dec(state.width as u64);
    serial::write_bytes(b"x");
    serial::write_u64_dec(state.height as u64);
    serial::write_bytes(b" fb_phys=0x");
    serial::write_hex(state.fb_phys);
    serial::write_line(b"");
    serial::write_bytes(b"[virtio-gpu] features: virgl3d=");
    serial::write_u64_dec_inline(state.virgl_3d as u64);
    serial::write_bytes(b" context_init=");
    serial::write_u64_dec(state.context_init as u64);
    if state.capset_id != 0 {
        serial::write_bytes(b"[virtio-gpu] capset: id=");
        serial::write_u64_dec_inline(state.capset_id as u64);
        serial::write_bytes(b" version=");
        serial::write_u64_dec_inline(state.capset_version as u64);
        serial::write_bytes(b" size=");
        serial::write_u64_dec(state.capset_size as u64);
    } else if state.virgl_3d {
        serial::write_line(b"[virtio-gpu] capset: none discovered");
    }

    ProbeResult::Bound
}

/// True when the device negotiated virgl 3D support.
pub fn gpu_3d_available() -> bool {
    let s = STATE.lock();
    s.present && s.virgl_3d
}

fn find_legacy_io_base(bus: u8, slot: u8, func: u8) -> u16 {
    for off in [0x10u8, 0x14, 0x18, 0x1C, 0x20, 0x24] {
        let bar = crate::arch::x86_64::pci::read_u32(bus, slot, func, off);
        if (bar & 0x1) == 0 {
            continue;
        }
        let base = (bar & !0x3) as u16;
        if base != 0 {
            return base;
        }
    }
    0
}

fn probe_modern_cfg(bus: u8, slot: u8, func: u8) -> Option<(usize, usize, u32)> {
    if crate::arch::x86_64::pci::read_u16(bus, slot, func, PCI_STATUS_OFFSET)
        & PCI_STATUS_CAPABILITIES
        == 0
    {
        return None;
    }

    let mut common_cfg_addr = 0usize;
    let mut notify_cfg_addr = 0usize;
    let mut notify_off_multiplier = 0u32;

    let mut cap_ptr = crate::arch::x86_64::pci::read_u8(bus, slot, func, PCI_CAP_PTR_OFFSET) & !0x3;
    let mut hops = 0u8;
    while cap_ptr >= 0x40 && cap_ptr != 0 && hops < 32 {
        let cap_id = crate::arch::x86_64::pci::read_u8(bus, slot, func, cap_ptr);
        let next = crate::arch::x86_64::pci::read_u8(bus, slot, func, cap_ptr + 1) & !0x3;
        if cap_id == PCI_CAP_ID_VENDOR_SPECIFIC {
            let cfg_type = crate::arch::x86_64::pci::read_u8(bus, slot, func, cap_ptr + 3);
            let bar = crate::arch::x86_64::pci::read_u8(bus, slot, func, cap_ptr + 4);
            let offset = crate::arch::x86_64::pci::read_u32(bus, slot, func, cap_ptr + 8) as usize;
            if let Some(bar_addr) = pci_read_bar_address(bus, slot, func, bar) {
                let addr = bar_addr.saturating_add(offset as u64) as usize;
                match cfg_type {
                    VIRTIO_PCI_CAP_COMMON_CFG => common_cfg_addr = addr,
                    VIRTIO_PCI_CAP_NOTIFY_CFG => {
                        notify_cfg_addr = addr;
                        notify_off_multiplier =
                            crate::arch::x86_64::pci::read_u32(bus, slot, func, cap_ptr + 16);
                    }
                    _ => {}
                }
            }
        }
        cap_ptr = next;
        hops = hops.saturating_add(1);
    }

    if common_cfg_addr == 0 || notify_cfg_addr == 0 {
        return None;
    }

    if !crate::mm::page_table::ensure_identity_mapped_2m(common_cfg_addr as u64)
        || !crate::mm::page_table::ensure_identity_mapped_2m(notify_cfg_addr as u64)
    {
        serial::write_line(b"[virtio-gpu] failed to map modern MMIO window");
        return None;
    }

    serial::write_bytes(b"[virtio-gpu] modern cfg common=");
    serial::write_hex_inline(common_cfg_addr as u64);
    serial::write_bytes(b" notify=");
    serial::write_hex(notify_cfg_addr as u64);

    Some((common_cfg_addr, notify_cfg_addr, notify_off_multiplier))
}

fn pci_read_bar_address(bus: u8, slot: u8, func: u8, bar: u8) -> Option<u64> {
    if bar >= 6 {
        return None;
    }
    let offset = 0x10 + bar * 4;
    let low = crate::arch::x86_64::pci::read_u32(bus, slot, func, offset);
    if low == 0 {
        return None;
    }
    if low & 0x1 != 0 {
        return Some((low & !0x3) as u64);
    }
    let addr = match (low >> 1) & 0x3 {
        0x0 => (low & !0xF) as u64,
        0x2 if bar + 1 < 6 => {
            let high = crate::arch::x86_64::pci::read_u32(bus, slot, func, offset + 4);
            ((high as u64) << 32) | (low & !0xF) as u64
        }
        _ => return None,
    };
    Some(addr)
}

fn setup_controlq_modern(state: &mut GpuState, notify_off_multiplier: u32) -> bool {
    let common = state.common_cfg_addr;
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
    let device_features_lo = mmio_read_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE);
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 0);
    let mut driver_features_lo = 0u32;
    if (device_features_lo & VIRTIO_GPU_F_VIRGL) != 0 {
        driver_features_lo |= VIRTIO_GPU_F_VIRGL;
    }
    if (device_features_lo & VIRTIO_GPU_F_CONTEXT_INIT) != 0 {
        driver_features_lo |= VIRTIO_GPU_F_CONTEXT_INIT;
    }
    mmio_write_u32(
        common + VIRTIO_PCI_COMMON_DRIVER_FEATURE,
        driver_features_lo,
    );

    mmio_write_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 1);
    let device_features_hi = mmio_read_u32(common + VIRTIO_PCI_COMMON_DEVICE_FEATURE);
    if device_features_hi & VIRTIO_F_VERSION_1 == 0 {
        serial::write_line(b"[virtio-gpu] modern VERSION_1 missing");
        return false;
    }
    mmio_write_u32(common + VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 1);
    mmio_write_u32(
        common + VIRTIO_PCI_COMMON_DRIVER_FEATURE,
        VIRTIO_F_VERSION_1,
    );

    state.virgl_3d = (driver_features_lo & VIRTIO_GPU_F_VIRGL) != 0;
    state.context_init = (driver_features_lo & VIRTIO_GPU_F_CONTEXT_INIT) != 0;

    let status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
    mmio_write_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS, status);
    if mmio_read_u8(common + VIRTIO_PCI_COMMON_DEVICE_STATUS) & VIRTIO_STATUS_FEATURES_OK == 0 {
        serial::write_line(b"[virtio-gpu] modern FEATURES_OK rejected");
        return false;
    }

    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_SELECT, 0);
    let queue_size = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_SIZE);
    if queue_size == 0 {
        serial::write_line(b"[virtio-gpu] modern queue0 size is zero");
        return false;
    }

    let queue_pa = unsafe { core::ptr::addr_of!(GPU_QUEUE_MEM.0) as u64 };
    let desc_addr = queue_pa;
    let avail_addr = queue_pa + GPU_DESC_BYTES as u64;
    let used_addr = queue_pa + GPU_USED_OFF as u64;
    let notify_off = mmio_read_u16(common + VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF) as usize;
    state.notify_addr =
        state.notify_cfg_addr + notify_off.saturating_mul(notify_off_multiplier as usize);

    mmio_write_u16(
        common + VIRTIO_PCI_COMMON_QUEUE_SIZE,
        queue_size.min(GPU_QUEUE_SIZE as u16),
    );
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_DESC, desc_addr);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_AVAIL, avail_addr);
    mmio_write_u64(common + VIRTIO_PCI_COMMON_QUEUE_USED, used_addr);
    mmio_write_u16(common + VIRTIO_PCI_COMMON_QUEUE_ENABLE, 1);
    mmio_write_u8(
        common + VIRTIO_PCI_COMMON_DEVICE_STATUS,
        status | VIRTIO_STATUS_DRIVER_OK,
    );
    true
}

/// Set up the virtio control queue (queue 0) using the legacy PCI interface.
///
/// # Safety
/// `io_base` must be the virtio-gpu legacy I/O port base address.
unsafe fn setup_controlq(io_base: u16) -> bool {
    unsafe {
        // Device reset.
        vio_write8(io_base, VIRTIO_PCI_STATUS, 0);

        // ACKNOWLEDGE | DRIVER
        let mut status = vio_read8(io_base, VIRTIO_PCI_STATUS);
        status |= VIRTIO_STATUS_ACKNOWLEDGE;
        vio_write8(io_base, VIRTIO_PCI_STATUS, status);
        status = vio_read8(io_base, VIRTIO_PCI_STATUS);
        status |= VIRTIO_STATUS_DRIVER;
        vio_write8(io_base, VIRTIO_PCI_STATUS, status);

        // Select queue 0.
        vio_write16(io_base, VIRTIO_PCI_QUEUE_SELECT, 0);
        let qsize = vio_read16(io_base, VIRTIO_PCI_QUEUE_SIZE) as usize;
        if qsize == 0 {
            serial::write_line(b"[virtio-gpu] control queue size is zero");
            return false;
        }

        // Program queue PFN (page frame number = phys_addr >> 12).
        let queue_pa = core::ptr::addr_of!(GPU_QUEUE_MEM.0) as u64;
        vio_write32(io_base, VIRTIO_PCI_QUEUE_PFN, (queue_pa >> 12) as u32);

        // DRIVER_OK
        status = vio_read8(io_base, VIRTIO_PCI_STATUS);
        status |= VIRTIO_STATUS_DRIVER_OK;
        vio_write8(io_base, VIRTIO_PCI_STATUS, status);
        true
    }
}

fn init_scanout_locked(state: &mut GpuState) -> bool {
    let transport_ready = if state.modern {
        state.common_cfg_addr != 0 && state.notify_addr != 0
    } else {
        state.io_base != 0
    };
    if !transport_ready || state.fb_phys == 0 || state.width == 0 || state.height == 0 {
        serial::write_line(b"[virtio-gpu] init precondition failed");
        return false;
    }

    let rid = state.resource_id;
    let w = state.width;
    let h = state.height;
    let fb = state.fb_phys;
    let fb_len = (w as u64).saturating_mul(h as u64).saturating_mul(4);

    let mut cmd = [0u8; GPU_CMD_BUF_SIZE];

    // RESOURCE_CREATE_2D
    cmd.fill(0);
    cmd[0..4].copy_from_slice(&VIRTIO_GPU_CMD_RESOURCE_CREATE_2D.to_le_bytes());
    cmd[24..28].copy_from_slice(&rid.to_le_bytes());
    cmd[28..32].copy_from_slice(&VIRTIO_GPU_FORMAT_BGRX8888.to_le_bytes());
    cmd[32..36].copy_from_slice(&w.to_le_bytes());
    cmd[36..40].copy_from_slice(&h.to_le_bytes());
    if !submit_controlq_locked(state, &cmd, 40) {
        serial::write_line(b"[virtio-gpu] RESOURCE_CREATE_2D failed");
        return false;
    }

    // RESOURCE_ATTACH_BACKING (single memory entry)
    cmd.fill(0);
    cmd[0..4].copy_from_slice(&VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING.to_le_bytes());
    cmd[24..28].copy_from_slice(&rid.to_le_bytes());
    cmd[28..32].copy_from_slice(&1u32.to_le_bytes());
    cmd[32..40].copy_from_slice(&fb.to_le_bytes());
    cmd[40..44].copy_from_slice(&(fb_len as u32).to_le_bytes());
    cmd[44..48].copy_from_slice(&0u32.to_le_bytes());
    if !submit_controlq_locked(state, &cmd, 48) {
        serial::write_line(b"[virtio-gpu] RESOURCE_ATTACH_BACKING failed");
        return false;
    }

    // SET_SCANOUT
    cmd.fill(0);
    cmd[0..4].copy_from_slice(&VIRTIO_GPU_CMD_SET_SCANOUT.to_le_bytes());
    cmd[24..28].copy_from_slice(&0u32.to_le_bytes());
    cmd[28..32].copy_from_slice(&0u32.to_le_bytes());
    cmd[32..36].copy_from_slice(&w.to_le_bytes());
    cmd[36..40].copy_from_slice(&h.to_le_bytes());
    cmd[40..44].copy_from_slice(&0u32.to_le_bytes());
    cmd[44..48].copy_from_slice(&rid.to_le_bytes());
    if !submit_controlq_locked(state, &cmd, 48) {
        serial::write_line(b"[virtio-gpu] SET_SCANOUT failed");
        return false;
    }
    true
}

fn query_capset_info_locked(state: &mut GpuState) {
    let mut chosen = None::<(u32, u32, u32)>;
    for index in 0..8u32 {
        let mut cmd = [0u8; GPU_CMD_BUF_SIZE];
        cmd[0..4].copy_from_slice(&VIRTIO_GPU_CMD_GET_CAPSET_INFO.to_le_bytes());
        cmd[24..28].copy_from_slice(&index.to_le_bytes());
        if !submit_controlq_locked(state, &cmd, 28) {
            break;
        }

        let resp = unsafe { &*core::ptr::addr_of!(GPU_RESP_BUF.0) };
        let resp_type = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
        if resp_type != VIRTIO_GPU_RESP_OK_CAPSET_INFO {
            continue;
        }

        let capset_id = u32::from_le_bytes([resp[24], resp[25], resp[26], resp[27]]);
        let capset_version = u32::from_le_bytes([resp[28], resp[29], resp[30], resp[31]]);
        let capset_size = u32::from_le_bytes([resp[32], resp[33], resp[34], resp[35]]);
        serial::write_bytes(b"[virtio-gpu] capset_info idx=");
        serial::write_u64_dec_inline(index as u64);
        serial::write_bytes(b" id=");
        serial::write_u64_dec_inline(capset_id as u64);
        serial::write_bytes(b" version=");
        serial::write_u64_dec_inline(capset_version as u64);
        serial::write_bytes(b" size=");
        serial::write_u64_dec(capset_size as u64);

        if capset_id == VIRTIO_GPU_CAPSET_VIRGL {
            chosen = Some((capset_id, capset_version, capset_size));
            break;
        }
        if chosen.is_none() && capset_id != 0 {
            chosen = Some((capset_id, capset_version, capset_size));
        }
    }

    if let Some((id, version, size)) = chosen {
        state.capset_id = id;
        state.capset_version = version;
        state.capset_size = size;
    }
}

fn submit_controlq_locked(state: &mut GpuState, cmd: &[u8], cmd_len: usize) -> bool {
    if cmd_len == 0 || cmd_len > GPU_CMD_BUF_SIZE {
        return false;
    }
    if !state.modern && state.io_base == 0 {
        return false;
    }
    if state.modern && state.notify_addr == 0 {
        return false;
    }

    let io_base = state.io_base;
    let notify_addr = state.notify_addr;

    crate::mm::page_table::with_kernel_address_space(|| unsafe {
        let cmd_buf = &mut *core::ptr::addr_of_mut!(GPU_CMD_BUF.0);
        let resp_buf = &mut *core::ptr::addr_of_mut!(GPU_RESP_BUF.0);
        cmd_buf.fill(0);
        resp_buf.fill(0);
        cmd_buf[..cmd_len].copy_from_slice(&cmd[..cmd_len]);

        let cmd_pa = cmd_buf.as_ptr() as u64;
        let resp_pa = resp_buf.as_ptr() as u64;
        let q = core::ptr::addr_of_mut!(GPU_QUEUE_MEM.0).cast::<u8>();

        // desc 0: request buffer, device reads.
        core::ptr::write_unaligned(q as *mut u64, cmd_pa);
        core::ptr::write_unaligned(q.add(8) as *mut u32, cmd_len as u32);
        core::ptr::write_unaligned(q.add(12) as *mut u16, 1u16); // NEXT
        core::ptr::write_unaligned(q.add(14) as *mut u16, 1u16); // next=1

        // desc 1: response buffer, device writes.
        let d1 = q.add(16);
        core::ptr::write_unaligned(d1 as *mut u64, resp_pa);
        core::ptr::write_unaligned(d1.add(8) as *mut u32, GPU_CMD_BUF_SIZE as u32);
        core::ptr::write_unaligned(d1.add(12) as *mut u16, 2u16); // WRITE
        core::ptr::write_unaligned(d1.add(14) as *mut u16, 0u16);

        let avail_base = q.add(GPU_DESC_BYTES);
        let next = state.avail_idx.wrapping_add(1);
        let ring_slot = (state.avail_idx as usize) % GPU_QUEUE_SIZE;
        core::ptr::write_unaligned(avail_base as *mut u16, 0u16);
        core::ptr::write_unaligned(avail_base.add(2) as *mut u16, next);
        core::ptr::write_unaligned(avail_base.add(4 + ring_slot * 2) as *mut u16, 0u16);
        state.avail_idx = next;

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        if state.modern {
            mmio_write_u16(notify_addr, 0);
        } else {
            vio_write16(io_base, VIRTIO_PCI_QUEUE_NOTIFY, 0);
        }

        let used_base = q.add(GPU_USED_OFF);
        let mut spins = 0u32;
        loop {
            let used_idx = core::ptr::read_unaligned(used_base.add(2) as *const u16);
            if used_idx != state.used_idx {
                state.used_idx = used_idx;
                let resp_type =
                    u32::from_le_bytes([resp_buf[0], resp_buf[1], resp_buf[2], resp_buf[3]]);
                let ok = (resp_type & 0xFF00) == 0x1100 || resp_type == 0;
                if !ok {
                    serial::write_bytes(b"[virtio-gpu] control response type=0x");
                    serial::write_hex(resp_type as u64);
                }
                return ok;
            }
            spins = spins.saturating_add(1);
            if spins > 200_000 {
                serial::write_line(b"[virtio-gpu] control queue timeout");
                return false;
            }
            core::hint::spin_loop();
        }
    })
}

fn mmio_read_u8(addr: usize) -> u8 {
    unsafe { core::ptr::read_volatile(addr as *const u8) }
}

fn mmio_read_u16(addr: usize) -> u16 {
    unsafe { core::ptr::read_volatile(addr as *const u16) }
}

fn mmio_read_u32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn mmio_write_u8(addr: usize, value: u8) {
    unsafe { core::ptr::write_volatile(addr as *mut u8, value) }
}

fn mmio_write_u16(addr: usize, value: u16) {
    unsafe { core::ptr::write_volatile(addr as *mut u16, value) }
}

fn mmio_write_u32(addr: usize, value: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

fn mmio_write_u64(addr: usize, value: u64) {
    mmio_write_u32(addr, value as u32);
    mmio_write_u32(addr + 4, (value >> 32) as u32);
}

pub fn fill_rect_scanout(x: i32, y: i32, w: u32, h: u32, color: u32) {
    if w == 0 || h == 0 {
        return;
    }
    let s = STATE.lock();
    if !s.present || s.fb_phys == 0 || s.width == 0 || s.height == 0 {
        return;
    }
    let fb_w = s.width as i32;
    let fb_h = s.height as i32;
    let x0 = x.clamp(0, fb_w.max(0));
    let y0 = y.clamp(0, fb_h.max(0));
    let x1 = x.saturating_add(w as i32).clamp(0, fb_w.max(0));
    let y1 = y.saturating_add(h as i32).clamp(0, fb_h.max(0));
    let stride = s.width as usize;
    let base = s.fb_phys as *mut u32;
    let fb_bytes = (s.width as u64)
        .saturating_mul(s.height as u64)
        .saturating_mul(4);
    drop(s);

    let _ = crate::mm::page_table::ensure_identity_mapped_2m(base as u64);
    if fb_bytes > 0 {
        let _ = crate::mm::page_table::ensure_identity_mapped_2m(
            (base as u64).saturating_add(fb_bytes.saturating_sub(1)),
        );
    }

    if x0 >= x1 || y0 >= y1 {
        return;
    }
    for row in y0 as usize..y1 as usize {
        let row_ptr = unsafe { base.add(row.saturating_mul(stride) + x0 as usize) };
        for col in 0..((x1 - x0) as usize) {
            unsafe { row_ptr.add(col).write_volatile(color) };
        }
    }
}

pub fn blit_pixels_scanout(
    src: &[u32],
    src_w: usize,
    src_h: usize,
    dst_x: i32,
    dst_y: i32,
    scale_fp: u16,
    opacity: u8,
) {
    if src_w == 0 || src_h == 0 || src.len() < src_w.saturating_mul(src_h) || opacity == 0 {
        return;
    }
    let s = STATE.lock();
    if !s.present || s.fb_phys == 0 || s.width == 0 || s.height == 0 {
        return;
    }
    let fb_w = s.width as i32;
    let fb_h = s.height as i32;
    let stride = s.width as usize;
    let base = s.fb_phys as *mut u32;
    let fb_bytes = (s.width as u64)
        .saturating_mul(s.height as u64)
        .saturating_mul(4);
    drop(s);

    let _ = crate::mm::page_table::ensure_identity_mapped_2m(base as u64);
    if fb_bytes > 0 {
        let _ = crate::mm::page_table::ensure_identity_mapped_2m(
            (base as u64).saturating_add(fb_bytes.saturating_sub(1)),
        );
    }

    let dst_len = stride.saturating_mul(fb_h.max(0) as usize);
    let dst = unsafe { core::slice::from_raw_parts_mut(base, dst_len) };
    crate::display_blit::blit_pixels_into(
        dst,
        stride,
        fb_w.max(0) as usize,
        fb_h.max(0) as usize,
        src,
        src_w,
        src_h,
        dst_x,
        dst_y,
        scale_fp,
        opacity,
    );
}

/// Draw a minimal software cursor overlay directly into the scanout buffer.
///
/// This fallback is used in no-compositor direct-present mode so pointer
/// movement remains visible even when no ring-3 compositor is active.
pub fn draw_cursor_overlay(x: i32, y: i32, buttons: u8) {
    let body_color = if (buttons & 0x01) != 0 {
        0xFFFF_5050
    } else {
        0xFFFF_FFFF
    };
    let shadow_color = 0xCC00_0000;
    let outline_color = 0xFF00_0000;

    // Shadow offset keeps the cursor visible against light and dark content.
    fill_rect_scanout(x + 2, y + 2, 4, 20, shadow_color);
    fill_rect_scanout(x - 5 + 2, y - 2 + 2, 14, 4, shadow_color);

    // Large high-contrast cross pointer.
    fill_rect_scanout(x, y, 3, 18, body_color);
    fill_rect_scanout(x - 5, y - 2, 13, 3, body_color);

    // Crisp outline improves visibility on bright content.
    fill_rect_scanout(x - 1, y - 1, 5, 1, outline_color);
    fill_rect_scanout(x - 1, y + 18, 5, 1, outline_color);
    fill_rect_scanout(x - 1, y, 1, 18, outline_color);
    fill_rect_scanout(x + 3, y, 1, 18, outline_color);
    fill_rect_scanout(x - 6, y - 3, 15, 1, outline_color);
    fill_rect_scanout(x - 6, y + 1, 15, 1, outline_color);
    fill_rect_scanout(x - 6, y - 2, 1, 3, outline_color);
    fill_rect_scanout(x + 8, y - 2, 1, 3, outline_color);

    // Hotspot marker.
    fill_rect_scanout(x, y, 3, 3, 0xFFFF_D060);
}

// ── Public interface ──────────────────────────────────────────────────────────

/// Returns `true` if the active display path exposes native GPU command
/// submission for the GraphOS compositor.
///
/// GraphOS now accepts compositor command packets through `SYS_GPU_SUBMIT`
/// and executes them in-kernel before presenting through virtio-gpu scanout.
pub fn native_gpu_supported() -> bool {
    STATE.lock().present
}

/// Blit a ring-3 surface directly from its physical backing pages into the
/// virtio-gpu scanout framebuffer.
///
/// Unlike the old `blit_pixels_scanout`, this function reads directly from the
/// surface's physical pages (identity-mapped in kernel address space) and
/// never allocates heap memory.  For the common case (scale=1.0, opacity=255)
/// the per-pixel branch is completely eliminated.
pub fn blit_surface_scene(
    surface_id: u32,
    src_w: u32,
    src_h: u32,
    dst_x: i32,
    dst_y: i32,
    scale_fp: u16, // 1024 = 1.0
    opacity: u8,
) {
    if src_w == 0 || src_h == 0 || opacity == 0 {
        return;
    }

    // Fetch physical page addresses for this surface (no heap alloc).
    let mut frames = [0u64; crate::wm::surface_table::MAX_SURFACE_FRAMES];
    let frame_count = crate::wm::surface_table::surface_frames(surface_id, &mut frames);
    if frame_count == 0 {
        return;
    }

    // Ensure all source backing pages are mapped in the identity window before
    // issuing direct physical reads below.
    for &page_phys in frames.iter().take(frame_count) {
        if page_phys == 0 {
            continue;
        }
        if !crate::mm::page_table::ensure_identity_mapped_2m(page_phys) {
            return;
        }
    }

    let s = STATE.lock();
    if !s.present || s.fb_phys == 0 || s.width == 0 || s.height == 0 {
        return;
    }
    let fb_w = s.width as i32;
    let fb_h = s.height as i32;
    let fb_stride = s.width as usize;
    let fb_base = s.fb_phys as *mut u32;
    drop(s);

    let scale = scale_fp.max(1) as u32;
    let dst_w = ((src_w as u64 * scale as u64) / 1024).max(0) as i32;
    let dst_h = ((src_h as u64 * scale as u64) / 1024).max(0) as i32;
    if dst_w <= 0 || dst_h <= 0 {
        return;
    }

    let x0 = dst_x.clamp(0, fb_w);
    let y0 = dst_y.clamp(0, fb_h);
    let x1 = dst_x.saturating_add(dst_w).clamp(0, fb_w);
    let y1 = dst_y.saturating_add(dst_h).clamp(0, fb_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let a = opacity as u32;
    let ia = 255u32.saturating_sub(a);

    for dy in y0..y1 {
        let sy = (((dy - dst_y) as i64 * 1024) / scale as i64).max(0) as usize;
        if sy >= src_h as usize {
            continue;
        }

        let fb_row = unsafe { fb_base.add(dy as usize * fb_stride) };

        for dx in x0..x1 {
            let sx = (((dx - dst_x) as i64 * 1024) / scale as i64).max(0) as usize;
            if sx >= src_w as usize {
                continue;
            }

            let pixel_idx = sy * src_w as usize + sx;
            let byte_off = pixel_idx * 4;
            let page_idx = byte_off / 4096;
            let word_in_pg = (byte_off % 4096) / 4;

            if page_idx >= frame_count {
                continue;
            }
            let page_phys = frames[page_idx];
            if page_phys == 0 {
                continue;
            }

            let src_px =
                unsafe { core::ptr::read_volatile((page_phys as *const u32).add(word_in_pg)) };

            let out = if opacity < 255 {
                let dst_px = unsafe { fb_row.add(dx as usize).read_volatile() };
                let sr = (src_px >> 16) & 0xFF;
                let sg = (src_px >> 8) & 0xFF;
                let sb = src_px & 0xFF;
                let dr = (dst_px >> 16) & 0xFF;
                let dg = (dst_px >> 8) & 0xFF;
                let db = dst_px & 0xFF;
                let r = (dr * ia + sr * a) / 255;
                let g = (dg * ia + sg * a) / 255;
                let b = (db * ia + sb * a) / 255;
                0xFF00_0000 | (r << 16) | (g << 8) | b
            } else {
                0xFF00_0000 | (src_px & 0x00FF_FFFF)
            };

            unsafe { fb_row.add(dx as usize).write_volatile(out) };
        }
    }
}

/// Returns `true` if a virtio-gpu device was bound.
pub fn is_present() -> bool {
    STATE.lock().present
}

/// Returns the current display resolution as `(width, height)`.
pub fn resolution() -> (u32, u32) {
    let s = STATE.lock();
    (s.width, s.height)
}

/// Returns the physical address of the framebuffer backing store (0 if none).
pub fn framebuffer_addr() -> u64 {
    STATE.lock().fb_phys
}

// ── Future native GPU resource management hooks ──────────────────────────────
// These are called by the SYS_GPU_RESOURCE_* syscall handlers.
// The current build keeps a minimal resource table while scanout remains the
// only hardware path. A later native GPU backend will replace these stubs with
// real device resource allocation and backing attachment.

use core::sync::atomic::{AtomicU32, Ordering as AtomicOrd};
static NEXT_RESOURCE_ID: AtomicU32 = AtomicU32::new(2); // 1 is reserved for scanout

/// Allocate a new GPU resource ID for a 2-D texture.
///
/// The current implementation allocates only a monotonic ID. The future native
/// GPU backend will replace this with real device resource creation.
pub fn resource_create_2d(width: u32, height: u32, _format: u8) -> Option<u32> {
    if !STATE.lock().present {
        return None;
    }
    // Sanity bounds already checked by the syscall handler.
    let _ = (width, height); // no-op in Phase 1
    let id = NEXT_RESOURCE_ID.fetch_add(1, AtomicOrd::Relaxed);
    Some(id)
}

/// Free a GPU resource previously allocated with `resource_create_2d`.
///
/// This is currently a no-op because the scanout path does not allocate native
/// GPU-side objects yet.
pub fn resource_destroy(_resource_id: u32) {
    // No-op until the native GPU backend owns real device resources.
}

/// Attach a physical backing page to a GPU resource so it can be accessed
/// as a texture without a CPU copy.
///
/// This is a placeholder for the future native GPU backend. The current scanout
/// path succeeds without performing hardware-side work.
pub fn resource_attach_backing(_resource_id: u32, _phys_addr: u64) -> bool {
    // Report success so higher layers can use one resource model now and swap
    // in the native backend later without changing the compositor contract.
    true
}

/// Signal the host to display the current framebuffer contents.
/// Submits a VIRTIO_GPU_CMD_RESOURCE_FLUSH command via the control virtqueue.
pub fn flush_rect(x: u32, y: u32, w: u32, h: u32) {
    let mut state = STATE.lock();
    if !state.present {
        return;
    }
    if !state.modern && state.io_base == 0 {
        return;
    }
    if state.modern && state.notify_addr == 0 {
        return;
    }
    let resource_id = state.resource_id;

    let mut transfer = [0u8; GPU_CMD_BUF_SIZE];
    transfer[0..4].copy_from_slice(&VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D.to_le_bytes());
    transfer[24..28].copy_from_slice(&x.to_le_bytes());
    transfer[28..32].copy_from_slice(&y.to_le_bytes());
    transfer[32..36].copy_from_slice(&w.to_le_bytes());
    transfer[36..40].copy_from_slice(&h.to_le_bytes());
    transfer[40..48].copy_from_slice(&0u64.to_le_bytes());
    transfer[48..52].copy_from_slice(&resource_id.to_le_bytes());
    let _ = submit_controlq_locked(&mut state, &transfer, 56);

    let mut flush = [0u8; GPU_CMD_BUF_SIZE];
    flush[0..4].copy_from_slice(&VIRTIO_GPU_CMD_RESOURCE_FLUSH.to_le_bytes());
    flush[24..28].copy_from_slice(&x.to_le_bytes());
    flush[28..32].copy_from_slice(&y.to_le_bytes());
    flush[32..36].copy_from_slice(&w.to_le_bytes());
    flush[36..40].copy_from_slice(&h.to_le_bytes());
    flush[40..44].copy_from_slice(&resource_id.to_le_bytes());
    let _ = submit_controlq_locked(&mut state, &flush, 48);
}
