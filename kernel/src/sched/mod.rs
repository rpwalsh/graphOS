// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Preemptive round-robin scheduler.
//!
//! ## Design
//! - Single-core only; no per-CPU run queue.
//! - Tasks are dispatched in round-robin order from the task table.
//! - A PIT timer fires every 1 ms and decrements a per-time-slice counter.
//!   When it reaches zero, the ISR calls `preempt()` which performs a
//!   context switch.
//!
//! ## Interrupt safety
//! Both `schedule()` and `preempt()` bracket the switch with CLI/STI.
//! `preempt()` is called from the timer ISR where IF is already 0, but
//! we issue CLI defensively. After `switch_context` returns (the old
//! task has been resumed), we restore IF via STI.
//!
//! ## Boot task (index 0)
//! The boot thread (graphos_kmain) is not a "real" task — it is the code
//! that runs before any task is dispatched. We represent it with a special
//! boot context at index 0 in the CONTEXTS array. The scheduler saves
//! into it on the first switch.
//!
//! ## Returning from a task
//! When a task's entry function returns, it lands in `task_return_trampoline`
//! which marks the task `Dead` and reschedules. If no Ready tasks remain,
//! the trampoline resumes the boot thread.
//!
//! ## Limitations
//! - Single-core only — no per-CPU run queue.
//! - No priority levels (yet).

pub mod percpu;

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use spin::Mutex;

use crate::arch::serial;
use crate::arch::timer;
use crate::task::context::CpuContext;
use crate::task::switch;
use crate::task::table;

// ============================================================================
// Timed sleep table
// ============================================================================

/// Sentinel `ChannelUuid` used to mark tasks blocked in a timed sleep
/// (not waiting for any real IPC channel).
pub const SLEEP_CHANNEL_SENTINEL: crate::uuid::ChannelUuid =
    crate::uuid::ChannelUuid(crate::uuid::Uuid128::from_u64_pair(u64::MAX, u64::MAX));

/// One entry in the timed-sleep table.
struct TimedSleepSlot {
    /// Task table index waiting.
    task_index: usize,
    /// Monotonic tick count at which this task should wake.
    wake_tick: u64,
    /// Whether this slot is occupied.
    valid: bool,
}

impl TimedSleepSlot {
    const fn empty() -> Self {
        Self {
            task_index: 0,
            wake_tick: 0,
            valid: false,
        }
    }
}

/// Maximum concurrent timed sleeps.
const TIMED_SLEEP_CAPACITY: usize = 64;

static TIMED_SLEEP_TABLE: Mutex<[TimedSleepSlot; TIMED_SLEEP_CAPACITY]> =
    Mutex::new([const { TimedSleepSlot::empty() }; TIMED_SLEEP_CAPACITY]);

// ---------------------------------------------------------------------------
// Sleep overflow queue — used when TIMED_SLEEP_TABLE is full.
// Drained on every tick alongside the main table so overflowed sleeps still
// wake correctly.  Never busy-spins.
// ---------------------------------------------------------------------------

/// Capacity of the overflow queue.
const SLEEP_OVERFLOW_CAP: usize = 64;

static SLEEP_OVERFLOW: Mutex<[TimedSleepSlot; SLEEP_OVERFLOW_CAP]> =
    Mutex::new([const { TimedSleepSlot::empty() }; SLEEP_OVERFLOW_CAP]);

/// Put the current task to sleep for at least `n` ticks (1 tick ≈ 1 ms).
///
/// Actual wake latency is 1–2 ms depending on when the next PIT tick fires.
/// The calling task must be in `Running` state.
pub fn sleep_for_ticks(n: u64) {
    if n == 0 {
        return;
    }
    let cur = load_current_index();
    if cur == 0 {
        // Boot thread: busy-wait rather than blocking.
        let deadline = timer::ticks().saturating_add(n);
        while timer::ticks() < deadline {
            core::hint::spin_loop();
        }
        return;
    }
    let wake_tick = timer::ticks().saturating_add(n);
    // Insert into timed sleep table (must be inside without_interrupts to
    // prevent the timer ISR from racing against the insertion).
    let inserted = crate::arch::interrupts::without_interrupts(|| {
        let mut tbl = TIMED_SLEEP_TABLE.lock();
        for slot in tbl.iter_mut() {
            if !slot.valid {
                slot.task_index = cur;
                slot.wake_tick = wake_tick;
                slot.valid = true;
                return true;
            }
        }
        false
    });
    if !inserted {
        // Main table full — spill into overflow queue.  This avoids a
        // busy-spin that would stall the only CPU core; the overflow queue
        // is drained on every tick alongside the main table.
        let spilled = crate::arch::interrupts::without_interrupts(|| {
            let mut ovf = SLEEP_OVERFLOW.lock();
            for slot in ovf.iter_mut() {
                if !slot.valid {
                    slot.task_index = cur;
                    slot.wake_tick = wake_tick;
                    slot.valid = true;
                    return true;
                }
            }
            false
        });
        if !spilled {
            // Both tables full — this is a scheduler capacity limit.
            // Log and yield rather than busy-spinning so other tasks run.
            serial::write_line(b"[sched] sleep_for_ticks: all sleep slots full, yielding");
            unsafe { self::schedule() };
            return;
        }
    }
    // Block and yield. When tick_advance() fires and wakes us, schedule()
    // will return here.
    if table::mark_blocked(cur, SLEEP_CHANNEL_SENTINEL) {
        unsafe { self::schedule() };
    }
}

/// Called from the PIT timer ISR on every tick.
///
/// Scans the timed-sleep table and wakes any tasks whose deadline has
/// passed. Runs with interrupts disabled (ISR context).
///
/// # Safety
/// Must be called only from the IRQ 0 handler (interrupts already disabled).
pub fn tick_advance() {
    let now = timer::ticks();
    // Avoid locking if nothing is pending — fast path.
    // We use try_lock to avoid deadlock if another ISR somehow holds the
    // lock (should not happen on single-core, but defensive).
    let Some(mut tbl) = TIMED_SLEEP_TABLE.try_lock() else {
        return;
    };
    for slot in tbl.iter_mut() {
        if slot.valid && now >= slot.wake_tick {
            slot.valid = false;
            // wake_task transitions Blocked → Ready without calling schedule().
            table::wake_task(slot.task_index);
        }
    }
    drop(tbl);
    // Drain overflow queue with the same try_lock discipline.
    let Some(mut ovf) = SLEEP_OVERFLOW.try_lock() else {
        return;
    };
    for slot in ovf.iter_mut() {
        if slot.valid && now >= slot.wake_tick {
            slot.valid = false;
            table::wake_task(slot.task_index);
        }
    }

}

/// Index of the currently running task in the task table.
/// 0 = boot thread (before first dispatch or after boot thread resumed).
static CURRENT_INDEX: AtomicUsize = AtomicUsize::new(0);
/// Index of the desktop shell task. Set during boot once the init task is created.
static DESKTOP_INDEX: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Raised by desktop-relevant activity so the scheduler can wake promptly.
static INTERACTIVE_WAKE: AtomicBool = AtomicBool::new(false);

/// Whether the scheduler has been initialised.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Dedicated saved context for the boot thread. This must not alias any task slot.
static mut BOOT_CONTEXT: CpuContext = CpuContext::zero();

#[inline]
fn preferred_desktop_task(_current: usize) -> Option<usize> {
    // Keep scheduling deterministic while we stabilise the display loop.
    // Plain round-robin avoids starving other tasks when the compositor
    // emits frequent wakeups or commits.
    None
}

#[inline]
fn load_current_index() -> usize {
    if percpu::is_active() {
        percpu::current_on_cpu(percpu::current_cpu_index())
            .unwrap_or_else(|| CURRENT_INDEX.load(Ordering::Relaxed))
    } else {
        CURRENT_INDEX.load(Ordering::Relaxed)
    }
}

#[inline]
fn store_current_index(index: usize) {
    CURRENT_INDEX.store(index, Ordering::Relaxed);
    if percpu::is_active() {
        percpu::set_current_on_cpu(index);
    }
}

/// Return the current task's table index.
pub fn current_index() -> usize {
    load_current_index()
}

/// Returns the current monotonic tick count in milliseconds (1 tick ≈ 1 ms).
#[inline]
pub fn current_tick_ms() -> u64 {
    timer::ticks()
}

/// Wake a task by index (Blocked → Ready) without forcing an immediate reschedule.
/// Used by the grid layer to unblock a task waiting for a remote reply.
pub fn wake_task(task_index: usize) {
    table::wake_task(task_index);
}

pub fn register_desktop_task(index: usize) {
    DESKTOP_INDEX.store(index, Ordering::Release);
}

pub fn desktop_task_index() -> usize {
    DESKTOP_INDEX.load(Ordering::Acquire)
}

pub fn wake_desktop_task() {
    let desktop = DESKTOP_INDEX.load(Ordering::Acquire);
    if desktop != usize::MAX {
        table::wake_task(desktop);
    }
}

/// Mark a desktop-relevant event without disturbing the scheduler quantum.
///
/// Previously this set `INTERACTIVE_WAKE=true` and shrank the quantum to 1 tick
/// to bias the next pick toward the desktop task. That made scheduling
/// non-deterministic (boot-to-boot variance, app starvation after the first
/// commit) and caused a feedback loop with the ring-3 compositor's flush IPC.
/// We now keep the flag for backward compatibility but no longer touch the
/// quantum or short-circuit the round-robin pick.
pub fn notify_desktop_activity() {
    INTERACTIVE_WAKE.store(true, Ordering::Release);
}

pub fn notify_interactive_input() {
    notify_desktop_activity();
    wake_desktop_task();
}

pub fn clear_interactive_input() {
    INTERACTIVE_WAKE.store(false, Ordering::Release);
}

fn kernel_cr3() -> u64 {
    crate::mm::page_table::active_pml4()
}

#[inline]
unsafe fn ensure_task_interrupts_enabled(index: usize) {
    let ctx = unsafe { table::context_ptr_mut(index) };
    if !ctx.is_null() {
        unsafe {
            // Keep IF=1 in the saved context so a task resumed after
            // scheduling cannot run with interrupts permanently disabled.
            (*ctx).rflags |= 0x200;
        }
    }
}

unsafe fn install_runtime_state_for(index: usize) {
    let stack_top = table::stack_top_at(index);
    if stack_top != 0 {
        crate::arch::x86_64::gdt::set_kernel_stack(stack_top);
        crate::arch::x86_64::ring3::set_syscall_kernel_stack(stack_top);
    }

    let task_cr3 = table::task_cr3_at(index);
    let next_cr3 = if task_cr3 != 0 {
        task_cr3
    } else {
        kernel_cr3()
    };
    if next_cr3 != 0 {
        unsafe { crate::mm::page_table::load_address_space(next_cr3) };
    }
}

/// Initialise the scheduler.
///
/// Must be called after the task table has been populated with at least
/// one Ready task.
pub fn init() {
    serial::write_line(b"[sched] Scheduler initialised (preemptive round-robin, 10ms slice)");
    store_current_index(0);
    // Preserve any desktop task registration done during boot task setup.
    // Clearing this here breaks wake_desktop_task()/interactive prioritization
    // for ring-3 surface commits.
    INTERACTIVE_WAKE.store(false, Ordering::Relaxed);
    INITIALIZED.store(true, Ordering::Release);
}

/// Run the first Ready task. Called once from the boot sequence.
///
/// This saves the boot thread's context and switches to the first Ready
/// task. When the boot thread is eventually resumed (all tasks are Dead
/// or yielded back), execution returns here.
///
/// # Safety
/// Must be called during single-threaded early init. The task table must
/// contain at least one Ready task.
pub unsafe fn dispatch_first() {
    if !INITIALIZED.load(Ordering::Acquire) {
        serial::write_line(b"[sched] ERROR: scheduler not initialized");
        return;
    }

    // Find the first Ready task (search from index 0).
    let next_idx = match table::find_next_ready(0) {
        Some(idx) => idx,
        None => {
            serial::write_line(b"[sched] No Ready tasks to dispatch");
            return;
        }
    };

    serial::write_bytes(b"[sched] First dispatch -> task at index ");
    serial::write_u64_dec_inline(next_idx as u64);
    serial::write_bytes(b" (id=");
    serial::write_u64_dec_inline(table::task_id_at(next_idx));
    serial::write_line(b")");

    table::mark_running(next_idx);
    unsafe { ensure_task_interrupts_enabled(next_idx) };
    unsafe { install_runtime_state_for(next_idx) };

    // Save the boot thread into a dedicated context, never into a task slot.
    let old_ctx = core::ptr::addr_of_mut!(BOOT_CONTEXT);
    let new_ctx = unsafe { table::context_ptr_mut(next_idx) };

    if old_ctx.is_null() || new_ctx.is_null() {
        serial::write_line(b"[sched] ERROR: null context pointer");
        return;
    }

    store_current_index(next_idx);

    // Perform the switch. This saves the boot thread's state into
    // CONTEXTS[0] and jumps to the new task's entry point.
    unsafe {
        switch::switch_context(old_ctx, new_ctx);
    }

    // If we reach here, we were switched back to (boot thread resumed).
    // Process any pending reap from the trampoline that resumed us.
    process_pending_reap();
    serial::write_line(b"[sched] Boot thread resumed after task dispatch");
}

/// Yield the current task: find the next Ready task and switch to it.
///
/// If no other Ready task exists, the current task continues running.
///
/// # Safety
/// Must be called from a task context (not from interrupt handlers —
/// use `preempt()` for that).
pub unsafe fn schedule() {
    // Disable interrupts around the switch to prevent re-entrant timer
    // preemption while we are manipulating the task table and stack.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    let cur = load_current_index();

    let next_idx = match preferred_desktop_task(cur)
        .or_else(|| percpu::find_next_ready_percpu())
        .or_else(|| table::find_next_ready(cur))
    {
        Some(idx) => idx,
        None => {
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
            return;
        }
    };

    if next_idx == cur {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    // Mark current as Ready, next as Running.
    table::mark_ready(cur);
    table::mark_running(next_idx);
    unsafe { ensure_task_interrupts_enabled(next_idx) };
    unsafe { install_runtime_state_for(next_idx) };

    let old_ctx = unsafe { table::context_ptr_mut(cur) };
    let new_ctx = unsafe { table::context_ptr_mut(next_idx) };

    if old_ctx.is_null() || new_ctx.is_null() {
        serial::write_line(b"[sched] ERROR: null context in schedule()");
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    store_current_index(next_idx);

    // Keep the boot and desktop path on a fixed, deterministic round-robin
    // quantum. Higher-level analytics may observe the system, but they do not
    // get to perturb core dispatch timing during bring-up.
    timer::reset_quantum();

    unsafe {
        switch::switch_context(old_ctx, new_ctx);
    }

    unsafe { install_runtime_state_for(cur) };

    // Resumed after being switched out. Process any pending reap from a
    // trampoline that may have switched to us.
    process_pending_reap();

    // Re-enable interrupts after we've been switched back in.
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

/// Preemptive reschedule — called from the timer IRQ handler.
///
/// This is the interrupt-driven counterpart to `schedule()`. The timer
/// ISR enters with IF=0, but we issue CLI defensively to guard against
/// any future call-site that might not have IF=0.
///
/// # Safety
/// Must be called from an interrupt handler or with interrupts disabled.
pub unsafe fn preempt() {
    // C2 fix: always CLI — defensive, costs 1 cycle, prevents nested preemption.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    if !INITIALIZED.load(Ordering::Acquire) {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    let cur = load_current_index();

    let next_idx = match preferred_desktop_task(cur)
        .or_else(|| percpu::find_next_ready_percpu())
        .or_else(|| table::find_next_ready(cur))
    {
        Some(idx) => idx,
        None => {
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
            return;
        }
    };

    if next_idx == cur {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    // Capture time-slice usage before reset — event-driven twin telemetry.
    let q_remaining = timer::quantum_remaining();
    let q_used = timer::SCHED_TIME_SLICE_TICKS.saturating_sub(q_remaining);

    table::mark_ready(cur);
    table::mark_running(next_idx);
    unsafe { ensure_task_interrupts_enabled(next_idx) };
    unsafe { install_runtime_state_for(next_idx) };

    let old_ctx = unsafe { table::context_ptr_mut(cur) };
    let new_ctx = unsafe { table::context_ptr_mut(next_idx) };

    if old_ctx.is_null() || new_ctx.is_null() {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    store_current_index(next_idx);

    // Predictive slice sizing: twin-informed adaptive scheduling.
    let _hint = crate::graph::twin::query_sched_hint();

    // Feed the digital twin — context switch is the natural CPU-util event.
    crate::graph::twin::ingest_context_switch(
        timer::ticks(),
        q_used,
        timer::SCHED_TIME_SLICE_TICKS,
    );

    // Keep the boot and desktop path on a fixed, deterministic round-robin
    // quantum. Higher-level analytics may observe the system, but they do not
    // get to perturb core dispatch timing during bring-up.
    timer::reset_quantum();

    unsafe {
        switch::switch_context(old_ctx, new_ctx);
    }

    unsafe { install_runtime_state_for(cur) };

    // Resumed after being switched out. Process any pending reap.
    process_pending_reap();

    // M7 fix: after being switched back in, explicitly re-enable interrupts.
    // The popfq in switch_context restores the saved RFLAGS (which has IF=1
    // for a previously-running task), but for the first switch-in from a fresh
    // context, RFLAGS may have IF=0 if saved during an IRQ. Belt-and-suspenders.
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

// ============================================================================
// Pending reap tracking
// ============================================================================

/// Information about a task pending garbage collection.
///
/// When the trampoline switches away from a dead task, it stores the dead
/// task's info here. The next task to run (or the boot thread) checks this
/// and queues the task for reaping.
struct PendingReap {
    /// Task table index of the dead task.
    task_index: usize,
    /// Stack base address.
    stack_base: u64,
    /// Whether there's a pending reap.
    valid: bool,
}

static PENDING_REAP: Mutex<PendingReap> = Mutex::new(PendingReap {
    task_index: 0,
    stack_base: 0,
    valid: false,
});

/// Report whether a dead task is still waiting to be handed off to the reaper.
pub fn has_pending_reap() -> bool {
    PENDING_REAP.lock().valid
}

/// Check for and process any pending reap from a previous trampoline.
///
/// Called after resuming from a context switch. If the previous switch was
/// from a dead task's trampoline, we queue it for garbage collection.
fn process_pending_reap() {
    let pending = {
        let mut p = PENDING_REAP.lock();
        if p.valid {
            let info = (p.task_index, p.stack_base);
            p.valid = false;
            Some(info)
        } else {
            None
        }
    };

    if let Some((idx, base)) = pending {
        crate::task::reaper::queue_for_reap(idx, base);
    }
}

/// Switch away from the current task after it has terminated.
///
/// This is the single teardown path for tasks that return normally or call
/// `sys_exit()`. It marks the task Dead, stages the stack for the reaper, and
/// switches either to the next Ready task or back to the boot thread.
///
/// # Safety
/// Must be called from a live task context on the current CPU.
unsafe fn terminate_current_task(log_reason: &[u8]) -> ! {
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    let cur = load_current_index();
    if cur == 0 {
        serial::write_line(b"[sched] FATAL: boot thread attempted task teardown");
        loop {
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
        }
    }

    serial::write_bytes(b"[sched] ");
    serial::write_bytes(log_reason);
    serial::write_bytes(b" at index ");
    serial::write_u64_dec_inline(cur as u64);
    serial::write_bytes(b" (id=");
    serial::write_u64_dec_inline(table::task_id_at(cur));
    serial::write_line(b")");

    let stack_base = table::stack_base_at(cur);
    table::mark_dead(cur);

    {
        let mut p = PENDING_REAP.lock();
        p.task_index = cur;
        p.stack_base = stack_base;
        p.valid = true;
    }

    let old_ctx = unsafe { table::context_ptr_mut(cur) };

    match table::find_next_ready(cur) {
        Some(idx) => {
            table::mark_running(idx);
            unsafe { ensure_task_interrupts_enabled(idx) };
            unsafe { install_runtime_state_for(idx) };
            let new_ctx = unsafe { table::context_ptr_mut(idx) };
            store_current_index(idx);
            unsafe {
                switch::switch_context(old_ctx, new_ctx);
            }
        }
        None => {
            serial::write_line(b"[sched] No Ready tasks remain - resuming boot thread");
            let kernel_root = kernel_cr3();
            if kernel_root != 0 {
                unsafe { crate::mm::page_table::load_address_space(kernel_root) };
            }
            let new_ctx = core::ptr::addr_of!(BOOT_CONTEXT);
            store_current_index(0);
            unsafe {
                switch::switch_context(old_ctx, new_ctx);
            }
        }
    }

    serial::write_line(b"[sched] FATAL: dead task resumed after teardown");
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

/// Exit the currently running task and never return.
///
/// This is used by `sys_exit()` so explicit task termination follows the same
/// path as a task whose entry function returns.
///
/// # Safety
/// Must be called from task context, not from the boot thread or an ISR.
pub unsafe fn exit_current_task() -> ! {
    unsafe { terminate_current_task(b"task exited") }
}

#[inline(never)]
fn user_task_bootstrap() {
    let cur = current_index();
    if !table::is_user_task(cur) {
        serial::write_line(b"[sched] FATAL: user bootstrap entered for kernel task");
        loop {
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
        }
    }

    unsafe { install_runtime_state_for(cur) };

    let user_rip = table::task_user_rip_at(cur);
    let user_rsp = table::task_user_rsp_at(cur);
    serial::write_bytes(b"[sched] entering ring3 task id=");
    serial::write_u64_dec_inline(table::task_id_at(cur));
    serial::write_bytes(b" rip=");
    serial::write_hex_inline(user_rip);
    serial::write_bytes(b" rsp=");
    serial::write_hex(user_rsp);
    serial::write_bytes(b"[sched] ring3 jump armed task id=");
    serial::write_u64_dec(table::task_id_at(cur));

    let thread_arg = table::task_thread_arg_at(cur);
    if thread_arg != 0 {
        unsafe {
            crate::arch::x86_64::ring3::enter_user_mode_with_arg(user_rip, user_rsp, thread_arg)
        }
    } else {
        unsafe { crate::arch::x86_64::ring3::enter_user_mode(user_rip, user_rsp) }
    }
}

/// Trampoline for tasks whose entry function returns.
///
/// When a task's entry function returns, it `ret`s into this function
/// (because we pushed its address onto the stack before first dispatch).
/// We mark the task Dead and attempt to schedule the next one. If no
/// tasks remain, we resume the boot thread.
#[inline(never)]
fn task_return_trampoline() {
    unsafe { terminate_current_task(b"task returned") }

    /*
    // Disable interrupts for the switch sequence.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    let cur = load_current_index();

    serial::write_bytes(b"[sched] Task at index ");
    serial::write_u64_dec_inline(cur as u64);
    serial::write_line(b" returned - marking Dead");

    // Get the stack base BEFORE marking dead (mark_dead doesn't clear it,
    // but we want to be defensive).
    let stack_base = table::stack_base_at(cur);

    table::mark_dead(cur);

    // Store pending reap info. The next task to run will process this.
    {
        let mut p = PENDING_REAP.lock();
        p.task_index = cur;
        p.stack_base = stack_base;
        p.valid = true;
    }

    // Try to schedule the next Ready task.
    let next_idx = table::find_next_ready(cur);
    match next_idx {
        Some(idx) => {
            table::mark_running(idx);
            let old_ctx = unsafe { table::context_ptr_mut(cur) };
            let new_ctx = unsafe { table::context_ptr_mut(idx) };
            store_current_index(idx);
            unsafe {
                switch::switch_context(old_ctx, new_ctx);
            }
            // Resumed from being switched out. Process any pending reap
            // and re-enable interrupts.
            process_pending_reap();
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        }
        None => {
            // No more tasks. Resume the boot thread from its dedicated context.
            serial::write_line(b"[sched] No more Ready tasks - resuming boot thread");
            let old_ctx = unsafe { table::context_ptr_mut(cur) };
            let new_ctx = core::ptr::addr_of!(BOOT_CONTEXT);
            store_current_index(0);
            unsafe {
                switch::switch_context(old_ctx, new_ctx);
            }
            // Boot thread resumed. Process pending reap.
            process_pending_reap();
            unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        }
    }

    // Should not be reached — but just in case:
    serial::write_line(b"[sched] FATAL: trampoline fell through");
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
    */
}

/// Returns the address of the task return trampoline.
///
/// This is pushed onto a new task's stack so that when the task's entry
/// function returns, execution jumps to the trampoline.
#[inline(never)]
pub fn task_return_trampoline_addr() -> u64 {
    task_return_trampoline as *const () as u64
}

/// Returns the address of the user-task bootstrap trampoline.
#[inline(never)]
pub fn user_task_bootstrap_addr() -> u64 {
    user_task_bootstrap as *const () as u64
}

/// Run a specific task on the current AP (called from the AP scheduler loop).
///
/// Performs a context switch into `task_idx`, marks it Running, saves the AP's
/// idle context, and returns when the task has been preempted or yielded.
///
/// # Safety
/// Must be called from an AP's idle loop with interrupts enabled.
pub unsafe fn run_on_ap(task_idx: usize) {
    // Brief CLI while we update the tables and perform the switch.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };

    if !table::is_ready(task_idx) {
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    table::mark_running(task_idx);
    unsafe { ensure_task_interrupts_enabled(task_idx) };
    unsafe { install_runtime_state_for(task_idx) };

    let ap_ctx = percpu::ap_idle_context_ptr();
    let task_ctx = unsafe { table::context_ptr_mut(task_idx) };
    if ap_ctx.is_null() || task_ctx.is_null() {
        table::mark_ready(task_idx);
        unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
        return;
    }

    store_current_index(task_idx);
    percpu::set_current_on_cpu(task_idx);

    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
    unsafe { switch::switch_context(ap_ctx, task_ctx) };
    // Task yielded / was preempted — we're back in the AP idle context.
    process_pending_reap();
}
