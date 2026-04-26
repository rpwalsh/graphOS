// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Task table — fixed-size table of all tasks in the system.
//!
//! The kernel pre-allocates a static array of `MAX_TASKS` slots. Task
//! creation fills the next free slot; task destruction marks it `Dead`
//! and reclaims the kernel stack.
//!
//! ## Concurrency
//! The task metadata (state, id, name, etc.) is behind a `spin::Mutex`.
//! CPU contexts are stored in a **separate** `static mut` array so that
//! the scheduler can obtain stable pointers without holding the lock
//! across a context switch (which would deadlock on single-core).
//!
//! ## Lock ordering (C4 fix)
//! System-wide lock acquisition order (always acquire in this order):
//!   1. task::table::TABLE
//!   2. ipc::channel::TABLE
//!   3. vfs::OPEN_FILES
//!   4. vfs::MOUNTS
//!   5. graph::arena::ARENA
//!   6. graph::twin::TWIN
//!   7. mm::frame_alloc::POOL
//!   8. task::table::NEXT_ID
//!
//! ## Why keep this static?
//! Even with a bootstrap heap now available, the task table and context
//! arrays stay in BSS so the scheduler has stable addresses and bounded
//! failure modes during early bring-up.

use crate::arch::interrupts;
use spin::Mutex;

use crate::arch::serial;
use crate::graph::handles::GraphHandle;
use crate::mm::address_space::AddressSpace;
use crate::uuid::ChannelUuid;

use super::context::CpuContext;
use super::tcb::{Task, TaskId, TaskState};

/// Maximum number of tasks.
pub const MAX_TASKS: usize = 64;

/// Next task ID to assign. Monotonically increasing.
static NEXT_ID: Mutex<TaskId> = Mutex::new(1);

/// The global task table (metadata only — no CpuContext).
static TABLE: Mutex<TaskTable> = Mutex::new(TaskTable::new());

/// CPU contexts live **outside** the Mutex so the scheduler can hold
/// stable pointers across context switches without deadlocking.
///
/// # Safety
/// Access is safe because:
/// - Slot `i` is only written during `create_kernel_task` (single writer,
///   task not yet scheduled) or by the switch stub when the task is the
///   *current* task (only one CPU writes its own context).
/// - The scheduler reads `CONTEXTS[next]` only after confirming the slot
///   is occupied and the task is Ready (via the TABLE lock).
static mut CONTEXTS: [CpuContext; MAX_TASKS] = [CpuContext::zero(); MAX_TASKS];

struct TaskTable {
    tasks: [Option<Task>; MAX_TASKS],
    count: usize,
}

fn seed_default_ipc_caps(task: &mut Task, service_name: &[u8]) {
    if task.is_user {
        // Least-privilege user default: bootstrap/status control channel only.
        // Use UUID v5 derived from the service name — no hardcoded integer alias.
        let bootstrap_uuid = ChannelUuid::from_service_name(b"bootstrap");
        let _ = task.grant_ipc_capability(bootstrap_uuid, crate::ipc::capability::CAP_SEND);
        let _ = task.grant_ipc_capability(bootstrap_uuid, crate::ipc::capability::CAP_RECV);
        // Also grant the task RECV on its own service inbox so early channel_recv
        // calls (e.g. claim_inbox) succeed before userland::grant_service_ipc_caps runs.
        if !service_name.is_empty() {
            let own_uuid = ChannelUuid::from_service_name(service_name);
            let _ = task.grant_ipc_capability(own_uuid, crate::ipc::capability::CAP_RECV);
        }
        return;
    }

    // Kernel/root tasks retain full channel management authority.
    // Grant CAP_MANAGE for all well-known service inbox channels.
    for name in [
        b"servicemgr".as_slice(),
        b"graphd".as_slice(),
        b"modeld".as_slice(),
        b"trainerd".as_slice(),
        b"artifactsd".as_slice(),
        b"sysd".as_slice(),
        b"compositor".as_slice(),
        b"init".as_slice(),
        b"bootstrap".as_slice(),
    ] {
        let uuid = ChannelUuid::from_service_name(name);
        let _ = task.grant_ipc_capability(uuid, crate::ipc::capability::CAP_MANAGE);
    }
}

impl TaskTable {
    const fn new() -> Self {
        const NONE: Option<Task> = None;
        Self {
            tasks: [NONE; MAX_TASKS],
            count: 0,
        }
    }
}

/// Allocate a kernel stack and a task ID, then create a kernel task that
/// will begin executing at `entry_fn`.
///
/// Returns the assigned `TaskId`, or `None` if the table is full or if
/// stack allocation fails.
///
/// The task starts in `Ready` state with its context pointing at `entry_fn`.
///
/// NOTE: Task stacks are allocated from the pre-reserved stack pool
/// (stack_pool module) to guarantee physically contiguous frames under
/// identity mapping. The pool must be initialized before creating tasks.
pub fn create_kernel_task(name: &[u8], entry_fn: u64) -> Option<TaskId> {
    // Allocate kernel stack from pre-reserved pool.
    let (stack_base, stack_top) = match super::stack_pool::alloc_stack() {
        Some(s) => s,
        None => {
            serial::write_line(b"[task] ERROR: no free stacks in pool");
            return None;
        }
    };

    // Push the return trampoline address onto the stack.
    // When the task's entry function returns, `ret` pops this address,
    // causing execution to jump to the trampoline which marks the task
    // Dead and reschedules.
    let trampoline_addr = crate::sched::task_return_trampoline_addr();
    let adjusted_stack_top = stack_top - 8; // Make room for return address
    unsafe {
        let ret_addr_ptr = adjusted_stack_top as *mut u64;
        core::ptr::write_volatile(ret_addr_ptr, trampoline_addr);
    }

    let ctx = CpuContext::new_kernel(entry_fn, adjusted_stack_top);

    interrupts::without_interrupts(|| {
        let id = {
            let mut next = NEXT_ID.lock();
            let id = *next;
            *next += 1;
            id
        };

        // Build the name field: copy up to 31 bytes, null-terminate.
        let mut name_buf = [0u8; 32];
        let copy_len = name.len().min(31);
        name_buf[..copy_len].copy_from_slice(&name[..copy_len]);

        let mut task = Task::new(id, name_buf);
        task.stack_base = stack_base;
        task.state = TaskState::Ready;
        seed_default_ipc_caps(&mut task, b"");

        let mut table = TABLE.lock();
        if table.count >= MAX_TASKS {
            serial::write_line(b"[task] ERROR: task table full");
            drop(table);
            super::stack_pool::free_stack(stack_base);
            return None;
        }

        for (slot_idx, slot) in table.tasks.iter_mut().enumerate() {
            if slot.is_none() {
                unsafe {
                    let ctx_ptr = core::ptr::addr_of_mut!(CONTEXTS[slot_idx]);
                    ctx_ptr.write(ctx);
                }
                *slot = Some(task);
                let _ = crate::security::seccomp::set_allow_all(slot_idx, true);
                // Graph-first: register a Task node so every kernel task
                // is a first-class citizen in the heterogeneous graph.
                // Lock order: TABLE(1) → ARENA(5) — always in order, safe.
                let gnode =
                    crate::graph::handles::register_task(crate::graph::types::NODE_ID_KERNEL, 0);
                if let Some(t) = slot.as_mut() {
                    t.graph_node = gnode.node_id();
                }
                table.count += 1;

                serial::write_bytes(b"[task] created id=");
                serial::write_u64_dec_inline(id);
                serial::write_bytes(b" name=");
                serial::write_bytes(&name_buf[..copy_len]);
                serial::write_bytes(b" stack=");
                serial::write_hex_inline(stack_base);
                serial::write_bytes(b"...");
                serial::write_hex_inline(stack_top);
                serial::write_bytes(b" entry=");
                serial::write_hex(entry_fn);

                crate::sched::percpu::enqueue_current_cpu(slot_idx);

                return Some(id);
            }
        }

        drop(table);
        super::stack_pool::free_stack(stack_base);
        None
    })
}

/// Like `create_kernel_task`, but also returns the task table index.
///
/// This is needed by subsystems (like the reaper) that need to know their
/// own index for event-driven wake mechanisms.
///
/// Returns `(TaskId, table_index)` or `None` if creation failed.
pub fn create_kernel_task_with_index(name: &[u8], entry_fn: u64) -> Option<(TaskId, usize)> {
    // Allocate kernel stack from pre-reserved pool.
    let (stack_base, stack_top) = match super::stack_pool::alloc_stack() {
        Some(s) => s,
        None => {
            serial::write_line(b"[task] ERROR: no free stacks in pool");
            return None;
        }
    };

    // Push the return trampoline address onto the stack.
    let trampoline_addr = crate::sched::task_return_trampoline_addr();
    let adjusted_stack_top = stack_top - 8;
    unsafe {
        let ret_addr_ptr = adjusted_stack_top as *mut u64;
        core::ptr::write_volatile(ret_addr_ptr, trampoline_addr);
    }

    let ctx = CpuContext::new_kernel(entry_fn, adjusted_stack_top);

    interrupts::without_interrupts(|| {
        let id = {
            let mut next = NEXT_ID.lock();
            let id = *next;
            *next += 1;
            id
        };

        let mut name_buf = [0u8; 32];
        let copy_len = name.len().min(31);
        name_buf[..copy_len].copy_from_slice(&name[..copy_len]);

        let mut task = Task::new(id, name_buf);
        task.stack_base = stack_base;
        task.state = TaskState::Ready;
        seed_default_ipc_caps(&mut task, b"");

        let mut table = TABLE.lock();
        if table.count >= MAX_TASKS {
            serial::write_line(b"[task] ERROR: task table full");
            drop(table);
            super::stack_pool::free_stack(stack_base);
            return None;
        }

        for (slot_idx, slot) in table.tasks.iter_mut().enumerate() {
            if slot.is_none() {
                unsafe {
                    let ctx_ptr = core::ptr::addr_of_mut!(CONTEXTS[slot_idx]);
                    ctx_ptr.write(ctx);
                }
                *slot = Some(task);
                let _ = crate::security::seccomp::set_allow_all(slot_idx, true);
                // Graph-first: register a Task node for this kernel task.
                let gnode =
                    crate::graph::handles::register_task(crate::graph::types::NODE_ID_KERNEL, 0);
                if let Some(t) = slot.as_mut() {
                    t.graph_node = gnode.node_id();
                }
                table.count += 1;

                serial::write_bytes(b"[task] created id=");
                serial::write_u64_dec_inline(id);
                serial::write_bytes(b" idx=");
                serial::write_u64_dec_inline(slot_idx as u64);
                serial::write_bytes(b" name=");
                serial::write_bytes(&name_buf[..copy_len]);
                serial::write_bytes(b" entry=");
                serial::write_hex(entry_fn);

                crate::sched::percpu::enqueue_current_cpu(slot_idx);

                return Some((id, slot_idx));
            }
        }

        drop(table);
        super::stack_pool::free_stack(stack_base);
        None
    })
}

fn finalize_user_task(
    name: &[u8],
    stack_base: u64,
    stack_top: u64,
    user_rip: u64,
    address_space: alloc::boxed::Box<AddressSpace>,
) -> Option<TaskId> {
    let trampoline_addr = crate::sched::task_return_trampoline_addr();
    let adjusted_stack_top = stack_top - 8;

    unsafe {
        let ret_addr_ptr = adjusted_stack_top as *mut u64;
        core::ptr::write_volatile(ret_addr_ptr, trampoline_addr);
    }

    let ctx = CpuContext::new_kernel(crate::sched::user_task_bootstrap_addr(), adjusted_stack_top);

    // Drain any pending reap entries before we search for a free slot.
    // The slot loop only accepts None entries; Dead-but-not-yet-reaped
    // slots (e.g. a kernel test task that just terminated) would cause
    // the search to fall through and return None even when capacity is
    // available.  Calling reap_pending() here synchronously reclaims
    // those slots before we hold TABLE inside without_interrupts.
    super::reaper::reap_pending();

    // address_space is already heap-allocated (Box<AddressSpace>).
    let task_cr3 = address_space.cr3();
    let task_user_rsp = address_space.user_stack_pointer();
    let boxed_address_space = address_space;

    interrupts::without_interrupts(|| {
        let id = {
            let mut next = NEXT_ID.lock();
            let id = *next;
            *next += 1;
            id
        };

        let mut name_buf = [0u8; 32];
        let copy_len = name.len().min(31);
        name_buf[..copy_len].copy_from_slice(&name[..copy_len]);

        let mut table = TABLE.lock();
        if table.count >= MAX_TASKS {
            serial::write_line(b"[task] ERROR: task table full");
            drop(table);
            super::stack_pool::free_stack(stack_base);
            return None;
        }

        for (slot_idx, slot) in table.tasks.iter_mut().enumerate() {
            if slot.is_none() {
                unsafe {
                    let ctx_ptr = core::ptr::addr_of_mut!(CONTEXTS[slot_idx]);
                    ctx_ptr.write(ctx);
                }

                *slot = Some(Task::new(id, name_buf));
                let Some(task) = slot.as_mut() else {
                    continue;
                };
                task.stack_base = stack_base;
                task.state = TaskState::Ready;
                task.is_user = true;
                task.cr3 = task_cr3;
                task.user_rip = user_rip;
                task.user_rsp = task_user_rsp;
                task.address_space = Some(boxed_address_space);
                seed_default_ipc_caps(task, &name_buf[..copy_len]);
                let _ = crate::security::seccomp::set_protected_strict(slot_idx);
                table.count += 1;

                serial::write_bytes(b"[task] created user task id=");
                serial::write_u64_dec_inline(id);
                serial::write_bytes(b" name=");
                serial::write_bytes(&name_buf[..copy_len]);
                serial::write_bytes(b" cr3=");
                serial::write_hex_inline(task_cr3);
                serial::write_bytes(b" entry=");
                serial::write_hex_inline(user_rip);
                serial::write_bytes(b" rsp=");
                serial::write_hex(task_user_rsp);

                crate::sched::percpu::enqueue_current_cpu(slot_idx);

                return Some(id);
            }
        }

        drop(table);
        super::stack_pool::free_stack(stack_base);
        None
    })
}

/// Allocate a kernel stack plus a private user address space and create a
/// task that enters ring 3 through the scheduler's user bootstrap path.
pub fn create_user_task(name: &[u8], image: &[u8]) -> Option<TaskId> {
    let (stack_base, stack_top) = match super::stack_pool::alloc_stack() {
        Some(s) => s,
        None => {
            serial::write_line(b"[task] ERROR: no free stacks in pool");
            return None;
        }
    };

    let kernel_pml4 = crate::mm::page_table::active_pml4();
    if kernel_pml4 == 0 {
        serial::write_line(b"[task] ERROR: no active kernel CR3 for user task");
        super::stack_pool::free_stack(stack_base);
        return None;
    }

    let mut address_space = match AddressSpace::new(kernel_pml4) {
        Some(space) => space,
        None => {
            serial::write_line(b"[task] ERROR: failed to create user address space");
            super::stack_pool::free_stack(stack_base);
            return None;
        }
    };

    let Some((user_rip, _code_pages)) = address_space.load_user_image(image) else {
        serial::write_line(b"[task] ERROR: failed to load user image");
        super::stack_pool::free_stack(stack_base);
        return None;
    };

    if !address_space.map_user_stack() {
        serial::write_line(b"[task] ERROR: failed to map user stack");
        super::stack_pool::free_stack(stack_base);
        return None;
    }

    finalize_user_task(name, stack_base, stack_top, user_rip, address_space)
}

/// Allocate a kernel stack plus a private address space and create a
/// ring-3 task from an ELF image.
pub fn create_user_task_from_elf(name: &[u8], image: &[u8]) -> Option<TaskId> {
    serial::write_bytes(b"[task] user-elf begin ");
    serial::write_line(name);

    let (stack_base, stack_top) = match super::stack_pool::alloc_stack() {
        Some(s) => s,
        None => {
            serial::write_line(b"[task] ERROR: no free stacks in pool");
            return None;
        }
    };

    let kernel_pml4 = crate::mm::page_table::active_pml4();
    if kernel_pml4 == 0 {
        serial::write_line(b"[task] ERROR: no active kernel CR3 for user ELF task");
        super::stack_pool::free_stack(stack_base);
        return None;
    }

    let mut address_space = match AddressSpace::new(kernel_pml4) {
        Some(space) => space,
        None => {
            serial::write_line(b"[task] ERROR: failed to create user address space");
            super::stack_pool::free_stack(stack_base);
            return None;
        }
    };
    serial::write_line(b"[task] user-elf addrspace ready");

    let Some(image_info) = address_space.load_elf_image(image) else {
        serial::write_line(b"[task] ERROR: failed to load user ELF image");
        super::stack_pool::free_stack(stack_base);
        return None;
    };
    serial::write_bytes(b"[task] user-elf entry=");
    serial::write_hex(image_info.entry);

    if !address_space.map_user_stack() {
        serial::write_line(b"[task] ERROR: failed to map user stack");
        super::stack_pool::free_stack(stack_base);
        return None;
    }
    serial::write_line(b"[task] user-elf stack ready");

    finalize_user_task(name, stack_base, stack_top, image_info.entry, address_space)
}

/// Log all tasks and their states to serial.
pub fn dump_all() {
    let table = TABLE.lock();
    serial::write_line(b"[task] === Task Table ===");
    serial::write_bytes(b"[task] count: ");
    serial::write_u64_dec(table.count as u64);
    for task in table.tasks.iter().flatten() {
        serial::write_bytes(b"  id=");
        serial::write_u64_dec_inline(task.id);
        serial::write_bytes(b" state=");
        let state_str = match task.state {
            TaskState::Created => b"Created " as &[u8],
            TaskState::Ready => b"Ready   ",
            TaskState::Running => b"Running ",
            TaskState::Blocked => b"Blocked ",
            TaskState::Dead => b"Dead    ",
        };
        serial::write_bytes(state_str);
        serial::write_bytes(b" prio=");
        serial::write_u64_dec_inline(task.priority as u64);
        serial::write_bytes(b" name=");
        serial::write_line(task.name_bytes());
    }
    serial::write_line(b"[task] === End Task Table ===");
}

// ====================================================================
// Scheduler-facing interface
// ====================================================================

/// Find the table index of the first `Ready` task, starting the search
/// after `after_index` (wrapping around). Returns `None` if no Ready task
/// exists.
pub fn find_next_ready(after_index: usize) -> Option<usize> {
    let table = TABLE.lock();
    let mut best_priority: Option<u8> = None;
    for task in table.tasks.iter().flatten() {
        if task.state == TaskState::Ready {
            best_priority = Some(match best_priority {
                Some(current) => current.max(task.priority),
                None => task.priority,
            });
        }
    }

    let best_priority = best_priority?;

    let len = table.tasks.len();
    for offset in 1..=len {
        let idx = (after_index + offset) % len;
        if let Some(ref task) = table.tasks[idx]
            && task.state == TaskState::Ready
            && task.priority == best_priority
        {
            return Some(idx);
        }
    }
    None
}

/// Mark the task at `index` as `Running`. Returns false if the slot is
/// empty or not in `Ready` state.
pub fn mark_running(index: usize) -> bool {
    let mut table = TABLE.lock();
    if let Some(ref mut task) = table.tasks[index]
        && task.state == TaskState::Ready
    {
        task.state = TaskState::Running;
        return true;
    }
    false
}

/// Mark the task at `index` as `Ready`. Returns false if the slot is
/// empty or not in `Running` state.
pub fn mark_ready(index: usize) -> bool {
    let mut table = TABLE.lock();
    if let Some(ref mut task) = table.tasks[index]
        && task.state == TaskState::Running
    {
        task.state = TaskState::Ready;
        return true;
    }
    false
}

/// Mark the task at `index` as `Dead`.
///
/// NOTE: The task's kernel stack is NOT immediately freed because the
/// trampoline may still be executing on it. Stack reclamation is deferred
/// to the reaper task via `queue_for_reap()`.
pub fn mark_dead(index: usize) {
    let mut table = TABLE.lock();
    let dead_id = match table.tasks[index] {
        Some(ref mut task) => {
            task.state = TaskState::Dead;
            task.wait_channel = ChannelUuid(crate::uuid::Uuid128::NIL);
            task.pending_wake_without_ipc = false;
            task.ipc_endpoint = 0;
            // Stack is intentionally NOT freed here - we may still be running on it.
            // The scheduler trampoline will call queue_for_reap() after switching
            // away from this task.
            task.id
        }
        None => return,
    };
    // Wake any tasks blocked in SYS_THREAD_JOIN waiting for this task.
    for (join_idx, slot) in table.tasks.iter_mut().enumerate() {
        if let Some(waiter) = slot.as_mut()
            && waiter.joining_on == dead_id
            && waiter.state == TaskState::Blocked
        {
            waiter.state = TaskState::Ready;
            waiter.joining_on = 0;
            crate::sched::percpu::enqueue_current_cpu(join_idx);
        }
    }
}

/// Get the stack base address for the task at `index`.
///
/// Returns 0 if the slot is empty.
pub fn stack_base_at(index: usize) -> u64 {
    let table = TABLE.lock();
    table.tasks[index].as_ref().map_or(0, |t| t.stack_base)
}

/// Get the top of the kernel stack for the task at `index`.
pub fn stack_top_at(index: usize) -> u64 {
    stack_base_at(index).saturating_add(super::tcb::KERNEL_STACK_SIZE as u64)
}

/// Clear a dead task's slot, making it available for reuse.
///
/// Called by the reaper after the task's stack has been freed.
///
/// # Safety
/// The caller must ensure:
/// - The task is Dead.
/// - The task's stack has already been freed via `stack_pool::free_stack()`.
/// - No references to the task remain.
pub fn clear_dead_slot(index: usize) {
    let mut table = TABLE.lock();
    if let Some(ref task) = table.tasks[index]
        && task.state != TaskState::Dead
    {
        serial::write_bytes(b"[task] WARNING: clear_dead_slot called on non-Dead task at ");
        serial::write_u64_dec(index as u64);
        return;
    }
    table.tasks[index] = None;
    if table.count > 0 {
        table.count -= 1;
    }
    // Also clear the context slot.
    unsafe {
        CONTEXTS[index] = CpuContext::zero();
    }
}

/// Mark the task at `index` as `Blocked` (waiting on a channel).
///
/// `channel_id` is the channel the task is waiting on — stored so that
/// `wake_blocked_on` can selectively wake only tasks waiting on a
/// specific channel.
///
/// Returns `false` if the slot is empty or not `Running`.
pub fn mark_blocked(index: usize, uuid: ChannelUuid) -> bool {
    let mut table = TABLE.lock();
    if let Some(ref mut task) = table.tasks[index]
        && task.state == TaskState::Running
    {
        task.state = TaskState::Blocked;
        task.wait_channel = uuid;
        task.pending_wake_without_ipc = false;
        return true;
    }
    false
}

/// Wake **all** `Blocked` tasks that are waiting on the given channel UUID.
pub fn wake_blocked_on(uuid: ChannelUuid) -> usize {
    let mut table = TABLE.lock();
    let mut count = 0usize;
    for (index, slot) in table.tasks.iter_mut().enumerate() {
        if let Some(task) = slot
            && task.state == TaskState::Blocked
            && task.wait_channel == uuid
        {
            task.state = TaskState::Ready;
            task.wait_channel = ChannelUuid(crate::uuid::Uuid128::NIL);
            task.pending_wake_without_ipc = false;
            crate::sched::percpu::enqueue_current_cpu(index);
            count += 1;
        }
    }
    count
}

/// Wake a specific task by index, regardless of what it's waiting on.
///
/// Used by event-driven subsystems (e.g., the reaper) that need to wake
/// a specific task directly rather than by channel.
///
/// Returns `true` if the task was Blocked and is now Ready.
/// Returns `false` if the slot is empty or the task wasn't Blocked.
pub fn wake_task(index: usize) -> bool {
    let mut table = TABLE.lock();
    if let Some(ref mut task) = table.tasks[index]
        && task.state == TaskState::Blocked
    {
        task.state = TaskState::Ready;
        task.wait_channel = ChannelUuid(crate::uuid::Uuid128::NIL);
        task.pending_wake_without_ipc = true;
        crate::sched::percpu::enqueue_current_cpu(index);
        return true;
    }
    false
}

/// Consume a direct wake token previously set by `wake_task`.
pub fn take_wake_without_ipc(index: usize) -> bool {
    let mut table = TABLE.lock();
    if let Some(Some(task)) = table.tasks.get_mut(index)
        && task.pending_wake_without_ipc
    {
        task.pending_wake_without_ipc = false;
        return true;
    }
    false
}

/// Set the scheduler priority hint for a task.
pub fn set_priority(index: usize, priority: u8) -> bool {
    let mut table = TABLE.lock();
    let Some(slot) = table.tasks.get_mut(index) else {
        return false;
    };
    if let Some(task) = slot.as_mut() {
        task.priority = priority;
        true
    } else {
        false
    }
}

/// Read the current scheduler priority hint for a task.
pub fn priority_at(index: usize) -> u8 {
    let table = TABLE.lock();
    match table.tasks.get(index) {
        Some(Some(task)) => task.priority,
        _ => 0,
    }
}

/// Return whether the task at `index` is currently ready.
pub fn is_ready(index: usize) -> bool {
    let table = TABLE.lock();
    matches!(table.tasks.get(index), Some(Some(task)) if task.state == TaskState::Ready)
}

/// Return a single-byte task state code for lightweight telemetry.
///
/// `C` = Created, `R` = Ready, `N` = Running, `B` = Blocked, `D` = Dead,
/// `-` = empty or out-of-range slot.
pub fn state_code(index: usize) -> u8 {
    let table = TABLE.lock();
    match table.tasks.get(index) {
        Some(Some(task)) => match task.state {
            TaskState::Created => b'C',
            TaskState::Ready => b'R',
            TaskState::Running => b'N',
            TaskState::Blocked => b'B',
            TaskState::Dead => b'D',
        },
        _ => b'-',
    }
}

/// Get a raw mutable pointer to the `CpuContext` of the task at `index`.
///
/// The context lives in a separate static array (not behind the Mutex),
/// so this pointer remains valid across lock acquisitions and context
/// switches. This is the C1 fix — no more dangling pointers.
///
/// # Safety
/// - `index` must be < MAX_TASKS.
/// - The caller must ensure the slot is occupied.
/// - Only one writer (the switch stub for the current task) at a time.
pub unsafe fn context_ptr_mut(index: usize) -> *mut CpuContext {
    if index >= MAX_TASKS {
        return core::ptr::null_mut();
    }
    unsafe { core::ptr::addr_of_mut!(CONTEXTS[index]) }
}

/// Get the task ID at `index`, or 0 if empty.
pub fn task_id_at(index: usize) -> TaskId {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.id,
        None => 0,
    }
}

/// Find the table index of the task with the given `TaskId`.
///
/// Returns `None` if no task with that ID exists.
pub fn task_index_by_id(id: TaskId) -> Option<usize> {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        for (idx, slot) in table.tasks.iter().enumerate() {
            if let Some(task) = slot
                && task.id == id
            {
                return Some(idx);
            }
        }
        None
    })
}

/// Find a runnable task by short task name.
///
/// Singleton services should only suppress relaunch if the existing instance
/// can still be scheduled. A Blocked task that never reached userspace should
/// not pin the service in a permanently half-launched state.
pub fn active_task_id_by_name(name: &[u8]) -> Option<TaskId> {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        for slot in table.tasks.iter() {
            if let Some(task) = slot
                && matches!(task.state, TaskState::Ready | TaskState::Running)
                && task.name_bytes() == name
            {
                return Some(task.id);
            }
        }
        None
    })
}

/// Record the IPC reply endpoint for the task at `index`.
///
/// Returns `false` if the slot is empty or out of range.
pub fn set_ipc_endpoint(index: usize, channel_id: u32) -> bool {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let Some(slot) = table.tasks.get_mut(index) else {
            return false;
        };
        if let Some(task) = slot.as_mut() {
            task.ipc_endpoint = channel_id;
            true
        } else {
            false
        }
    })
}

/// Get the IPC reply endpoint for the task at `index`, or 0 if none is set.
pub fn ipc_endpoint_at(index: usize) -> u32 {
    let table = TABLE.lock();
    match table.tasks.get(index) {
        Some(Some(task)) => task.ipc_endpoint,
        _ => 0,
    }
}

/// Grant IPC capability bits for a target task.
pub fn ipc_cap_grant(index: usize, channel: ChannelUuid, perms: u8) -> bool {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let Some(slot) = table.tasks.get_mut(index) else {
            return false;
        };
        if let Some(task) = slot.as_mut() {
            task.grant_ipc_capability(channel, perms)
        } else {
            false
        }
    })
}

/// Grant IPC capability bits by target TaskId.
pub fn ipc_cap_grant_by_task_id(task_id: TaskId, channel: ChannelUuid, perms: u8) -> bool {
    let Some(index) = task_index_by_id(task_id) else {
        return false;
    };
    ipc_cap_grant(index, channel, perms)
}

/// Revoke IPC capability bits from a target task.
pub fn ipc_cap_revoke(index: usize, channel: ChannelUuid, perms: u8) -> bool {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let Some(slot) = table.tasks.get_mut(index) else {
            return false;
        };
        if let Some(task) = slot.as_mut() {
            task.revoke_ipc_capability(channel, perms)
        } else {
            false
        }
    })
}

/// Revoke IPC capability bits by target TaskId.
pub fn ipc_cap_revoke_by_task_id(task_id: TaskId, channel: ChannelUuid, perms: u8) -> bool {
    let Some(index) = task_index_by_id(task_id) else {
        return false;
    };
    ipc_cap_revoke(index, channel, perms)
}

/// Check whether a task owns all requested IPC capability bits.
pub fn ipc_cap_has(index: usize, channel: ChannelUuid, perms: u8) -> bool {
    interrupts::without_interrupts(|| {
        let table = TABLE.lock();
        let Some(slot) = table.tasks.get(index) else {
            return false;
        };
        if let Some(task) = slot.as_ref() {
            task.has_ipc_capability(channel, perms)
        } else {
            false
        }
    })
}

/// Revoke capability bits for all tasks that currently hold them.
pub fn ipc_cap_revoke_all(channel: ChannelUuid, perms: u8) -> usize {
    ipc_cap_revoke_all_except(channel, perms, usize::MAX)
}

/// Revoke capability bits for all tasks except `exclude_index`.
pub fn ipc_cap_revoke_all_except(channel: ChannelUuid, perms: u8, exclude_index: usize) -> usize {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let mut revoked = 0usize;
        for (index, slot) in table.tasks.iter_mut().enumerate() {
            if index == exclude_index {
                continue;
            }
            if let Some(task) = slot.as_mut()
                && task.revoke_ipc_capability(channel, perms)
            {
                revoked += 1;
            }
        }
        revoked
    })
}

/// Get the name bytes of the task at `index`.
pub fn task_name_at(index: usize) -> [u8; 32] {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.name,
        None => [0u8; 32],
    }
}

/// Return the number of currently occupied slots.
pub fn count() -> usize {
    TABLE.lock().count
}

/// Count ready tasks other than `exclude_index`.
pub fn ready_count_excluding(exclude_index: usize) -> usize {
    let table = TABLE.lock();
    table
        .tasks
        .iter()
        .enumerate()
        .filter(|(index, slot)| {
            *index != exclude_index && matches!(slot, Some(task) if task.state == TaskState::Ready)
        })
        .count()
}

/// Returns whether the task at `index` is a user-mode task.
pub fn is_user_task(index: usize) -> bool {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.is_user,
        None => false,
    }
}

/// Read UID/GID for task at index.
pub fn task_identity_at(index: usize) -> (u32, u32) {
    let table = TABLE.lock();
    match table.tasks.get(index) {
        Some(Some(task)) => (task.uid, task.gid),
        _ => (0, 0),
    }
}

/// Set UID/GID for task at index.
pub fn set_task_identity(index: usize, uid: u32, gid: u32) -> bool {
    let mut table = TABLE.lock();
    let Some(slot) = table.tasks.get_mut(index) else {
        return false;
    };
    if let Some(task) = slot.as_mut() {
        task.uid = uid;
        task.gid = gid;
        true
    } else {
        false
    }
}

/// Read session UUID for task at index.
pub fn task_session_id_at(index: usize) -> crate::uuid::Uuid128 {
    let table = TABLE.lock();
    match table.tasks.get(index) {
        Some(Some(task)) => task.session_id,
        _ => crate::uuid::Uuid128::NIL,
    }
}

/// Set session UUID for task at index.
pub fn set_task_session_id(index: usize, session_id: crate::uuid::Uuid128) -> bool {
    let mut table = TABLE.lock();
    let Some(slot) = table.tasks.get_mut(index) else {
        return false;
    };
    if let Some(task) = slot.as_mut() {
        task.session_id = session_id;
        true
    } else {
        false
    }
}

/// Returns the CR3 root for the task at `index`, or 0 for the shared kernel address space.
/// Return the graph node ID for the task at `index`, or 0 if not registered.
pub fn task_graph_node(index: usize) -> crate::graph::types::NodeId {
    let table = TABLE.lock();
    match table.tasks.get(index).and_then(|s| s.as_ref()) {
        Some(t) => t.graph_node,
        None => 0,
    }
}

pub fn task_cr3_at(index: usize) -> u64 {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.cr3,
        None => 0,
    }
}

/// Returns the initial user RIP for the task at `index`, or 0 if not a user task.
pub fn task_user_rip_at(index: usize) -> u64 {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.user_rip,
        None => 0,
    }
}

/// Returns the initial user RSP for the task at `index`, or 0 if not a user task.
pub fn task_user_rsp_at(index: usize) -> u64 {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.user_rsp,
        None => 0,
    }
}

/// Give the current task's address space a chance to resolve a user-mode page fault.
///
/// Threads share the parent's CR3 but have `address_space = None`.  When that
/// happens we fall back to the first task that owns the same CR3 and has an
/// address space, so lazy-mapped pages (mmap'd stacks, BSS, etc.) are resolved
/// correctly for all threads in the same process.
pub fn handle_user_page_fault(index: usize, fault_addr: u64, error_code_bits: u64) -> bool {
    let mut table = TABLE.lock();
    let Some(task) = table.tasks.get_mut(index).and_then(Option::as_mut) else {
        return false;
    };
    if task.address_space.is_some() {
        let space = task.address_space.as_mut().unwrap();
        return space.handle_page_fault(fault_addr, error_code_bits);
    }
    // Thread has no address_space — find the owner with the same CR3.
    let cr3 = task.cr3;
    for slot in table.tasks.iter_mut() {
        let Some(t) = slot.as_mut() else { continue };
        if t.cr3 == cr3 {
            if let Some(space) = t.address_space.as_mut() {
                return space.handle_page_fault(fault_addr, error_code_bits);
            }
        }
    }
    false
}

pub fn mmap_current_user(
    path: Option<&[u8]>,
    len: u64,
    prot: u64,
    flags: u64,
    offset: u64,
) -> Option<u64> {
    let current = crate::sched::current_index();
    if current == 0 {
        return None;
    }

    let mut table = TABLE.lock();
    let task = table.tasks.get_mut(current)?.as_mut()?;
    let space = task.address_space.as_mut()?;
    match path {
        Some(path) => space.mmap_file(path, len, prot, flags, offset),
        None => space.mmap_anon(len, prot),
    }
}

/// Map pre-allocated shared surface frames into the current user task's
/// address space. Returns the mapped user virtual address, or `0` on error.
///
/// The frames are not owned by the address space; they are managed by the
/// surface table. This is the implementation of `SYS_SURFACE_CREATE`'s
/// mapping step.
pub fn mmap_current_user_shared(frames: &[u64], prot: u64, surface_id: u32) -> u64 {
    let current = crate::sched::current_index();
    if current == 0 {
        return 0;
    }
    let mut table = TABLE.lock();
    let Some(task) = table.tasks.get_mut(current).and_then(Option::as_mut) else {
        return 0;
    };
    let Some(space) = task.address_space.as_mut() else {
        return 0;
    };
    space
        .map_shared_frames(frames, prot, surface_id)
        .unwrap_or(0)
}

pub fn munmap_current_user(addr: u64, len: u64) -> bool {
    let current = crate::sched::current_index();
    if current == 0 {
        return false;
    }

    let mut table = TABLE.lock();
    let Some(task) = table.tasks.get_mut(current).and_then(Option::as_mut) else {
        return false;
    };
    let Some(space) = task.address_space.as_mut() else {
        return false;
    };
    space.munmap(addr, len)
}

// ====================================================================
// Security helpers
// ====================================================================

/// Maximum number of IPC channels a single user-mode task may create.
/// Prevents channel-table exhaustion from a compromised or misbehaving app.
pub const MAX_USER_TASK_CHANNELS: u8 = 32;

/// Downgrade the seccomp profile of the task identified by `task_id` to
/// the unprivileged-app tier.  Called by the userland loader for every ELF
/// whose package path starts with `/pkg/apps/`.
///
/// This is a one-way ratchet: the seccomp module refuses to escalate a task
/// from APP_STRICT back to a wider profile.
pub fn apply_app_seccomp_profile(task_id: super::tcb::TaskId) -> bool {
    let Some(index) = task_index_by_id(task_id) else {
        return false;
    };
    crate::security::seccomp::set_app_strict(index)
}

/// Attempt to allocate a channel creation token for the user task at `index`.
///
/// Returns `true` and increments the counter if the task is below
/// `MAX_USER_TASK_CHANNELS`.  Returns `false` (without modifying state) if
/// the limit has already been reached or the slot is empty.
///
/// Only meaningful for user-mode tasks; kernel tasks are not limited.
pub fn user_task_channel_alloc(index: usize) -> bool {
    interrupts::without_interrupts(|| {
        let mut table = TABLE.lock();
        let Some(Some(task)) = table.tasks.get_mut(index) else {
            return false;
        };
        if task.channels_created >= MAX_USER_TASK_CHANNELS {
            return false;
        }
        task.channels_created += 1;
        true
    })
}

// ============================================================================
// Thread support
// ============================================================================

pub enum JoinBlockResult {
    Blocked,
    TargetAlreadyDead,
    InvalidCaller,
}

/// Atomically checks join target liveness and blocks the caller if needed.
///
/// This closes a race where the target could exit after a separate liveness
/// check but before the caller transitions to `Blocked`, causing a lost wakeup.
pub fn block_for_join_if_target_alive(
    my_index: usize,
    target_id: super::tcb::TaskId,
) -> JoinBlockResult {
    let mut table = TABLE.lock();

    let target_alive = table.tasks.iter().any(|slot| {
        if let Some(task) = slot {
            task.id == target_id && task.state != TaskState::Dead
        } else {
            false
        }
    });
    if !target_alive {
        // Not found or already dead/reaped: join is already satisfied.
        return JoinBlockResult::TargetAlreadyDead;
    }

    if let Some(ref mut task) = table.tasks[my_index]
        && task.state == TaskState::Running
    {
        task.state = TaskState::Blocked;
        task.joining_on = target_id;
        task.wait_channel = ChannelUuid(crate::uuid::Uuid128::NIL);
        return JoinBlockResult::Blocked;
    }

    JoinBlockResult::InvalidCaller
}

/// Returns the `thread_arg` for the task at `index`, or 0 if the slot is empty.
///
/// Read by the user-task bootstrap to pass the argument in rdi before entering ring-3.
pub fn task_thread_arg_at(index: usize) -> u64 {
    let table = TABLE.lock();
    match table.tasks[index] {
        Some(ref task) => task.thread_arg,
        None => 0,
    }
}

/// Create a new user-mode thread that shares `parent_cr3`'s address space.
///
/// Unlike `create_user_task`, this function does NOT allocate a new address
/// space; the thread shares the parent's page tables (same CR3).
/// The caller must provide a valid `user_stack_top` in the shared address space.
///
/// The `thread_arg` value will be passed in rdi when the thread enters ring-3.
///
/// Returns the new `TaskId`, or `None` if the table is full or stack allocation fails.
pub fn create_user_thread(
    entry: u64,
    thread_arg: u64,
    user_stack_top: u64,
    parent_cr3: u64,
) -> Option<super::tcb::TaskId> {
    use super::tcb::{Task, TaskState};

    let (stack_base, stack_top) = match super::stack_pool::alloc_stack() {
        Some(s) => s,
        None => {
            serial::write_line(b"[task] ERROR: create_user_thread: no free kernel stacks");
            return None;
        }
    };

    // Push the task-return trampoline so `ret` from entry lands in the trampoline.
    let trampoline_addr = crate::sched::task_return_trampoline_addr();
    let adjusted_stack_top = stack_top - 8;
    unsafe {
        let ret_addr_ptr = adjusted_stack_top as *mut u64;
        core::ptr::write_volatile(ret_addr_ptr, trampoline_addr);
    }

    // Kernel context starts at the user-task bootstrap which will switch to ring-3.
    let ctx = CpuContext::new_kernel(crate::sched::user_task_bootstrap_addr(), adjusted_stack_top);

    super::reaper::reap_pending();

    interrupts::without_interrupts(|| {
        let id = {
            let mut next = NEXT_ID.lock();
            let id = *next;
            *next += 1;
            id
        };

        let name = b"thread";
        let mut name_buf = [0u8; 32];
        name_buf[..6].copy_from_slice(name);

        let mut task = Task::new(id, name_buf);
        task.stack_base = stack_base;
        task.state = TaskState::Ready;
        task.is_user = true;
        task.cr3 = parent_cr3;
        task.user_rip = entry;
        // Adjust RSP down by 8 so the thread enters with its stack pointer
        // pointing inside the mmap'd region (callers pass the exclusive top).
        // The Rust thread trampoline reads [rsp] as a return-address slot,
        // so it must be within the mapped pages.
        task.user_rsp = user_stack_top.saturating_sub(8);
        task.thread_arg = thread_arg;
        // Threads inherit no address_space Box — they share the parent's CR3.
        task.address_space = None;
        seed_default_ipc_caps(&mut task, b"");

        let mut table = TABLE.lock();
        if table.count >= MAX_TASKS {
            serial::write_line(b"[task] ERROR: create_user_thread: table full");
            drop(table);
            super::stack_pool::free_stack(stack_base);
            return None;
        }

        for (slot_idx, slot) in table.tasks.iter_mut().enumerate() {
            if slot.is_none() {
                unsafe {
                    let ctx_ptr = core::ptr::addr_of_mut!(CONTEXTS[slot_idx]);
                    ctx_ptr.write(ctx);
                }
                *slot = Some(task);
                let _ = crate::security::seccomp::set_protected_strict(slot_idx);
                table.count += 1;

                serial::write_bytes(b"[task] created user thread id=");
                serial::write_u64_dec_inline(id);
                serial::write_bytes(b" cr3=");
                serial::write_hex_inline(parent_cr3);
                serial::write_bytes(b" entry=");
                serial::write_hex_inline(entry);
                serial::write_bytes(b" rsp=");
                serial::write_hex(user_stack_top);

                crate::sched::percpu::enqueue_current_cpu(slot_idx);

                return Some(id);
            }
        }

        drop(table);
        super::stack_pool::free_stack(stack_base);
        None
    })
}
