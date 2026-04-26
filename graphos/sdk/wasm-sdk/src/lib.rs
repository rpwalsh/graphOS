// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS WASM SDK — WASI shim and .gapp bundle tooling.
//!
//! Provides the Rust types that map to the GraphOS WASM ABI, helpers for
//! building `.gapp` bundles, and a WASI shim so that code targeting
//! `wasm32-wasip1` can be recompiled for the GraphOS WASM sandbox with
//! minimal changes.
//!
//! # Capability flags
//! Each `.gapp` declares a `caps` bitmask in its manifest:
//!
//! | Flag | Value | Meaning |
//! |------|-------|---------|
//! | `CAP_NET` | `1 << 0` | Outbound network (TCP/UDP) |
//! | `CAP_FS_READ` | `1 << 1` | Read-only VFS access |
//! | `CAP_FS_WRITE` | `1 << 2` | Read-write VFS access |
//! | `CAP_IPC` | `1 << 3` | IPC channel creation |
//! | `CAP_GPU` | `1 << 4` | GPU/surface access |
//! | `CAP_AUDIO` | `1 << 5` | Audio output |

#![warn(missing_docs)]

/// Outbound network access.
pub const CAP_NET: u32 = 1 << 0;
/// Read-only VFS access.
pub const CAP_FS_READ: u32 = 1 << 1;
/// Read-write VFS access.
pub const CAP_FS_WRITE: u32 = 1 << 2;
/// IPC channel creation.
pub const CAP_IPC: u32 = 1 << 3;
/// GPU / compositor surface access.
pub const CAP_GPU: u32 = 1 << 4;
/// Audio output.
pub const CAP_AUDIO: u32 = 1 << 5;

// ---------------------------------------------------------------------------
// WASM ABI host functions (imported by the WASM module)
// ---------------------------------------------------------------------------

/// GraphOS-WASM host imports exposed to sandboxed modules.
///
/// These are `extern "C"` declarations that the WASM runtime resolves at
/// module instantiation.  Each import is gated by the declared capability set.
pub mod host {
    /// Write `len` bytes from the WASM linear memory at `ptr` to stdout.
    #[link(wasm_import_module = "graphos")]
    unsafe extern "C" {
        /// `graphos::write_stdout(ptr: i32, len: i32) -> i32`
        pub fn write_stdout(ptr: i32, len: i32) -> i32;
        /// `graphos::channel_send(ch: i32, ptr: i32, len: i32) -> i32`
        pub fn channel_send(ch: i32, ptr: i32, len: i32) -> i32;
        /// `graphos::channel_recv(ch: i32, ptr: i32, max: i32) -> i32`
        pub fn channel_recv(ch: i32, ptr: i32, max: i32) -> i32;
        /// `graphos::surface_present(surf: i32) -> i32`
        pub fn surface_present(surf: i32) -> i32;
        /// `graphos::exit(code: i32) -> !`
        pub fn exit(code: i32) -> !;
    }
}

// ---------------------------------------------------------------------------
// .gapp bundle builder
// ---------------------------------------------------------------------------

extern crate alloc;
use alloc::vec::Vec;

/// A .gapp bundle being assembled.
pub struct GappBuilder {
    /// App name.
    pub name: [u8; 63],
    /// Name length.
    pub name_len: u8,
    /// Declared capability set.
    pub caps: u32,
    /// UUID (16 bytes).
    pub uuid: [u8; 16],
    /// WASM binary payload.
    pub wasm: Vec<u8>,
}

impl GappBuilder {
    /// Create a new builder.
    pub fn new(name: &str, caps: u32, uuid: [u8; 16]) -> Self {
        let mut n = [0u8; 63];
        let name_len = name.len().min(63) as u8;
        n[..name_len as usize].copy_from_slice(&name.as_bytes()[..name_len as usize]);
        Self {
            name: n,
            name_len,
            caps,
            uuid,
            wasm: Vec::new(),
        }
    }

    /// Set the WASM binary.
    pub fn wasm(mut self, wasm: Vec<u8>) -> Self {
        self.wasm = wasm;
        self
    }

    /// Serialize to a `.gapp` byte blob (unsigned; caller must append signature).
    pub fn build_unsigned(&self) -> Vec<u8> {
        const MAGIC: u32 = 0x4750_5041;
        let mut out = Vec::with_capacity(128 + self.wasm.len());
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // version
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&self.uuid);
        out.extend_from_slice(&self.caps.to_le_bytes());
        let wasm_off = (out.len() + 4 + 4 + 64 + 1 + 63) as u32; // after header
        out.extend_from_slice(&wasm_off.to_le_bytes());
        out.extend_from_slice(&(self.wasm.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 64]); // signature placeholder
        out.push(self.name_len);
        out.extend_from_slice(&self.name);
        out.extend_from_slice(&self.wasm);
        out
    }
}
