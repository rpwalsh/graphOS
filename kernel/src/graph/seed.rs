// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Boot-time graph seeding — populate the kernel graph from hardware state.
//!
//! This module is called once during early boot, after the graph arena and
//! frame allocator are online. It reads BootInfo and kernel state to create
//! graph nodes and edges that represent the real system.
//!
//! ## Why this matters
//! The graph is not decorative metadata. After seeding, the system can
//! answer questions like:
//! - "What physical memory regions exist and who owns them?"
//! - "Which tasks are running and what resources do they hold?"
//! - "What is the provenance chain from the active display surface to the kernel?"
//!
//! Every node created here has `creator = NODE_ID_KERNEL` and a timestamp
//! from the arena's generation counter.
//!
//! ## Current scope
//! - Kernel root node (created in arena::init, not here)
//! - Display surface node + kernel→display Owns edge
//! - Reserved range nodes + kernel→range Contains edges
//! - CPU core node + kernel→cpu Owns edge
//! - Init task node + kernel→task Created edge (after task creation)
//!
//! Future passes will add: memory region nodes, device nodes, interrupt
//! vector nodes, address space nodes.

use crate::arch::serial;
use crate::bootinfo::BootInfo;
use crate::graph::arena;
use crate::graph::types::*;

/// Seed the graph with hardware state discovered during boot.
///
/// Call this after `arena::init()` and after reserved ranges have been
/// registered. The kernel root node (id=1) must already exist.
pub fn seed_from_boot(boot_info: &BootInfo) {
    serial::write_line(b"[graph] === Seeding graph from boot state ===");

    let mut node_count = 0u32;
    let mut edge_count = 0u32;

    // ---- CPU core (BSP) ----
    // We know there is exactly one bootstrap processor at this point.
    // AP enumeration will add more cores later via ACPI/MADT parsing.
    if let Some(cpu_id) = arena::add_node(NodeKind::CpuCore, 0, NODE_ID_KERNEL) {
        arena::add_edge(NODE_ID_KERNEL, cpu_id, EdgeKind::Owns, 0);
        node_count += 1;
        edge_count += 1;

        serial::write_bytes(b"[graph] CPU core (BSP) node=");
        serial::write_u64_dec(cpu_id);
    }

    // ---- Display surface ----
    if boot_info.framebuffer_addr != 0
        && let Some(display_id) = arena::add_node(NodeKind::DisplaySurface, 0, NODE_ID_KERNEL)
    {
        arena::add_edge(NODE_ID_KERNEL, display_id, EdgeKind::Owns, 0);
        node_count += 1;
        edge_count += 1;

        serial::write_bytes(b"[graph] Display surface node=");
        serial::write_u64_dec_inline(display_id);
        serial::write_bytes(b" addr=");
        serial::write_hex(boot_info.framebuffer_addr);
    }

    // ---- Reserved physical ranges ----
    // Each reserved range registered in mm::reserved becomes a
    // ReservedRange node owned by the kernel. This makes the memory
    // map graph-queryable.
    let reserved_count = crate::mm::reserved::count();
    for i in 0..reserved_count {
        if let Some((start, end)) = crate::mm::reserved::get(i)
            && let Some(rr_id) = arena::add_node(NodeKind::ReservedRange, 0, NODE_ID_KERNEL)
        {
            arena::add_edge(NODE_ID_KERNEL, rr_id, EdgeKind::Contains, 0);
            node_count += 1;
            edge_count += 1;

            // The range addresses are not stored in the graph node itself —
            // they live in mm::reserved. The graph node is the identity;
            // mm::reserved is the detail store. This separation is deliberate:
            // the graph stores relationships and provenance, not bulk data.
            let _ = (start, end); // Acknowledge; used only for the serial log.
        }
    }
    serial::write_bytes(b"[graph] Reserved range nodes: ");
    serial::write_u64_dec(reserved_count as u64);

    // ---- Summary ----
    serial::write_bytes(b"[graph] seed complete: +");
    serial::write_u64_dec_inline(node_count as u64);
    serial::write_bytes(b" nodes, +");
    serial::write_u64_dec_inline(edge_count as u64);
    serial::write_line(b" edges");

    serial::write_line(b"[graph] === End graph seed ===");
}

/// Register a newly created task in the graph.
///
/// Called from the boot sequence after `task::table::create_kernel_task()`
/// succeeds. Creates a Task node and a kernel→task Created edge.
///
/// Returns the `NodeId` assigned to the task, or `None` if the arena is full.
pub fn register_task(task_name: &[u8], creator: NodeId) -> Option<NodeId> {
    let task_node = arena::add_node(NodeKind::Task, 0, creator)?;
    arena::add_edge(creator, task_node, EdgeKind::Created, 0);

    serial::write_bytes(b"[graph] task node=");
    serial::write_u64_dec_inline(task_node);
    serial::write_bytes(b" name=");
    serial::write_line(task_name);

    Some(task_node)
}
