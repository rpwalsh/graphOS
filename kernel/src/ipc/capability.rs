// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::graph::arena;
use crate::graph::types::{EdgeKind, NODE_ID_KERNEL};
use crate::task::tcb::TaskId;
use crate::uuid::ChannelUuid;

pub const CAP_SEND: u8 = 1 << 0;
pub const CAP_RECV: u8 = 1 << 1;
pub const CAP_MANAGE: u8 = 1 << 2;
pub const CAP_ALL: u8 = CAP_SEND | CAP_RECV | CAP_MANAGE;

/// Maximum number of per-task IPC capability entries.
/// Must match `tcb::MAX_TASK_IPC_CAPS`.
pub const CAPABILITY_SET_CAPACITY: usize = 64;

/// Typed per-task IPC capability set.
///
/// Channel handles are `ChannelUuid` (the primary key after Session 1 migration).
/// The legacy integer alias shim lives only at the syscall ABI boundary.
#[derive(Debug)]
pub struct CapabilitySet {
    /// Channel UUIDs this task can access.
    pub channels: [ChannelUuid; CAPABILITY_SET_CAPACITY],
    /// Permission bitmask per channel entry (CAP_SEND | CAP_RECV | CAP_MANAGE).
    pub perms: [u8; CAPABILITY_SET_CAPACITY],
    /// Number of active entries.
    pub count: u8,
}

impl CapabilitySet {
    pub const fn new() -> Self {
        use crate::uuid::Uuid128;
        const NIL: ChannelUuid = ChannelUuid(Uuid128::NIL);
        Self {
            channels: [NIL; CAPABILITY_SET_CAPACITY],
            perms: [0; CAPABILITY_SET_CAPACITY],
            count: 0,
        }
    }

    /// Grant permission bits for a channel. Returns `false` if the set is full.
    pub fn grant(&mut self, channel: ChannelUuid, perms: u8) -> bool {
        use crate::uuid::Uuid128;
        if channel.into_inner() == Uuid128::NIL || perms == 0 {
            return false;
        }
        let len = self.count as usize;
        let mut i = 0usize;
        while i < len {
            if self.channels[i] == channel {
                self.perms[i] |= perms;
                return true;
            }
            i += 1;
        }
        if len >= CAPABILITY_SET_CAPACITY {
            return false;
        }
        self.channels[len] = channel;
        self.perms[len] = perms;
        self.count = self.count.saturating_add(1);
        true
    }

    /// Revoke permission bits for a channel. Returns `false` if the entry didn't exist.
    pub fn revoke(&mut self, channel: ChannelUuid, perms: u8) -> bool {
        use crate::uuid::Uuid128;
        if channel.into_inner() == Uuid128::NIL || perms == 0 {
            return false;
        }
        let len = self.count as usize;
        let mut i = 0usize;
        while i < len {
            if self.channels[i] == channel {
                let remaining = self.perms[i] & !perms;
                if remaining == self.perms[i] {
                    return false;
                }
                if remaining == 0 {
                    use crate::uuid::Uuid128;
                    let last = len - 1;
                    self.channels[i] = self.channels[last];
                    self.perms[i] = self.perms[last];
                    self.channels[last] = ChannelUuid(Uuid128::NIL);
                    self.perms[last] = 0;
                    self.count = self.count.saturating_sub(1);
                } else {
                    self.perms[i] = remaining;
                }
                return true;
            }
            i += 1;
        }
        false
    }

    /// Check whether all requested bits are present for a channel.
    pub fn has(&self, channel: ChannelUuid, perms: u8) -> bool {
        use crate::uuid::Uuid128;
        if channel.into_inner() == Uuid128::NIL || perms == 0 {
            return false;
        }
        let len = self.count as usize;
        let mut i = 0usize;
        while i < len {
            if self.channels[i] == channel {
                return (self.perms[i] & perms) == perms;
            }
            i += 1;
        }
        false
    }
}

const CAP_EDGE_FLAG_DELEGATE: u32 = 1 << 0;
const CAP_EDGE_FLAG_REVOKE: u32 = 1 << 1;

#[inline]
pub fn can_send(task_index: usize, channel: ChannelUuid) -> bool {
    crate::task::table::ipc_cap_has(task_index, channel, CAP_SEND)
}

#[inline]
pub fn can_recv(task_index: usize, channel: ChannelUuid) -> bool {
    crate::task::table::ipc_cap_has(task_index, channel, CAP_RECV)
}

#[inline]
pub fn can_manage(task_index: usize, channel: ChannelUuid) -> bool {
    crate::task::table::ipc_cap_has(task_index, channel, CAP_MANAGE)
}

pub fn delegate(
    granter_index: usize,
    target_task_id: TaskId,
    channel: ChannelUuid,
    perms: u8,
) -> bool {
    use crate::uuid::Uuid128;
    if channel.into_inner() == Uuid128::NIL {
        return false;
    }
    let perms = perms & CAP_ALL;
    if perms == 0 {
        return false;
    }

    let Some(target_index) = crate::task::table::task_index_by_id(target_task_id) else {
        return false;
    };

    if granter_index != 0 && !can_manage(granter_index, channel) {
        return false;
    }

    if granter_index != 0 && !crate::task::table::ipc_cap_has(granter_index, channel, perms) {
        return false;
    }

    if !crate::task::table::ipc_cap_grant(target_index, channel, perms) {
        return false;
    }

    let granter_task_id = crate::task::table::task_id_at(granter_index);
    emit_capability_edge(
        granter_task_id,
        target_task_id,
        channel,
        CAP_EDGE_FLAG_DELEGATE,
    );
    true
}

pub fn revoke(
    granter_index: usize,
    target_task_id: TaskId,
    channel: ChannelUuid,
    perms: u8,
) -> bool {
    use crate::uuid::Uuid128;
    if channel.into_inner() == Uuid128::NIL {
        return false;
    }
    let perms = perms & CAP_ALL;
    if perms == 0 {
        return false;
    }

    let Some(target_index) = crate::task::table::task_index_by_id(target_task_id) else {
        return false;
    };

    if granter_index != 0 && !can_manage(granter_index, channel) {
        return false;
    }

    if !crate::task::table::ipc_cap_revoke(target_index, channel, perms) {
        return false;
    }

    // Cascade revocation to all descendant holders.
    let _ = crate::task::table::ipc_cap_revoke_all_except(channel, perms, granter_index);

    let granter_task_id = crate::task::table::task_id_at(granter_index);
    emit_capability_edge(
        granter_task_id,
        target_task_id,
        channel,
        CAP_EDGE_FLAG_REVOKE,
    );
    true
}

fn emit_capability_edge(
    granter_task_id: TaskId,
    target_task_id: TaskId,
    channel: ChannelUuid,
    flag: u32,
) {
    let from =
        crate::graph::bootstrap::service_node_by_task_id(granter_task_id).unwrap_or(NODE_ID_KERNEL);
    let to =
        crate::graph::bootstrap::service_node_by_channel_uuid(channel).unwrap_or(NODE_ID_KERNEL);

    let _ = arena::add_edge(from, to, EdgeKind::Accesses, flag);

    let target_service =
        crate::graph::bootstrap::service_node_by_task_id(target_task_id).unwrap_or(NODE_ID_KERNEL);
    let _ = arena::add_edge(to, target_service, EdgeKind::Triggers, flag);
}
