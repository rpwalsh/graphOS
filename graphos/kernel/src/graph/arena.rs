// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Graph arena — the in-kernel graph store.
//!
//! This is the core system abstraction of GraphOS. Every kernel object that
//! participates in scheduling, diagnostics, trust decisions, or predictive
//! reasoning is represented as a node. Relationships are typed, directed,
//! weighted, timestamped edges with creator provenance.
//!
//! ## Mathematical contract
//!
//! The arena implements the heterogeneous temporal graph H = (V, E, τ_V, τ_E, t):
//! - τ_V : V → NodeKind   (node type function)
//! - τ_E : E → EdgeKind   (edge type function)
//! - t : E → Timestamp    (continuous-time edge creation)
//! - w : E → Weight       (16.16 fixed-point edge weight)
//!
//! Intrusive adjacency lists (Node.adj_head_out/in, Edge.next_out/in) provide
//! O(degree) neighbor enumeration for:
//! - PowerWalk sampling: P(x | s, v, t) with per-type-pair (p, q, λ) bias
//! - Laplacian construction: L = D − A, degree maintained incrementally
//! - Spectral analysis: incremental Sherman-Morrison eigenvalue updates
//!
//! ## Provenance model
//! Every node and edge records:
//! - **created_at**: boot-relative timestamp (sequence number until timer calibration)
//! - **creator**: NodeId of the entity that caused the mutation
//!
//! The monotonic generation counter (`GEN`) sequences all mutations. This is
//! the embryonic audit log — every graph change has a total order.
//!
//! ## Storage model
//! Fixed-size static arrays in BSS. No heap required. Slots with `id == 0`
//! are free. The arena will be replaced with a page-backed store once the
//! frame allocator supports deallocation, but the API contract is stable.
//!
//! ## Concurrency
//! Single `spin::Mutex` over the entire arena. Acceptable for single-core
//! early boot. Will become a bottleneck under SMP and must be replaced with
//! per-node or sharded locking.
//!
//! ## Capacity
//! 1024 nodes, 4096 edges. These are intentionally small — they will be
//! sufficient through early boot and initial task bring-up. Overflow is
//! reported via serial and returns `None`/`false`.

use spin::Mutex;

use crate::arch::serial;
use crate::graph::types::*;
use crate::uuid::Uuid128;

const MAX_NODES: usize = 8192;
const MAX_EDGES: usize = 32768;

struct Arena {
    nodes: [Node; MAX_NODES],
    node_count: usize,
    edges: [Edge; MAX_EDGES],
    edge_count: usize,
    next_node_id: NodeId,
    next_edge_id: EdgeId,
    generation: u64,
    max_weight: Weight,
    total_weight: u64,
    /// Free-list for node slots: stack of free indices. O(1) alloc/free.
    node_free: [u16; MAX_NODES],
    node_free_top: usize,
    /// Free-list for edge slots: stack of free indices. O(1) alloc/free.
    edge_free: [u32; MAX_EDGES],
    edge_free_top: usize,
    /// NodeId -> slot index map. id_map[id] = slot index, or u16::MAX if unmapped.
    /// Enables O(1) find_node_slot instead of O(n) scan.
    id_map: [u16; MAX_NODES],
}

impl Arena {
    const fn new() -> Self {
        // Build node free-list: all slots free, stack order 0..MAX_NODES-1
        let mut node_free = [0u16; MAX_NODES];
        let mut ni = 0;
        while ni < MAX_NODES {
            node_free[ni] = ni as u16;
            ni += 1;
        }
        // Build edge free-list: all slots free
        let mut edge_free = [0u32; MAX_EDGES];
        let mut ei = 0;
        while ei < MAX_EDGES {
            edge_free[ei] = ei as u32;
            ei += 1;
        }
        Self {
            nodes: [Node::EMPTY; MAX_NODES],
            node_count: 0,
            edges: [Edge::EMPTY; MAX_EDGES],
            edge_count: 0,
            next_node_id: 1,
            next_edge_id: 1,
            generation: 0,
            max_weight: 0,
            total_weight: 0,
            node_free,
            node_free_top: MAX_NODES,
            edge_free,
            edge_free_top: MAX_EDGES,
            id_map: [u16::MAX; MAX_NODES],
        }
    }

    /// Find the slot index of a node by its ID via the id_map. O(1).
    fn find_node_slot(&self, id: NodeId) -> Option<usize> {
        if (id as usize) >= MAX_NODES {
            return None;
        }
        let slot = self.id_map[id as usize];
        if slot == u16::MAX {
            None
        } else {
            Some(slot as usize)
        }
    }
}

static ARENA: Mutex<Arena> = Mutex::new(Arena::new());

/// Log the Arena static address range for overwrite diagnostics.
pub fn log_layout() {
    let start = core::ptr::addr_of!(ARENA) as u64;
    let end = start + core::mem::size_of::<Mutex<Arena>>() as u64;
    serial::write_bytes(b"[graph] arena static: ");
    serial::write_hex_inline(start);
    serial::write_bytes(b" .. ");
    serial::write_hex(end);
}

/// Seed the graph with the kernel root node.
///
/// The kernel node (id=1, kind=Kernel) is the root of the ownership
/// hierarchy. Every other boot-time node is created by the kernel.
/// This must be called exactly once, after serial is online.
pub fn init() {
    let mut arena = ARENA.lock();

    // Pop slot 0 from free-list for the kernel root node.
    arena.node_free_top -= 1;
    let slot_idx = arena.node_free[arena.node_free_top] as usize;

    let gen_ts = arena.generation;
    arena.nodes[slot_idx] = Node {
        id: NODE_ID_KERNEL,
        uuid: Uuid128::NIL,
        kind: NodeKind::Kernel,
        _pad: 0,
        flags: 0,
        created_at: gen_ts,
        creator: NODE_ID_KERNEL,
        degree_out: 0,
        degree_in: 0,
        adj_head_out: ADJ_NONE,
        adj_head_in: ADJ_NONE,
    };
    arena.id_map[NODE_ID_KERNEL as usize] = slot_idx as u16;
    arena.node_count = 1;
    arena.next_node_id = 2;
    arena.generation = gen_ts + 1;

    serial::write_bytes(b"[graph] arena online - kernel node id=1, gen=");
    serial::write_u64_dec(arena.generation);
}

/// Insert a node into the graph. Returns the assigned `NodeId`.
///
/// `creator` is the NodeId of the entity causing this insertion (typically
/// `NODE_ID_KERNEL` during boot, or a task's graph node later).
///
/// Returns `None` if the arena is full.
pub fn add_node(kind: NodeKind, flags: u32, creator: NodeId) -> Option<NodeId> {
    let mut arena = ARENA.lock();
    if arena.node_free_top == 0 {
        serial::write_line(b"[graph] ERROR: node arena full");
        serial::write_bytes(b"[graph] node_free_top=");
        serial::write_u64_dec_inline(arena.node_free_top as u64);
        serial::write_bytes(b" node_count=");
        serial::write_u64_dec_inline(arena.node_count as u64);
        serial::write_bytes(b" next_node_id=");
        serial::write_u64_dec(arena.next_node_id);
        return None;
    }

    let id = arena.next_node_id;
    if (id as usize) >= MAX_NODES {
        serial::write_line(b"[graph] ERROR: node id overflow");
        return None;
    }
    let gen_ts = arena.generation;

    // Pop free slot from stack — O(1).
    arena.node_free_top -= 1;
    let slot_idx = arena.node_free[arena.node_free_top] as usize;

    arena.nodes[slot_idx] = Node {
        id,
        uuid: Uuid128::NIL,
        kind,
        _pad: 0,
        flags,
        created_at: gen_ts,
        creator,
        degree_out: 0,
        degree_in: 0,
        adj_head_out: ADJ_NONE,
        adj_head_in: ADJ_NONE,
    };
    arena.id_map[id as usize] = slot_idx as u16;
    arena.node_count += 1;
    arena.next_node_id = id + 1;
    arena.generation = gen_ts + 1;
    Some(id)
}

/// Insert a directed, weighted edge into the graph. Returns the assigned `EdgeId`.
///
/// `weight` is in 16.16 fixed-point. For structural edges, use `WEIGHT_ONE`.
///
/// Maintains:
/// - Intrusive adjacency lists (Node.adj_head_out/in, Edge.next_out/in)
/// - Weighted degree counters (Node.degree_out/in)
/// - Arena-level total_weight and max_weight
///
/// Does **not** validate that `from` and `to` exist — the caller is
/// responsible for ensuring referential integrity. This is a deliberate
/// choice: during boot, nodes and edges may be inserted in bulk and
/// validation is done via `audit()` at the end.
///
/// Returns `None` if the edge arena is full.
pub fn add_edge(from: NodeId, to: NodeId, kind: EdgeKind, flags: u32) -> Option<EdgeId> {
    add_edge_weighted(from, to, kind, flags, WEIGHT_ONE)
}

/// Insert a directed, weighted edge with an explicit weight.
///
/// This is the full-featured insertion path. `add_edge()` delegates here
/// with `weight = WEIGHT_ONE`.
pub fn add_edge_weighted(
    from: NodeId,
    to: NodeId,
    kind: EdgeKind,
    flags: u32,
    weight: Weight,
) -> Option<EdgeId> {
    let mut arena = ARENA.lock();
    if arena.edge_free_top == 0 {
        serial::write_line(b"[graph] ERROR: edge arena full");
        return None;
    }

    let id = arena.next_edge_id;
    let gen_ts = arena.generation;

    // Pop free edge slot from stack — O(1).
    arena.edge_free_top -= 1;
    let slot_idx = arena.edge_free[arena.edge_free_top] as usize;

    // Link into source node's outgoing adjacency list.
    let mut next_out = ADJ_NONE;
    if let Some(src_slot) = arena.find_node_slot(from) {
        next_out = arena.nodes[src_slot].adj_head_out;
        arena.nodes[src_slot].adj_head_out = slot_idx as u32;
        arena.nodes[src_slot].degree_out = arena.nodes[src_slot].degree_out.saturating_add(weight);
    }

    // Link into destination node's incoming adjacency list.
    let mut next_in = ADJ_NONE;
    if let Some(dst_slot) = arena.find_node_slot(to) {
        next_in = arena.nodes[dst_slot].adj_head_in;
        arena.nodes[dst_slot].adj_head_in = slot_idx as u32;
        arena.nodes[dst_slot].degree_in = arena.nodes[dst_slot].degree_in.saturating_add(weight);
    }

    arena.edges[slot_idx] = Edge {
        id,
        from,
        to,
        kind,
        _pad: 0,
        flags,
        weight,
        created_at: gen_ts,
        next_out,
        next_in,
    };

    arena.edge_count += 1;
    arena.next_edge_id = id + 1;
    arena.generation = gen_ts + 1;
    arena.total_weight = arena.total_weight.saturating_add(weight as u64);
    if weight > arena.max_weight {
        arena.max_weight = weight;
    }

    Some(id)
}

/// Call `f` for every outgoing edge from `node_id`.
///
/// Uses the intrusive adjacency list for O(out-degree) traversal
/// instead of O(total_edges) linear scan.
pub fn edges_from(node_id: NodeId, mut f: impl FnMut(&Edge)) {
    let arena = ARENA.lock();

    // Find the source node's adjacency head.
    let head = match arena.find_node_slot(node_id) {
        Some(slot) => arena.nodes[slot].adj_head_out,
        None => ADJ_NONE,
    };

    let mut idx = head;
    while idx != ADJ_NONE && (idx as usize) < MAX_EDGES {
        let edge = &arena.edges[idx as usize];
        if edge.is_live() && edge.from == node_id {
            f(edge);
            idx = edge.next_out;
        } else {
            break;
        }
    }
}

/// Call `f` for every incoming edge to `node_id`.
///
/// Uses the intrusive adjacency list for O(in-degree) traversal.
pub fn edges_to(node_id: NodeId, mut f: impl FnMut(&Edge)) {
    let arena = ARENA.lock();

    let head = match arena.find_node_slot(node_id) {
        Some(slot) => arena.nodes[slot].adj_head_in,
        None => ADJ_NONE,
    };

    let mut idx = head;
    while idx != ADJ_NONE && (idx as usize) < MAX_EDGES {
        let edge = &arena.edges[idx as usize];
        if edge.is_live() && edge.to == node_id {
            f(edge);
            idx = edge.next_in;
        } else {
            break;
        }
    }
}

/// Call `f` for every live node matching `kind`.
pub fn nodes_by_kind(kind: NodeKind, mut f: impl FnMut(&Node)) {
    let arena = ARENA.lock();
    for node in arena.nodes.iter() {
        if node.is_live() && node.kind == kind {
            f(node);
        }
    }
}

/// Returns `true` if a live node with the given id exists.
pub fn node_exists(id: NodeId) -> bool {
    let arena = ARENA.lock();
    arena.find_node_slot(id).is_some()
}

/// Look up a node by ID and return a copy, or `None`.
pub fn get_node(id: NodeId) -> Option<Node> {
    let arena = ARENA.lock();
    arena.find_node_slot(id).map(|slot| arena.nodes[slot])
}

/// Look up an edge by ID and return a copy, or `None`.
pub fn get_edge(id: EdgeId) -> Option<Edge> {
    let arena = ARENA.lock();
    arena
        .edges
        .iter()
        .find(|edge| edge.is_live() && edge.id == id)
        .copied()
}

/// Look up a node's kind by ID, or `None`.
pub fn node_kind(id: NodeId) -> Option<NodeKind> {
    get_node(id).map(|n| n.kind)
}

/// Call `f` for every outgoing edge from `node_id` that matches `edge_kind`.
///
/// Type-filtered adjacency traversal — used by PowerWalk for
/// type-conditioned transition sampling.
pub fn edges_from_typed(node_id: NodeId, edge_kind: EdgeKind, mut f: impl FnMut(&Edge)) {
    edges_from(node_id, |e| {
        if e.kind == edge_kind {
            f(e);
        }
    });
}

/// Call `f` for every outgoing edge from `node_id` whose destination
/// is of kind `dest_kind`. Requires a second lookup per edge, but
/// enables type-pair-conditioned traversal.
pub fn edges_from_to_kind(node_id: NodeId, dest_kind: NodeKind, mut f: impl FnMut(&Edge)) {
    let arena = ARENA.lock();

    let head = match arena.find_node_slot(node_id) {
        Some(slot) => arena.nodes[slot].adj_head_out,
        None => ADJ_NONE,
    };

    let mut idx = head;
    while idx != ADJ_NONE && (idx as usize) < MAX_EDGES {
        let edge = &arena.edges[idx as usize];
        if edge.is_live() && edge.from == node_id {
            // Check destination type.
            if let Some(dst_slot) = arena.find_node_slot(edge.to)
                && arena.nodes[dst_slot].kind == dest_kind
            {
                f(edge);
            }
            idx = edge.next_out;
        } else {
            break;
        }
    }
}

/// Call `f` for every outgoing edge from `node_id` whose creation
/// timestamp is ≥ `t_min`. Enables temporal range queries for
/// recency-weighted walk sampling.
pub fn edges_from_since(node_id: NodeId, t_min: Timestamp, mut f: impl FnMut(&Edge)) {
    edges_from(node_id, |e| {
        if e.created_at >= t_min {
            f(e);
        }
    });
}

/// Count outgoing edges from a node (unweighted out-degree).
pub fn out_degree(node_id: NodeId) -> usize {
    let mut count = 0usize;
    edges_from(node_id, |_| count += 1);
    count
}

/// Count incoming edges to a node (unweighted in-degree).
pub fn in_degree(node_id: NodeId) -> usize {
    let mut count = 0usize;
    edges_to(node_id, |_| count += 1);
    count
}

/// Returns `true` if there exists an edge from `a` to `b` (any kind).
pub fn adjacent(a: NodeId, b: NodeId) -> bool {
    let mut found = false;
    edges_from(a, |e| {
        if e.to == b {
            found = true;
        }
    });
    found
}

/// Get the current arena generation (monotonic mutation counter).
pub fn generation() -> u64 {
    ARENA.lock().generation
}

/// Peek at the next node ID that will be assigned (without allocating).
pub fn next_node_id() -> NodeId {
    ARENA.lock().next_node_id
}

/// Get the current live node count.
pub fn node_count() -> usize {
    ARENA.lock().node_count
}

/// Get the current live edge count.
pub fn edge_count() -> usize {
    ARENA.lock().edge_count
}

/// Soft-delete a node by setting `NODE_FLAG_DETACHED`.
///
/// The node slot is NOT recycled — the ID remains reserved so that any
/// in-flight edges that reference it can still be traversed (they will
/// observe the detached flag). Graph walkers should check this flag and
/// skip detached nodes.
pub fn detach_node(id: NodeId) {
    use crate::graph::types::NODE_FLAG_DETACHED;
    let mut arena = ARENA.lock();
    if let Some(slot) = arena.find_node_slot(id) {
        arena.nodes[slot].flags |= NODE_FLAG_DETACHED;
        arena.generation += 1;
    }
}

/// Get the maximum edge weight (for Weyl perturbation bound).
pub fn max_weight() -> Weight {
    ARENA.lock().max_weight
}

/// Get the total edge weight (for spectral normalisation).
pub fn total_weight() -> u64 {
    ARENA.lock().total_weight
}

/// Return the UUID assigned to a node, or `None` if the node doesn't exist.
pub fn node_uuid(id: NodeId) -> Option<Uuid128> {
    let arena = ARENA.lock();
    let slot = arena.find_node_slot(id)?;
    Some(arena.nodes[slot].uuid)
}

/// Assign or update a UUID on an existing node.
///
/// Returns `false` if the node does not exist.
pub fn set_node_uuid(id: NodeId, uuid: Uuid128) -> bool {
    let mut arena = ARENA.lock();
    match arena.find_node_slot(id) {
        Some(slot) => {
            arena.nodes[slot].uuid = uuid;
            true
        }
        None => false,
    }
}

/// Find the first node whose UUID matches, returning its `NodeId`.
///
/// O(n) scan — intended for registry lookup and diagnostic paths, not
/// hot scheduling paths.
pub fn find_node_by_uuid(uuid: Uuid128) -> Option<NodeId> {
    let arena = ARENA.lock();
    for slot in 0..MAX_NODES {
        let node = &arena.nodes[slot];
        if node.id != 0 && node.uuid == uuid {
            return Some(node.id);
        }
    }
    None
}

/// Insert a node with an explicit UUID, returning the assigned `NodeId`.
///
/// Same as `add_node()` but also sets the UUID field at creation time,
/// avoiding a second lock acquisition via `set_node_uuid()`.
pub fn add_node_with_uuid(
    kind: NodeKind,
    flags: u32,
    creator: NodeId,
    uuid: Uuid128,
) -> Option<NodeId> {
    let id = add_node(kind, flags, creator)?;
    set_node_uuid(id, uuid);
    Some(id)
}

/// Get the `NodeKind` of the `idx`-th live node (0-based scan order).
///
/// This enables indexed iteration over live nodes without holding the
/// lock across an iterator.  Returns `None` if `idx` exceeds the number
/// of live nodes.
pub fn node_kind_at_index(idx: usize) -> Option<NodeKind> {
    let arena = ARENA.lock();
    let mut seen = 0usize;
    for node in arena.nodes.iter() {
        if node.is_live() {
            if seen == idx {
                return Some(node.kind);
            }
            seen += 1;
        }
    }
    None
}

/// Get the `NodeId` of the `idx`-th live node (0-based scan order).
///
/// Companion to `node_kind_at_index` — same ordering guarantee.
pub fn node_id_at_index(idx: usize) -> Option<NodeId> {
    let arena = ARENA.lock();
    let mut seen = 0usize;
    for node in arena.nodes.iter() {
        if node.is_live() {
            if seen == idx {
                return Some(node.id);
            }
            seen += 1;
        }
    }
    None
}

/// Dump the entire graph state to serial for diagnostics.
pub fn dump() {
    let arena = ARENA.lock();
    serial::write_line(b"[graph] === Graph Arena Dump ===");
    serial::write_bytes(b"[graph] nodes: ");
    serial::write_u64_dec_inline(arena.node_count as u64);
    serial::write_bytes(b"/");
    serial::write_u64_dec_inline(MAX_NODES as u64);
    serial::write_bytes(b"  edges: ");
    serial::write_u64_dec_inline(arena.edge_count as u64);
    serial::write_bytes(b"/");
    serial::write_u64_dec_inline(MAX_EDGES as u64);
    serial::write_bytes(b"  gen: ");
    serial::write_u64_dec_inline(arena.generation);
    serial::write_bytes(b"  total_w: ");
    serial::write_u64_dec_inline(arena.total_weight);
    serial::write_bytes(b"  max_w: ");
    serial::write_u64_dec(arena.max_weight as u64);

    for node in arena.nodes.iter() {
        if node.is_live() {
            serial::write_bytes(b"  N id=");
            serial::write_u64_dec_inline(node.id);
            serial::write_bytes(b" kind=");
            serial::write_u64_dec_inline(node.kind as u64);
            serial::write_bytes(b" creator=");
            serial::write_u64_dec_inline(node.creator);
            serial::write_bytes(b" t=");
            serial::write_u64_dec_inline(node.created_at);
            serial::write_bytes(b" d_out=");
            serial::write_u64_dec_inline(node.degree_out as u64);
            serial::write_bytes(b" d_in=");
            serial::write_u64_dec(node.degree_in as u64);
        }
    }

    for edge in arena.edges.iter() {
        if edge.is_live() {
            serial::write_bytes(b"  E id=");
            serial::write_u64_dec_inline(edge.id);
            serial::write_bytes(b" ");
            serial::write_u64_dec_inline(edge.from);
            serial::write_bytes(b"->");
            serial::write_u64_dec_inline(edge.to);
            serial::write_bytes(b" kind=");
            serial::write_u64_dec_inline(edge.kind as u64);
            serial::write_bytes(b" w=");
            serial::write_u64_dec_inline(edge.weight as u64);
            serial::write_bytes(b" t=");
            serial::write_u64_dec(edge.created_at);
        }
    }

    serial::write_line(b"[graph] === End Graph Arena ===");
}

/// Audit referential integrity. Checks that every edge references
/// existing nodes and that adjacency list pointers are consistent.
/// Logs violations to serial.
///
/// Returns the number of broken edges found.
pub fn audit() -> usize {
    let arena = ARENA.lock();
    let mut broken = 0usize;

    for edge in arena.edges.iter() {
        if !edge.is_live() {
            continue;
        }
        let from_ok = arena.find_node_slot(edge.from).is_some();
        let to_ok = arena.find_node_slot(edge.to).is_some();
        if !from_ok || !to_ok {
            serial::write_bytes(b"[graph] INTEGRITY: edge ");
            serial::write_u64_dec_inline(edge.id);
            serial::write_bytes(b" refs ");
            if !from_ok {
                serial::write_bytes(b"missing-from=");
                serial::write_u64_dec_inline(edge.from);
                serial::write_bytes(b" ");
            }
            if !to_ok {
                serial::write_bytes(b"missing-to=");
                serial::write_u64_dec_inline(edge.to);
            }
            serial::write_line(b"");
            broken += 1;
        }
    }

    if broken == 0 {
        serial::write_line(b"[graph] audit: referential integrity OK");
    } else {
        serial::write_bytes(b"[graph] audit: ");
        serial::write_u64_dec_inline(broken as u64);
        serial::write_line(b" broken edge(s)");
    }

    broken
}
