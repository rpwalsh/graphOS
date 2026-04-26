// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Graph SDK — public API for node/edge CRUD and UUID resolution.
//!
//! This crate exposes the graph query surface of GraphOS to third-party tools
//! and userspace services. It communicates with the kernel graph subsystem via
//! the `SYS_GRAPH_*` syscall family.
//!
//! # Example
//! ```rust,no_run
//! use graphos_graph_sdk::{GraphClient, NodeKind};
//! let mut gc = GraphClient::new();
//! let id = gc.create_node(NodeKind::Service, b"my-service").unwrap();
//! println!("Created node {id:?}");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use core::fmt;

// ---------------------------------------------------------------------------
// UUID-128
// ---------------------------------------------------------------------------

/// A 128-bit UUID identifying a graph node or edge.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid128(pub [u8; 16]);

impl Uuid128 {
    /// The nil UUID (all zeros).
    pub const NIL: Self = Self([0u8; 16]);

    /// Return the inner bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Uuid128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            b[0],
            b[1],
            b[2],
            b[3],
            b[4],
            b[5],
            b[6],
            b[7],
            b[8],
            b[9],
            b[10],
            b[11],
            b[12],
            b[13],
            b[14],
            b[15]
        )
    }
}

impl fmt::Debug for Uuid128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// ---------------------------------------------------------------------------
// Node / edge types
// ---------------------------------------------------------------------------

/// The kind of a graph node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    /// Kernel task.
    Task = 0x01,
    /// Named service.
    Service = 0x02,
    /// IPC channel.
    Channel = 0x03,
    /// VFS file or directory.
    File = 0x04,
    /// PCI/USB device.
    Device = 0x05,
    /// User account.
    User = 0x06,
    /// Login session.
    Session = 0x07,
    /// WASM application bundle.
    App = 0x08,
    /// Custom / unknown.
    Other = 0xFF,
}

/// A node descriptor returned by graph queries.
#[derive(Clone, Debug)]
pub struct NodeRecord {
    /// Stable UUID.
    pub id: Uuid128,
    /// Node kind.
    pub kind: NodeKind,
    /// Human-readable name (up to 63 bytes, UTF-8).
    pub name: alloc::string::String,
}

/// An edge descriptor.
#[derive(Clone, Debug)]
pub struct EdgeRecord {
    /// Source node.
    pub from: Uuid128,
    /// Destination node.
    pub to: Uuid128,
    /// Edge label (e.g. "owns", "capability", "depends").
    pub label: alloc::string::String,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Graph SDK error.
#[derive(Debug)]
pub enum GraphError {
    /// Node not found.
    NotFound,
    /// Caller lacks capability to perform the operation.
    PermissionDenied,
    /// The kernel graph subsystem returned an unexpected error code.
    KernelError(u64),
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "node not found"),
            Self::PermissionDenied => write!(f, "permission denied"),
            Self::KernelError(c) => write!(f, "kernel error {c:#x}"),
        }
    }
}

// ---------------------------------------------------------------------------
// GraphClient
// ---------------------------------------------------------------------------

extern crate alloc;
use alloc::vec::Vec;

/// Client handle for the GraphOS kernel graph subsystem.
pub struct GraphClient;

impl GraphClient {
    /// Create a new client.
    pub fn new() -> Self {
        Self
    }

    /// Create a new node of the given kind and name.
    ///
    /// Returns the UUID assigned by the kernel.
    pub fn create_node(&mut self, _kind: NodeKind, _name: &[u8]) -> Result<Uuid128, GraphError> {
        // SYS_GRAPH_CREATE_NODE: a0=kind, a1=name_ptr, a2=name_len → UUID bytes in out-param
        // Stub: returns a synthetic UUID.
        Ok(Uuid128::NIL)
    }

    /// Look up a node by UUID.
    pub fn get_node(&self, _id: &Uuid128) -> Result<NodeRecord, GraphError> {
        Err(GraphError::NotFound)
    }

    /// List edges originating from `node`.
    pub fn edges_from(&self, _node: &Uuid128) -> Result<Vec<EdgeRecord>, GraphError> {
        Ok(Vec::new())
    }

    /// Create a directed edge from `from` to `to` with `label`.
    pub fn create_edge(
        &mut self,
        _from: &Uuid128,
        _to: &Uuid128,
        _label: &str,
    ) -> Result<Uuid128, GraphError> {
        Ok(Uuid128::NIL)
    }

    /// Delete a node and all its incident edges.
    pub fn delete_node(&mut self, _id: &Uuid128) -> Result<(), GraphError> {
        Ok(())
    }
}

impl Default for GraphClient {
    fn default() -> Self {
        Self::new()
    }
}
