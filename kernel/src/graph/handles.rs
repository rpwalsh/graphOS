// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Typed OS object handles — compile-time graph-kind discrimination.
//!
//! Every kernel object that lives in the graph is accessed through a typed
//! handle that carries its `NodeId` AND encodes the `NodeKind` at the type
//! level. This prevents mixing, say, a `TaskHandle` with a `ChannelHandle`
//! — the compiler rejects it before it reaches the arena.
//!
//! ## Design
//!
//! Each handle is a newtype `struct FooHandle(NodeId)` with associated
//! constants and a `kind()` method. All handles implement the `GraphHandle`
//! trait, which lets generic graph operations (spectral refresh, walk
//! sampling, twin sync) work over any handle type uniformly.
//!
//! ## Typesafety invariant
//!
//! A `TaskHandle(id)` guarantees that arena node `id` has `kind == NodeKind::Task`
//! *at the time the handle was created*. Handles are only constructed by the
//! `register_*` functions in this module, which atomically insert the node.
//! Nothing else constructs handles, so the invariant is sealed.
//!
//! ## Walsh connection
//!
//! The per-type-pair Walsh operator S_{ττ'}(h, Δt) requires knowing τ (the
//! NodeKind) at every transition. Typed handles make τ available statically,
//! allowing the `walsh::aggregate()` function to select the correct
//! W_{ττ'} matrix at compile time (monomorphisation) instead of a runtime
//! branch.

use crate::graph::arena;
use crate::graph::types::{EdgeKind, NODE_FLAG_TRUSTED, NodeId, NodeKind, WEIGHT_ONE, Weight};

// ────────────────────────────────────────────────────────────────────
// Core trait
// ────────────────────────────────────────────────────────────────────

/// Trait implemented by every typed OS-object handle.
///
/// Provides uniform access to the underlying `NodeId` and `NodeKind`,
/// enabling generic graph algorithms to work over any handle.
pub trait GraphHandle: Copy {
    /// The `NodeKind` that this handle always represents.
    const KIND: NodeKind;

    /// The raw arena `NodeId` for this handle.
    fn node_id(self) -> NodeId;

    /// Is this handle valid (non-null)?
    fn is_valid(self) -> bool {
        self.node_id() != 0
    }
}

// ────────────────────────────────────────────────────────────────────
// Macro: generate handle newtypes
// ────────────────────────────────────────────────────────────────────

macro_rules! define_handle {
    ($name:ident, $kind:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(transparent)]
        pub struct $name(pub NodeId);

        impl $name {
            /// Null handle (not yet registered in the graph).
            pub const NULL: Self = Self(0);
        }

        impl GraphHandle for $name {
            const KIND: NodeKind = $kind;

            #[inline(always)]
            fn node_id(self) -> NodeId {
                self.0
            }
        }
    };
}

// ── OS-object handle types ────────────────────────────────────────────
define_handle!(
    KernelHandle,
    NodeKind::Kernel,
    "Handle to the kernel root node (singleton)."
);
define_handle!(
    CpuHandle,
    NodeKind::CpuCore,
    "Handle to a physical CPU core node."
);
define_handle!(
    TaskHandle,
    NodeKind::Task,
    "Handle to a schedulable task node."
);
define_handle!(
    AddressSpaceHandle,
    NodeKind::AddressSpace,
    "Handle to an address space (PML4 root) node."
);
define_handle!(
    MemoryRegionHandle,
    NodeKind::MemoryRegion,
    "Handle to a physical memory region node."
);
define_handle!(
    DeviceHandle,
    NodeKind::Device,
    "Handle to a hardware device node."
);
define_handle!(
    ServiceHandle,
    NodeKind::Service,
    "Handle to a kernel/userspace service node."
);
define_handle!(FileHandle, NodeKind::File, "Handle to a file object node.");
define_handle!(
    InterruptHandle,
    NodeKind::Interrupt,
    "Handle to an interrupt vector node."
);
define_handle!(
    TimerHandle,
    NodeKind::Timer,
    "Handle to a timer source node."
);
define_handle!(
    SurfaceHandle,
    NodeKind::DisplaySurface,
    "Handle to a display surface node."
);
define_handle!(
    ChannelHandle,
    NodeKind::Channel,
    "Handle to an IPC channel endpoint node."
);
define_handle!(
    PrincipalHandle,
    NodeKind::Principal,
    "Handle to a user/principal identity node."
);
define_handle!(
    AnomalyHandle,
    NodeKind::Anomaly,
    "Handle to an anomaly event node."
);
define_handle!(
    SpectralSnapshotHandle,
    NodeKind::SpectralSnapshot,
    "Handle to a frozen spectral snapshot node."
);
define_handle!(
    DriverPackageGraphHandle,
    NodeKind::DriverPackage,
    "Handle to an installed, kernel-verified driver package node."
);
define_handle!(
    SocketHandle,
    NodeKind::Socket,
    "Handle to a network socket (TCP or UDP endpoint) node."
);
define_handle!(
    WasmSandboxHandle,
    NodeKind::WasmSandbox,
    "Handle to a WebAssembly sandbox module instance node."
);

// ────────────────────────────────────────────────────────────────────
// Registration functions — the ONLY way to construct handles
// ────────────────────────────────────────────────────────────────────
//
// Each `register_*` function inserts a node of the correct kind into the
// arena and returns a typed handle. On arena-full the handle is NULL.
//
// The `creator` parameter is the NodeId of the entity that caused this
// registration (use `NODE_ID_KERNEL` during early boot).

/// Register a new task node in the graph.
///
/// Call this immediately after allocating a `TaskId` in `task::table`.
/// Store the returned `TaskHandle` in the TCB's `graph_node` field.
///
/// # Arguments
/// * `creator` — NodeId of the creating entity (kernel or parent task).
/// * `flags`   — Optional extra node flags (e.g. `NODE_FLAG_TRUSTED`).
#[inline]
pub fn register_task(creator: NodeId, flags: u32) -> TaskHandle {
    let id = arena::add_node(NodeKind::Task, NODE_FLAG_TRUSTED | flags, creator);
    TaskHandle(id.unwrap_or(0))
}

/// Register a new kernel service node in the graph.
#[inline]
pub fn register_service(creator: NodeId, flags: u32) -> ServiceHandle {
    let id = arena::add_node(NodeKind::Service, NODE_FLAG_TRUSTED | flags, creator);
    ServiceHandle(id.unwrap_or(0))
}

/// Register a new IPC channel node in the graph.
///
/// Call this from `sys_channel_create` / `ipc::channel::alloc`.
/// The returned `ChannelHandle` should be embedded in the channel descriptor.
#[inline]
pub fn register_channel(creator: NodeId) -> ChannelHandle {
    let id = arena::add_node(NodeKind::Channel, 0, creator);
    ChannelHandle(id.unwrap_or(0))
}

/// Register a display surface node in the graph.
#[inline]
pub fn register_surface(creator: NodeId) -> SurfaceHandle {
    let id = arena::add_node(NodeKind::DisplaySurface, 0, creator);
    SurfaceHandle(id.unwrap_or(0))
}

/// Register a memory region node in the graph.
#[inline]
pub fn register_memory_region(creator: NodeId) -> MemoryRegionHandle {
    let id = arena::add_node(NodeKind::MemoryRegion, 0, creator);
    MemoryRegionHandle(id.unwrap_or(0))
}

/// Register a device node in the graph.
#[inline]
pub fn register_device(creator: NodeId) -> DeviceHandle {
    let id = arena::add_node(NodeKind::Device, NODE_FLAG_TRUSTED, creator);
    DeviceHandle(id.unwrap_or(0))
}

/// Register a principal (user identity) node in the graph.
#[inline]
pub fn register_principal(creator: NodeId) -> PrincipalHandle {
    let id = arena::add_node(NodeKind::Principal, NODE_FLAG_TRUSTED, creator);
    PrincipalHandle(id.unwrap_or(0))
}

/// Register an anomaly event node in the graph.
/// Wired from the spectral CUSUM detector when it fires.
#[inline]
pub fn register_anomaly(creator: NodeId) -> AnomalyHandle {
    let id = arena::add_node(NodeKind::Anomaly, NODE_FLAG_TRUSTED, creator);
    AnomalyHandle(id.unwrap_or(0))
}

/// Register a driver package node in the graph.
///
/// Called from `drivers::installer` after signature verification succeeds.
/// Returns a `DriverPackageGraphHandle` carrying the new node ID.
#[inline]
pub fn register_driver_package(creator: NodeId) -> DriverPackageGraphHandle {
    let id = arena::add_node(NodeKind::DriverPackage, NODE_FLAG_TRUSTED, creator);
    DriverPackageGraphHandle(id.unwrap_or(0))
}

/// Register a network socket node in the graph.
///
/// Called from `net::sys_socket_open` when a new socket is allocated.
#[inline]
pub fn register_socket(creator: NodeId) -> SocketHandle {
    let id = arena::add_node(NodeKind::Socket, 0, creator);
    SocketHandle(id.unwrap_or(0))
}

/// Register a WebAssembly sandbox node in the graph.
///
/// Called from `wasm::load_module` when a sandbox is instantiated.
#[inline]
pub fn register_wasm_sandbox(creator: NodeId) -> WasmSandboxHandle {
    let id = arena::add_node(NodeKind::WasmSandbox, 0, creator);
    WasmSandboxHandle(id.unwrap_or(0))
}

define_handle!(
    TpmDeviceHandle,
    NodeKind::TpmDevice,
    "Handle to the TPM 2.0 hardware security module node (singleton)."
);
define_handle!(
    FidoCredentialHandle,
    NodeKind::FidoCredential,
    "Handle to an enrolled FIDO2 / CTAP2 credential node."
);
define_handle!(
    BootSlotHandle,
    NodeKind::BootSlot,
    "Handle to an OTA A/B boot slot node."
);

/// Register the TPM 2.0 singleton node (call once at init).
#[inline]
pub fn register_tpm_device(creator: NodeId) -> TpmDeviceHandle {
    let id = arena::add_node(NodeKind::TpmDevice, NODE_FLAG_TRUSTED, creator);
    TpmDeviceHandle(id.unwrap_or(0))
}

/// Register a FIDO2 credential node on enrolment.
#[inline]
pub fn register_fido_credential(creator: NodeId) -> FidoCredentialHandle {
    let id = arena::add_node(NodeKind::FidoCredential, NODE_FLAG_TRUSTED, creator);
    FidoCredentialHandle(id.unwrap_or(0))
}

/// Register an A/B boot slot node.
#[inline]
pub fn register_boot_slot(creator: NodeId) -> BootSlotHandle {
    let id = arena::add_node(NodeKind::BootSlot, 0, creator);
    BootSlotHandle(id.unwrap_or(0))
}

// ────────────────────────────────────────────────────────────────────
// Edge helpers — typed wiring between handles
// ────────────────────────────────────────────────────────────────────

/// Wire two typed handles with a directed, weighted edge.
///
/// Generic over any two `GraphHandle` types — the compiler validates that
/// both handles are live graph objects. Returns the new `EdgeId` or 0.
///
/// # Arguments
/// * `from`   — Source handle (edge originates here).
/// * `to`     — Target handle.
/// * `kind`   — The semantic relationship.
/// * `weight` — Edge weight in 16.16 fixed-point.
#[inline]
pub fn wire<F: GraphHandle, T: GraphHandle>(
    from: F,
    to: T,
    kind: EdgeKind,
    weight: Weight,
) -> crate::graph::types::EdgeId {
    if !from.is_valid() || !to.is_valid() {
        return 0;
    }
    arena::add_edge_weighted(from.node_id(), to.node_id(), kind, 0, weight).unwrap_or(0)
}

/// Wire with unit weight (WEIGHT_ONE = 1.0 in 16.16).
#[inline]
pub fn wire_unit<F: GraphHandle, T: GraphHandle>(
    from: F,
    to: T,
    kind: EdgeKind,
) -> crate::graph::types::EdgeId {
    wire(from, to, kind, WEIGHT_ONE)
}
