// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use crate::bootstrap_manifest::GraphManifest;
use crate::graph::handles::GraphHandle;
use crate::graph::types::{EdgeKind, NODE_ID_KERNEL, NodeId};
use crate::uuid::{ChannelUuid, ServiceUuid, TaskUuid, Uuid128};

const MAX_REGISTRY_SERVICES: usize = 32;
const MAX_NAME_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RegistryHealth {
    Unknown = 0,
    Defined = 1,
    Reserved = 2,
    Launched = 3,
    Ready = 4,
    Degraded = 5,
    Missing = 6,
    Failed = 7,
    Stopped = 8,
}

#[derive(Clone, Copy)]
struct ServiceRecord {
    active: bool,
    name: [u8; MAX_NAME_BYTES],
    name_len: u8,
    service_uuid: ServiceUuid,
    channel_alias: u32,
    channel_uuid: ChannelUuid,
    task_uuid: TaskUuid,
    health: RegistryHealth,
    /// Graph arena node ID for this service (0 = not yet registered).
    graph_node: NodeId,
}

impl ServiceRecord {
    const EMPTY: Self = Self {
        active: false,
        name: [0; MAX_NAME_BYTES],
        name_len: 0,
        service_uuid: ServiceUuid(Uuid128::NIL),
        channel_alias: 0,
        channel_uuid: ChannelUuid(Uuid128::NIL),
        task_uuid: TaskUuid(Uuid128::NIL),
        health: RegistryHealth::Unknown,
        graph_node: 0,
    };

    fn name(&self) -> &[u8] {
        &self.name[..self.name_len as usize]
    }
}

struct Registry {
    records: [ServiceRecord; MAX_REGISTRY_SERVICES],
}

impl Registry {
    const fn new() -> Self {
        Self {
            records: [ServiceRecord::EMPTY; MAX_REGISTRY_SERVICES],
        }
    }

    fn find_index_by_name(&self, name: &[u8]) -> Option<usize> {
        self.records
            .iter()
            .position(|record| record.active && record.name() == name)
    }

    fn first_free_index(&self) -> Option<usize> {
        self.records.iter().position(|record| !record.active)
    }
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());
static REGISTRY_GENERATION: AtomicU64 = AtomicU64::new(1);

// ── subscriber notification table ────────────────────────────────────────────

const MAX_SUBSCRIBERS: usize = 16;
static SUBSCRIBERS: Mutex<[u32; MAX_SUBSCRIBERS]> = Mutex::new([0u32; MAX_SUBSCRIBERS]);

/// Register a channel alias to receive `MsgTag::RegistryChanged` on
/// every registry mutation.  Duplicate registrations are silently ignored.
/// Returns `true` if the channel was added (or already present).
pub fn register_subscriber(channel_alias: u32) -> bool {
    if channel_alias == 0 {
        return false;
    }
    let mut subs = SUBSCRIBERS.lock();
    if subs.contains(&channel_alias) {
        return true;
    }
    for slot in subs.iter_mut() {
        if *slot == 0 {
            *slot = channel_alias;
            return true;
        }
    }
    false // table full
}

fn notify_subscribers() {
    let generation = REGISTRY_GENERATION.load(Ordering::Relaxed);
    let payload = generation.to_le_bytes();
    let subs = SUBSCRIBERS.lock();
    for &ch in subs.iter() {
        if ch != 0 {
            let ch_uuid = crate::ipc::channel::uuid_for_alias(ch);
            crate::ipc::channel_send_tagged(ch_uuid, crate::ipc::MsgTag::RegistryChanged, &payload);
        }
    }
}

#[derive(Clone, Copy)]
pub struct RegistryLookup {
    pub service_uuid: ServiceUuid,
    pub channel_alias: u32,
    pub channel_uuid: ChannelUuid,
    pub task_uuid: TaskUuid,
    pub health: RegistryHealth,
}

fn bump_generation() {
    REGISTRY_GENERATION.fetch_add(1, Ordering::Relaxed);
    notify_subscribers();
}

fn upsert_internal(
    registry: &mut Registry,
    name: &[u8],
    service_uuid: ServiceUuid,
    channel_alias: u32,
    task_uuid: TaskUuid,
    health: RegistryHealth,
) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }

    let channel_uuid = crate::ipc::channel::uuid_for_alias(channel_alias);

    if let Some(index) = registry.find_index_by_name(name) {
        let record = &mut registry.records[index];
        record.service_uuid = service_uuid;
        record.channel_alias = channel_alias;
        record.channel_uuid = channel_uuid;
        record.task_uuid = task_uuid;
        record.health = health;
        return true;
    }

    let Some(index) = registry.first_free_index() else {
        return false;
    };

    let mut stored_name = [0u8; MAX_NAME_BYTES];
    stored_name[..name.len()].copy_from_slice(name);

    let gn = crate::graph::handles::register_service(NODE_ID_KERNEL, 0);
    if gn.is_valid() {
        crate::graph::arena::add_edge(NODE_ID_KERNEL, gn.node_id(), EdgeKind::Owns, 0);
    }
    registry.records[index] = ServiceRecord {
        active: true,
        name: stored_name,
        name_len: name.len() as u8,
        service_uuid,
        channel_alias,
        channel_uuid,
        task_uuid,
        health,
        graph_node: gn.node_id(),
    };

    true
}

pub fn generation() -> u64 {
    REGISTRY_GENERATION.load(Ordering::Relaxed)
}

pub fn sync_from_manifest(manifest: &GraphManifest) {
    let mut changed = false;
    {
        let mut registry = REGISTRY.lock();
        for service in &manifest.services {
            changed |= upsert_internal(
                &mut registry,
                &service.name,
                service.stable_uuid,
                crate::ipc::channel::alias_for_uuid(crate::uuid::ChannelUuid::from_service_name(
                    &service.name,
                ))
                .unwrap_or(0),
                TaskUuid(Uuid128::NIL),
                RegistryHealth::Defined,
            );
        }
    }
    if changed {
        bump_generation();
    }
}

pub fn register_dynamic(name: &[u8], channel_alias: u32, task_uuid: TaskUuid) -> bool {
    let ok = {
        let mut registry = REGISTRY.lock();
        upsert_internal(
            &mut registry,
            name,
            ServiceUuid::from_service_name(name),
            channel_alias,
            task_uuid,
            RegistryHealth::Launched,
        )
    };
    if ok {
        bump_generation();
    }
    ok
}

pub fn mark_service_launched(name: &[u8], task_uuid: TaskUuid) {
    set_service_state(name, Some(task_uuid), RegistryHealth::Launched);
}

pub fn mark_service_reserved(name: &[u8]) {
    set_service_state(name, None, RegistryHealth::Reserved);
}

pub fn mark_service_ready(name: &[u8]) {
    set_service_state(name, None, RegistryHealth::Ready);
}

pub fn mark_service_degraded(name: &[u8]) {
    set_service_state(name, None, RegistryHealth::Degraded);
}

pub fn mark_service_missing(name: &[u8]) {
    set_service_state(name, None, RegistryHealth::Missing);
}

pub fn mark_service_failed(name: &[u8]) {
    set_service_state(name, None, RegistryHealth::Failed);
}

pub fn mark_service_stopped(name: &[u8]) {
    set_service_state(name, None, RegistryHealth::Stopped);
}

fn set_service_state(name: &[u8], task_uuid: Option<TaskUuid>, health: RegistryHealth) {
    let changed = {
        let mut registry = REGISTRY.lock();
        let index = if let Some(index) = registry.find_index_by_name(name) {
            index
        } else {
            let expected_uuid = crate::uuid::ChannelUuid::from_service_name(name);
            let alias = crate::ipc::channel::alias_for_uuid(expected_uuid).unwrap_or(0);
            if !upsert_internal(
                &mut registry,
                name,
                ServiceUuid::from_service_name(name),
                alias,
                TaskUuid(Uuid128::NIL),
                health,
            ) {
                return;
            }
            // Re-resolve index after insertion.
            if let Some(inserted) = registry.find_index_by_name(name) {
                inserted
            } else {
                return;
            }
        };
        let record = &mut registry.records[index];
        if record.channel_alias == 0 {
            let expected_uuid = crate::uuid::ChannelUuid::from_service_name(name);
            if let Some(alias) = crate::ipc::channel::alias_for_uuid(expected_uuid) {
                record.channel_alias = alias;
                record.channel_uuid = expected_uuid;
            }
        }
        record.health = health;
        if let Some(task_uuid) = task_uuid {
            record.task_uuid = task_uuid;
        }
        true
    };
    if changed {
        bump_generation();
    }
}

pub fn lookup(name: &[u8]) -> Option<RegistryLookup> {
    let registry = REGISTRY.lock();
    let index = registry.find_index_by_name(name)?;
    let record = registry.records[index];
    Some(RegistryLookup {
        service_uuid: record.service_uuid,
        channel_alias: record.channel_alias,
        channel_uuid: crate::ipc::channel::uuid_for_alias(record.channel_alias),
        task_uuid: record.task_uuid,
        health: record.health,
    })
}

pub fn channel_alias_by_name(name: &[u8]) -> Option<u32> {
    let record = lookup(name)?;
    (record.channel_alias != 0).then_some(record.channel_alias)
}
