// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Task Control Block (TCB) and task state definitions.
//!
//! Each task in GraphOS is represented by a `Task` struct that holds its
//! state, identity, kernel stack, and (for user tasks) address-space metadata.
//!
//! ## Current status
//! - Kernel tasks run in ring 0 using the shared kernel page tables.
//! - User tasks may now carry a private CR3 and user entry/stack metadata.
//! - Scheduler/user-mode transition glue is still incremental, but the TCB
//!   is now production-capable for isolated address spaces.

use alloc::boxed::Box;

use crate::ipc::capability::CapabilitySet;
use crate::mm::address_space::AddressSpace;
use crate::uuid::ChannelUuid;
use crate::uuid::Uuid128;

/// Unique task identifier. Kernel-wide, monotonically increasing, never reused.
pub type TaskId = u64;

/// Task lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskState {
    /// Created but not yet runnable (context not fully initialized).
    Created = 0,
    /// Ready to run; in the scheduler's run queue.
    Ready = 1,
    /// Currently executing on a CPU.
    Running = 2,
    /// Blocked on a syscall, IPC wait, or timer.
    Blocked = 3,
    /// Terminated. Awaiting reap by parent or kernel.
    Dead = 4,
}

/// Size of each task's kernel stack, in bytes.
pub const KERNEL_STACK_SIZE: usize = 1024 * 1024;

/// Number of 4 KiB frames per kernel stack.
pub const KERNEL_STACK_FRAMES: usize = KERNEL_STACK_SIZE / 4096;

/// Maximum number of per-task IPC capability entries.
/// Re-exported from `ipc::capability::CAPABILITY_SET_CAPACITY` for callers
/// that need the constant without importing the capability module.
pub const MAX_TASK_IPC_CAPS: usize = crate::ipc::capability::CAPABILITY_SET_CAPACITY;

/// Task Control Block.
#[derive(Debug)]
pub struct Task {
    /// Unique identifier.
    pub id: TaskId,

    /// Human-readable name for diagnostics (null-terminated byte string).
    pub name: [u8; 32],

    /// Current lifecycle state.
    pub state: TaskState,

    /// Scheduling hint used for interactive wakeups. Higher means more urgent.
    pub priority: u8,

    /// Physical address of the base (lowest address) of this task's kernel stack.
    pub stack_base: u64,

    /// The IPC channel this task is blocked on (NIL = not blocked on any channel).
    pub wait_channel: ChannelUuid,

    /// Set when the task is woken directly without an IPC payload.
    /// `sys_channel_recv` consumes this to return a sentinel wake result
    /// instead of immediately blocking again on the same channel.
    pub pending_wake_without_ipc: bool,

    /// The IPC channel other tasks should use to send replies to this task.
    pub ipc_endpoint: u32,

    /// Whether this task is a user-mode (ring-3) task.
    pub is_user: bool,

    /// CR3 root (PML4 physical address) for this task.
    /// 0 means: use the shared kernel address space.
    pub cr3: u64,

    /// Initial user RIP (entry point) for ring-3 dispatch.
    pub user_rip: u64,

    /// Initial user RSP (top of user stack) for ring-3 dispatch.
    pub user_rsp: u64,

    /// Owned user address-space resources, if this is a user task.
    pub address_space: Option<Box<AddressSpace>>,

    /// Effective user and group identity.
    pub uid: u32,
    pub gid: u32,

    /// Active login/session UUID for this task (NIL if detached).
    pub session_id: Uuid128,

    /// Typed IPC capability set — channels this task holds and their permissions.
    pub caps: CapabilitySet,

    /// Number of IPC channels this task has created via SYS_CHANNEL_CREATE.
    /// Used to enforce a per-task channel limit and prevent channel exhaustion.
    pub channels_created: u8,

    /// The graph node ID for this task in the kernel arena.
    /// Populated immediately after task creation via `graph::handles::register_task()`.
    /// 0 = not yet registered (arena full or early-boot race).
    pub graph_node: crate::graph::types::NodeId,

    /// If non-zero, this task is blocked in `SYS_THREAD_JOIN` waiting for the
    /// task with this ID to become `Dead`.  Cleared when that task dies and
    /// wakes this task.
    pub joining_on: TaskId,

    /// Argument passed to a thread at spawn time (stored by `create_user_thread`;
    /// consumed by the user-task bootstrap to set rdi before entering ring-3).
    pub thread_arg: u64,
}

impl Task {
    /// Create a new task in the `Created` state with no stack and kernel address space.
    pub const fn new(id: TaskId, name: [u8; 32]) -> Self {
        Self {
            id,
            name,
            state: TaskState::Created,
            priority: 0,
            stack_base: 0,
            wait_channel: ChannelUuid(crate::uuid::Uuid128::NIL),
            pending_wake_without_ipc: false,
            ipc_endpoint: 0,
            is_user: false,
            cr3: 0,
            user_rip: 0,
            user_rsp: 0,
            address_space: None,
            uid: 0,
            gid: 0,
            session_id: Uuid128::NIL,
            caps: CapabilitySet::new(),
            channels_created: 0,
            graph_node: 0,
            joining_on: 0,
            thread_arg: 0,
        }
    }

    /// Short name for serial output. Returns the name bytes up to the first
    /// null or the full 32 bytes.
    pub fn name_bytes(&self) -> &[u8] {
        let end = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.name.len());
        &self.name[..end]
    }

    /// Grant capability bits for a channel.
    pub fn grant_ipc_capability(&mut self, channel: ChannelUuid, perms: u8) -> bool {
        self.caps.grant(channel, perms)
    }

    /// Revoke capability bits for a channel.
    pub fn revoke_ipc_capability(&mut self, channel: ChannelUuid, perms: u8) -> bool {
        self.caps.revoke(channel, perms)
    }

    /// Check whether this task has all requested capability bits for a channel.
    pub fn has_ipc_capability(&self, channel: ChannelUuid, perms: u8) -> bool {
        self.caps.has(channel, perms)
    }
}
