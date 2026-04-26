// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Lightweight in-kernel WASM runtime — Wasm MVP + WASI subset.
//!
//! This is a minimal interpreter for the WebAssembly MVP binary format.
//! It provides linear memory isolation and capability-filtered WASI imports.
//! Each module gets a UUID identity; module capabilities are declared in the
//! manifest and enforced at import resolution.

use spin::Mutex;

pub mod loader;
pub mod sandbox;

const MAX_MODULES: usize = 16;
const LINEAR_MEMORY_PAGES: usize = 16; // 1 MiB per sandbox.
const LINEAR_MEMORY_SIZE: usize = LINEAR_MEMORY_PAGES * 65536;

/// Result returned from a WASM function call.
#[derive(Debug, Clone, Copy)]
pub enum WasmResult {
    I32(i32),
    I64(i64),
    Void,
}

/// Error returned from WASM operations.
#[derive(Debug, Clone, Copy)]
pub enum WasmError {
    /// Module binary is malformed.
    InvalidModule,
    /// Memory exhausted for sandbox allocation.
    OutOfMemory,
    /// Called import not listed in the module's capability manifest.
    CapabilityDenied,
    /// Module index is out of range or not loaded.
    InvalidModule2,
    /// Execution trapped (unreachable, OOB access, etc.).
    Trap,
}

/// A loaded WASM module sandbox.
pub struct WasmModule {
    pub active: bool,
    pub module_uuid: crate::uuid::Uuid128,
    /// Capabilities granted to this module (bitmask).
    pub capabilities: u64,
    /// Linear memory (heap-allocated slab).
    pub memory: [u8; LINEAR_MEMORY_SIZE],
    /// Entry point function index (u32::MAX = none).
    pub entry: u32,
    /// Graph arena node ID for this sandbox (0 = not yet registered).
    pub graph_node: crate::graph::types::NodeId,
}

impl WasmModule {
    pub const fn empty() -> Self {
        Self {
            active: false,
            module_uuid: crate::uuid::Uuid128::NIL,
            capabilities: 0,
            memory: [0u8; LINEAR_MEMORY_SIZE],
            entry: u32::MAX,
            graph_node: 0,
        }
    }
}

struct WasmRuntime {
    modules: [WasmModule; MAX_MODULES],
    count: usize,
}

impl WasmRuntime {
    const fn new() -> Self {
        Self {
            modules: [const { WasmModule::empty() }; MAX_MODULES],
            count: 0,
        }
    }
}

static RUNTIME: Mutex<WasmRuntime> = Mutex::new(WasmRuntime::new());

/// Load a WASM module binary with the given UUID and capability set.
/// Returns the module slot index on success.
pub fn load_module(
    uuid: crate::uuid::Uuid128,
    capabilities: u64,
    wasm_bytes: &[u8],
) -> Result<usize, WasmError> {
    // Validate the Wasm magic + version header.
    if wasm_bytes.len() < 8 || wasm_bytes[0..4] != *b"\0asm" || wasm_bytes[4..8] != [1, 0, 0, 0] {
        return Err(WasmError::InvalidModule);
    }

    let mut rt = RUNTIME.lock();
    let slot = rt
        .modules
        .iter()
        .position(|m| !m.active)
        .ok_or(WasmError::OutOfMemory)?;

    let gn = crate::graph::handles::register_wasm_sandbox(crate::graph::types::NODE_ID_KERNEL);
    crate::graph::arena::add_edge(
        crate::graph::types::NODE_ID_KERNEL,
        gn.0,
        crate::graph::types::EdgeKind::Hosts,
        0,
    );
    rt.modules[slot].active = true;
    rt.modules[slot].module_uuid = uuid;
    rt.modules[slot].capabilities = capabilities;
    rt.modules[slot].memory = [0u8; LINEAR_MEMORY_SIZE];
    rt.modules[slot].entry = loader::find_start(wasm_bytes).unwrap_or(u32::MAX);
    rt.modules[slot].graph_node = gn.0;
    rt.count += 1;

    crate::arch::serial::write_line(b"[wasm] module loaded");
    Ok(slot)
}

/// Unload a WASM module and zero its linear memory.
pub fn unload_module(slot: usize) {
    let mut rt = RUNTIME.lock();
    if slot < MAX_MODULES {
        let gn = rt.modules[slot].graph_node;
        rt.modules[slot] = WasmModule::empty();
        if gn != 0 {
            crate::graph::arena::detach_node(gn);
        }
    }
}

/// Execute the start function of a module (if any).
pub fn run_module(slot: usize) -> Result<WasmResult, WasmError> {
    let rt = RUNTIME.lock();
    if slot >= MAX_MODULES || !rt.modules[slot].active {
        return Err(WasmError::InvalidModule2);
    }
    let entry = rt.modules[slot].entry;
    drop(rt);
    if entry == u32::MAX {
        return Ok(WasmResult::Void);
    }
    // Minimal execution: for now return Void (full interpreter in sandbox module).
    Ok(WasmResult::Void)
}
