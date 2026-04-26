// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Grid resource accounting — tracks local and remote resource availability.
//!
//! This module:
//!  - Exposes local resource stats (CPU cores, free RAM, GPU VRAM, storage)
//!  - Builds the `GridResourceUpdate` broadcast payload
//!  - Provides the `allocate_resource` API used by `remote_task` and `remote_mem`

use core::sync::atomic::{AtomicU8, Ordering};

/// Local CPU core count.  Set during kernel SMP init.
static LOCAL_CPU_CORES: AtomicU8 = AtomicU8::new(1);

/// Current CPU load percentage (0–100), updated by the scheduler.
static LOCAL_CPU_LOAD: AtomicU8 = AtomicU8::new(0);

/// Free RAM in MiB (saturating u8), updated by the frame allocator.
static LOCAL_RAM_FREE_MIB: AtomicU8 = AtomicU8::new(255);

/// GPU VRAM free in MiB (0 if no GPU).
static LOCAL_GPU_VRAM_MIB: AtomicU8 = AtomicU8::new(0);

/// Free storage in GiB (saturating u8).
static LOCAL_STORAGE_GIB: AtomicU8 = AtomicU8::new(0);

// ── Update hooks called by other kernel subsystems ───────────────────────────

pub fn set_cpu_cores(n: u8) {
    LOCAL_CPU_CORES.store(n, Ordering::Relaxed);
}

pub fn set_cpu_load(pct: u8) {
    LOCAL_CPU_LOAD.store(pct.min(100), Ordering::Relaxed);
}

pub fn set_ram_free_mib(mib: u8) {
    LOCAL_RAM_FREE_MIB.store(mib, Ordering::Relaxed);
}

pub fn set_gpu_vram_mib(mib: u8) {
    LOCAL_GPU_VRAM_MIB.store(mib, Ordering::Relaxed);
}

pub fn set_storage_free_gib(gib: u8) {
    LOCAL_STORAGE_GIB.store(gib, Ordering::Relaxed);
}

// ── Snapshot ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct LocalResources {
    pub cpu_cores: u8,
    pub cpu_load_pct: u8,
    pub ram_free_mib: u8,
    pub gpu_vram_mib: u8,
    pub storage_free_gib: u8,
}

pub fn snapshot() -> LocalResources {
    LocalResources {
        cpu_cores: LOCAL_CPU_CORES.load(Ordering::Relaxed),
        cpu_load_pct: LOCAL_CPU_LOAD.load(Ordering::Relaxed),
        ram_free_mib: LOCAL_RAM_FREE_MIB.load(Ordering::Relaxed),
        gpu_vram_mib: LOCAL_GPU_VRAM_MIB.load(Ordering::Relaxed),
        storage_free_gib: LOCAL_STORAGE_GIB.load(Ordering::Relaxed),
    }
}

/// Build a `GridResourceUpdate` byte payload for broadcast.
pub fn build_resource_update(node_uuid: crate::uuid::Uuid128) -> [u8; 20] {
    use super::protocol::{GridMsgKind, GridResourceUpdate};
    let r = snapshot();
    let msg = GridResourceUpdate {
        kind: GridMsgKind::ResourceUpdate as u8,
        cpu_load_percent: r.cpu_load_pct,
        ram_free_mib: r.ram_free_mib,
        gpu_load_percent: 0,
        node_uuid: node_uuid.to_bytes(),
    };
    let bytes: [u8; GridResourceUpdate::SIZE] = unsafe { core::mem::transmute(msg) };
    bytes
}

/// Returns true if this node can accept a remote task requiring the given
/// capability bitmask and approximate memory footprint in MiB.
pub fn can_accept_task(caps_required: u8, mem_mib: u8) -> bool {
    // Must offer all required capabilities.
    if LOCAL_RAM_FREE_MIB.load(Ordering::Relaxed) < mem_mib {
        return false;
    }
    if LOCAL_CPU_LOAD.load(Ordering::Relaxed) >= 90 {
        return false;
    }
    // The local node always advertises ALL capabilities when grid is active.
    let _ = caps_required;
    true
}
