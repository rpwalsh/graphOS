// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Service watchdog — per-service heartbeat monitoring with auto-restart.
//!
//! Each registered service receives a capability token (the "heartbeat cap").
//! The service must call `SYS_WATCHDOG_KICK` at least once every `timeout_ms`
//! milliseconds, passing its token.  If the watchdog tick fires and the
//! deadline has passed, the service is killed and re-spawned via the launcher.

use spin::Mutex;

use crate::uuid::Uuid128 as Uuid;

// ── Configuration ─────────────────────────────────────────────────────────────

const MAX_WATCHES: usize = 64;

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WatchState {
    Empty,
    Active,
    Expired,
}

struct Watch {
    state: WatchState,
    token: Uuid,
    task_uuid: Uuid,
    /// Numeric task ID — used to look up the service in svc::TABLE for restart.
    task_id: u64,
    timeout_ms: u64,
    deadline_ms: u64,
    miss_count: u8,
    /// Graph arena node ID representing this watchdog watch (0 = not registered).
    graph_node: crate::graph::types::NodeId,
}

impl Watch {
    const fn empty() -> Self {
        Self {
            state: WatchState::Empty,
            token: Uuid::NIL,
            task_uuid: Uuid::NIL,
            task_id: 0,
            timeout_ms: 0,
            deadline_ms: 0,
            miss_count: 0,
            graph_node: 0,
        }
    }
}

static WATCHES: Mutex<[Watch; MAX_WATCHES]> = Mutex::new(
    // SAFETY: Watch::empty() is const.
    [const { Watch::empty() }; MAX_WATCHES],
);

// ── Public API ────────────────────────────────────────────────────────────────

/// Register a service for watchdog monitoring.
/// Returns the capability token the service must present when kicking the watchdog.
/// Returns `None` if the watchdog table is full.
pub fn register(task_uuid: Uuid, task_id: u64, timeout_ms: u64, now_ms: u64) -> Option<Uuid> {
    let token = crate::uuid::Uuid128Gen::v4();
    let mut watches = WATCHES.lock();
    for w in watches.iter_mut() {
        if w.state == WatchState::Empty {
            let gn = crate::graph::arena::add_node(
                crate::graph::types::NodeKind::Service,
                crate::graph::types::NODE_FLAG_TRUSTED,
                crate::graph::types::NODE_ID_KERNEL,
            );
            let graph_node = gn.unwrap_or(0);
            *w = Watch {
                state: WatchState::Active,
                token,
                task_uuid,
                task_id,
                timeout_ms,
                deadline_ms: now_ms + timeout_ms,
                miss_count: 0,
                graph_node,
            };
            return Some(token);
        }
    }
    None
}

/// Kick the watchdog for the given capability token.
/// Returns `true` if the token was valid, `false` otherwise.
pub fn kick(token: Uuid, now_ms: u64) -> bool {
    let mut watches = WATCHES.lock();
    for w in watches.iter_mut() {
        if w.state == WatchState::Active && w.token == token {
            w.deadline_ms = now_ms + w.timeout_ms;
            w.miss_count = 0;
            return true;
        }
    }
    false
}

/// Unregister a service watchdog by token.
pub fn unregister(token: Uuid) {
    let mut watches = WATCHES.lock();
    for w in watches.iter_mut() {
        if w.token == token {
            let gn = w.graph_node;
            *w = Watch::empty();
            if gn != 0 {
                crate::graph::arena::detach_node(gn);
            }
            return;
        }
    }
}

/// Tick the watchdog.  Call from the timer interrupt / scheduler tick.
/// `now_ms`: current monotonic millisecond counter.
///
/// For each expired watch the callback calls `restart_fn(task_id)` with the
/// numeric task ID of the expired service.
/// The caller is responsible for actually killing/restarting the task.
pub fn tick<F>(now_ms: u64, mut restart_fn: F)
where
    F: FnMut(u64),
{
    let mut watches = WATCHES.lock();
    for w in watches.iter_mut() {
        if w.state != WatchState::Active {
            continue;
        }
        if now_ms >= w.deadline_ms {
            w.miss_count = w.miss_count.saturating_add(1);
            if w.miss_count >= 3 {
                // Three consecutive misses: mark expired, trigger restart.
                w.state = WatchState::Expired;
                restart_fn(w.task_id);
            } else {
                // Give it one more window.
                w.deadline_ms = now_ms + w.timeout_ms;
            }
        }
    }
}
