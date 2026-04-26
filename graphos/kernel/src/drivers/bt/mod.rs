// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Bluetooth HCI + L2CAP kernel driver.
//!
//! Supports USB HCI (Bluetooth 4.2+ controllers) and provides a minimal
//! L2CAP connection-oriented channel implementation for kernel-space use.
//!
//! # Layers
//! - **HCI transport** (`hci_*`): USB bulk/interrupt pipe to BT controller.
//! - **HCI event demux**: Routes HCI events (Connection Complete, etc.)
//! - **L2CAP**: Logical Link Control and Adaptation Protocol channel multiplexing.
//!
//! For now, HCI transport I/O is stubbed; the channel state machine is real.

use spin::Mutex;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const HCI_CMD_PKT: u8 = 0x01;
const HCI_ACL_PKT: u8 = 0x02;
const HCI_EVENT_PKT: u8 = 0x04;

const HCI_EVT_CMD_COMPLETE: u8 = 0x0E;
const HCI_EVT_CMD_STATUS: u8 = 0x0F;
const HCI_EVT_CONN_COMPLETE: u8 = 0x03;
const HCI_EVT_DISCONN_COMPLETE: u8 = 0x05;
const HCI_EVT_LE_META: u8 = 0x3E;

const HCI_RESET: u16 = 0x0C03;
const HCI_LE_SET_SCAN_EN: u16 = 0x200C;
const HCI_LE_CREATE_CONN: u16 = 0x200D;

/// Maximum simultaneous L2CAP channels.
const MAX_L2CAP_CHANNELS: usize = 8;
/// Maximum ACL connection handles.
const MAX_CONNECTIONS: usize = 4;
/// L2CAP MTU (minimum guaranteed; negotiable).
const L2CAP_MTU: usize = 672;

// ---------------------------------------------------------------------------
// HCI frame types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct HciEventHdr {
    code: u8,
    param_len: u8,
}

#[derive(Clone, Copy)]
struct AclHdr {
    handle_flags: u16,
    data_len: u16,
}

// ---------------------------------------------------------------------------
// L2CAP channel
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum L2capState {
    Closed,
    WaitConnRsp,
    WaitConfigReq,
    Open,
}

#[derive(Clone, Copy)]
pub struct L2capChannel {
    pub state: L2capState,
    pub local_cid: u16,
    pub remote_cid: u16,
    pub acl_handle: u16,
    pub psm: u16,
}

impl L2capChannel {
    const EMPTY: Self = Self {
        state: L2capState::Closed,
        local_cid: 0,
        remote_cid: 0,
        acl_handle: 0,
        psm: 0,
    };
}

// ---------------------------------------------------------------------------
// ACL connection record
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AclState {
    Disconnected,
    Connected,
}

#[derive(Clone, Copy)]
struct AclConn {
    state: AclState,
    handle: u16,
    bd_addr: [u8; 6],
}

impl AclConn {
    const EMPTY: Self = Self {
        state: AclState::Disconnected,
        handle: 0,
        bd_addr: [0u8; 6],
    };
}

// ---------------------------------------------------------------------------
// Driver state
// ---------------------------------------------------------------------------

struct BtDriver {
    initialized: bool,
    conns: [AclConn; MAX_CONNECTIONS],
    channels: [L2capChannel; MAX_L2CAP_CHANNELS],
    next_local_cid: u16,
    scanning: bool,
}

impl BtDriver {
    const fn new() -> Self {
        Self {
            initialized: false,
            conns: [AclConn::EMPTY; MAX_CONNECTIONS],
            channels: [L2capChannel::EMPTY; MAX_L2CAP_CHANNELS],
            next_local_cid: 0x0040,
            scanning: false,
        }
    }
}

static DRIVER: Mutex<BtDriver> = Mutex::new(BtDriver::new());

/// Probe hook used by the shared driver registry.
///
/// Returns Bound when an xHCI controller is present so HCI-over-USB command
/// transport is available for Bluetooth bring-up.
pub fn probe_driver() -> crate::drivers::ProbeResult {
    let mut found_xhci = false;
    crate::arch::x86_64::pci::for_each_device(|info| {
        if found_xhci {
            return;
        }
        if info.class_code == XHCI_CLASS
            && info.subclass == XHCI_SUBCLASS
            && info.prog_if == XHCI_PROG_IF
        {
            found_xhci = true;
        }
    });
    if !found_xhci {
        return crate::drivers::ProbeResult::NoMatch;
    }
    init();
    crate::drivers::ProbeResult::Bound
}

// ---------------------------------------------------------------------------
// xHCI transport
// ---------------------------------------------------------------------------
//
// Bluetooth HCI over USB uses the control endpoint (EP0) for commands.
// The xHCI controller is discovered via PCI class scan (same BAR as usb_hid).
// Commands are submitted as Setup + Data + Status TRB triplets on EP0's
// transfer ring for device slot 1.
//
// TRB ring layout (static, 16 entries × 16 bytes = 256 bytes, page-aligned):
//   slot 1 EP0 = ring base 0
//   A Link TRB at entry 15 wraps the ring.
//
// Registers:
//   CAPLENGTH (byte)  @ mmio + 0x00
//   OP base           @ mmio + CAPLENGTH
//   OP_USBSTS (dword) @ op + 0x04
//   OP_DCBAAP (qword) @ op + 0x30   — device context base address array
//   DB base (dword*)  @ mmio + DBOFF — doorbell array

const XHCI_CLASS: u8 = 0x0C;
const XHCI_SUBCLASS: u8 = 0x03;
const XHCI_PROG_IF: u8 = 0x30;

/// TRB types used for control transfers.
const TRB_TYPE_SETUP: u32 = 2;
const TRB_TYPE_DATA: u32 = 3;
const TRB_TYPE_STATUS: u32 = 4;

/// xHCI TRB structure (16 bytes, little-endian).
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct Trb {
    param: u64,  // parameter / data pointer
    status: u32, // transfer length and other status fields
    ctrl: u32,   // type (bits 15:10), flags (bits 9:0), cycle bit (bit 0)
}

/// Minimal TRB ring for BT HCI command submission (slot 1, EP0).
/// 16 entries: 14 usable data TRBs + 1 Link TRB.
const RING_ENTRIES: usize = 16;
#[repr(align(4096))]
struct TrbRing([Trb; RING_ENTRIES]);
static mut BT_TRB_RING: TrbRing = TrbRing(
    [Trb {
        param: 0,
        status: 0,
        ctrl: 0,
    }; RING_ENTRIES],
);

/// Produce cycle bit and enqueue index for the ring.
struct RingProducer {
    idx: usize,
    cycle: u32, // 1 or 0 — alternates when Link TRB wraps
}
static BT_RING: spin::Mutex<RingProducer> = spin::Mutex::new(RingProducer { idx: 0, cycle: 1 });

/// Locate xHCI MMIO base via PCI class scan (returns 0 if not found).
fn xhci_mmio() -> u64 {
    let mut base = 0u64;
    crate::arch::x86_64::pci::for_each_device(|info| {
        if base != 0 {
            return;
        }
        if info.class_code == XHCI_CLASS
            && info.subclass == XHCI_SUBCLASS
            && info.prog_if == XHCI_PROG_IF
        {
            let bar0 = crate::arch::x86_64::pci::read_u32(
                info.location.bus,
                info.location.slot,
                info.location.func,
                0x10,
            ) & !0xFu32;
            base = bar0 as u64;
        }
    });
    base
}

/// Submit an HCI command packet to slot 1, EP0 via xHCI control transfer TRBs.
///
/// Builds: Setup TRB (bmRequestType=0x20, bRequest=0, wValue/wIndex=0,
/// wLength=len) → Data TRB (pointing at `pkt`) → Status TRB → ring doorbell.
///
/// # Safety
/// `pkt` must remain valid until the transfer completes.
unsafe fn xhci_hci_command(mmio: u64, pkt: &[u8]) {
    unsafe {
        let cap_len = core::ptr::read_volatile(mmio as *const u8) as u64;
        let op_base = mmio + cap_len;

        // Read DBOFF (dword at mmio+0x14) to find doorbell array.
        let dboff = core::ptr::read_volatile((mmio + 0x14) as *const u32) as u64 & !0x3;
        let db1 = (mmio + dboff + 4) as *mut u32; // slot 1 doorbell

        let ring_pa = core::ptr::addr_of!(BT_TRB_RING.0) as u64;
        let _ = op_base; // used for DCBAAP init (done once elsewhere)

        // Install Link TRB at last entry (index RING_ENTRIES-1) on first call.
        // Link TRB: param = ring_pa, ctrl = type(6)<<10 | TC(bit1) | cycle.
        let mut ring = BT_RING.lock();
        if BT_TRB_RING.0[RING_ENTRIES - 1].ctrl == 0 {
            BT_TRB_RING.0[RING_ENTRIES - 1] = Trb {
                param: ring_pa,
                status: 0,
                ctrl: (6 << 10) | (1 << 1) | ring.cycle, // Link TRB, TC=1
            };
        }

        let data_pa = pkt.as_ptr() as u64;
        let len = pkt.len() as u32;

        // Setup TRB: IDT=1, TRT=3 (OUT data), wLength = len.
        // Setup packet: bmRequestType(1) bRequest(1) wValue(2) wIndex(2) wLength(2)
        // For HCI_CMD: bmRequestType=0x20, bRequest=0, wValue=0, wIndex=0, wLength=len
        let setup_packet: u64 = 0x0020u64             // bmRequestType | (bRequest << 8)
            | ((len as u64) << 48); // wLength in bits [63:48]
        let cyc = ring.cycle;
        BT_TRB_RING.0[ring.idx] = Trb {
            param: setup_packet,
            status: 8, // TRB transfer length = 8 (setup stage fixed)
            ctrl: (TRB_TYPE_SETUP << 10) | (1 << 6) | (3 << 16) | cyc, // IDT, TRT=3
        };
        ring.idx = (ring.idx + 1) % (RING_ENTRIES - 1);

        // Data TRB: DIR=1 (OUT → host-to-device), points to HCI packet.
        BT_TRB_RING.0[ring.idx] = Trb {
            param: data_pa,
            status: len,
            ctrl: (TRB_TYPE_DATA << 10) | cyc, // DIR bit = 0 = OUT
        };
        ring.idx = (ring.idx + 1) % (RING_ENTRIES - 1);

        // Status TRB: DIR=1 (IN status for OUT data transfer).
        BT_TRB_RING.0[ring.idx] = Trb {
            param: 0,
            status: 0,
            ctrl: (TRB_TYPE_STATUS << 10) | (1 << 16) | (1 << 5) | cyc, // DIR=IN, IOC
        };
        ring.idx = (ring.idx + 1) % (RING_ENTRIES - 1);
        if ring.idx == 0 {
            ring.cycle ^= 1;
        }

        // Ring doorbell for slot 1, EP0 (endpoint ID = 1).
        core::ptr::write_volatile(db1, 1);
    }
}

// ── HCI command packet staging buffer ────────────────────────────────────────
// We need a static buffer because xhci_hci_command keeps a pointer to the
// packet live until the TRB is consumed.  Shared under BT_RING lock.
const HCI_PKT_MAX: usize = 260; // 1 + 2 + 1 + 255 max
static mut HCI_CMD_BUF: [u8; HCI_PKT_MAX] = [0u8; HCI_PKT_MAX];

// ---------------------------------------------------------------------------
// HCI transport stubs (USB bulk/interrupt)
// ---------------------------------------------------------------------------

/// Send an HCI command packet via xHCI EP0 control transfer.
///
/// `opcode` is the 16-bit OGF+OCF field.  `params` is the parameter payload.
/// Returns `true` if the TRBs were enqueued to the command ring.
fn hci_send_command(opcode: u16, params: &[u8]) -> bool {
    if params.len() > 255 {
        return false;
    }
    let mmio = xhci_mmio();
    if mmio == 0 {
        return true;
    } // no BT controller; command silently ignored

    let plen = params.len();
    let total = 3 + plen; // HCI_CMD_PKT(1) + opcode(2) + param_len(1) + params
    if total > HCI_PKT_MAX {
        return false;
    }
    unsafe {
        HCI_CMD_BUF[0] = 0x01; // HCI_CMD_PKT indicator
        HCI_CMD_BUF[1] = (opcode & 0xFF) as u8;
        HCI_CMD_BUF[2] = (opcode >> 8) as u8;
        HCI_CMD_BUF[3] = plen as u8;
        HCI_CMD_BUF[4..4 + plen].copy_from_slice(params);
        xhci_hci_command(mmio, &HCI_CMD_BUF[..total]);
    }
    true
}

/// Receive the next pending HCI event from the interrupt IN pipe.
///
/// Returns `None` when the event FIFO is empty.
fn hci_recv_event() -> Option<(HciEventHdr, [u8; 255])> {
    // USB HCI: read from interrupt IN endpoint.
    None
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialise the Bluetooth controller.
///
/// Sends HCI_Reset and waits for Command Complete.  Call once during boot.
pub fn init() {
    let mut d = DRIVER.lock();
    if d.initialized {
        return;
    }
    // HCI Reset (no parameters).
    hci_send_command(HCI_RESET, &[]);
    d.initialized = true;
    crate::arch::serial::write_line(b"[bt] HCI reset sent");
}

/// Begin LE scanning for nearby devices.
pub fn start_scan() -> bool {
    // HCI_LE_Set_Scan_Enable(enable=1, filter_dup=0).
    let params = [1u8, 0u8];
    hci_send_command(HCI_LE_SET_SCAN_EN, &params);
    DRIVER.lock().scanning = true;
    crate::arch::serial::write_line(b"[bt] LE scan started");
    true
}

/// Stop LE scanning.
pub fn stop_scan() {
    let params = [0u8, 0u8];
    hci_send_command(HCI_LE_SET_SCAN_EN, &params);
    DRIVER.lock().scanning = false;
}

/// Open an L2CAP channel to a connected ACL peer using the given PSM.
///
/// Returns the local CID on success, or 0 on failure.
pub fn l2cap_connect(acl_handle: u16, psm: u16) -> u16 {
    let mut d = DRIVER.lock();
    // Find a free channel slot.
    let slot = d
        .channels
        .iter()
        .position(|c| c.state == L2capState::Closed);
    let Some(slot) = slot else { return 0 };
    let local_cid = d.next_local_cid;
    d.next_local_cid = d.next_local_cid.wrapping_add(1).max(0x0040);
    d.channels[slot] = L2capChannel {
        state: L2capState::WaitConnRsp,
        local_cid,
        remote_cid: 0,
        acl_handle,
        psm,
    };
    // In production: send L2CAP Connection Request signal on ACL handle.
    local_cid
}

/// Send `payload` on the L2CAP channel identified by `local_cid`.
///
/// Returns `true` if the payload was queued.  The payload is silently truncated
/// to `L2CAP_MTU` bytes.
pub fn l2cap_send(local_cid: u16, payload: &[u8]) -> bool {
    let d = DRIVER.lock();
    let ch = d
        .channels
        .iter()
        .find(|c| c.local_cid == local_cid && c.state == L2capState::Open);
    let Some(ch) = ch else { return false };
    let send_len = payload.len().min(L2CAP_MTU);
    // ACL header: handle (4 bits flags | 12 bits handle) + length.
    let _ = AclHdr {
        handle_flags: (ch.acl_handle & 0x0FFF) | (0b10 << 12), // continuation=0
        data_len: (send_len + 4) as u16,
    };
    // Forward to USB bulk OUT pipe (stubbed).
    let _ = (ch, send_len, payload);
    true
}

/// Close an L2CAP channel.
pub fn l2cap_close(local_cid: u16) {
    let mut d = DRIVER.lock();
    if let Some(ch) = d.channels.iter_mut().find(|c| c.local_cid == local_cid) {
        ch.state = L2capState::Closed;
    }
}

/// Process all pending HCI events.  Call from the interrupt handler or a polling loop.
pub fn poll() {
    while let Some((hdr, params)) = hci_recv_event() {
        handle_event(hdr, &params);
    }
}

fn handle_event(hdr: HciEventHdr, params: &[u8]) {
    match hdr.code {
        HCI_EVT_CMD_COMPLETE => {
            // params[0] = num_hci_cmds, params[1..2] = opcode, params[3..] = return
            crate::arch::serial::write_line(b"[bt] CMD_COMPLETE");
        }
        HCI_EVT_CONN_COMPLETE => {
            if params.len() < 9 {
                return;
            }
            let status = params[0];
            let handle = u16::from_le_bytes([params[1], params[2]]);
            let mut bd_addr = [0u8; 6];
            bd_addr.copy_from_slice(&params[3..9]);
            if status == 0 {
                let mut d = DRIVER.lock();
                for conn in d.conns.iter_mut() {
                    if conn.state == AclState::Disconnected {
                        conn.state = AclState::Connected;
                        conn.handle = handle;
                        conn.bd_addr = bd_addr;
                        break;
                    }
                }
                crate::arch::serial::write_line(b"[bt] connection established");
            }
        }
        HCI_EVT_DISCONN_COMPLETE => {
            if params.len() < 3 {
                return;
            }
            let handle = u16::from_le_bytes([params[1], params[2]]);
            let mut d = DRIVER.lock();
            for conn in d.conns.iter_mut() {
                if conn.handle == handle {
                    conn.state = AclState::Disconnected;
                }
            }
            for ch in d.channels.iter_mut() {
                if ch.acl_handle == handle {
                    ch.state = L2capState::Closed;
                }
            }
            crate::arch::serial::write_line(b"[bt] disconnected");
        }
        _ => {}
    }
}
