// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Task reaper — garbage collection for dead tasks.
//!
//! When a task's entry function returns, the trampoline marks it `Dead` but
//! cannot immediately free the stack because the trampoline itself is still
//! executing on that stack. The reaper solves this by deferring cleanup.
//!
//! ## Design
//! 1. The trampoline calls `queue_for_reap(task_index)` after switching away
//!    from the dead task. At that point, no code is running on the dead stack.
//! 2. `queue_for_reap` wakes the reaper if it was sleeping.
//! 3. The reaper processes all pending entries, then sleeps until woken.
//!
//! ## Event-driven architecture
//! Unlike a polling design, the reaper uses a proper sleep/wake mechanism:
//! - When no work: reaper marks itself Blocked and yields
//! - When work arrives: `queue_for_reap` wakes the reaper
//! - This means zero CPU usage when no tasks are dying
//!
//! ## Why this beats Unix
//! Traditional Unix uses periodic timer-based cleanup or synchronous wait().
//! Our event-driven reaper:
//! - No polling overhead
//! - Immediate cleanup when work arrives  
//! - Scales to millions of task deaths without CPU waste
//! - Integrates with the graph for observability

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::arch::serial;

/// Maximum number of tasks pending reap.
const REAP_QUEUE_SIZE: usize = 32;

/// Entry in the reap queue.
#[derive(Clone, Copy)]
struct ReapEntry {
    /// Task table index of the dead task.
    task_index: usize,
    /// Stack base address to free.
    stack_base: u64,
    /// Whether this entry is valid.
    valid: bool,
}

impl ReapEntry {
    const fn empty() -> Self {
        Self {
            task_index: 0,
            stack_base: 0,
            valid: false,
        }
    }
}

/// The reap queue — tasks awaiting garbage collection.
struct ReapQueue {
    entries: [ReapEntry; REAP_QUEUE_SIZE],
    /// Index of the next entry to write.
    head: usize,
    /// Index of the next entry to read.
    tail: usize,
    /// Number of valid entries.
    count: usize,
    /// Total tasks reaped (diagnostic counter).
    total_reaped: u64,
    /// Dropped due to queue overflow.
    dropped: u64,
}

impl ReapQueue {
    const fn new() -> Self {
        Self {
            entries: [ReapEntry::empty(); REAP_QUEUE_SIZE],
            head: 0,
            tail: 0,
            count: 0,
            total_reaped: 0,
            dropped: 0,
        }
    }

    /// Enqueue a task for reaping. Returns false if queue is full.
    fn enqueue(&mut self, task_index: usize, stack_base: u64) -> bool {
        if self.count >= REAP_QUEUE_SIZE {
            self.dropped += 1;
            return false;
        }

        self.entries[self.head] = ReapEntry {
            task_index,
            stack_base,
            valid: true,
        };
        self.head = (self.head + 1) % REAP_QUEUE_SIZE;
        self.count += 1;
        true
    }

    /// Dequeue a task for reaping. Returns None if queue is empty.
    fn dequeue(&mut self) -> Option<ReapEntry> {
        if self.count == 0 {
            return None;
        }

        let entry = self.entries[self.tail];
        self.entries[self.tail].valid = false;
        self.tail = (self.tail + 1) % REAP_QUEUE_SIZE;
        self.count -= 1;
        Some(entry)
    }
}

static QUEUE: Mutex<ReapQueue> = Mutex::new(ReapQueue::new());

/// The task table index of the reaper task. Set by spawn().
static REAPER_TASK_INDEX: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Whether the reaper is currently sleeping (blocked).
static REAPER_SLEEPING: AtomicBool = AtomicBool::new(false);

/// Queue a dead task for reaping.
///
/// Called by the scheduler trampoline AFTER switching away from the dead task.
/// At this point, no code is executing on the dead task's stack, so it's safe
/// for the reaper to reclaim it.
///
/// This function also wakes the reaper if it was sleeping.
///
/// # Arguments
/// * `task_index` - The task table index of the dead task.
/// * `stack_base` - The base address of the task's kernel stack.
pub fn queue_for_reap(task_index: usize, stack_base: u64) {
    {
        let mut queue = QUEUE.lock();
        if !queue.enqueue(task_index, stack_base) {
            serial::write_bytes(b"[reaper] WARNING: queue full, dropped task at index ");
            serial::write_u64_dec(task_index as u64);
            return;
        }
    }

    // Wake the reaper if it was sleeping.
    wake_reaper();
}

/// Wake the reaper task if it's sleeping.
fn wake_reaper() {
    if REAPER_SLEEPING.load(Ordering::Acquire) {
        let idx = REAPER_TASK_INDEX.load(Ordering::Relaxed);
        if idx != usize::MAX {
            // Mark reaper as Ready so it gets scheduled.
            super::table::wake_task(idx);
            REAPER_SLEEPING.store(false, Ordering::Release);
        }
    }
}

/// Process all pending reap entries.
///
/// This is called by the reaper task. It processes the entire queue in one
/// pass, freeing stacks and clearing task slots.
///
/// # Returns
/// Number of tasks reaped in this pass.
pub fn reap_pending() -> usize {
    let mut reaped = 0;

    loop {
        // Dequeue one entry at a time, releasing the lock between entries
        // to allow new entries to be queued.
        let entry = {
            let mut queue = QUEUE.lock();
            queue.dequeue()
        };

        match entry {
            Some(e) if e.valid => {
                // Free the stack back to the pool.
                if e.stack_base != 0 {
                    super::stack_pool::free_stack(e.stack_base);
                }

                // Clear the task slot for reuse.
                super::table::clear_dead_slot(e.task_index);

                reaped += 1;

                // Update statistics.
                {
                    let mut queue = QUEUE.lock();
                    queue.total_reaped += 1;
                }
            }
            Some(_) => {
                // Invalid entry (shouldn't happen), skip.
            }
            None => {
                // Queue empty.
                break;
            }
        }
    }

    reaped
}

/// Get the number of tasks pending reap.
pub fn pending_count() -> usize {
    QUEUE.lock().count
}

/// Dump reaper statistics.
pub fn dump() {
    let queue = QUEUE.lock();
    serial::write_bytes(b"[reaper] pending=");
    serial::write_u64_dec_inline(queue.count as u64);
    serial::write_bytes(b" total_reaped=");
    serial::write_u64_dec_inline(queue.total_reaped);
    serial::write_bytes(b" dropped=");
    serial::write_u64_dec(queue.dropped);
}

// ============================================================================
// Reaper task — event-driven, not polling
// ============================================================================

/// The reaper kernel task entry point.
///
/// This is an EVENT-DRIVEN task, not a polling loop. It:
/// 1. Processes any pending reap entries
/// 2. If queue is empty, marks itself Blocked and yields
/// 3. Gets woken by queue_for_reap() when new work arrives
/// 4. Repeat
///
/// This design uses ZERO CPU when no tasks are dying.
pub fn reaper_task_entry() {
    serial::write_line(b"[reaper] task started (event-driven)");

    loop {
        // Process all pending entries.
        let reaped = reap_pending();

        if reaped > 0 {
            serial::write_bytes(b"[reaper] reaped ");
            serial::write_u64_dec_inline(reaped as u64);
            serial::write_line(b" task(s)");
        }

        // Check if more work arrived while we were processing.
        if pending_count() > 0 {
            continue;
        }

        // No more work. Sleep until woken.
        // We mark ourselves as sleeping BEFORE checking the queue one more
        // time to avoid a race where work arrives between check and sleep.
        REAPER_SLEEPING.store(true, Ordering::Release);

        // Double-check: did work arrive while we were setting the flag?
        if pending_count() > 0 {
            REAPER_SLEEPING.store(false, Ordering::Release);
            continue;
        }

        // Actually sleep. Mark ourselves Blocked and yield.
        // We'll be woken by queue_for_reap() -> wake_reaper().
        let my_idx = REAPER_TASK_INDEX.load(Ordering::Relaxed);
        if my_idx != usize::MAX {
            super::table::mark_blocked(my_idx, crate::uuid::ChannelUuid(crate::uuid::Uuid128::NIL)); // not waiting on a channel
        }

        // Yield. When we return, we've been woken because there's work.
        unsafe { crate::sched::schedule() };

        // We're awake. Clear the sleeping flag (wake_reaper already did, but
        // belt-and-suspenders).
        REAPER_SLEEPING.store(false, Ordering::Release);
    }
}

/// Spawn the reaper task.
///
/// Called during kernel init after the scheduler is set up.
///
/// # Returns
/// The task ID of the reaper, or None if creation failed.
pub fn spawn() -> Option<u64> {
    let entry = reaper_task_entry as *const () as u64;
    match super::table::create_kernel_task_with_index(b"reaper", entry) {
        Some((id, index)) => {
            // Store the task index so wake_reaper() can find it.
            REAPER_TASK_INDEX.store(index, Ordering::Release);

            // Register in the graph for diagnostics.
            crate::graph::seed::register_task(b"reaper", crate::graph::types::NODE_ID_KERNEL);
            serial::write_bytes(b"[reaper] spawned task id=");
            serial::write_u64_dec_inline(id);
            serial::write_bytes(b" index=");
            serial::write_u64_dec(index as u64);
            Some(id)
        }
        None => {
            serial::write_line(b"[reaper] ERROR: failed to spawn reaper task");
            None
        }
    }
}
