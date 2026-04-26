// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Bootstrap service graph registry.
//!
//! This layer sits above the raw graph arena and gives the protected bootstrap
//! fabric stable service identities, visible health state, dependency edges,
//! and a live execution-graph view built from observed bootstrap IPC.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::arch::serial;
use crate::bootstrap_manifest::{GraphManifest, ServiceLauncher};
use crate::graph::arena;
use crate::graph::types::{
    EdgeId, EdgeKind, NODE_FLAG_PINNED, NODE_FLAG_TRUSTED, NODE_ID_KERNEL, NodeId, NodeKind,
};
use crate::task::tcb::TaskId;
use crate::uuid::{ChannelUuid, ServiceUuid, TaskUuid, Uuid128};

#[inline(always)]
fn without_interrupts<F: FnOnce() -> R, R>(f: F) -> R {
    crate::arch::interrupts::without_interrupts(f)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceHealth {
    Defined,
    Reserved,
    Launched,
    Ready,
    Degraded,
    Missing,
    Failed,
    Stopped,
}

impl ServiceHealth {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Defined => b"defined",
            Self::Reserved => b"reserved",
            Self::Launched => b"launched",
            Self::Ready => b"ready",
            Self::Degraded => b"degraded",
            Self::Missing => b"missing",
            Self::Failed => b"failed",
            Self::Stopped => b"stopped",
        }
    }
}

#[derive(Clone)]
struct BootstrapService {
    stable_id: u16,
    stable_uuid: ServiceUuid,
    node_id: NodeId,
    current_task_id: TaskId,
    current_task_uuid: TaskUuid,
    current_task_node: Option<NodeId>,
    channel_uuid: ChannelUuid,
    critical: bool,
    launcher: ServiceLauncher,
    health: ServiceHealth,
    name: Vec<u8>,
    path: Vec<u8>,
}

#[derive(Clone, Copy)]
struct DependencyEdge {
    from_id: u16,
    from_uuid: ServiceUuid,
    to_id: u16,
    to_uuid: ServiceUuid,
    edge_id: EdgeId,
}

#[derive(Clone, Copy)]
struct IpcEdge {
    from_id: u16,
    from_uuid: ServiceUuid,
    to_id: u16,
    to_uuid: ServiceUuid,
    channel_uuid: ChannelUuid,
    edge_id: EdgeId,
}

#[derive(Default)]
struct BootstrapGraphState {
    manifest: Option<GraphManifest>,
    services: Vec<BootstrapService>,
    dependencies: Vec<DependencyEdge>,
    ipc_edges: Vec<IpcEdge>,
}

#[derive(Clone)]
pub struct BootstrapServiceView {
    pub stable_id: u16,
    pub stable_uuid: ServiceUuid,
    pub node_id: NodeId,
    pub task_id: TaskId,
    pub task_uuid: TaskUuid,
    pub channel_uuid: ChannelUuid,
    pub critical: bool,
    pub launcher: ServiceLauncher,
    pub health: ServiceHealth,
    pub name: [u8; 32],
    pub name_len: usize,
}

#[derive(Clone, Copy)]
pub struct BootstrapEdgeView {
    pub from_id: u16,
    pub from_uuid: ServiceUuid,
    pub to_id: u16,
    pub to_uuid: ServiceUuid,
    pub channel_uuid: ChannelUuid,
}

#[derive(Clone)]
pub struct BootstrapGraphSnapshot {
    pub services: Vec<BootstrapServiceView>,
    pub dependencies: Vec<BootstrapEdgeView>,
    pub ipc_edges: Vec<BootstrapEdgeView>,
}

static BOOTSTRAP_GRAPH: Mutex<Option<BootstrapGraphState>> = Mutex::new(None);
static IPC_OBSERVATION_ENABLED: AtomicBool = AtomicBool::new(false);

fn capture_snapshot(state: &BootstrapGraphState) -> BootstrapGraphSnapshot {
    BootstrapGraphSnapshot {
        services: state
            .services
            .iter()
            .map(|service| {
                let mut name = [0u8; 32];
                let name_len = service.name.len().min(name.len());
                name[..name_len].copy_from_slice(&service.name[..name_len]);
                BootstrapServiceView {
                    stable_id: service.stable_id,
                    stable_uuid: service.stable_uuid,
                    node_id: service.node_id,
                    task_id: service.current_task_id,
                    task_uuid: service.current_task_uuid,
                    channel_uuid: service.channel_uuid,
                    critical: service.critical,
                    launcher: service.launcher,
                    health: service.health,
                    name,
                    name_len,
                }
            })
            .collect(),
        dependencies: state
            .dependencies
            .iter()
            .map(|edge| BootstrapEdgeView {
                from_id: edge.from_id,
                from_uuid: edge.from_uuid,
                to_id: edge.to_id,
                to_uuid: edge.to_uuid,
                channel_uuid: ChannelUuid(Uuid128::NIL),
            })
            .collect(),
        ipc_edges: state
            .ipc_edges
            .iter()
            .map(|edge| BootstrapEdgeView {
                from_id: edge.from_id,
                from_uuid: edge.from_uuid,
                to_id: edge.to_id,
                to_uuid: edge.to_uuid,
                channel_uuid: edge.channel_uuid,
            })
            .collect(),
    }
}

fn publish_snapshot(snapshot: BootstrapGraphSnapshot) {
    let _ = snapshot;
}

pub fn sync_manifest(manifest: &GraphManifest) {
    let snapshot = without_interrupts(|| {
        let mut guard = BOOTSTRAP_GRAPH.lock();
        let state = guard.get_or_insert_with(BootstrapGraphState::default);
        state.manifest = Some(manifest.clone());

        for service in &manifest.services {
            if let Some(existing) = state
                .services
                .iter_mut()
                .find(|entry| entry.stable_id == service.stable_id)
            {
                existing.stable_uuid = service.stable_uuid;
                existing.channel_uuid = ChannelUuid::from_service_name(&service.name);
                existing.critical = service.critical;
                existing.launcher = service.launcher;
                existing.name = service.name.clone();
                existing.path = service.path.clone();
                if crate::ipc::channel::is_active(existing.channel_uuid)
                    && matches!(existing.health, ServiceHealth::Defined)
                {
                    existing.health = ServiceHealth::Reserved;
                }
            } else if let Some(node_id) = arena::add_node(
                NodeKind::Service,
                NODE_FLAG_TRUSTED | NODE_FLAG_PINNED,
                NODE_ID_KERNEL,
            ) {
                let _ = arena::add_edge(NODE_ID_KERNEL, node_id, EdgeKind::Created, 0);
                state.services.push(BootstrapService {
                    stable_id: service.stable_id,
                    stable_uuid: service.stable_uuid,
                    node_id,
                    current_task_id: 0,
                    current_task_uuid: TaskUuid(Uuid128::NIL),
                    current_task_node: None,
                    channel_uuid: ChannelUuid::from_service_name(&service.name),
                    critical: service.critical,
                    launcher: service.launcher,
                    health: if crate::ipc::channel::is_active(ChannelUuid::from_service_name(
                        &service.name,
                    )) {
                        ServiceHealth::Reserved
                    } else {
                        ServiceHealth::Defined
                    },
                    name: service.name.clone(),
                    path: service.path.clone(),
                });
            }
        }

        for dep in &manifest.dependencies {
            if state
                .dependencies
                .iter()
                .any(|edge| edge.from_id == dep.from_id && edge.to_id == dep.to_id)
            {
                continue;
            }

            let Some(from) = state
                .services
                .iter()
                .find(|service| service.stable_id == dep.from_id)
                .map(|service| service.node_id)
            else {
                continue;
            };
            let Some(to) = state
                .services
                .iter()
                .find(|service| service.stable_id == dep.to_id)
                .map(|service| service.node_id)
            else {
                continue;
            };
            if let Some(edge_id) = arena::add_edge(from, to, EdgeKind::DependsOn, 0) {
                state.dependencies.push(DependencyEdge {
                    from_id: dep.from_id,
                    from_uuid: dep.from_uuid,
                    to_id: dep.to_id,
                    to_uuid: dep.to_uuid,
                    edge_id,
                });
            }
        }

        capture_snapshot(state)
    });
    publish_snapshot(snapshot);
}

pub fn service_binding(name: &[u8]) -> Option<(u16, NodeId)> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    let service = state.services.iter().find(|service| service.name == name)?;
    Some((service.stable_id, service.node_id))
}

pub fn service_binding_with_uuid(name: &[u8]) -> Option<(u16, ServiceUuid, NodeId)> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    let service = state.services.iter().find(|service| service.name == name)?;
    Some((service.stable_id, service.stable_uuid, service.node_id))
}

pub fn service_critical(name: &[u8]) -> Option<bool> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    state
        .services
        .iter()
        .find(|service| service.name == name)
        .map(|service| service.critical)
}

pub fn service_health(name: &[u8]) -> Option<ServiceHealth> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    state
        .services
        .iter()
        .find(|service| service.name == name)
        .map(|service| service.health)
}

pub fn service_node_id(name: &[u8]) -> Option<NodeId> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    state
        .services
        .iter()
        .find(|service| service.name == name)
        .map(|service| service.node_id)
}

pub fn service_node_by_channel(channel: u32) -> Option<NodeId> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    state
        .services
        .iter()
        .find(|service| crate::ipc::channel::alias_for_uuid(service.channel_uuid) == Some(channel))
        .map(|service| service.node_id)
}

pub fn service_node_by_channel_uuid(uuid: ChannelUuid) -> Option<NodeId> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    state
        .services
        .iter()
        .find(|service| service.channel_uuid == uuid)
        .map(|service| service.node_id)
}

pub fn service_node_by_task_id(task_id: TaskId) -> Option<NodeId> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let state = guard.as_ref()?;
    state
        .services
        .iter()
        .find(|service| service.current_task_id == task_id)
        .map(|service| service.node_id)
}

pub fn service_count() -> usize {
    let guard = BOOTSTRAP_GRAPH.lock();
    guard.as_ref().map_or(0, |state| state.services.len())
}

pub fn manifest() -> Option<GraphManifest> {
    let guard = BOOTSTRAP_GRAPH.lock();
    guard.as_ref()?.manifest.clone()
}

pub fn manifest_snapshot() -> Vec<u8> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let Some(state) = guard.as_ref() else {
        return b"graph-manifest-v1\n".to_vec();
    };
    let Some(manifest) = state.manifest.as_ref() else {
        return b"graph-manifest-v1\n".to_vec();
    };

    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(b"graph-manifest-v1\n");
    for service in &manifest.services {
        out.extend_from_slice(b"service ");
        out.extend_from_slice(&service.name);
        out.push(b' ');
        out.extend_from_slice(if service.critical {
            b"critical"
        } else {
            b"optional"
        });
        out.push(b' ');
        out.extend_from_slice(service.launcher.as_bytes());
        out.push(b' ');
        out.extend_from_slice(&service.path);
        out.push(b'\n');
    }
    for dep in &manifest.dependencies {
        let Some(from) = manifest.find_service_by_id(dep.from_id) else {
            continue;
        };
        let Some(to) = manifest.find_service_by_id(dep.to_id) else {
            continue;
        };
        out.extend_from_slice(b"depends ");
        out.extend_from_slice(&from.name);
        out.push(b' ');
        out.extend_from_slice(&to.name);
        out.push(b'\n');
    }
    out
}

pub fn mark_channel_reserved(uuid: ChannelUuid) {
    set_health_by_channel_uuid(uuid, ServiceHealth::Reserved);
}

pub fn mark_service_launched(name: &[u8], task_id: TaskId) -> Option<(u16, NodeId)> {
    let (binding, snapshot) =
        without_interrupts(|| -> Option<((u16, NodeId), BootstrapGraphSnapshot)> {
            let mut guard = BOOTSTRAP_GRAPH.lock();
            let state = guard.as_mut()?;
            let service = state
                .services
                .iter_mut()
                .find(|service| service.name == name)?;
            service.current_task_id = task_id;
            service.current_task_uuid = TaskUuid::from_task_id(task_id);
            service.health = ServiceHealth::Launched;
            let task_node = arena::add_node(NodeKind::Task, 0, service.node_id)?;
            let _ = arena::add_edge(service.node_id, task_node, EdgeKind::Created, 0);
            service.current_task_node = Some(task_node);
            let binding = (service.stable_id, service.node_id);
            Some((binding, capture_snapshot(state)))
        })?;
    publish_snapshot(snapshot);
    Some(binding)
}

pub fn mark_service_ready(name: &[u8]) {
    set_health_by_name(name, ServiceHealth::Ready);
}

pub fn mark_service_degraded(name: &[u8]) {
    set_health_by_name(name, ServiceHealth::Degraded);
}

pub fn mark_service_missing(name: &[u8]) {
    set_health_by_name(name, ServiceHealth::Missing);
}

pub fn mark_service_failed(name: &[u8]) {
    set_health_by_name(name, ServiceHealth::Failed);
}

pub fn mark_service_stopped(name: &[u8]) {
    set_health_by_name(name, ServiceHealth::Stopped);
}

pub fn observe_ipc_send(task_id: TaskId, uuid: ChannelUuid) {
    if task_id == 0 || !IPC_OBSERVATION_ENABLED.load(Ordering::Acquire) {
        return;
    }

    let snapshot =
        without_interrupts(|| -> Option<BootstrapGraphSnapshot> {
            let mut guard = BOOTSTRAP_GRAPH.lock();
            let state = guard.as_mut()?;

            let from = state
                .services
                .iter()
                .find(|service| service.current_task_id == task_id)
                .map(|service| (service.stable_id, service.stable_uuid, service.node_id))?;
            let to = state
                .services
                .iter()
                .find(|service| service.channel_uuid == uuid)
                .map(|service| (service.stable_id, service.stable_uuid, service.node_id))?;
            // legacy alias (may be 0 if channel was never assigned a slot)
            let _channel_alias = crate::ipc::channel::alias_for_uuid(uuid).unwrap_or(0);

            if state.ipc_edges.iter().any(|edge| {
                edge.from_id == from.0 && edge.to_id == to.0 && edge.channel_uuid == uuid
            }) {
                return None;
            }

            if let Some(edge_id) = arena::add_edge(from.2, to.2, EdgeKind::CommunicatesWith, 0) {
                state.ipc_edges.push(IpcEdge {
                    from_id: from.0,
                    from_uuid: from.1,
                    to_id: to.0,
                    to_uuid: to.1,
                    channel_uuid: uuid,
                    edge_id,
                });
                Some(capture_snapshot(state))
            } else {
                None
            }
        });
    if let Some(snapshot) = snapshot {
        publish_snapshot(snapshot);
    }
}

pub fn dump() {
    let guard = BOOTSTRAP_GRAPH.lock();
    let Some(state) = guard.as_ref() else {
        serial::write_line(b"[bootgraph] bootstrap graph not initialised");
        return;
    };

    serial::write_line(b"[bootgraph] === Bootstrap Service Graph ===");
    for service in &state.services {
        serial::write_bytes(b"[bootgraph] S sid=");
        serial::write_u64_dec_inline(service.stable_id as u64);
        serial::write_bytes(b" node=");
        serial::write_u64_dec_inline(service.node_id);
        serial::write_bytes(b" task=");
        serial::write_u64_dec_inline(service.current_task_id);
        serial::write_bytes(b" ch=");
        serial::write_u64_dec_inline(
            crate::ipc::channel::alias_for_uuid(service.channel_uuid).unwrap_or(0) as u64,
        );
        serial::write_bytes(b" launcher=");
        serial::write_bytes(service.launcher.as_bytes());
        serial::write_bytes(b" critical=");
        serial::write_bytes(if service.critical { b"yes" } else { b"no" });
        serial::write_bytes(b" health=");
        serial::write_bytes(service.health.as_bytes());
        serial::write_bytes(b" name=");
        serial::write_line(&service.name);
    }

    for edge in &state.dependencies {
        serial::write_bytes(b"[bootgraph] D ");
        serial::write_u64_dec_inline(edge.from_id as u64);
        serial::write_bytes(b" -> ");
        serial::write_u64_dec_inline(edge.to_id as u64);
        serial::write_bytes(b" edge=");
        serial::write_u64_dec(edge.edge_id);
    }

    for edge in &state.ipc_edges {
        serial::write_bytes(b"[bootgraph] I ");
        serial::write_u64_dec_inline(edge.from_id as u64);
        serial::write_bytes(b" -> ");
        serial::write_u64_dec_inline(edge.to_id as u64);
        serial::write_bytes(b" ch=");
        serial::write_u64_dec_inline(
            crate::ipc::channel::alias_for_uuid(edge.channel_uuid).unwrap_or(0) as u64,
        );
        serial::write_bytes(b" edge=");
        serial::write_u64_dec(edge.edge_id);
    }
    serial::write_line(b"[bootgraph] === End Bootstrap Service Graph ===");
}

pub fn snapshot() -> Vec<u8> {
    let guard = BOOTSTRAP_GRAPH.lock();
    let Some(state) = guard.as_ref() else {
        return b"bootstrap-graph=unavailable\n".to_vec();
    };

    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(b"graph-manifest-v1\n");
    for service in &state.services {
        out.extend_from_slice(b"service sid=");
        append_u64(&mut out, service.stable_id as u64);
        out.extend_from_slice(b" node=");
        append_u64(&mut out, service.node_id);
        out.extend_from_slice(b" task=");
        append_u64(&mut out, service.current_task_id);
        out.extend_from_slice(b" ch=");
        append_u64(
            &mut out,
            crate::ipc::channel::alias_for_uuid(service.channel_uuid).unwrap_or(0) as u64,
        );
        out.extend_from_slice(b" critical=");
        out.extend_from_slice(if service.critical { b"yes" } else { b"no" });
        out.extend_from_slice(b" health=");
        out.extend_from_slice(service.health.as_bytes());
        out.extend_from_slice(b" name=");
        out.extend_from_slice(&service.name);
        out.push(b'\n');
    }
    for edge in &state.dependencies {
        out.extend_from_slice(b"depends ");
        append_u64(&mut out, edge.from_id as u64);
        out.push(b' ');
        append_u64(&mut out, edge.to_id as u64);
        out.push(b'\n');
    }
    for edge in &state.ipc_edges {
        out.extend_from_slice(b"ipc ");
        append_u64(&mut out, edge.from_id as u64);
        out.push(b' ');
        append_u64(&mut out, edge.to_id as u64);
        out.extend_from_slice(b" ch=");
        append_u64(
            &mut out,
            crate::ipc::channel::alias_for_uuid(edge.channel_uuid).unwrap_or(0) as u64,
        );
        out.push(b'\n');
    }
    out
}

fn set_health_by_channel_uuid(uuid: ChannelUuid, health: ServiceHealth) {
    let snapshot = without_interrupts(|| -> Option<BootstrapGraphSnapshot> {
        let mut guard = BOOTSTRAP_GRAPH.lock();
        let state = guard.as_mut()?;
        if let Some(service) = state
            .services
            .iter_mut()
            .find(|service| service.channel_uuid == uuid)
        {
            service.health = health;
        }
        Some(capture_snapshot(state))
    });
    if let Some(snapshot) = snapshot {
        publish_snapshot(snapshot);
    }
}

fn set_health_by_channel(channel: u32, health: ServiceHealth) {
    let snapshot = without_interrupts(|| -> Option<BootstrapGraphSnapshot> {
        let mut guard = BOOTSTRAP_GRAPH.lock();
        let state = guard.as_mut()?;
        if let Some(service) = state.services.iter_mut().find(|service| {
            crate::ipc::channel::alias_for_uuid(service.channel_uuid) == Some(channel)
        }) {
            service.health = health;
        }
        Some(capture_snapshot(state))
    });
    if let Some(snapshot) = snapshot {
        publish_snapshot(snapshot);
    }
}

fn set_health_by_name(name: &[u8], health: ServiceHealth) {
    let snapshot = without_interrupts(|| -> Option<BootstrapGraphSnapshot> {
        let mut guard = BOOTSTRAP_GRAPH.lock();
        let state = guard.as_mut()?;
        if let Some(service) = state
            .services
            .iter_mut()
            .find(|service| service.name == name)
        {
            service.health = health;
        }
        Some(capture_snapshot(state))
    });
    if let Some(snapshot) = snapshot {
        publish_snapshot(snapshot);
    }
}

pub fn set_ipc_observation_enabled(enabled: bool) {
    IPC_OBSERVATION_ENABLED.store(enabled, Ordering::Release);
}

fn append_u64(out: &mut Vec<u8>, mut value: u64) {
    if value == 0 {
        out.push(b'0');
        return;
    }

    let mut digits = [0u8; 20];
    let mut len = 0usize;
    while value > 0 {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        out.push(digits[len]);
    }
}
