// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use spin::Mutex;

use crate::graph::handles::GraphHandle;
use crate::graph::types::{EdgeKind, NODE_ID_KERNEL, NodeId};
use crate::uuid::{DeviceUuid, DriverPackageUuid, Uuid128};

const MAX_DRIVER_PACKAGES: usize = 16;

/// The kernel-embedded trusted signing public key (ed25519, 32 bytes).
/// In production this would be baked in at build time; here it is the test key.
/// `0x00…00` means "no trusted key enrolled" — reject all packages.
static TRUSTED_PUBKEY: spin::RwLock<[u8; 32]> = spin::RwLock::new([0u8; 32]);

/// Enroll the trusted ed25519 signing key.  May only be called once during
/// early boot (before any `install_signed_driver_package` call).
pub fn enroll_signing_key(key: &[u8; 32]) {
    let mut k = TRUSTED_PUBKEY.write();
    *k = *key;
}

#[derive(Clone, Copy)]
struct DriverPackage {
    active: bool,
    package_uuid: DriverPackageUuid,
    target_device_uuid: DeviceUuid,
    /// Graph arena node ID for this package (0 = not yet registered).
    graph_node: NodeId,
}

impl DriverPackage {
    const EMPTY: Self = Self {
        active: false,
        package_uuid: DriverPackageUuid(Uuid128::NIL),
        target_device_uuid: DeviceUuid(Uuid128::NIL),
        graph_node: 0,
    };
}

struct InstallerState {
    packages: [DriverPackage; MAX_DRIVER_PACKAGES],
}

impl InstallerState {
    const fn new() -> Self {
        Self {
            packages: [DriverPackage::EMPTY; MAX_DRIVER_PACKAGES],
        }
    }
}

static STATE: Mutex<InstallerState> = Mutex::new(InstallerState::new());

/// Install a driver package after verifying its ed25519 signature.
///
/// * `package_uuid`       — unique ID of this driver package
/// * `target_device_uuid` — PCI/virtio device this driver claims
/// * `manifest`           — raw manifest bytes that were signed
/// * `sig`                — 64-byte ed25519 signature over `manifest`
///
/// Returns `true` on success, `false` if the signature is invalid,
/// no trusted key is enrolled, or the package table is full.
pub fn install_signed_driver_package(
    package_uuid: DriverPackageUuid,
    target_device_uuid: DeviceUuid,
    manifest: &[u8],
    sig: &[u8; 64],
) -> bool {
    if package_uuid.into_inner() == Uuid128::NIL || target_device_uuid.into_inner() == Uuid128::NIL
    {
        return false;
    }
    let pubkey = *TRUSTED_PUBKEY.read();
    if pubkey == [0u8; 32] {
        // No trusted key enrolled — reject every package.
        return false;
    }
    if !crate::crypto::ed25519::verify(&pubkey, manifest, sig) {
        return false;
    }
    install_verified(package_uuid, target_device_uuid)
}

/// Internal: register a package that has already been verified.
fn install_verified(package_uuid: DriverPackageUuid, target_device_uuid: DeviceUuid) -> bool {
    let graph_handle = crate::graph::handles::register_driver_package(NODE_ID_KERNEL);
    let graph_node = graph_handle.node_id();
    if graph_node != 0 {
        crate::graph::arena::add_edge(NODE_ID_KERNEL, graph_node, EdgeKind::Owns, 0);
    }
    let mut state = STATE.lock();
    for slot in &mut state.packages {
        if slot.active && slot.package_uuid == package_uuid {
            slot.target_device_uuid = target_device_uuid;
            // Detach the freshly-allocated duplicate node; the existing slot keeps its graph node.
            if graph_node != 0 {
                crate::graph::arena::detach_node(graph_node);
            }
            return true;
        }
    }
    for slot in &mut state.packages {
        if !slot.active {
            *slot = DriverPackage {
                active: true,
                package_uuid,
                target_device_uuid,
                graph_node,
            };
            // Wire Device → DriverPackage with DriverAttached edge if the device is in the graph.
            if graph_node != 0
                && let Some(dev_node) = crate::drivers::device_node_for_uuid(target_device_uuid)
            {
                crate::graph::arena::add_edge(dev_node, graph_node, EdgeKind::DriverAttached, 0);
            }
            return true;
        }
    }
    // Table full — detach the node we just allocated.
    if graph_node != 0 {
        crate::graph::arena::detach_node(graph_node);
    }
    false
}

pub fn driver_package_for_device(device_uuid: DeviceUuid) -> Option<DriverPackageUuid> {
    let state = STATE.lock();
    state
        .packages
        .iter()
        .find(|s| s.active && s.target_device_uuid == device_uuid)
        .map(|s| s.package_uuid)
}

pub fn probe_driver_for_device(device_uuid: DeviceUuid) -> bool {
    driver_package_for_device(device_uuid).is_some()
}

/// Detach an installed driver package: deactivate the table slot and
/// soft-delete the corresponding graph node (sets `NODE_FLAG_DETACHED`).
///
/// Called from the hot-unplug path when a device is removed.
/// Returns `true` if the package was found and detached.
pub fn detach_driver_package(package_uuid: DriverPackageUuid) -> bool {
    let mut state = STATE.lock();
    for slot in &mut state.packages {
        if slot.active && slot.package_uuid == package_uuid {
            slot.active = false;
            let node = slot.graph_node;
            drop(state);
            if node != 0 {
                crate::graph::arena::detach_node(node);
            }
            return true;
        }
    }
    false
}
