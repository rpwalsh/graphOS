// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Per-CPU run queues and work-stealing scheduler.
//!
//! ## Design
//! Each logical CPU (BSP + APs) owns a bounded ring-buffer run queue
//! (`RunQueue`).  Tasks are enqueued on the owning CPU's queue.  When a
//! CPU's local queue is empty, it attempts to steal half the tasks from
//! the busiest peer queue (Chase-Lev-inspired greedy steal).
//!
//! ## CPU identity
//! `current_cpu_id()` reads the LAPIC ID from MMIO, which is set by
//! firmware/SIPI sequence before any Rust code runs on an AP.  We map
//! LAPIC IDs → sequential CPU indices (0 = BSP) using a table built
//! during `lapic::start_all_aps()`.
//!
//! ## Integration with legacy single-core path
//! The per-CPU infrastructure is activated only after `init_percpu(cpu_count)`
//! is called (by `lapic::ap_entry`).  Until then, `PERCPU_ACTIVE` remains
//! false and `find_next_ready_percpu()` returns `None`, falling through to
//! the legacy round-robin scan.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use spin::Mutex;

use crate::arch::serial;
use crate::task::context::CpuContext;

// ── constants ────────────────────────────────────────────────────────────────

/// Maximum CPUs supported.  Must match `lapic::MAX_CPUS`.
pub const MAX_CPUS: usize = 16;

/// Slots per per-CPU run queue.  Must be a power of two.
const QUEUE_CAPACITY: usize = 256;

/// Minimum queue size considered for stealing.  Don't steal from
/// queues with ≤ this many tasks to avoid ping-pong.
const STEAL_THRESHOLD: usize = 2;

// ── activation flag ─────────────────────────────────────────────────────────

/// Set to true once the BSP calls `init_percpu()` after all APs are alive.
static PERCPU_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Number of active CPUs (1 = BSP-only, up to MAX_CPUS).
static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);

// ── run queue ────────────────────────────────────────────────────────────────

/// Bounded FIFO ring-buffer of task-table indices.
///
/// The inner `Mutex` is a spin lock.  We keep critical sections extremely
/// short (one array write + two atomic additions) so contention is rare
/// even under heavy load.
struct RunQueue {
    buf: [u16; QUEUE_CAPACITY],
    head: usize, // next read position
    tail: usize, // next write position
    len: usize,
}

impl RunQueue {
    const fn new() -> Self {
        Self {
            buf: [0u16; QUEUE_CAPACITY],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    /// Enqueue a task-table index.  Returns false if the queue is full.
    fn push(&mut self, idx: u16) -> bool {
        if self.len >= QUEUE_CAPACITY {
            return false;
        }
        self.buf[self.tail] = idx;
        self.tail = (self.tail + 1) & (QUEUE_CAPACITY - 1);
        self.len += 1;
        true
    }

    /// Dequeue the front task-table index.  Returns None if empty.
    fn pop(&mut self) -> Option<u16> {
        if self.len == 0 {
            return None;
        }
        let val = self.buf[self.head];
        self.head = (self.head + 1) & (QUEUE_CAPACITY - 1);
        self.len -= 1;
        Some(val)
    }

    fn len(&self) -> usize {
        self.len
    }
}

// ── per-CPU state ─────────────────────────────────────────────────────────────

/// MLFQ priority levels (lower index = higher priority).
/// - 0: Realtime / interrupt-class (real-time threads, UI event loop)
/// - 1: Interactive (foreground apps, keyboard focus holder)
/// - 2: Normal (background services)
/// - 3: Batch (training, indexing, bulk I/O)
pub const MLFQ_LEVELS: usize = 4;

struct CpuState {
    /// Per-priority run queues. Index 0 is highest priority.
    queues: [RunQueue; MLFQ_LEVELS],
    /// LAPIC ID for this CPU slot.
    lapic_id: u8,
    /// Table index of the task currently Running on this CPU, or usize::MAX.
    current: usize,
}

impl CpuState {
    const fn new() -> Self {
        Self {
            queues: [
                RunQueue::new(),
                RunQueue::new(),
                RunQueue::new(),
                RunQueue::new(),
            ],
            lapic_id: 0,
            current: usize::MAX,
        }
    }
}

// Static array — one slot per possible CPU.
static CPU_STATES: [Mutex<CpuState>; MAX_CPUS] = {
    // const-init requires an explicit array literal.
    [
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
        Mutex::new(CpuState::new()),
    ]
};

// ── LAPIC-ID → CPU-index mapping ─────────────────────────────────────────────

/// Maps LAPIC ID (u8) to a sequential CPU index.
/// Index 0 is always the BSP.
static LAPIC_TO_CPU: Mutex<[u8; 256]> = Mutex::new([u8::MAX; 256]);

/// Register a CPU's LAPIC ID and return its sequential CPU index.
///
/// Called during `lapic::init_bsp()` (for the BSP, index 0) and
/// `lapic::ap_entry()` (for each AP).
pub fn register_cpu(lapic_id: u8) -> usize {
    let count = CPU_COUNT.load(Ordering::Relaxed);
    // BSP pre-registers at index 0 before APs are started.
    // APs call this and get the next sequential slot.
    let mut mapping = LAPIC_TO_CPU.lock();
    // Check if already registered (idempotent for BSP double-calls).
    if mapping[lapic_id as usize] != u8::MAX {
        return mapping[lapic_id as usize] as usize;
    }
    // Find next free slot.
    let idx = {
        let mut found = 0usize;
        for (i, state) in CPU_STATES.iter().enumerate().take(MAX_CPUS) {
            let st = state.lock();
            if st.lapic_id == 0 && i != 0 {
                // Unregistered slot (lapic_id == 0 is only valid for BSP at i==0).
                found = i;
                break;
            } else if i == count {
                found = i;
                break;
            }
        }
        found
    };
    let safe_idx = if idx >= MAX_CPUS { MAX_CPUS - 1 } else { idx };
    mapping[lapic_id as usize] = safe_idx as u8;
    {
        let mut st = CPU_STATES[safe_idx].lock();
        st.lapic_id = lapic_id;
    }
    CPU_COUNT.fetch_add(1, Ordering::Relaxed);
    safe_idx
}

/// Look up the sequential CPU index for the current CPU.
///
/// Reads the LAPIC ID from MMIO via `lapic::lapic_id()`.
/// Falls back to 0 (BSP) if the mapping is not found.
pub fn current_cpu_index() -> usize {
    let lapic_id = crate::arch::x86_64::lapic::lapic_id();
    let mapping = LAPIC_TO_CPU.lock();
    let idx = mapping[lapic_id as usize];
    if idx == u8::MAX { 0 } else { idx as usize }
}

// ── public API ────────────────────────────────────────────────────────────────

/// Initialise per-CPU infrastructure.
///
/// Called by the BSP after all APs have signalled readiness.
/// `cpu_count` is the total number of live CPUs (BSP + APs).
pub fn init_percpu(cpu_count: usize) {
    let n = if cpu_count > MAX_CPUS {
        MAX_CPUS
    } else {
        cpu_count
    };
    CPU_COUNT.store(n, Ordering::Release);

    // Register BSP as CPU 0 using the live LAPIC ID.
    let bsp_lapic = crate::arch::x86_64::lapic::lapic_id();
    {
        let mut mapping = LAPIC_TO_CPU.lock();
        mapping[bsp_lapic as usize] = 0;
    }
    {
        let mut st = CPU_STATES[0].lock();
        st.lapic_id = bsp_lapic;
    }

    PERCPU_ACTIVE.store(true, Ordering::Release);
    serial::write_bytes(b"[sched/percpu] activated, cpu_count=");
    serial::write_u64_dec_inline(n as u64);
    serial::write_line(b"");
}

/// Enqueue a task (by table index) onto the current CPU's run queue.
///
/// `priority` is clamped to `[0, MLFQ_LEVELS)`. Use 0 for realtime,
/// 1 for interactive, 2 for normal (default), 3 for batch.
/// Falls back to CPU 0 if the per-CPU system is not yet active.
pub fn enqueue_current_cpu(task_idx: usize) {
    enqueue_current_cpu_at(task_idx, 2); // default to Normal priority
}

/// Enqueue with explicit priority level.
pub fn enqueue_current_cpu_at(task_idx: usize, priority: usize) {
    if task_idx >= crate::task::table::MAX_TASKS {
        return;
    }
    // When per-CPU scheduling is not yet active (single-core BSP-only boot),
    // the legacy round-robin scanner reads task state directly from the task
    // table and does not consume this ring buffer.  Pushing here would fill
    // the queue with stale entries that are never popped, eventually causing
    // every new task enqueue to be silently dropped.  Skip the push entirely.
    if !PERCPU_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let cpu = current_cpu_index();
    let prio = priority.min(MLFQ_LEVELS - 1);
    let mut st = CPU_STATES[cpu].lock();
    if !st.queues[prio].push(task_idx as u16) {
        serial::write_line(b"[sched/percpu] run queue full, task not enqueued");
    }
}

/// Enqueue a task onto the specified CPU's run queue at normal priority.
pub fn enqueue_on_cpu(cpu: usize, task_idx: usize) {
    enqueue_on_cpu_at(cpu, task_idx, 2);
}

/// Enqueue a task onto the specified CPU at an explicit MLFQ priority level.
pub fn enqueue_on_cpu_at(cpu: usize, task_idx: usize, priority: usize) {
    if cpu >= MAX_CPUS || task_idx >= crate::task::table::MAX_TASKS {
        return;
    }
    let prio = priority.min(MLFQ_LEVELS - 1);
    let mut st = CPU_STATES[cpu].lock();
    if !st.queues[prio].push(task_idx as u16) {
        serial::write_line(b"[sched/percpu] enqueue_on_cpu: run queue full");
    }
}

/// Find the next Ready task for the current CPU.
///
/// 1. Pop from the local queue.
/// 2. If empty, attempt to steal from the busiest peer.
/// 3. Returns `None` if no tasks are found (fall through to legacy scan).
///
/// This is called from `schedule()` and `preempt()` when `PERCPU_ACTIVE`.
pub fn find_next_ready_percpu() -> Option<usize> {
    if !PERCPU_ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    let cpu = current_cpu_index();
    // 1. Try local queue.
    if let Some(idx) = dequeue_ready_local(cpu) {
        return Some(idx);
    }
    // 2. Work-steal from the busiest peer.
    steal_from_peer(cpu)
}

/// Record that `task_idx` is the currently running task on the current CPU.
pub fn set_current_on_cpu(task_idx: usize) {
    if !PERCPU_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let cpu = current_cpu_index();
    let mut st = CPU_STATES[cpu].lock();
    st.current = task_idx;
}

/// Return the task index currently running on the given CPU.
pub fn current_on_cpu(cpu: usize) -> Option<usize> {
    if cpu >= MAX_CPUS {
        return None;
    }
    let st = CPU_STATES[cpu].lock();
    if st.current == usize::MAX {
        None
    } else {
        Some(st.current)
    }
}

/// Return whether the per-CPU scheduler is active.
#[inline]
pub fn is_active() -> bool {
    PERCPU_ACTIVE.load(Ordering::Acquire)
}

// ── per-AP idle contexts ──────────────────────────────────────────────────────

/// Each AP saves its idle-loop context here so `run_on_ap` can switch back
/// after a task yields.  Index 0 = BSP (unused by `run_on_ap`, but kept for
/// uniform indexing).
static mut AP_IDLE_CTXS: [CpuContext; MAX_CPUS] = [CpuContext::zero(); MAX_CPUS];

/// Return a raw pointer to the idle context for the current CPU.
///
/// # Safety
/// The pointer is stable (static) and valid for the lifetime of the kernel.
pub fn ap_idle_context_ptr() -> *mut CpuContext {
    let cpu = current_cpu_index();
    if cpu >= MAX_CPUS {
        return core::ptr::null_mut();
    }
    // SAFETY: indexed by current CPU; only one AP writes/reads its own slot.
    unsafe { core::ptr::addr_of_mut!(AP_IDLE_CTXS[cpu]) }
}

/// Return the number of active CPUs.
#[inline]
pub fn cpu_count() -> usize {
    CPU_COUNT.load(Ordering::Acquire)
}

// ── internal helpers ─────────────────────────────────────────────────────────

/// Pop the first Ready task from the local MLFQ queues, highest priority first.
fn dequeue_ready_local(cpu: usize) -> Option<usize> {
    // Drain from highest-priority (0) to lowest (MLFQ_LEVELS-1).
    for prio in 0..MLFQ_LEVELS {
        let limit = CPU_STATES[cpu].lock().queues[prio].len();
        for _ in 0..limit {
            let candidate = CPU_STATES[cpu].lock().queues[prio].pop();
            match candidate {
                None => break,
                Some(idx) => {
                    let idx = idx as usize;
                    if crate::task::table::is_ready(idx) {
                        return Some(idx);
                    }
                    // Not ready — discard and continue scanning.
                }
            }
        }
    }
    None
}

/// Greedy work-steal: find the busiest peer CPU and take half its normal-priority tasks.
fn steal_from_peer(thief_cpu: usize) -> Option<usize> {
    let n = CPU_COUNT.load(Ordering::Acquire);
    // Work-steal at the lowest filled priority level (normal/batch first to preserve
    // latency on the realtime and interactive queues of the victim).
    for prio in (0..MLFQ_LEVELS).rev() {
        let mut busiest_cpu = usize::MAX;
        let mut busiest_len = STEAL_THRESHOLD;
        for (peer, state) in CPU_STATES.iter().enumerate().take(n) {
            if peer == thief_cpu {
                continue;
            }
            let len = state.lock().queues[prio].len();
            if len > busiest_len {
                busiest_len = len;
                busiest_cpu = peer;
            }
        }
        if busiest_cpu == usize::MAX {
            continue;
        }

        // Steal up to half the tasks from this priority level.
        let steal_count = (busiest_len / 2).max(1);
        let mut first: Option<usize> = None;
        for _ in 0..steal_count {
            let candidate = CPU_STATES[busiest_cpu].lock().queues[prio].pop();
            match candidate {
                None => break,
                Some(idx) => {
                    let idx = idx as usize;
                    if crate::task::table::is_ready(idx) {
                        if first.is_none() {
                            first = Some(idx);
                        } else {
                            let mut st = CPU_STATES[thief_cpu].lock();
                            let _ = st.queues[prio].push(idx as u16);
                        }
                    }
                }
            }
        }
        if first.is_some() {
            return first;
        }
    }
    None
}
