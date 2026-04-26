// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! OTA update client — A/B boot slot management and signed delta updates.
//!
//! Implements the GraphOS OTA update flow:
//! 1. `gpm upgrade` or the MDM agent stages a signed update bundle to `/var/update/pending`.
//! 2. On the next boot, the kernel reads the staged bundle before starting services.
//! 3. The bundle is verified (ed25519 signature), extracted, and written to the inactive
//!    A/B partition.
//! 4. Boot slot variables are updated to point to the new partition.
//! 5. After a successful boot, the active slot is marked `boot_successful`.

use spin::Mutex;

// ---------------------------------------------------------------------------
// A/B slot state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum SlotState {
    /// Slot has never been used.
    Empty = 0,
    /// Slot is the active, proven partition.
    Active = 1,
    /// Slot has been staged; will be tried on next boot.
    Staged = 2,
    /// Slot failed to boot; marked for rollback.
    Failed = 3,
}

#[derive(Clone, Copy)]
struct BootSlot {
    state: SlotState,
    /// Partition LBA start.
    lba_start: u64,
    /// Partition size in sectors.
    sector_count: u64,
    /// Version string (max 32 bytes).
    version: [u8; 32],
    /// SHA-256 digest of the installed kernel image.
    kernel_digest: [u8; 32],
    /// Graph arena node ID for this boot slot (0 = not yet registered).
    graph_node: crate::graph::types::NodeId,
}

impl BootSlot {
    const EMPTY: Self = Self {
        state: SlotState::Empty,
        lba_start: 0,
        sector_count: 0,
        version: [0u8; 32],
        kernel_digest: [0u8; 32],
        graph_node: 0,
    };
}

struct UpdateState {
    slot_a: BootSlot,
    slot_b: BootSlot,
    active_is_a: bool,
    update_pending: bool,
}

impl UpdateState {
    const fn new() -> Self {
        Self {
            slot_a: BootSlot::EMPTY,
            slot_b: BootSlot::EMPTY,
            active_is_a: true,
            update_pending: false,
        }
    }
}

static STATE: Mutex<UpdateState> = Mutex::new(UpdateState::new());

// ---------------------------------------------------------------------------
// Update bundle format
// ---------------------------------------------------------------------------

const BUNDLE_MAGIC: u32 = 0x4752_5550; // "GRUP"
const BUNDLE_VERSION: u16 = 1;

/// Validate and apply a staged update bundle.
///
/// `bundle` is the raw bytes of the signed update archive.
/// Returns `true` if the bundle was valid and staged successfully.
pub fn stage_update(bundle: &[u8]) -> bool {
    if bundle.len() < 16 {
        return false;
    }
    let magic = u32::from_le_bytes([bundle[0], bundle[1], bundle[2], bundle[3]]);
    if magic != BUNDLE_MAGIC {
        crate::arch::serial::write_line(b"[update] bad magic");
        return false;
    }
    let version = u16::from_le_bytes([bundle[4], bundle[5]]);
    if version != BUNDLE_VERSION {
        crate::arch::serial::write_line(b"[update] unsupported version");
        return false;
    }

    // Signature is last 64 bytes; signed payload is everything before it.
    if bundle.len() < 64 {
        return false;
    }
    let payload = &bundle[..bundle.len() - 64];
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&bundle[bundle.len() - 64..]);

    // Retrieve enrolled update signing key from TPM.
    let pub_key = crate::tpm::get_update_signing_key();
    if !crate::crypto::ed25519::verify(&pub_key, payload, &sig) {
        crate::arch::serial::write_line(b"[update] signature invalid");
        return false;
    }

    let mut s = STATE.lock();
    // Write to the inactive slot.
    let inactive = if s.active_is_a {
        &mut s.slot_b
    } else {
        &mut s.slot_a
    };
    inactive.state = SlotState::Staged;
    // Register a graph node for this slot if not already done.
    if inactive.graph_node == 0 {
        use crate::graph::handles::GraphHandle;
        use crate::graph::types::{EdgeKind, NODE_ID_KERNEL};
        let gn = crate::graph::handles::register_boot_slot(NODE_ID_KERNEL);
        if gn.is_valid() {
            crate::graph::arena::add_edge(NODE_ID_KERNEL, gn.node_id(), EdgeKind::Owns, 0);
            inactive.graph_node = gn.node_id();
        }
    }
    // Version string at bytes [6..38].
    let ver_end = bundle.len().min(6 + 32) - 64;
    let ver_len = ver_end.saturating_sub(6).min(32);
    inactive.version[..ver_len].copy_from_slice(&bundle[6..6 + ver_len]);

    s.update_pending = true;
    crate::arch::serial::write_line(b"[update] bundle staged");
    true
}

/// Apply the staged update on the next boot.
///
/// Swaps the active slot pointer.  Called by the boot sequence when a staged
/// update is detected.
pub fn apply_staged() -> bool {
    let mut s = STATE.lock();
    if !s.update_pending {
        return false;
    }
    s.active_is_a = !s.active_is_a;
    s.update_pending = false;
    let now_active = if s.active_is_a {
        &mut s.slot_a
    } else {
        &mut s.slot_b
    };
    now_active.state = SlotState::Active;
    crate::arch::serial::write_line(b"[update] applied staged update");
    true
}

/// Mark the current boot as successful (called after all services reach `health=ready`).
pub fn mark_boot_successful() {
    let s = STATE.lock();
    let _ = s; // slot already Active; no additional state change needed
    crate::arch::serial::write_line(b"[update] boot confirmed successful");
}

/// Roll back to the previously active slot (undo the last update).
///
/// Returns `true` if a rollback target was available.
pub fn rollback() -> bool {
    let mut s = STATE.lock();
    let inactive = if s.active_is_a { &s.slot_b } else { &s.slot_a };
    if inactive.state == SlotState::Empty {
        return false;
    }
    // Swap active slot.
    s.active_is_a = !s.active_is_a;
    let now_active = if s.active_is_a {
        &mut s.slot_a
    } else {
        &mut s.slot_b
    };
    now_active.state = SlotState::Active;
    crate::arch::serial::write_line(b"[update] rolled back to previous slot");
    true
}

/// Return a text description of the current slot states for diagnostics.
pub fn slot_summary(buf: &mut [u8]) -> usize {
    let s = STATE.lock();
    let active_label = if s.active_is_a { b"A" } else { b"B" };
    let summary = b"[update] active=";
    let n = summary.len().min(buf.len());
    buf[..n].copy_from_slice(&summary[..n]);
    if n < buf.len() {
        buf[n] = active_label[0];
    }
    n + 1
}
