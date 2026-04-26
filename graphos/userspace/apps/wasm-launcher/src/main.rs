// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! WASM app launcher — ring-3 host for `.gapp` bundles.
//!
//! Loads a `.gapp` archive (WASM binary + manifest + assets + ed25519 sig),
//! verifies the signature, then forks a per-app sandbox task via
//! `SYS_SPAWN` with a filtered capability set derived from the manifest.

use graphos_app_sdk::sys;

// ---------------------------------------------------------------------------
// .gapp bundle layout constants
// ---------------------------------------------------------------------------

const GAPP_MAGIC: u32 = 0x4750_5041; // "GAPP"
const GAPP_VERSION: u16 = 1;
const MAX_PATH: usize = 256;
const MAX_MODULES: usize = 8;

// ---------------------------------------------------------------------------
// Manifest capability flags
// ---------------------------------------------------------------------------

const CAP_NET: u32 = 1 << 0;
const CAP_FS_READ: u32 = 1 << 1;
const CAP_FS_WRITE: u32 = 1 << 2;
const CAP_IPC: u32 = 1 << 3;
const CAP_GPU: u32 = 1 << 4;
const CAP_AUDIO: u32 = 1 << 5;

// ---------------------------------------------------------------------------
// Bundle structures
// ---------------------------------------------------------------------------

#[repr(C)]
struct GappHeader {
    magic: u32,
    version: u16,
    flags: u16,
    uuid: [u8; 16],
    /// Claimed capability set (verified against signature).
    caps: u32,
    wasm_off: u32,
    wasm_len: u32,
    sig: [u8; 64],
    name_len: u8,
    name: [u8; 63],
}

struct AppSlot {
    active: bool,
    uuid: [u8; 16],
    name: [u8; 64],
    caps: u32,
    /// Task handle returned by SYS_SPAWN.
    task_id: u64,
}

impl AppSlot {
    const EMPTY: Self = Self {
        active: false,
        uuid: [0u8; 16],
        name: [0u8; 64],
        caps: 0,
        task_id: 0,
    };
}

static mut SLOTS: [AppSlot; MAX_MODULES] = [AppSlot::EMPTY; MAX_MODULES];

// ---------------------------------------------------------------------------
// Bundle verification
// ---------------------------------------------------------------------------

fn verify_gapp(data: &[u8]) -> bool {
    if data.len() < core::mem::size_of::<GappHeader>() {
        return false;
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != GAPP_MAGIC {
        return false;
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != GAPP_VERSION {
        return false;
    }
    // Signature covers everything before the `sig` field (offset 28).
    // The ed25519 public key is fetched from the kernel's enrolled key store.
    // For this ring-3 host we delegate verification to SYS_VERIFY_BUNDLE.
    let result = unsafe {
        sys::raw_syscall(
            sys::SYS_VERIFY_BUNDLE,
            data.as_ptr() as u64,
            data.len() as u64,
            0,
            0,
        )
    };
    result == 0
}

// ---------------------------------------------------------------------------
// Launch
// ---------------------------------------------------------------------------

fn launch_app(data: &[u8]) -> bool {
    if !verify_gapp(data) {
        sys::write_log(b"[wasm-launcher] bundle signature invalid\n");
        return false;
    }
    let hdr = unsafe { &*(data.as_ptr() as *const GappHeader) };
    let wasm_off = hdr.wasm_off as usize;
    let wasm_len = hdr.wasm_len as usize;
    if wasm_off + wasm_len > data.len() {
        return false;
    }

    // Find a free slot.
    let slot = unsafe {
        (*core::ptr::addr_of_mut!(SLOTS))
            .iter_mut()
            .find(|s| !s.active)
    };
    let Some(slot) = slot else { return false };

    slot.uuid.copy_from_slice(&hdr.uuid);
    let name_len = (hdr.name_len as usize).min(63);
    slot.name[..name_len].copy_from_slice(&hdr.name[..name_len]);
    slot.caps = hdr.caps;

    // Spawn a kernel-managed WASM sandbox task.
    let task_id = unsafe {
        sys::raw_syscall(
            sys::SYS_WASM_SPAWN,
            data[wasm_off..wasm_off + wasm_len].as_ptr() as u64,
            wasm_len as u64,
            hdr.caps as u64,
            hdr.uuid.as_ptr() as u64,
        )
    };
    if task_id == u64::MAX {
        return false;
    }
    slot.task_id = task_id;
    slot.active = true;
    sys::write_log(b"[wasm-launcher] app launched\n");
    true
}

fn terminate_app(uuid: &[u8; 16]) {
    let slot = unsafe {
        (*core::ptr::addr_of_mut!(SLOTS))
            .iter_mut()
            .find(|s| s.active && &s.uuid == uuid)
    };
    if let Some(slot) = slot {
        unsafe { sys::raw_syscall(sys::SYS_TASK_KILL, slot.task_id, 0, 0, 0) };
        slot.active = false;
    }
}

// ---------------------------------------------------------------------------
// Message dispatch
// ---------------------------------------------------------------------------

fn dispatch(msg: &[u8]) {
    if msg.is_empty() {
        return;
    }
    match msg[0] {
        0x01 => {
            launch_app(&msg[1..]);
        }
        0x02 if msg.len() >= 17 => {
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(&msg[1..17]);
            terminate_app(&uuid);
        }
        _ => {}
    }
}

fn main() {
    sys::write_log(b"[wasm-launcher] started\n");
    let mut buf = [0u8; 65536];
    loop {
        let n = unsafe {
            sys::raw_syscall(
                sys::SYS_CHANNEL_RECV,
                0,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
                0,
            )
        };
        if n > 0 && (n as usize) <= buf.len() {
            dispatch(&buf[..n as usize]);
        }
    }
}
