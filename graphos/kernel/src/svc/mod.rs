// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Service Manager — orchestrates kernel and user services.
//!
//! The service manager is responsible for:
//! - Defining and registering services (graphd, modeld, etc.)
//! - Starting services in dependency order
//! - Monitoring service health via the digital twin
//! - Restarting failed services (future)
//!
//! ## Service Lifecycle
//! 1. `Registered` — Service is defined but not started.
//! 2. `Starting` — Service task has been created, awaiting ready signal.
//! 3. `Running` — Service is operational.
//! 4. `Failed` — Service crashed or failed health check.
//! 5. `Stopped` — Service was gracefully stopped.
//!
//! ## Current Implementation
//! Phase 1: Static service table, manual start, no restart policy.
//! Future: Dynamic registration, dependency graphs, health monitoring.

use spin::Mutex;

use crate::arch::serial;
use crate::diag;
use crate::graph;
use crate::task;

/// Maximum number of services.
pub const MAX_SERVICES: usize = 16;

/// Service lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceState {
    /// Registered but not started.
    Registered = 0,
    /// Task created, awaiting ready signal.
    Starting = 1,
    /// Fully operational.
    Running = 2,
    /// Crashed or failed health check.
    Failed = 3,
    /// Gracefully stopped.
    Stopped = 4,
    /// Queued for automatic restart by the watchdog.
    Restarting = 5,
}

// ─── restart queue ──────────────────────────────────────────────────────────
// Populated by watchdog_check() from the timer ISR; drained by
// drain_restart_queue() which is also called from the timer handler but
// outside the TABLE lock so it can safely call userland::spawn_named_service.

const RESTART_QUEUE_CAP: usize = 8;

struct RestartQueue {
    names: [[u8; 32]; RESTART_QUEUE_CAP],
    count: usize,
}
impl RestartQueue {
    const fn new() -> Self {
        Self {
            names: [[0u8; 32]; RESTART_QUEUE_CAP],
            count: 0,
        }
    }
    fn push(&mut self, name: &[u8; 32]) -> bool {
        if self.count >= RESTART_QUEUE_CAP {
            return false;
        }
        self.names[self.count] = *name;
        self.count += 1;
        true
    }
    fn pop(&mut self) -> Option<[u8; 32]> {
        if self.count == 0 {
            return None;
        }
        let name = self.names[0];
        self.names.copy_within(1..self.count, 0);
        self.count -= 1;
        Some(name)
    }
}

static RESTART_QUEUE: Mutex<RestartQueue> = Mutex::new(RestartQueue::new());

/// Service definition.
#[derive(Clone, Copy)]
pub struct Service {
    /// Human-readable name (null-terminated).
    pub name: [u8; 32],
    /// Entry point function address.
    pub entry: u64,
    /// Current state.
    pub state: ServiceState,
    /// Task ID (if started).
    pub task_id: u64,
    /// Graph node ID (if registered in graph).
    pub node_id: u64,
    /// Priority (lower = starts earlier).
    pub priority: u8,
    /// Whether this is a critical service (panic on failure).
    pub critical: bool,
    /// Monotonic tick count of the last received heartbeat.
    /// 0 = never heartbeated (watchdog ignores until first beat).
    pub last_heartbeat_tick: u64,
    /// TSC counter at service registration — baseline for cycle telemetry.
    pub start_tsc: u64,
    /// TSC counter at the most-recent heartbeat.
    pub last_heartbeat_tsc: u64,
}

impl Service {
    const fn empty() -> Self {
        Self {
            name: [0; 32],
            entry: 0,
            state: ServiceState::Registered,
            task_id: 0,
            node_id: 0,
            priority: 128,
            critical: false,
            last_heartbeat_tick: 0,
            start_tsc: 0,
            last_heartbeat_tsc: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.entry == 0
    }
}

/// Service table.
struct ServiceTable {
    services: [Service; MAX_SERVICES],
    count: usize,
    initialized: bool,
}

impl ServiceTable {
    const fn new() -> Self {
        Self {
            services: [Service::empty(); MAX_SERVICES],
            count: 0,
            initialized: false,
        }
    }
}

static TABLE: Mutex<ServiceTable> = Mutex::new(ServiceTable::new());

#[inline(always)]
fn read_tsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// Initialize the service manager.
pub fn init() {
    let mut table = TABLE.lock();
    if table.initialized {
        diag::warn(diag::Category::Boot, b"servicemgr already initialized");
        return;
    }
    table.initialized = true;
    diag::info(diag::Category::Boot, b"Service manager initialized");
}

/// Register a service for later startup.
///
/// # Arguments
/// * `name` - Service name (max 31 chars).
/// * `entry` - Entry point function pointer.
/// * `priority` - Start priority (0 = highest).
/// * `critical` - If true, kernel panics on service failure.
///
/// # Returns
/// Service index, or `None` if table is full.
pub fn register(name: &[u8], entry: u64, priority: u8, critical: bool) -> Option<usize> {
    let mut table = TABLE.lock();

    if table.count >= MAX_SERVICES {
        diag::error(diag::Category::Boot, b"service table full");
        return None;
    }

    // Find first empty slot.
    for (i, svc) in table.services.iter_mut().enumerate() {
        if svc.is_empty() {
            let mut name_buf = [0u8; 32];
            let copy_len = name.len().min(31);
            name_buf[..copy_len].copy_from_slice(&name[..copy_len]);

            *svc = Service {
                name: name_buf,
                entry,
                state: ServiceState::Registered,
                task_id: 0,
                node_id: 0,
                priority,
                critical,
                last_heartbeat_tick: 0,
                start_tsc: read_tsc(),
                last_heartbeat_tsc: 0,
            };
            table.count += 1;

            serial::write_bytes(b"[svcmgr] registered: ");
            serial::write_bytes(&name_buf[..copy_len]);
            serial::write_bytes(b" priority=");
            serial::write_u64_dec_inline(priority as u64);
            if critical {
                serial::write_line(b" [CRITICAL]");
            } else {
                serial::write_line(b"");
            }

            return Some(i);
        }
    }

    None
}

/// Start all registered services in priority order.
///
/// Services with lower priority values start first.
///
/// # Returns
/// Number of services successfully started.
pub fn start_all() -> usize {
    // First pass: collect services to start (sorted by priority).
    // We use a fixed-size array to avoid heap allocation.
    let mut to_start: [(u8, usize, [u8; 32], u64, bool); MAX_SERVICES] =
        [(255, 0, [0; 32], 0, false); MAX_SERVICES];
    let mut num_to_start = 0;

    {
        let table = TABLE.lock();

        if table.count == 0 {
            diag::info(diag::Category::Boot, b"no services registered");
            return 0;
        }

        serial::write_bytes(b"[svcmgr] Starting ");
        serial::write_u64_dec_inline(table.count as u64);
        serial::write_line(b" service(s)...");

        for (idx, svc) in table.services.iter().enumerate() {
            if !svc.is_empty() && svc.state == ServiceState::Registered {
                to_start[num_to_start] = (svc.priority, idx, svc.name, svc.entry, svc.critical);
                num_to_start += 1;
            }
        }
    } // Release lock.

    // Sort by priority (simple bubble sort for small N).
    for i in 0..num_to_start {
        for j in (i + 1)..num_to_start {
            if to_start[j].0 < to_start[i].0 {
                to_start.swap(i, j);
            }
        }
    }

    // Second pass: start services in priority order.
    let mut started = 0;

    for (_, idx, name, entry, critical) in to_start.iter().copied().take(num_to_start) {
        let name_len = name.iter().position(|&b| b == 0).unwrap_or(32);

        // Mark as starting.
        {
            let mut table = TABLE.lock();
            if let Some(svc) = table.services.get_mut(idx) {
                svc.state = ServiceState::Starting;
            }
        }

        // Create task (outside lock).
        match task::table::create_kernel_task(&name[..name_len], entry) {
            Some(task_id) => {
                // Update state.
                let mut table = TABLE.lock();
                if let Some(svc) = table.services.get_mut(idx) {
                    svc.task_id = task_id;
                    svc.state = ServiceState::Running;

                    // Register in graph (graph lock is safe to acquire while holding svc lock).
                    if let Some(node_id) =
                        graph::seed::register_task(&name[..name_len], graph::types::NODE_ID_KERNEL)
                    {
                        svc.node_id = node_id;
                    }

                    serial::write_bytes(b"[svcmgr] started: ");
                    serial::write_bytes(&name[..name_len]);
                    serial::write_bytes(b" task_id=");
                    serial::write_u64_dec_inline(task_id);
                    serial::write_bytes(b" node=");
                    serial::write_u64_dec(svc.node_id);

                    started += 1;
                }
            }
            None => {
                let mut table = TABLE.lock();
                if let Some(svc) = table.services.get_mut(idx) {
                    svc.state = ServiceState::Failed;
                }

                serial::write_bytes(b"[svcmgr] FAILED to start: ");
                serial::write_line(&name[..name_len]);

                if critical {
                    diag::fatal(diag::Category::Boot, b"critical service failed to start");
                }
            }
        }
    }

    let total = {
        let table = TABLE.lock();
        table.count
    };

    serial::write_bytes(b"[svcmgr] ");
    serial::write_u64_dec_inline(started as u64);
    serial::write_bytes(b" of ");
    serial::write_u64_dec_inline(total as u64);
    serial::write_line(b" service(s) started");

    started
}

/// Mark a service as failed (called from task death handler).
pub fn mark_failed(task_id: u64) {
    let mut table = TABLE.lock();

    for svc in table.services.iter_mut() {
        if svc.task_id == task_id && svc.state == ServiceState::Running {
            svc.state = ServiceState::Failed;

            let name_len = svc.name.iter().position(|&b| b == 0).unwrap_or(32);
            serial::write_bytes(b"[svcmgr] service failed: ");
            serial::write_line(&svc.name[..name_len]);

            if svc.critical {
                diag::fatal(diag::Category::Boot, b"critical service died");
            }
            return;
        }
    }
}

/// Dump service table to serial.
pub fn dump() {
    let table = TABLE.lock();

    serial::write_line(b"[svcmgr] === Service Table ===");
    serial::write_bytes(b"[svcmgr] ");
    serial::write_u64_dec_inline(table.count as u64);
    serial::write_line(b" service(s) registered");

    for svc in table.services.iter() {
        if svc.is_empty() {
            continue;
        }

        let name_len = svc.name.iter().position(|&b| b == 0).unwrap_or(32);
        serial::write_bytes(b"  ");
        serial::write_bytes(&svc.name[..name_len]);
        serial::write_bytes(b": ");

        match svc.state {
            ServiceState::Registered => serial::write_bytes(b"registered"),
            ServiceState::Starting => serial::write_bytes(b"starting"),
            ServiceState::Running => serial::write_bytes(b"running"),
            ServiceState::Failed => serial::write_bytes(b"FAILED"),
            ServiceState::Stopped => serial::write_bytes(b"stopped"),
            ServiceState::Restarting => serial::write_bytes(b"restarting"),
        }

        if svc.task_id != 0 {
            serial::write_bytes(b" task=");
            serial::write_u64_dec_inline(svc.task_id);
        }
        // Phase J: TSC-based cycle telemetry.
        if svc.start_tsc != 0 {
            let tsc_ref = if svc.last_heartbeat_tsc != 0 {
                svc.last_heartbeat_tsc
            } else {
                read_tsc()
            };
            let delta = tsc_ref.saturating_sub(svc.start_tsc);
            serial::write_bytes(b" cycles=");
            serial::write_u64_dec_inline(delta);
        }
        if svc.critical {
            serial::write_bytes(b" [CRITICAL]");
        }
        serial::write_line(b"");
    }

    serial::write_line(b"[svcmgr] === End Service Table ===");
}

/// Returns the number of running services.
pub fn running_count() -> usize {
    let table = TABLE.lock();
    table
        .services
        .iter()
        .filter(|s| s.state == ServiceState::Running)
        .count()
}

/// Returns the number of failed services.
pub fn failed_count() -> usize {
    let table = TABLE.lock();
    table
        .services
        .iter()
        .filter(|s| s.state == ServiceState::Failed)
        .count()
}

/// Record a heartbeat from the task with the given `task_id`.
///
/// Called from the `SYS_HEARTBEAT` syscall handler.  Updates
/// `last_heartbeat_tick` and `last_heartbeat_tsc` on the matching
/// Running service entry.  Returns `true` if a match was found.
pub fn heartbeat_from_task(task_id: u64) -> bool {
    let now = crate::arch::timer::ticks();
    let tsc = read_tsc();
    let mut table = TABLE.lock();
    for svc in table.services.iter_mut() {
        if svc.task_id == task_id && svc.state == ServiceState::Running {
            svc.last_heartbeat_tick = now;
            svc.last_heartbeat_tsc = tsc;
            return true;
        }
    }
    false
}

/// Watchdog check — marks any `Running` service that has missed
/// heartbeats for longer than `WATCHDOG_TIMEOUT_TICKS` as `Failed`.
///
/// Safe to call from the timer ISR (uses `try_lock` to avoid deadlock).
/// Only active once the system has been up for at least twice the
/// timeout duration (prevents false alarms during slow boot).
pub fn watchdog_check() {
    /// 5 000 ms at 1 kHz PIT.
    const WATCHDOG_TIMEOUT_TICKS: u64 = 5_000;
    let now = crate::arch::timer::ticks();
    if now < WATCHDOG_TIMEOUT_TICKS * 2 {
        return;
    }
    let Some(mut table) = TABLE.try_lock() else {
        return;
    };
    for svc in table.services.iter_mut() {
        if svc.state != ServiceState::Running {
            continue;
        }
        // Skip services that have never heartbeated (boot grace period).
        if svc.last_heartbeat_tick == 0 {
            continue;
        }
        let age = now.saturating_sub(svc.last_heartbeat_tick);
        if age > WATCHDOG_TIMEOUT_TICKS {
            let name_len = svc.name.iter().position(|&b| b == 0).unwrap_or(32);
            serial::write_bytes(b"[watchdog] MISS ");
            serial::write_bytes(&svc.name[..name_len]);
            serial::write_bytes(b" age=");
            serial::write_u64_dec_inline(age);
            serial::write_line(b" ticks -- queuing restart");
            // Queue restart; mark as Restarting so we don't re-queue on
            // subsequent ticks until the new task is up and heartbeating.
            if let Some(mut q) = RESTART_QUEUE.try_lock() {
                if q.push(&svc.name) {
                    svc.state = ServiceState::Restarting;
                } else {
                    // Queue full — mark Failed so operator can see it.
                    svc.state = ServiceState::Failed;
                }
            } else {
                svc.state = ServiceState::Failed;
            }
        }
    }
}

/// Drain the restart queue by calling `userland::spawn_named_service` for
/// each pending service name.  Must be called from a context where heap /
/// VFS access is safe (not deep inside a spinlock).
pub fn drain_restart_queue() {
    loop {
        let name = {
            let Some(mut q) = RESTART_QUEUE.try_lock() else {
                return;
            };
            match q.pop() {
                Some(n) => n,
                None => return,
            }
        };
        let name_len = name.iter().position(|&b| b == 0).unwrap_or(32);
        serial::write_bytes(b"[watchdog] restarting ");
        serial::write_bytes(&name[..name_len]);
        serial::write_line(b"");
        if let Some(_task_id) = crate::userland::spawn_named_service(&name[..name_len]) {
            serial::write_bytes(b"[watchdog] restart ok ");
            serial::write_bytes(&name[..name_len]);
            serial::write_line(b"");
        } else {
            serial::write_bytes(b"[watchdog] restart FAILED ");
            serial::write_bytes(&name[..name_len]);
            serial::write_line(b"");
        }
    }
}

/// Queue a restart for the service that owns `task_id`.
///
/// Called from the token-based watchdog callback with the task ID that
/// missed its heartbeat deadline.  If the task belongs to a critical service
/// and the restart queue is full, the kernel triggers a hardware reboot.
pub fn queue_restart_by_task_id(task_id: u64) {
    let Some(mut table) = TABLE.try_lock() else {
        return;
    };
    for svc in table.services.iter_mut() {
        if svc.task_id != task_id || svc.state != ServiceState::Running {
            continue;
        }
        let name_len = svc.name.iter().position(|&b| b == 0).unwrap_or(32);
        serial::write_bytes(b"[watchdog] token deadline missed for task_id=");
        serial::write_u64_dec_inline(task_id);
        serial::write_bytes(b" svc=");
        serial::write_bytes(&svc.name[..name_len]);
        serial::write_line(b"");

        if svc.critical {
            // Critical service missed watchdog — reboot immediately.
            serial::write_line(b"[watchdog] CRITICAL service missed deadline, rebooting");
            drop(table);
            crate::arch::machine::reboot();
        }

        if let Some(mut q) = RESTART_QUEUE.try_lock() {
            if q.push(&svc.name) {
                svc.state = ServiceState::Restarting;
            } else {
                svc.state = ServiceState::Failed;
            }
        } else {
            svc.state = ServiceState::Failed;
        }
        return;
    }
}
