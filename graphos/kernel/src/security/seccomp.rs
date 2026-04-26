// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use spin::Mutex;

use crate::task::table::MAX_TASKS;

const MODE_ALLOW_ALL: u64 = 1;
/// Privileged-service profile: includes SETUID, DRIVER_*, SPAWN, cap-grant,
/// cognitive write, registry-register.  Set on all /pkg/services/ tasks.
const MODE_PROTECTED_STRICT: u64 = 2;
/// Unprivileged-app profile: cannot escalate identity, install drivers,
/// spawn arbitrary services, delegate capabilities, or write to cognitive
/// index.  Set on all /pkg/apps/ tasks.
const MODE_APP_STRICT: u64 = 3;

#[derive(Clone, Copy)]
struct TaskPolicy {
    mode: u64,
}

impl TaskPolicy {
    /// New tasks start with the least-privilege profile.  The kernel spawn
    /// path must explicitly call `set_protected_strict` for trusted services.
    const EMPTY: Self = Self {
        mode: MODE_APP_STRICT,
    };
}

struct PolicyTable {
    tasks: [TaskPolicy; MAX_TASKS],
}

impl PolicyTable {
    const fn new() -> Self {
        Self {
            tasks: [TaskPolicy::EMPTY; MAX_TASKS],
        }
    }
}

static POLICIES: Mutex<PolicyTable> = Mutex::new(PolicyTable::new());

pub fn set_allow_all(task_index: usize, allow: bool) -> bool {
    let mut table = POLICIES.lock();
    let Some(policy) = table.tasks.get_mut(task_index) else {
        return false;
    };
    policy.mode = if allow {
        MODE_ALLOW_ALL
    } else {
        MODE_PROTECTED_STRICT
    };
    true
}

pub fn set_protected_strict(task_index: usize) -> bool {
    let mut table = POLICIES.lock();
    let Some(policy) = table.tasks.get_mut(task_index) else {
        return false;
    };
    policy.mode = MODE_PROTECTED_STRICT;
    true
}

/// Set unprivileged-app profile.  Must be called for every task whose ELF
/// lives under /pkg/apps/.  Accepted from any current mode — always a
/// downgrade or no-op relative to ALLOW_ALL.
pub fn set_app_strict(task_index: usize) -> bool {
    let mut table = POLICIES.lock();
    let Some(policy) = table.tasks.get_mut(task_index) else {
        return false;
    };
    policy.mode = MODE_APP_STRICT;
    true
}

/// Clear the policy slot when a task exits so a recycled slot cannot
/// inherit a prior task's privileges.
pub fn clear_policy(task_index: usize) {
    let mut table = POLICIES.lock();
    if let Some(policy) = table.tasks.get_mut(task_index) {
        *policy = TaskPolicy::EMPTY;
    }
}

/// Return true if the task has `MODE_PROTECTED_STRICT` — i.e. it is a
/// kernel-registered system service running with uid=0 privileges.
pub fn is_protected_strict(task_index: usize) -> bool {
    let table = POLICIES.lock();
    matches!(
        table.tasks.get(task_index),
        Some(p) if p.mode == MODE_PROTECTED_STRICT
    )
}

pub fn is_allowed(task_index: usize, sys_nr: u64) -> bool {
    let table = POLICIES.lock();
    match table.tasks.get(task_index) {
        Some(policy) if policy.mode == MODE_ALLOW_ALL => true,
        Some(policy) if policy.mode == MODE_PROTECTED_STRICT => {
            use crate::syscall::numbers;
            matches!(
                sys_nr,
                numbers::SYS_EXIT
                    | numbers::SYS_YIELD
                    | numbers::SYS_SPAWN
                    | numbers::SYS_HEARTBEAT
                    | numbers::SYS_WRITE
                    | numbers::SYS_CHANNEL_CREATE
                    | numbers::SYS_CHANNEL_SEND
                    | numbers::SYS_CHANNEL_RECV
                    | numbers::SYS_SOCKET
                    | numbers::SYS_BIND
                    | numbers::SYS_CONNECT
                    | numbers::SYS_SEND
                    | numbers::SYS_RECV
                    | numbers::SYS_CLOSE_SOCK
                    | numbers::SYS_LISTEN
                    | numbers::SYS_ACCEPT
                    | numbers::SYS_NET_STATS
                    | numbers::SYS_GETUID
                    | numbers::SYS_GETGID
                    | numbers::SYS_SETUID
                    | numbers::SYS_LOGIN
                    | numbers::SYS_LOGOUT
                    | numbers::SYS_DRIVER_PROBE
                    | numbers::SYS_DRIVER_INSTALL
                    | numbers::SYS_VFS_OPEN
                    | numbers::SYS_VFS_READ
                    | numbers::SYS_VFS_CLOSE
                    | numbers::SYS_VFS_WRITE
                    | numbers::SYS_VFS_CREATE
                    | numbers::SYS_MOUNT
                    | numbers::SYS_UMOUNT
                    | numbers::SYS_MMAP
                    | numbers::SYS_MUNMAP
                    | numbers::SYS_GRAPH_SERVICE_LOOKUP
                    | numbers::SYS_GRAPH_SERVICE_LOOKUP_UUID
                    | numbers::SYS_REGISTRY_LOOKUP
                    | numbers::SYS_REGISTRY_REGISTER
                    | numbers::SYS_REGISTRY_SUBSCRIBE
                    | numbers::SYS_IPC_CAP_GRANT
                    | numbers::SYS_IPC_CAP_REVOKE
                    | numbers::SYS_SLEEP
                    | numbers::SYS_SURFACE_CREATE
                    | numbers::SYS_SURFACE_PRESENT
                    | numbers::SYS_SURFACE_DESTROY
                    | numbers::SYS_SURFACE_QUERY_PENDING
                    | numbers::SYS_SURFACE_FLUSH
                    | numbers::SYS_SURFACE_COMMIT
                    | numbers::SYS_GPU_QUERY_CAPS
                    | numbers::SYS_GPU_RESOURCE_CREATE
                    | numbers::SYS_GPU_RESOURCE_DESTROY
                    | numbers::SYS_GPU_SUBMIT_3D
                    | numbers::SYS_GPU_FENCE_ALLOC
                    | numbers::SYS_GPU_FENCE_WAIT
                    | numbers::SYS_GPU_SUBMIT
                    | numbers::SYS_FRAME_TICK_SUBSCRIBE
                    | numbers::SYS_COMPOSITOR_CLAIM_DISPLAY
                    | numbers::SYS_INPUT_SET_FOCUS
                    | numbers::SYS_INPUT_REGISTER_WINDOW
                    | numbers::SYS_INPUT_UNREGISTER_WINDOW
                    | numbers::SYS_COGNITIVE_INDEX
                    | numbers::SYS_COGNITIVE_QUERY
                    | numbers::SYS_COGNITIVE_REDACT
                    | numbers::SYS_PERF_SAMPLE
                    | numbers::SYS_PERF_READ
                    | numbers::SYS_AUDIT_READ
                    | numbers::SYS_GETRANDOM
                    | numbers::SYS_GRAPH_EM_STEP
                    | numbers::SYS_GRAPH_EM_STATS
                    | numbers::SYS_WIFI_SCAN
                    | numbers::SYS_WIFI_CONNECT
                    | numbers::SYS_WIFI_STATE
                    | numbers::SYS_BT_SCAN
                    | numbers::SYS_BT_CONNECT
                    | numbers::SYS_BT_SEND
                    | numbers::SYS_BT_CLOSE
                    | numbers::SYS_POWEROFF
                    | numbers::SYS_REBOOT
                    | numbers::SYS_SUSPEND
                    | numbers::SYS_TLS_SET_AVAILABLE
                    | numbers::SYS_TLS_SET_UNAVAILABLE
                    | numbers::SYS_TLS_AVAILABLE
                    | numbers::SYS_THREAD_SPAWN
                    | numbers::SYS_THREAD_JOIN
                    | numbers::SYS_THREAD_EXIT
            )
        }
        // Unprivileged app profile: no identity escalation, no driver access,
        // no arbitrary service spawn, no capability delegation, no cognitive
        // write, no service registration.
        Some(policy) if policy.mode == MODE_APP_STRICT => {
            use crate::syscall::numbers;
            matches!(
                sys_nr,
                numbers::SYS_EXIT
                    | numbers::SYS_YIELD
                    | numbers::SYS_SPAWN
                    | numbers::SYS_HEARTBEAT
                    | numbers::SYS_GETRANDOM
                    | numbers::SYS_WRITE
                    | numbers::SYS_CHANNEL_CREATE
                    | numbers::SYS_CHANNEL_SEND
                    | numbers::SYS_CHANNEL_RECV
                    | numbers::SYS_GETUID
                    | numbers::SYS_GETGID
                    | numbers::SYS_LOGIN
                    | numbers::SYS_LOGOUT
                    | numbers::SYS_VFS_OPEN
                    | numbers::SYS_VFS_READ
                    | numbers::SYS_VFS_CLOSE
                    | numbers::SYS_VFS_WRITE
                    | numbers::SYS_VFS_CREATE
                    | numbers::SYS_MMAP
                    | numbers::SYS_MUNMAP
                    | numbers::SYS_GRAPH_SERVICE_LOOKUP
                    | numbers::SYS_GRAPH_SERVICE_LOOKUP_UUID
                    | numbers::SYS_REGISTRY_LOOKUP
                    | numbers::SYS_REGISTRY_SUBSCRIBE
                    | numbers::SYS_IPC_CAP_REVOKE
                    | numbers::SYS_SLEEP
                    | numbers::SYS_SURFACE_CREATE
                    | numbers::SYS_SURFACE_PRESENT
                    | numbers::SYS_SURFACE_DESTROY
                    | numbers::SYS_SURFACE_QUERY_PENDING
                    | numbers::SYS_SURFACE_FLUSH
                    | numbers::SYS_SURFACE_COMMIT
                    | numbers::SYS_GPU_QUERY_CAPS
                    | numbers::SYS_GPU_RESOURCE_CREATE
                    | numbers::SYS_GPU_RESOURCE_DESTROY
                    | numbers::SYS_GPU_SUBMIT_3D
                    | numbers::SYS_GPU_FENCE_ALLOC
                    | numbers::SYS_GPU_FENCE_WAIT
                    | numbers::SYS_GPU_SUBMIT
                    | numbers::SYS_FRAME_TICK_SUBSCRIBE
                    | numbers::SYS_INPUT_SET_FOCUS
                    | numbers::SYS_INPUT_REGISTER_WINDOW
                    | numbers::SYS_INPUT_UNREGISTER_WINDOW
                    | numbers::SYS_GRAPH_EM_STATS
                    | numbers::SYS_COGNITIVE_QUERY
            )
        }
        None => false,
        _ => false,
    }
}
