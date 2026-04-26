// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Syscall ABI and dispatch â€” x86_64.
//!
//! GraphOS uses `syscall`/`sysret` (MSR-based fast system calls) on x86_64.
//! The register convention follows a custom ABI (not Linux-compatible):
//!
//! | Register | Purpose                          |
//! |----------|----------------------------------|
//! | rax      | Syscall number (in) / return (out)|
//! | rdi      | Argument 0                       |
//! | rsi      | Argument 1                       |
//! | rdx      | Argument 2                       |
//! | r10      | Argument 3                       |
//! | r8       | Argument 4                       |
//! | r9       | Argument 5                       |
//! | rcx      | Clobbered by `syscall` (saved RIP)|
//! | r11      | Clobbered by `syscall` (saved RFLAGS)|
//!
//! ## Current status
//! - The syscall ABI and dispatch table are live for kernel-mode and
//!   protected ring-3 callers.
//! - Protected userspace now enters primarily through the hardware
//!   `syscall/sysret` path.
//! - The DPL3 `int 0x80` trap gate remains available as a compatibility/debug
//!   bridge during bring-up.
//! - All pointer arguments are validated via `validate_user_slice` /
//!   `validate_user_slice_mut` before being dereferenced.

pub mod numbers;

use core::mem::size_of;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch::serial;

/// Error sentinel returned in rax for failed syscalls.
pub const SYSCALL_ERROR: u64 = u64::MAX;

/// Task table index of the ring-3 compositor service.
///
/// Set by `register_compositor_task()` once the compositor ELF boots.
/// Used by `SYS_SURFACE_PRESENT` to wake the compositor.
/// `usize::MAX` = not yet registered.
pub static COMPOSITOR_TASK_INDEX: AtomicUsize = AtomicUsize::new(usize::MAX);
static SURFACE_COMMIT_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
static SURFACE_FLUSH_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
static SURFACE_COMMIT_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static SURFACE_COMMIT_FAIL_OWNER: AtomicUsize = AtomicUsize::new(0);
static SURFACE_COMMIT_FAIL_MISSING: AtomicUsize = AtomicUsize::new(0);
static SURFACE_COMMIT_FAIL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static SURFACE_COMMIT_PATH_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
static INPUT_REGISTER_WINDOW_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Per-surface commit-log budget so that a noisy bootstrap surface (sid=1)
/// can't exhaust the global cap and hide app-surface (sid=2..) commits.
const PER_SID_COMMIT_LOG_LIMIT: u32 = 32;
const TRACKED_SURFACE_IDS: usize = 16;
static PER_SID_COMMIT_LOG: [AtomicUsize; TRACKED_SURFACE_IDS] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// Total commits per surface (uncapped) for periodic heartbeat reporting.
static PER_SID_COMMIT_TOTAL: [AtomicUsize; TRACKED_SURFACE_IDS] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

#[inline]
fn note_surface_commit_for_log(surface_id: u32) -> bool {
    let idx = surface_id as usize;
    if idx >= TRACKED_SURFACE_IDS {
        return false;
    }
    PER_SID_COMMIT_TOTAL[idx].fetch_add(1, Ordering::Relaxed);
    let n = PER_SID_COMMIT_LOG[idx].fetch_add(1, Ordering::Relaxed);
    (n as u32) < PER_SID_COMMIT_LOG_LIMIT
}

/// Snapshot of total commits per surface, used by the heartbeat trace.
pub fn surface_commit_totals_snapshot(out: &mut [usize; TRACKED_SURFACE_IDS]) {
    for i in 0..TRACKED_SURFACE_IDS {
        out[i] = PER_SID_COMMIT_TOTAL[i].load(Ordering::Relaxed);
    }
}

pub const SURFACE_COMMIT_TOTAL_SLOTS: usize = TRACKED_SURFACE_IDS;

#[derive(Clone, Copy)]
pub struct SurfaceCommitStats {
    pub attempts: usize,
    pub fail_owner: usize,
    pub fail_missing: usize,
    pub fail_present: usize,
}

pub fn surface_commit_stats_snapshot() -> SurfaceCommitStats {
    SurfaceCommitStats {
        attempts: SURFACE_COMMIT_ATTEMPTS.load(Ordering::Relaxed),
        fail_owner: SURFACE_COMMIT_FAIL_OWNER.load(Ordering::Relaxed),
        fail_missing: SURFACE_COMMIT_FAIL_MISSING.load(Ordering::Relaxed),
        fail_present: SURFACE_COMMIT_FAIL_PRESENT.load(Ordering::Relaxed),
    }
}

/// Register the compositor task index so the surface present path can wake it.
pub fn register_compositor_task(index: usize) {
    COMPOSITOR_TASK_INDEX.store(index, Ordering::Release);
}

// ====================================================================
// Pointer validation (C3 fix)
// ====================================================================

/// Minimum valid pointer address. Anything below this (including NULL)
/// is rejected. This catches null derefs and low-address bugs.
const MIN_VALID_ADDR: u64 = 0x1000;

/// Kernel-mode direct dispatch still only uses the early identity map.
const KERNEL_DIRECT_MAX_VALID_ADDR: u64 = 128 * 1024 * 1024;
/// Bootstrap ring-3 tasks live in the lower canonical half.
const USER_CANONICAL_TOP: u64 = 0x0000_8000_0000_0000;
/// Hard cap to avoid pathological copy loops from bogus user lengths.
const MAX_USER_SLICE_LEN: u64 = 1024 * 1024;

fn active_user_cr3() -> Option<u64> {
    let cur = crate::sched::current_index();
    if cur == 0 || !crate::task::table::is_user_task(cur) {
        return None;
    }
    let cr3 = crate::task::table::task_cr3_at(cur);
    (cr3 != 0).then_some(cr3)
}

fn validate_range_shape(ptr: u64, len: u64, max_end: u64) -> Option<u64> {
    if ptr < MIN_VALID_ADDR {
        return None;
    }
    let end = ptr.checked_add(len)?;
    if end > max_end {
        return None;
    }
    Some(end)
}

fn user_page_allows(cr3: u64, vaddr: u64, writable: bool) -> bool {
    use crate::arch::x86_64::paging::{flags, pd_index, pdpt_index, pml4_index, pt_index};

    const FRAME_MASK: u64 = 0x000F_FFFF_FFFF_F000;
    unsafe {
        let pml4 = (cr3 & FRAME_MASK) as *const u64;
        let pml4e = pml4.add(pml4_index(vaddr)).read();
        if pml4e & flags::PRESENT == 0 || pml4e & flags::USER == 0 {
            return false;
        }

        let pdpt = (pml4e & FRAME_MASK) as *const u64;
        let pdpte = pdpt.add(pdpt_index(vaddr)).read();
        if pdpte & flags::PRESENT == 0 || pdpte & flags::USER == 0 {
            return false;
        }
        if pdpte & flags::HUGE_PAGE != 0 {
            return !writable || (pdpte & flags::WRITABLE != 0);
        }

        let pd = (pdpte & FRAME_MASK) as *const u64;
        let pde = pd.add(pd_index(vaddr)).read();
        if pde & flags::PRESENT == 0 || pde & flags::USER == 0 {
            return false;
        }
        if pde & flags::HUGE_PAGE != 0 {
            return !writable || (pde & flags::WRITABLE != 0);
        }

        let pt = (pde & FRAME_MASK) as *const u64;
        let pte = pt.add(pt_index(vaddr)).read();
        if pte & flags::PRESENT == 0 || pte & flags::USER == 0 {
            return false;
        }

        !writable || (pte & flags::WRITABLE != 0)
    }
}

fn user_range_mapped(cr3: u64, ptr: u64, len: u64, writable: bool) -> bool {
    let Some(end) = validate_range_shape(ptr, len, USER_CANONICAL_TOP) else {
        return false;
    };
    if len > MAX_USER_SLICE_LEN {
        return false;
    }
    if len == 0 {
        return true;
    }

    let mut page = ptr & !0xFFF;
    while page < end {
        if !user_page_allows(cr3, page, writable) {
            return false;
        }
        page = match page.checked_add(4096) {
            Some(next) => next,
            None => return false,
        };
    }
    true
}

/// Validate a user-provided (ptr, len) pair and return a `&[u8]`, or None.
///
/// This function **must only be called from a ring-3 syscall context** where
/// an active user CR3 is present.  When called from a kernel-mode path (no
/// user task active), it returns None unconditionally to prevent accidental
/// access to kernel memory through the user-pointer validation bypass.
pub(crate) fn validate_user_slice(ptr: u64, len: u64) -> Option<&'static [u8]> {
    if len == 0 {
        return Some(&[]);
    }

    // Require an active user CR3.  Kernel-mode callers must not pass
    // arbitrary pointers through this path; they have typed Rust references.
    let cr3 = active_user_cr3()?;
    if !user_range_mapped(cr3, ptr, len, false) {
        return None;
    }

    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

/// Validate a user-provided (ptr, len) pair and return a `&mut [u8]`, or None.
///
/// See `validate_user_slice` for safety requirements.
fn validate_user_slice_mut(ptr: u64, len: u64) -> Option<&'static mut [u8]> {
    if len == 0 {
        return Some(&mut []);
    }

    let cr3 = active_user_cr3()?;
    if !user_range_mapped(cr3, ptr, len, true) {
        return None;
    }

    Some(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

/// Validate a user-provided null-terminated string pointer.
/// Returns the byte slice up to (not including) the null terminator,
/// capped at `max_len` bytes.  Requires an active user CR3.
fn validate_user_cstr(ptr: u64, max_len: usize) -> Option<&'static [u8]> {
    let cr3 = active_user_cr3()?;
    if !user_range_mapped(cr3, ptr, max_len as u64, false) {
        return None;
    }
    let base = ptr as *const u8;
    let mut len = 0usize;
    while len < max_len {
        let b = unsafe { base.add(len).read() };
        if b == 0 {
            break;
        }
        len += 1;
    }
    Some(unsafe { core::slice::from_raw_parts(base, len) })
}

/// Dispatch a syscall by number.
///
/// `nr`   â€” syscall number (from rax).
/// `args` â€” up to 6 arguments [rdi, rsi, rdx, r10, r8, r9].
///
/// Returns the result value placed into rax.
pub fn dispatch(nr: u64, args: &[u64; 6]) -> u64 {
    let current = crate::sched::current_index();
    if current != 0
        && crate::task::table::is_user_task(current)
        && !crate::security::seccomp::is_allowed(current, nr)
    {
        serial::write_bytes(b"[syscall] seccomp deny nr=0x");
        serial::write_hex_inline(nr);
        serial::write_line(b"");
        crate::security::audit::emit_seccomp_deny(current, nr);
        return SYSCALL_ERROR;
    }

    // Table-driven dispatch: sorted by syscall number for O(log n) lookup.
    type Fn = fn(&[u64; 6]) -> u64;
    static TABLE: &[(u64, Fn)] = &[
        (numbers::SYS_EXIT, |a| sys_exit(a[0])),
        (numbers::SYS_YIELD, |_| sys_yield()),
        (numbers::SYS_SPAWN, |a| sys_spawn(a[0], a[1])),
        (numbers::SYS_THREAD_SPAWN, |a| {
            sys_thread_spawn(a[0], a[1], a[2])
        }),
        (numbers::SYS_THREAD_JOIN, |a| sys_thread_join(a[0])),
        (numbers::SYS_THREAD_EXIT, |a| sys_thread_exit(a[0])),
        (numbers::SYS_WRITE, |a| sys_write(a[0], a[1], a[2])),
        (numbers::SYS_CHANNEL_CREATE, |a| sys_channel_create(a[0])),
        (numbers::SYS_CHANNEL_SEND, |a| {
            sys_channel_send(a[0], a[1], a[2], a[3])
        }),
        (numbers::SYS_CHANNEL_RECV, |a| {
            sys_channel_recv(a[0], a[1], a[2], a[3])
        }),
        (numbers::SYS_VFS_OPEN, |a| sys_vfs_open(a[0])),
        (numbers::SYS_VFS_READ, |a| sys_vfs_read(a[0], a[1], a[2])),
        (numbers::SYS_VFS_CLOSE, |a| sys_vfs_close(a[0])),
        (numbers::SYS_VFS_WRITE, |a| sys_vfs_write(a[0], a[1], a[2])),
        (numbers::SYS_VFS_CREATE, |a| sys_vfs_create(a[0])),
        (numbers::SYS_MOUNT, |a| sys_mount(a[0], a[1], a[2])),
        (numbers::SYS_UMOUNT, |a| sys_umount(a[0])),
        (numbers::SYS_VFS_MKDIR, |a| sys_vfs_mkdir(a[0])),
        (numbers::SYS_VFS_UNLINK, |a| sys_vfs_unlink(a[0])),
        (numbers::SYS_SOCKET, |a| sys_socket(a[0], a[1])),
        (numbers::SYS_BIND, |a| sys_bind(a[0], a[1])),
        (numbers::SYS_CONNECT, |a| sys_connect(a[0], a[1], a[2])),
        (numbers::SYS_SEND, |a| sys_send(a[0], a[1], a[2])),
        (numbers::SYS_RECV, |a| sys_recv(a[0], a[1], a[2])),
        (numbers::SYS_CLOSE_SOCK, |a| sys_close_sock(a[0])),
        (numbers::SYS_LISTEN, |a| sys_listen(a[0])),
        (numbers::SYS_ACCEPT, |a| sys_accept(a[0], a[1])),
        (numbers::SYS_NET_STATS, |_| sys_net_stats()),
        (numbers::SYS_GETUID, |_| sys_getuid()),
        (numbers::SYS_GETGID, |_| sys_getgid()),
        (numbers::SYS_SETUID, |a| sys_setuid(a[0], a[1])),
        (numbers::SYS_LOGIN, |a| sys_login(a[0], a[1])),
        (numbers::SYS_LOGOUT, |_| sys_logout()),
        (numbers::SYS_PTY_ALLOC, |_| sys_pty_alloc()),
        (numbers::SYS_SESSION_ATTACH, |a| sys_session_attach(a[0])),
        (numbers::SYS_PTY_WRITE, |a| sys_pty_write(a[0], a[1], a[2])),
        (numbers::SYS_PTY_READ, |a| sys_pty_read(a[0], a[1], a[2])),
        (numbers::SYS_DRIVER_PROBE, |a| sys_driver_probe(a[0], a[1])),
        (numbers::SYS_DRIVER_INSTALL, |a| {
            sys_driver_install(a[0], a[1], a[2], a[3])
        }),
        (numbers::SYS_HEARTBEAT, |_| sys_heartbeat()),
        (numbers::SYS_PERF_SAMPLE, |a| sys_perf_sample(a[0])),
        (numbers::SYS_PERF_READ, |a| sys_perf_read(a[0], a[1], a[2])),
        (numbers::SYS_AUDIT_READ, |a| sys_audit_read(a[0], a[1])),
        (numbers::SYS_GETRANDOM, |a| sys_getrandom(a[0], a[1])),
        (numbers::SYS_WIFI_SCAN, |_| sys_wifi_scan()),
        (numbers::SYS_WIFI_CONNECT, |a| sys_wifi_connect(a[0], a[1])),
        (numbers::SYS_WIFI_STATE, |_| sys_wifi_state()),
        (numbers::SYS_BT_SCAN, |_| sys_bt_scan()),
        (numbers::SYS_BT_CONNECT, |a| sys_bt_connect(a[0], a[1])),
        (numbers::SYS_BT_SEND, |a| sys_bt_send(a[0], a[1], a[2])),
        (numbers::SYS_BT_CLOSE, |a| sys_bt_close(a[0])),
        (numbers::SYS_MMAP, |a| {
            sys_mmap(a[0], a[1], a[2], a[3], a[4])
        }),
        (numbers::SYS_MUNMAP, |a| sys_munmap(a[0], a[1])),
        (numbers::SYS_SLEEP, |a| sys_sleep(a[0])),
        (numbers::SYS_POWEROFF, |_| {
            crate::acpi::pm::poweroff();
        }),
        (numbers::SYS_REBOOT, |_| {
            crate::acpi::pm::reboot();
        }),
        (numbers::SYS_SUSPEND, |_| {
            crate::acpi::pm::request_suspend();
            0
        }),
        (numbers::SYS_GRAPH_ADD_NODE, |a| {
            sys_graph_add_node(a[0], a[1], a[2])
        }),
        (numbers::SYS_GRAPH_ADD_EDGE, |a| {
            sys_graph_add_edge(a[0], a[1], a[2], a[3], a[4])
        }),
        (numbers::SYS_GRAPH_NODE_EXISTS, |a| {
            sys_graph_node_exists(a[0])
        }),
        (numbers::SYS_GRAPH_NODE_KIND, |a| sys_graph_node_kind(a[0])),
        (numbers::SYS_GRAPH_STATS, |_| sys_graph_stats()),
        (numbers::SYS_GRAPH_GENERATION, |_| sys_graph_generation()),
        (numbers::SYS_GRAPH_SERVICE_LOOKUP, |a| {
            sys_graph_service_lookup(a[0])
        }),
        (numbers::SYS_GRAPH_SERVICE_LOOKUP_UUID, |a| {
            sys_graph_service_lookup_uuid(a[0], a[1], a[2])
        }),
        (numbers::SYS_REGISTRY_LOOKUP, |a| {
            sys_registry_lookup(a[0], a[1], a[2])
        }),
        (numbers::SYS_REGISTRY_REGISTER, |a| {
            sys_registry_register(a[0], a[1])
        }),
        (numbers::SYS_REGISTRY_SUBSCRIBE, |a| {
            sys_registry_subscribe(a[0], a[1] as u32)
        }),
        (numbers::SYS_IPC_CAP_GRANT, |a| {
            sys_ipc_cap_grant(a[0], a[1], a[2])
        }),
        (numbers::SYS_IPC_CAP_REVOKE, |a| {
            sys_ipc_cap_revoke(a[0], a[1], a[2])
        }),
        (numbers::SYS_GRAPH_EM_STEP, |_| sys_graph_em_step()),
        (numbers::SYS_GRAPH_EM_STATS, |a| {
            sys_graph_em_stats(a[0], a[1])
        }),
        (numbers::SYS_COGNITIVE_INDEX, |a| {
            sys_cognitive_index(a[0], a[1], a[2], a[3])
        }),
        (numbers::SYS_COGNITIVE_QUERY, |a| {
            sys_cognitive_query(a[0], a[1], a[2])
        }),
        (numbers::SYS_COGNITIVE_REDACT, |a| {
            sys_cognitive_redact(a[0], a[1], a[2], a[3])
        }),
        (numbers::SYS_SURFACE_CREATE, |a| {
            sys_surface_create(a[0], a[1])
        }),
        (numbers::SYS_SURFACE_PRESENT, |a| sys_surface_present(a[0])),
        (numbers::SYS_SURFACE_DESTROY, |a| sys_surface_destroy(a[0])),
        (numbers::SYS_SURFACE_QUERY_PENDING, |_| {
            crate::wm::surface_table::present_queue_pending() as u64
        }),
        (numbers::SYS_SURFACE_FLUSH, |_| sys_surface_flush()),
        (numbers::SYS_SURFACE_COMMIT, |a| sys_surface_commit(a[0])),
        (numbers::SYS_EXPOSE_TOGGLE, |a| {
            sys_expose_toggle(a[0], a[1])
        }),
        (numbers::SYS_SURFACE_TRANSFORM, |a| {
            sys_surface_transform(a[0], a[1])
        }),
        (numbers::SYS_INPUT_SET_FOCUS, |a| sys_input_set_focus(a[0])),
        (numbers::SYS_INPUT_REGISTER_WINDOW, |a| {
            sys_input_register_window(a[0], a[1], a[2])
        }),
        (numbers::SYS_INPUT_UNREGISTER_WINDOW, |_| {
            sys_input_unregister_window()
        }),
        (numbers::SYS_FRAME_TICK_SUBSCRIBE, |a| {
            sys_frame_tick_subscribe(a[0])
        }),
        (numbers::SYS_THEME_SET, |a| {
            crate::ui::tokens::syscall_set_theme(a[0] as u8)
        }),
        (numbers::SYS_THEME_GET, |_| {
            crate::ui::tokens::syscall_get_theme()
        }),
        // GPU resource management (0x500â€“0x508)
        (numbers::SYS_GPU_QUERY_CAPS, |a| sys_gpu_query_caps(a[0])),
        (numbers::SYS_GPU_RESOURCE_CREATE, |a| {
            sys_gpu_resource_create(a[0])
        }),
        (numbers::SYS_GPU_RESOURCE_DESTROY, |a| {
            sys_gpu_resource_destroy(a[0] as u32)
        }),
        (numbers::SYS_GPU_SUBMIT_3D, |a| {
            sys_gpu_submit_3d(a[0], a[1])
        }),
        (numbers::SYS_GPU_SURFACE_IMPORT, |a| {
            sys_gpu_surface_import(a[0] as u32, a[1] as u32)
        }),
        (numbers::SYS_GPU_FENCE_ALLOC, |_| sys_gpu_fence_alloc()),
        (numbers::SYS_GPU_FENCE_WAIT, |a| {
            sys_gpu_fence_wait(a[0], a[1])
        }),
        (numbers::SYS_GPU_FENCE_POLL, |a| sys_gpu_fence_poll(a[0])),
        (numbers::SYS_GPU_SUBMIT, |a| sys_gpu_submit(a[0], a[1])),
        (numbers::SYS_COMPOSITOR_CLAIM_DISPLAY, |a| {
            sys_compositor_claim_display(a[0] as u32)
        }),
        (numbers::SYS_FETCH_UPDATE, |a| sys_fetch_update(a[0], a[1])),
        (numbers::SYS_TLS_SET_AVAILABLE, |_| sys_tls_set_available()),
        (numbers::SYS_TLS_SET_UNAVAILABLE, |_| {
            sys_tls_set_unavailable()
        }),
        (numbers::SYS_TLS_AVAILABLE, |_| {
            if crate::net::tls::is_available() {
                1
            } else {
                0
            }
        }),
    ];

    // TABLE must be sorted by the first element. Binary search via partition_point.
    let pos = TABLE.partition_point(|e| e.0 < nr);
    if pos < TABLE.len() && TABLE[pos].0 == nr {
        return TABLE[pos].1(args);
    }

    serial::write_bytes(b"[syscall] unknown nr=0x");
    serial::write_hex(nr);
    SYSCALL_ERROR
}

// ====================================================================
// Process lifecycle syscalls
// ====================================================================

fn sys_exit(code: u64) -> u64 {
    serial::write_bytes(b"[syscall] exit code=");
    serial::write_u64_dec(code);
    let current = crate::sched::current_index();
    // Clear the seccomp policy slot before the task slot is recycled so a
    // new task cannot inherit a prior task's privilege level.
    crate::security::seccomp::clear_policy(current);
    if crate::task::table::is_user_task(current) {
        crate::arch::x86_64::ring3::unwind_nonreturning_fast_syscall();
    }
    unsafe { crate::sched::exit_current_task() }
}

fn sys_yield() -> u64 {
    unsafe { crate::sched::schedule() };
    0
}

fn sys_spawn(name_ptr: u64, entry_ptr: u64) -> u64 {
    let name = match validate_user_cstr(name_ptr, 127) {
        Some(s) => s,
        None => {
            serial::write_line(b"[syscall] spawn: invalid name pointer");
            return SYSCALL_ERROR;
        }
    };

    if crate::task::table::is_user_task(crate::sched::current_index()) || entry_ptr == 0 {
        return match crate::userland::spawn_named_service(name) {
            Some(id) => id,
            None => {
                serial::write_line(b"[syscall] spawn: unknown or failed user service");
                SYSCALL_ERROR
            }
        };
    }

    match crate::task::table::create_kernel_task(name, entry_ptr) {
        Some(id) => {
            use crate::graph::arena;
            use crate::graph::types::{EdgeKind, NODE_ID_KERNEL, NodeKind};
            let node = arena::add_node(NodeKind::Task, 0, 0);
            if let Some(task_node) = node {
                arena::add_edge_weighted(
                    NODE_ID_KERNEL,
                    task_node,
                    EdgeKind::Created,
                    0,
                    crate::graph::types::WEIGHT_ONE,
                );
            }
            id
        }
        None => SYSCALL_ERROR,
    }
}

/// Write to a file descriptor. Currently only fd=1 (serial) is supported.
fn sys_write(fd: u64, buf_ptr: u64, len: u64) -> u64 {
    if fd == 1 {
        if len == 0 {
            return 0;
        }
        let slice = match validate_user_slice(buf_ptr, len) {
            Some(s) => s,
            None => {
                serial::write_line(b"[syscall] write: invalid buffer");
                return SYSCALL_ERROR;
            }
        };
        serial::write_bytes(slice);
        len
    } else {
        SYSCALL_ERROR
    }
}

// ====================================================================
// IPC syscalls
// ====================================================================

fn sys_channel_create(max_msg_size: u64) -> u64 {
    let current = crate::sched::current_index();

    // Per-task channel quota: prevent a compromised user task from exhausting
    // the global channel table (MAX_CHANNELS = 64, system-wide).
    if crate::task::table::is_user_task(current)
        && !crate::task::table::user_task_channel_alloc(current)
    {
        serial::write_line(b"[syscall] channel_create: per-task limit reached");
        return SYSCALL_ERROR;
    }

    match crate::ipc::channel_create(max_msg_size as usize) {
        Some(uuid) => {
            // Grant the creating task full capabilities on the new channel.
            let _ =
                crate::task::table::ipc_cap_grant(current, uuid, crate::ipc::capability::CAP_ALL);
            // Graph-first: register a Channel node.
            let _ = crate::graph::handles::register_channel(crate::task::table::task_graph_node(
                current,
            ));
            // Return the legacy integer alias as the ABI handle (shim).
            let alias = crate::ipc::channel::alias_for_uuid(uuid).unwrap_or(0);
            alias as u64
        }
        None => SYSCALL_ERROR,
    }
}

fn sys_channel_send(channel_id: u64, buf_ptr: u64, len: u64, tag_raw: u64) -> u64 {
    let current = crate::sched::current_index();
    // Convert legacy integer alias to UUID at the syscall ABI boundary (shim).
    let channel_uuid = crate::ipc::channel::uuid_for_alias(channel_id as u32);
    if current != 0
        && crate::task::table::is_user_task(current)
        && !crate::ipc::capability::can_send(current, channel_uuid)
    {
        serial::write_line(b"[syscall] channel_send: capability denied");
        return SYSCALL_ERROR;
    }

    let payload = if len == 0 {
        &[]
    } else {
        match validate_user_slice(buf_ptr, len) {
            Some(s) => s,
            None => {
                serial::write_line(b"[syscall] channel_send: invalid buffer");
                return SYSCALL_ERROR;
            }
        }
    };
    let tag =
        crate::ipc::msg::MsgTag::from_u8(tag_raw as u8).unwrap_or(crate::ipc::msg::MsgTag::Data);
    if crate::ipc::channel_send_tagged(channel_uuid, tag, payload) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_channel_recv(channel_id: u64, buf_ptr: u64, buf_len: u64, flags: u64) -> u64 {
    const CHANNEL_RECV_NONBLOCK: u64 = 1;

    if buf_len == 0 {
        return SYSCALL_ERROR;
    }
    let buf = match validate_user_slice_mut(buf_ptr, buf_len) {
        Some(s) => s,
        None => {
            serial::write_line(b"[syscall] channel_recv: invalid buffer");
            return SYSCALL_ERROR;
        }
    };

    let current = crate::sched::current_index();
    // Convert legacy integer alias to UUID at the syscall ABI boundary (shim).
    let channel_uuid = crate::ipc::channel::uuid_for_alias(channel_id as u32);
    if current != 0
        && crate::task::table::is_user_task(current)
        && !crate::ipc::capability::can_recv(current, channel_uuid)
    {
        serial::write_line(b"[syscall] channel_recv: capability denied");
        return SYSCALL_ERROR;
    }

    if !crate::ipc::channel::is_active(channel_uuid) {
        return SYSCALL_ERROR;
    }

    loop {
        match crate::ipc::channel_recv(channel_uuid, buf) {
            Some(meta) => {
                let len_bits = (meta.payload_len as u64) & 0xFFFF;
                let tag_bits = (meta.tag as u8 as u64) << 16;
                let reply_endpoint_bits = (meta.reply_endpoint as u64) << 24;
                return len_bits | tag_bits | reply_endpoint_bits;
            }
            None if flags & CHANNEL_RECV_NONBLOCK != 0 => return 0,
            None => {
                if current == 0 || !crate::task::table::mark_blocked(current, channel_uuid) {
                    return 0;
                }
                unsafe { crate::sched::schedule() };
                if crate::task::table::take_wake_without_ipc(current) {
                    return u64::MAX;
                }
            }
        }
    }
}

fn read_uuid_handle(handle_ptr: u64) -> Option<crate::uuid::Uuid128> {
    let raw = validate_user_slice(handle_ptr, 16)?;
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(raw);
    Some(crate::uuid::Uuid128::from_bytes(bytes))
}

fn sys_socket(out_ptr: u64, out_len: u64) -> u64 {
    if out_len < 16 {
        return SYSCALL_ERROR;
    }
    let Some(out) = validate_user_slice_mut(out_ptr, 16) else {
        return SYSCALL_ERROR;
    };
    let Some(handle) = crate::net::socket_open(crate::sched::current_index()) else {
        return SYSCALL_ERROR;
    };
    out.copy_from_slice(&handle.to_bytes());
    0
}

fn sys_bind(handle_ptr: u64, local_port: u64) -> u64 {
    let Some(handle) = read_uuid_handle(handle_ptr) else {
        return SYSCALL_ERROR;
    };
    if crate::net::socket_bind(crate::sched::current_index(), handle, local_port as u16) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_connect(handle_ptr: u64, remote_ipv4: u64, remote_port: u64) -> u64 {
    let Some(handle) = read_uuid_handle(handle_ptr) else {
        return SYSCALL_ERROR;
    };
    if crate::net::socket_connect(
        crate::sched::current_index(),
        handle,
        remote_ipv4 as u32,
        remote_port as u16,
    ) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_send(handle_ptr: u64, payload_ptr: u64, payload_len: u64) -> u64 {
    let Some(handle) = read_uuid_handle(handle_ptr) else {
        return SYSCALL_ERROR;
    };
    let payload = if payload_len == 0 {
        &[]
    } else {
        let Some(slice) = validate_user_slice(payload_ptr, payload_len) else {
            return SYSCALL_ERROR;
        };
        slice
    };
    match crate::net::socket_send(crate::sched::current_index(), handle, payload) {
        Some(n) => n as u64,
        None => SYSCALL_ERROR,
    }
}

fn sys_recv(handle_ptr: u64, out_ptr: u64, out_len: u64) -> u64 {
    if out_len == 0 {
        return 0;
    }
    let Some(handle) = read_uuid_handle(handle_ptr) else {
        return SYSCALL_ERROR;
    };
    let Some(out) = validate_user_slice_mut(out_ptr, out_len) else {
        return SYSCALL_ERROR;
    };
    match crate::net::socket_recv(crate::sched::current_index(), handle, out) {
        Some(n) => n as u64,
        None => SYSCALL_ERROR,
    }
}

fn sys_close_sock(handle_ptr: u64) -> u64 {
    let Some(handle) = read_uuid_handle(handle_ptr) else {
        return SYSCALL_ERROR;
    };
    if crate::net::socket_close(crate::sched::current_index(), handle) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_listen(handle_ptr: u64) -> u64 {
    let Some(handle) = read_uuid_handle(handle_ptr) else {
        serial::write_line(b"[syscall] listen: invalid handle ptr");
        return SYSCALL_ERROR;
    };
    if crate::net::socket_listen(crate::sched::current_index(), handle) {
        0
    } else {
        serial::write_line(b"[syscall] listen: socket_listen failed");
        SYSCALL_ERROR
    }
}

fn sys_accept(listen_ptr: u64, out_key_ptr: u64) -> u64 {
    let Some(handle) = read_uuid_handle(listen_ptr) else {
        return SYSCALL_ERROR;
    };
    let Some(out) = validate_user_slice_mut(out_key_ptr, 16) else {
        return SYSCALL_ERROR;
    };
    let owner = crate::sched::current_index();
    let mut key = [0u8; 16];

    match crate::net::socket_accept(owner, handle, &mut key) {
        Some((ip, port)) => {
            serial::write_line(b"[syscall] accept got connection");
            out.copy_from_slice(&key);
            ((ip as u64) << 16) | (port as u64)
        }
        None => SYSCALL_ERROR,
    }
}

fn sys_net_stats() -> u64 {
    let stats = crate::net::stats();
    let tx = stats.tx_packets.min(u32::MAX as u64);
    let rx = stats.rx_packets.min(0x7FFF_FFFFu64);
    let link = if stats.link_ready { 1u64 << 63 } else { 0 };
    link | (rx << 32) | tx
}

fn sys_getuid() -> u64 {
    let current = crate::sched::current_index();
    let (uid, _) = crate::task::table::task_identity_at(current);
    uid as u64
}

fn sys_getgid() -> u64 {
    let current = crate::sched::current_index();
    let (_, gid) = crate::task::table::task_identity_at(current);
    gid as u64
}

fn sys_setuid(uid: u64, gid: u64) -> u64 {
    let current = crate::sched::current_index();
    let (current_uid, _) = crate::task::table::task_identity_at(current);
    if current_uid != 0 {
        // Audit: privilege escalation denied.
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::Setuid,
            current,
            numbers::SYS_SETUID,
            false,
            &(uid as u32).to_le_bytes(),
        );
        return SYSCALL_ERROR;
    }
    let ok = crate::task::table::set_task_identity(current, uid as u32, gid as u32);
    crate::security::audit::emit(
        crate::security::audit::AuditEvent::Setuid,
        current,
        numbers::SYS_SETUID,
        ok,
        &(uid as u32).to_le_bytes(),
    );
    if ok { 0 } else { SYSCALL_ERROR }
}

fn sys_login(username_ptr: u64, password_ptr: u64) -> u64 {
    // args[2] = username_len, args[3] = password_len (added for correctness)
    // This function signature only exposes ptr; lengths come from dispatch.
    // Use cstr fallback â€” lengths are wired in dispatch below.
    let username = match validate_user_cstr(username_ptr, 63) {
        Some(v) if !v.is_empty() => v,
        _ => return SYSCALL_ERROR,
    };
    let password = match validate_user_cstr(password_ptr, 127) {
        Some(v) => v,
        None => return SYSCALL_ERROR,
    };

    let current = crate::sched::current_index();

    let Some((uid, gid)) = crate::users::login(username, password) else {
        // Audit: authentication failure.
        let mut ctx = [0u8; 32];
        let n = username.len().min(ctx.len());
        ctx[..n].copy_from_slice(&username[..n]);
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::Login,
            current,
            numbers::SYS_LOGIN,
            false,
            &ctx,
        );
        return SYSCALL_ERROR;
    };
    let Some(session_uuid) = crate::session::open(uid, gid) else {
        return SYSCALL_ERROR;
    };

    if !crate::task::table::set_task_identity(current, uid, gid) {
        return SYSCALL_ERROR;
    }
    if !crate::task::table::set_task_session_id(current, session_uuid.into_inner()) {
        return SYSCALL_ERROR;
    }

    // Audit: successful login.
    let mut ctx = [0u8; 32];
    let n = username.len().min(ctx.len());
    ctx[..n].copy_from_slice(&username[..n]);
    crate::security::audit::emit(
        crate::security::audit::AuditEvent::Login,
        current,
        numbers::SYS_LOGIN,
        true,
        &ctx,
    );
    0
}

fn sys_logout() -> u64 {
    let current = crate::sched::current_index();
    let session_id = crate::task::table::task_session_id_at(current);
    if session_id != crate::uuid::Uuid128::NIL {
        let _ = crate::session::close(crate::uuid::SessionUuid(session_id));
    }
    let _ = crate::task::table::set_task_identity(current, 0, 0);
    let _ = crate::task::table::set_task_session_id(current, crate::uuid::Uuid128::NIL);
    crate::security::audit::emit(
        crate::security::audit::AuditEvent::Logout,
        current,
        numbers::SYS_LOGOUT,
        true,
        &[],
    );
    0
}

fn sys_pty_alloc() -> u64 {
    let current = crate::sched::current_index();
    let (uid, gid) = crate::task::table::task_identity_at(current);
    match crate::session::pty_alloc(uid, gid) {
        Some(tty) => tty as u64,
        None => SYSCALL_ERROR,
    }
}

fn sys_pty_write(tty_raw: u64, buf_ptr: u64, len: u64) -> u64 {
    let tty = tty_raw as u32;
    let Some(data) = validate_user_slice(buf_ptr, len) else {
        return SYSCALL_ERROR;
    };
    crate::session::pty_write(tty, data) as u64
}

fn sys_pty_read(tty_raw: u64, buf_ptr: u64, len: u64) -> u64 {
    let tty = tty_raw as u32;
    let Some(out) = validate_user_slice_mut(buf_ptr, len) else {
        return SYSCALL_ERROR;
    };
    crate::session::pty_read(tty, out) as u64
}

fn sys_session_attach(session_uuid_ptr: u64) -> u64 {
    let Some(raw) = read_uuid_handle(session_uuid_ptr) else {
        return SYSCALL_ERROR;
    };
    let session_uuid = crate::uuid::SessionUuid(raw);
    let Some((uid, gid)) = crate::session::owner(session_uuid) else {
        return SYSCALL_ERROR;
    };
    let current = crate::sched::current_index();
    if !crate::task::table::set_task_identity(current, uid, gid) {
        return SYSCALL_ERROR;
    }
    if !crate::task::table::set_task_session_id(current, raw) {
        return SYSCALL_ERROR;
    }
    0
}

fn sys_driver_probe(device_uuid_ptr: u64, out_driver_uuid_ptr: u64) -> u64 {
    let Some(device_uuid_raw) = read_uuid_handle(device_uuid_ptr) else {
        return SYSCALL_ERROR;
    };
    let Some(package_uuid) = crate::drivers::installer::driver_package_for_device(
        crate::uuid::DeviceUuid(device_uuid_raw),
    ) else {
        return 0;
    };

    if out_driver_uuid_ptr != 0 {
        let Some(out) = validate_user_slice_mut(out_driver_uuid_ptr, 16) else {
            return SYSCALL_ERROR;
        };
        out.copy_from_slice(&package_uuid.to_bytes());
    }

    1
}

fn sys_driver_install(
    package_uuid_ptr: u64,
    device_uuid_ptr: u64,
    manifest_ptr: u64,
    sig_ptr: u64,
) -> u64 {
    let current = crate::sched::current_index();
    let (uid, _) = crate::task::table::task_identity_at(current);
    if uid != 0 {
        return SYSCALL_ERROR;
    }

    let Some(package_uuid) = read_uuid_handle(package_uuid_ptr) else {
        return SYSCALL_ERROR;
    };
    let Some(device_uuid) = read_uuid_handle(device_uuid_ptr) else {
        return SYSCALL_ERROR;
    };
    // manifest_ptr encodes: low 32 bits = address, high 32 bits = length
    let manifest_addr = manifest_ptr & 0xffff_ffff;
    let manifest_len = manifest_ptr >> 32;
    if manifest_len == 0 || manifest_len > 4096 {
        return SYSCALL_ERROR;
    }
    let manifest = match validate_user_slice(manifest_addr, manifest_len) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    // sig: exactly 64 bytes at sig_ptr
    let sig_bytes = match validate_user_slice(sig_ptr, 64) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    let sig: &[u8; 64] = match sig_bytes.try_into() {
        Ok(a) => a,
        Err(_) => return SYSCALL_ERROR,
    };

    if crate::drivers::installer::install_signed_driver_package(
        crate::uuid::DriverPackageUuid(package_uuid),
        crate::uuid::DeviceUuid(device_uuid),
        manifest,
        sig,
    ) {
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::DriverInstall,
            current,
            numbers::SYS_DRIVER_INSTALL,
            true,
            &package_uuid.to_bytes(),
        );
        0
    } else {
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::DriverInstall,
            current,
            numbers::SYS_DRIVER_INSTALL,
            false,
            &package_uuid.to_bytes(),
        );
        SYSCALL_ERROR
    }
}

fn sys_vfs_open(path_ptr: u64) -> u64 {
    let path = match validate_user_cstr(path_ptr, 127) {
        Some(s) if !s.is_empty() => s,
        _ => {
            serial::write_line(b"[syscall] vfs_open: invalid path");
            return SYSCALL_ERROR;
        }
    };
    // Session 6: enforce home directory UID ownership.
    // Paths under /home/<user>/ are readable only by root (uid=0) or that user.
    if let Some(owner_uid) = crate::users::home_owner_uid(path) {
        let current = crate::sched::current_index();
        let (caller_uid, _) = crate::task::table::task_identity_at(current);
        if caller_uid != 0 && caller_uid != owner_uid {
            return SYSCALL_ERROR;
        }
    }
    match crate::vfs::open(path) {
        Ok(fd) => fd as u64,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_vfs_read(fd: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    if buf_len == 0 {
        return 0;
    }
    let buf = match validate_user_slice_mut(buf_ptr, buf_len) {
        Some(s) => s,
        None => {
            serial::write_line(b"[syscall] vfs_read: invalid buffer");
            return SYSCALL_ERROR;
        }
    };
    match crate::vfs::read(fd as u32, buf) {
        Ok(n) => n as u64,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_vfs_close(fd: u64) -> u64 {
    match crate::vfs::close(fd as u32) {
        Ok(()) => 0,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_vfs_write(fd: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    if buf_len == 0 {
        return 0;
    }
    let buf = match validate_user_slice(buf_ptr, buf_len) {
        Some(s) => s,
        None => {
            serial::write_line(b"[syscall] vfs_write: invalid buffer");
            return SYSCALL_ERROR;
        }
    };
    match crate::vfs::write(fd as u32, buf) {
        Ok(n) => n as u64,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_vfs_create(path_ptr: u64) -> u64 {
    let path = match validate_user_cstr(path_ptr, 127) {
        Some(s) if !s.is_empty() => s,
        _ => {
            serial::write_line(b"[syscall] vfs_create: invalid path");
            return SYSCALL_ERROR;
        }
    };
    // root-only for now: creating files outside /tmp requires uid=0
    if !path.starts_with(b"/tmp") {
        let current = crate::sched::current_index();
        let (uid, _) = crate::task::table::task_identity_at(current);
        if uid != 0 {
            return SYSCALL_ERROR;
        }
    }
    match crate::vfs::create(path) {
        Ok(fd) => fd as u64,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_vfs_mkdir(path_ptr: u64) -> u64 {
    let path = match validate_user_cstr(path_ptr, 127) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };
    match crate::vfs::mkdir(path) {
        Ok(()) => 0,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_vfs_unlink(path_ptr: u64) -> u64 {
    let path = match validate_user_cstr(path_ptr, 127) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };
    match crate::vfs::unlink(path) {
        Ok(()) => 0,
        Err(_) => SYSCALL_ERROR,
    }
}

/// fs_type constants mirrored in userspace runtime.rs.
const FS_TYPE_RAMFS: u64 = 0;
const FS_TYPE_EXT2: u64 = 1;
const FS_TYPE_FAT32: u64 = 2;

fn sys_mount(path_ptr: u64, fs_type: u64, _reserved: u64) -> u64 {
    // Mount is root-only.
    let current = crate::sched::current_index();
    let (uid, _) = crate::task::table::task_identity_at(current);
    if uid != 0 {
        return SYSCALL_ERROR;
    }
    let path = match validate_user_cstr(path_ptr, 63) {
        Some(s) if !s.is_empty() && s[0] == b'/' => s,
        _ => return SYSCALL_ERROR,
    };
    let ops = match fs_type {
        FS_TYPE_RAMFS => crate::vfs::ramfs::builtin_ops(),
        FS_TYPE_EXT2 => {
            if !crate::vfs::ext2fs::is_mounted() && !crate::vfs::ext2fs::try_mount() {
                return SYSCALL_ERROR;
            }
            crate::vfs::FsOps {
                lookup: crate::vfs::ext2fs::lookup,
                read: crate::vfs::ext2fs::read,
                write: crate::vfs::ext2fs::write,
                fs_name: || b"ext2",
                mkdir: |_| Err(crate::vfs::VfsError::NotSupported),
                unlink: |_| Err(crate::vfs::VfsError::NotSupported),
            }
        }
        FS_TYPE_FAT32 => {
            if !crate::vfs::fat32fs::is_mounted() && !crate::vfs::fat32fs::try_mount() {
                return SYSCALL_ERROR;
            }
            crate::vfs::FsOps {
                lookup: crate::vfs::fat32fs::lookup,
                read: crate::vfs::fat32fs::read,
                write: crate::vfs::fat32fs::write,
                fs_name: || b"fat32",
                mkdir: |_| Err(crate::vfs::VfsError::NotSupported),
                unlink: |_| Err(crate::vfs::VfsError::NotSupported),
            }
        }
        _ => return SYSCALL_ERROR,
    };
    // Use a static path copy so the prefix lifetime is 'static.
    let result = match crate::vfs::mount(path, ops) {
        Ok(()) => 0u64,
        Err(_) => SYSCALL_ERROR,
    };
    let mut ctx = [0u8; 32];
    let n = path.len().min(ctx.len());
    ctx[..n].copy_from_slice(&path[..n]);
    crate::security::audit::emit(
        crate::security::audit::AuditEvent::Mount,
        current,
        numbers::SYS_MOUNT,
        result == 0,
        &ctx,
    );
    result
}

fn sys_umount(path_ptr: u64) -> u64 {
    // Umount is root-only.
    let current = crate::sched::current_index();
    let (uid, _) = crate::task::table::task_identity_at(current);
    if uid != 0 {
        return SYSCALL_ERROR;
    }
    let path = match validate_user_cstr(path_ptr, 63) {
        Some(s) if !s.is_empty() && s[0] == b'/' => s,
        _ => return SYSCALL_ERROR,
    };
    match crate::vfs::umount(path) {
        Ok(()) => 0,
        Err(_) => SYSCALL_ERROR,
    }
}

fn sys_mmap(path_ptr: u64, len: u64, prot: u64, map_flags: u64, offset: u64) -> u64 {
    if crate::task::table::is_user_task(crate::sched::current_index()) {
        let path = if map_flags & crate::mm::address_space::MMAP_FLAG_ANON != 0 {
            None
        } else {
            match validate_user_cstr(path_ptr, 127) {
                Some(s) if !s.is_empty() => Some(s),
                _ => return SYSCALL_ERROR,
            }
        };

        return crate::task::table::mmap_current_user(path, len, prot, map_flags, offset)
            .unwrap_or(SYSCALL_ERROR);
    }

    SYSCALL_ERROR
}

fn sys_munmap(addr: u64, len: u64) -> u64 {
    if crate::task::table::munmap_current_user(addr, len) {
        0
    } else {
        SYSCALL_ERROR
    }
}

// ====================================================================
// Graph syscalls (0x300)
// ====================================================================

fn sys_graph_add_node(kind_raw: u64, flags: u64, creator: u64) -> u64 {
    use crate::graph::types::NodeKind;
    let kind = match NodeKind::from_u16(kind_raw as u16) {
        Some(k) => k,
        None => {
            serial::write_bytes(b"[syscall] graph_add_node: invalid kind=");
            serial::write_u64_dec(kind_raw);
            return SYSCALL_ERROR;
        }
    };
    match crate::graph::arena::add_node(kind, flags as u32, creator) {
        Some(id) => id,
        None => SYSCALL_ERROR,
    }
}

fn sys_graph_add_edge(from: u64, to: u64, kind_raw: u64, flags: u64, weight: u64) -> u64 {
    use crate::graph::types::EdgeKind;
    let kind = match EdgeKind::from_u16(kind_raw as u16) {
        Some(k) => k,
        None => {
            serial::write_bytes(b"[syscall] graph_add_edge: invalid kind=");
            serial::write_u64_dec(kind_raw);
            return SYSCALL_ERROR;
        }
    };
    match crate::graph::arena::add_edge_weighted(from, to, kind, flags as u32, weight as u32) {
        Some(id) => id,
        None => SYSCALL_ERROR,
    }
}

fn sys_graph_node_exists(node_id: u64) -> u64 {
    if crate::graph::arena::node_exists(node_id) {
        1
    } else {
        0
    }
}

fn sys_graph_node_kind(node_id: u64) -> u64 {
    match crate::graph::arena::node_kind(node_id) {
        Some(k) => k as u64,
        None => SYSCALL_ERROR,
    }
}

fn sys_graph_stats() -> u64 {
    let nc = crate::graph::arena::node_count() as u64;
    let ec = crate::graph::arena::edge_count() as u64;
    (ec << 32) | (nc & 0xFFFF_FFFF)
}

/// Trigger one EM M-step to fit per-type-pair walk parameters from
/// accumulated walk observations.  Only uid=0 tasks may call this.
fn sys_graph_em_step() -> u64 {
    let current = crate::sched::current_index();
    let (uid, _) = crate::task::table::task_identity_at(current);
    if uid != 0 {
        return SYSCALL_ERROR;
    }
    crate::graph::temporal::em_step() as u64
}

/// Read EM calibration statistics for a single type pair.
/// arg0 = src_kind (u16), arg1 = dst_kind (u16).
/// Returns packed [0..32]=transitions, [32..64]=epoch.
fn sys_graph_em_stats(src_kind: u64, dst_kind: u64) -> u64 {
    use crate::graph::types::NodeKind;
    let Some(_src) = NodeKind::from_u16(src_kind as u16) else {
        return SYSCALL_ERROR;
    };
    let Some(_dst) = NodeKind::from_u16(dst_kind as u16) else {
        return SYSCALL_ERROR;
    };
    let mut buf = [crate::graph::temporal::TypePairStats::ZERO; 1];
    let epoch =
        crate::graph::temporal::snapshot_stats_pair(src_kind as u16, dst_kind as u16, &mut buf);
    ((epoch as u64) << 32) | (buf[0].transitions as u64)
}

fn sys_graph_generation() -> u64 {
    crate::graph::arena::generation()
}

fn sys_graph_service_lookup(name_ptr: u64) -> u64 {
    let name = match validate_user_cstr(name_ptr, 63) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };

    match crate::graph::bootstrap::service_binding(name) {
        Some((stable_id, node_id)) => {
            ((stable_id as u64) << 48) | (node_id & 0x0000_FFFF_FFFF_FFFF)
        }
        None => SYSCALL_ERROR,
    }
}

fn sys_graph_service_lookup_uuid(name_ptr: u64, uuid_out_ptr: u64, uuid_out_len: u64) -> u64 {
    let name = match validate_user_cstr(name_ptr, 63) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };

    let Some((stable_id, stable_uuid, node_id)) =
        crate::graph::bootstrap::service_binding_with_uuid(name)
    else {
        return SYSCALL_ERROR;
    };

    if uuid_out_ptr != 0 {
        if uuid_out_len < 16 {
            return SYSCALL_ERROR;
        }
        let Some(out) = validate_user_slice_mut(uuid_out_ptr, 16) else {
            return SYSCALL_ERROR;
        };
        out.copy_from_slice(&stable_uuid.into_inner().to_bytes());
    }

    ((stable_id as u64) << 48) | (node_id & 0x0000_FFFF_FFFF_FFFF)
}

#[repr(C)]
struct RegistryLookupOut {
    service_uuid: [u8; 16],
    channel_uuid: [u8; 16],
    task_uuid: [u8; 16],
    channel_alias: u32,
    health: u8,
    _pad: [u8; 3],
}

fn sys_registry_lookup(name_ptr: u64, out_ptr: u64, out_len: u64) -> u64 {
    let name = match validate_user_cstr(name_ptr, 63) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };

    let Some(record) = crate::registry::lookup(name) else {
        return SYSCALL_ERROR;
    };

    if out_ptr != 0 {
        if out_len < size_of::<RegistryLookupOut>() as u64 {
            return SYSCALL_ERROR;
        }
        let Some(out) = validate_user_slice_mut(out_ptr, size_of::<RegistryLookupOut>() as u64)
        else {
            return SYSCALL_ERROR;
        };

        let payload = RegistryLookupOut {
            service_uuid: record.service_uuid.into_inner().to_bytes(),
            channel_uuid: record.channel_uuid.into_inner().to_bytes(),
            task_uuid: record.task_uuid.into_inner().to_bytes(),
            channel_alias: record.channel_alias,
            health: record.health as u8,
            _pad: [0; 3],
        };

        let raw = unsafe {
            core::slice::from_raw_parts(
                (&payload as *const RegistryLookupOut).cast::<u8>(),
                size_of::<RegistryLookupOut>(),
            )
        };
        out[..raw.len()].copy_from_slice(raw);
    }

    record.channel_alias as u64
}

fn sys_registry_register(name_ptr: u64, channel_alias: u64) -> u64 {
    let name = match validate_user_cstr(name_ptr, 63) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };

    let channel_alias = channel_alias as u32;
    let channel_uuid = crate::ipc::channel::uuid_for_alias(channel_alias);
    if channel_alias == 0 || !crate::ipc::channel::is_active(channel_uuid) {
        return SYSCALL_ERROR;
    }

    let task_id = crate::task::table::task_id_at(crate::sched::current_index());
    let task_uuid = crate::uuid::TaskUuid::from_task_id(task_id);
    if crate::registry::register_dynamic(name, channel_alias, task_uuid) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_registry_subscribe(last_seen_generation: u64, notify_channel: u32) -> u64 {
    if notify_channel != 0 {
        let uuid = crate::ipc::channel::uuid_for_alias(notify_channel);
        if crate::ipc::channel::is_active(uuid) {
            crate::registry::register_subscriber(notify_channel);
        }
    }
    let current = crate::registry::generation();
    if current == last_seen_generation {
        0
    } else {
        current
    }
}

fn sys_ipc_cap_grant(target_task_id: u64, channel_alias: u64, perms: u64) -> u64 {
    if channel_alias == 0 {
        return SYSCALL_ERROR;
    }
    let target_task_id = target_task_id as crate::task::tcb::TaskId;
    let channel_alias = channel_alias as u32;
    let channel_uuid = crate::ipc::channel::uuid_for_alias(channel_alias);
    if !crate::ipc::channel::is_active(channel_uuid) {
        return SYSCALL_ERROR;
    }
    let perms = perms as u8;

    if crate::ipc::capability::delegate(
        crate::sched::current_index(),
        target_task_id,
        channel_uuid,
        perms,
    ) {
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::CapGrant,
            crate::sched::current_index(),
            numbers::SYS_IPC_CAP_GRANT,
            true,
            &channel_alias.to_le_bytes(),
        );
        0
    } else {
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::CapGrant,
            crate::sched::current_index(),
            numbers::SYS_IPC_CAP_GRANT,
            false,
            &channel_alias.to_le_bytes(),
        );
        SYSCALL_ERROR
    }
}

fn sys_ipc_cap_revoke(target_task_id: u64, channel_alias: u64, perms: u64) -> u64 {
    if channel_alias == 0 {
        return SYSCALL_ERROR;
    }
    let target_task_id = target_task_id as crate::task::tcb::TaskId;
    let channel_alias = channel_alias as u32;
    let channel_uuid = crate::ipc::channel::uuid_for_alias(channel_alias);
    if !crate::ipc::channel::is_active(channel_uuid) {
        return SYSCALL_ERROR;
    }
    let perms = perms as u8;

    if crate::ipc::capability::revoke(
        crate::sched::current_index(),
        target_task_id,
        channel_uuid,
        perms,
    ) {
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::CapRevoke,
            crate::sched::current_index(),
            numbers::SYS_IPC_CAP_REVOKE,
            true,
            &channel_alias.to_le_bytes(),
        );
        0
    } else {
        crate::security::audit::emit(
            crate::security::audit::AuditEvent::CapRevoke,
            crate::sched::current_index(),
            numbers::SYS_IPC_CAP_REVOKE,
            false,
            &channel_alias.to_le_bytes(),
        );
        SYSCALL_ERROR
    }
}

// ====================================================================
// Memory syscalls (SYS_SLEEP)
// ====================================================================

fn sys_sleep(ticks: u64) -> u64 {
    crate::sched::sleep_for_ticks(ticks);
    0
}

// ====================================================================
// Thread syscalls (0x010â€“0x012)
// ====================================================================

/// Spawn a new user-mode thread in the caller's address space.
///
/// arg0 = ring-3 entry point
/// arg1 = argument passed in rdi when the thread starts
/// arg2 = top of caller-allocated user stack (result of SYS_MMAP)
fn sys_thread_spawn(entry: u64, arg: u64, user_stack_top: u64) -> u64 {
    let cur = crate::sched::current_index();
    if !crate::task::table::is_user_task(cur) {
        serial::write_line(b"[syscall] thread_spawn: only user tasks may spawn threads");
        return SYSCALL_ERROR;
    }
    if entry == 0 || user_stack_top == 0 {
        serial::write_line(b"[syscall] thread_spawn: invalid entry or stack");
        return SYSCALL_ERROR;
    }
    let cr3 = crate::task::table::task_cr3_at(cur);
    match crate::task::table::create_user_thread(entry, arg, user_stack_top, cr3) {
        Some(id) => id,
        None => {
            serial::write_line(b"[syscall] thread_spawn: failed to create thread");
            SYSCALL_ERROR
        }
    }
}

/// Block the calling thread until thread `tid` has exited.
fn sys_thread_join(tid: u64) -> u64 {
    if tid == 0 {
        return SYSCALL_ERROR;
    }
    let cur = crate::sched::current_index();
    match crate::task::table::block_for_join_if_target_alive(cur, tid) {
        crate::task::table::JoinBlockResult::TargetAlreadyDead => 0,
        crate::task::table::JoinBlockResult::InvalidCaller => SYSCALL_ERROR,
        crate::task::table::JoinBlockResult::Blocked => {
            // The task is now Blocked; calling schedule() will switch to the
            // next Ready task. We resume here only when mark_dead() wakes us.
            unsafe { crate::sched::schedule() };
            0
        }
    }
}

/// Exit the calling thread.
fn sys_thread_exit(code: u64) -> u64 {
    sys_exit(code)
}

// ====================================================================
// Surface syscalls (0x400)
// ====================================================================

fn sys_surface_create(width_raw: u64, height_raw: u64) -> u64 {
    // Only ring-3 tasks may create surfaces.
    if !crate::task::table::is_user_task(crate::sched::current_index()) {
        serial::write_line(b"[syscall] surface_create: kernel task not allowed");
        return SYSCALL_ERROR;
    }

    let width = (width_raw & 0xFFFF) as u16;
    let height = (height_raw & 0xFFFF) as u16;
    if width == 0 || height == 0 {
        serial::write_line(b"[syscall] surface_create: zero dimension");
        return SYSCALL_ERROR;
    }

    // Enforce a per-surface size cap to prevent frame exhaustion.
    let pixel_bytes = (width as u64) * (height as u64) * 4;
    if pixel_bytes > (crate::wm::surface_table::MAX_SURFACE_FRAMES as u64) * 4096 {
        serial::write_line(b"[syscall] surface_create: surface too large");
        return SYSCALL_ERROR;
    }

    let owner = crate::sched::current_index();
    let surface_id = match crate::wm::surface_table::alloc_surface(width, height, owner) {
        Ok(id) => id,
        Err(e) => {
            serial::write_bytes(b"[syscall] surface_create: alloc failed: ");
            match e {
                crate::wm::surface_table::SurfaceError::TableFull => {
                    serial::write_line(b"table full")
                }
                crate::wm::surface_table::SurfaceError::DimensionsTooLarge => {
                    serial::write_line(b"dimensions too large")
                }
                crate::wm::surface_table::SurfaceError::OutOfMemory => {
                    serial::write_line(b"out of memory")
                }
            }
            return SYSCALL_ERROR;
        }
    };

    // Retrieve the allocated frames and map them into the calling task's AS.
    let mut frames = [0u64; crate::wm::surface_table::MAX_SURFACE_FRAMES];
    let frame_count = crate::wm::surface_table::surface_frames(surface_id, &mut frames);
    if frame_count == 0 {
        crate::wm::surface_table::free_surface(surface_id);
        return SYSCALL_ERROR;
    }

    let mapped_vaddr = crate::task::table::mmap_current_user_shared(
        &frames[..frame_count],
        crate::mm::address_space::MMAP_PROT_READ | crate::mm::address_space::MMAP_PROT_WRITE,
        surface_id,
    );
    if mapped_vaddr == 0 {
        crate::wm::surface_table::free_surface(surface_id);
        serial::write_line(b"[syscall] surface_create: failed to map frames");
        return SYSCALL_ERROR;
    }

    serial::write_bytes(b"[syscall] surface_create: sid=");
    serial::write_u64_dec_inline(surface_id as u64);
    serial::write_bytes(b" frames=");
    serial::write_u64_dec_inline(frame_count as u64);
    serial::write_bytes(b" vaddr=");
    serial::write_hex(mapped_vaddr);

    // Register with the Phase J compositor pipeline.
    crate::wm::gpu::register_surface(surface_id, 0, 0);

    // Return packed: surface_id in low 32 bits, vaddr low 32 bits in high 32.
    let id_bits = surface_id as u64;
    let vaddr_bits = (mapped_vaddr & 0xFFFF_FFFF) << 32;
    id_bits | vaddr_bits
}

fn sys_surface_present(surface_id_raw: u64) -> u64 {
    let surface_id = surface_id_raw as u32;
    let caller = crate::sched::current_index();

    // Validate ownership.
    match crate::wm::surface_table::surface_owner(surface_id) {
        Some(owner) if owner == caller => {}
        Some(_) => {
            serial::write_line(b"[syscall] surface_present: caller does not own surface");
            return SYSCALL_ERROR;
        }
        None => {
            serial::write_line(b"[syscall] surface_present: surface not found");
            return SYSCALL_ERROR;
        }
    }

    // Cube-only desktop mode: if no compositor service is declared, let the
    // first presenting app claim runtime scanout so committed frames become
    // visible without waiting for compositor-driven handoff.
    if crate::ui::direct_present_policy::should_auto_claim_runtime_display(
        crate::ui::desktop::runtime_display_claimed(),
        crate::userland::manifest_declares_service(b"compositor"),
    ) {
        serial::write_bytes(b"[syscall] surface_present: auto-claim runtime display sid=");
        serial::write_u64_dec(surface_id as u64);
        if !crate::ui::desktop::claim_runtime_display(surface_id) {
            serial::write_line(b"[syscall] surface_present: runtime auto-claim failed");
        }
    }

    match surface_commit_and_wake(surface_id) {
        Ok(_) => 0,
        Err(_) => {
            serial::write_line(b"[syscall] surface_present: present queue full");
            SYSCALL_ERROR
        }
    }
}

fn surface_commit_and_wake(surface_id: u32) -> Result<u32, crate::wm::surface_table::PresentError> {
    let commit = crate::wm::gpu::surface_commit(surface_id)?;

    let n = SURFACE_COMMIT_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    let per_sid_ok = note_surface_commit_for_log(surface_id);
    if n < 64 || per_sid_ok {
        serial::write_bytes(b"[syscall] surface_commit sid=");
        serial::write_u64_dec_inline(surface_id as u64);
        serial::write_bytes(b" commit=");
        serial::write_u64_dec_inline(commit as u64);
        serial::write_bytes(b" wake display broker\n");
    }

    // A commit registers new content for scanout/composition. Keep the kernel
    // in the role of display broker only: wake the display task, but do not
    // synthesize a parallel compositor wake protocol from inside the kernel.
    crate::sched::wake_desktop_task();

    // Keep syscall latency bounded: do not run full-frame composition from the
    // commit syscall path. The dedicated display task ticker is responsible
    // for cadence-driven frame composition once runtime scanout is claimed.

    // No-compositor desktop mode: present app surfaces directly to scanout on
    // every commit so visual output does not depend on compositor scene ticks.
    if crate::ui::desktop::runtime_display_claimed()
        && !crate::userland::manifest_declares_service(b"compositor")
        && let Some((w, h)) = crate::wm::surface_table::surface_dimensions(surface_id)
    {
        let (cursor_x, cursor_y, cursor_buttons) = crate::input::router::pointer_state();
        crate::drivers::gpu::virtio_gpu::blit_surface_scene(
            surface_id, w as u32, h as u32, 0, 0, 1024, 255,
        );
        crate::drivers::gpu::virtio_gpu::draw_cursor_overlay(cursor_x, cursor_y, cursor_buttons);
        crate::drivers::gpu::virtio_gpu::flush_rect(0, 0, w as u32, h as u32);
    }

    Ok(commit)
}

fn sys_surface_flush() -> u64 {
    let current = crate::sched::current_index();
    let compositor = COMPOSITOR_TASK_INDEX.load(Ordering::Acquire);
    if current == 0 || compositor == usize::MAX || current != compositor {
        return SYSCALL_ERROR;
    }

    crate::sched::wake_desktop_task();

    let n = SURFACE_FLUSH_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 64 {
        serial::write_bytes(b"[syscall] surface_flush deprecated; waking display broker idx=");
        serial::write_u64_dec(current as u64);
        serial::write_bytes(b"\n");
    }
    0
}

fn sys_surface_destroy(surface_id_raw: u64) -> u64 {
    let surface_id = surface_id_raw as u32;
    let caller = crate::sched::current_index();

    match crate::wm::surface_table::surface_owner(surface_id) {
        Some(owner) if owner == caller => {}
        Some(_) => {
            serial::write_line(b"[syscall] surface_destroy: caller does not own surface");
            return SYSCALL_ERROR;
        }
        None => {
            serial::write_line(b"[syscall] surface_destroy: not found");
            return SYSCALL_ERROR;
        }
    }

    // Free the surface (frames returned to allocator).
    // The VMA in the caller's address space is left as-is; the kernel does
    // not automatically unmap. Callers should munmap before destroy.
    if crate::wm::surface_table::free_surface(surface_id) {
        crate::wm::gpu::unregister_surface(surface_id);
        0
    } else {
        SYSCALL_ERROR
    }
}

// ====================================================================
// Input syscalls (0x410)
// ====================================================================

/// Set keyboard focus for the calling task on the given IPC channel.
/// arg0 = channel id (u32); 0 releases focus.
fn sys_input_set_focus(channel_raw: u64) -> u64 {
    let channel = (channel_raw & 0xFFFF_FFFF) as u32;
    let caller = crate::sched::current_index();

    if channel == 0 {
        crate::input::router::set_focus(usize::MAX, 0);
    } else {
        crate::input::router::set_focus(caller, channel);
    }
    0
}

/// Register the calling task's window rect for pointer hit-testing.
/// arg0 = x (i16 low 16) | y (i16 high 16) as packed i32.
/// arg1 = w (u16 low 16) | h (u16 high 16).
/// arg2 = IPC channel to deliver pointer events.
fn sys_input_register_window(xy_raw: u64, wh_raw: u64, channel_raw: u64) -> u64 {
    let x = (xy_raw & 0xFFFF) as i16;
    let y = ((xy_raw >> 16) & 0xFFFF) as i16;
    let w = (wh_raw & 0xFFFF) as u16;
    let h = ((wh_raw >> 16) & 0xFFFF) as u16;
    let channel = (channel_raw & 0xFFFF_FFFF) as u32;
    let caller = crate::sched::current_index();

    let log_n = INPUT_REGISTER_WINDOW_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if log_n < 64 {
        serial::write_bytes(b"[syscall] input_register_window enter task=");
        serial::write_u64_dec_inline(caller as u64);
        serial::write_bytes(b" ch=");
        serial::write_u64_dec_inline(channel as u64);
        serial::write_bytes(b" x=");
        serial::write_u64_dec_inline(x as u16 as u64);
        serial::write_bytes(b" y=");
        serial::write_u64_dec_inline(y as u16 as u64);
        serial::write_bytes(b" w=");
        serial::write_u64_dec_inline(w as u64);
        serial::write_bytes(b" h=");
        serial::write_u64_dec(h as u64);
    }

    if w == 0 || h == 0 || channel == 0 {
        if log_n < 64 {
            serial::write_line(b"[syscall] input_register_window rejected invalid args");
        }
        return SYSCALL_ERROR;
    }

    let ok = crate::input::router::register_window(caller, channel, x, y, w, h);
    if ok && caller == COMPOSITOR_TASK_INDEX.load(Ordering::Acquire) {
        crate::input::router::set_compositor_channel(channel);
    }
    if log_n < 64 {
        if ok {
            serial::write_line(b"[syscall] input_register_window ok");
        } else {
            serial::write_line(b"[syscall] input_register_window failed: table full");
        }
    }

    if ok { 0 } else { SYSCALL_ERROR }
}

/// Unregister the calling task's window rectangle.
fn sys_input_unregister_window() -> u64 {
    let caller = crate::sched::current_index();
    crate::input::router::unregister_window(caller);
    if caller == COMPOSITOR_TASK_INDEX.load(Ordering::Acquire) {
        crate::input::router::set_compositor_channel(0);
    }
    0
}

fn sys_frame_tick_subscribe(channel_raw: u64) -> u64 {
    let channel_alias = (channel_raw & 0xFFFF_FFFF) as u32;
    if crate::ui::desktop::subscribe_frame_tick(channel_alias) {
        0
    } else {
        SYSCALL_ERROR
    }
}

// ====================================================================
// Watchdog heartbeat (Phase H)
// ====================================================================

fn sys_heartbeat() -> u64 {
    let task_id = crate::task::table::task_id_at(crate::sched::current_index());
    if crate::svc::heartbeat_from_task(task_id) {
        0
    } else {
        SYSCALL_ERROR
    }
}

// â”€â”€ PMU performance counters â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn sys_perf_sample(uuid_ptr: u64) -> u64 {
    let uuid = if uuid_ptr == 0 {
        // Sample the calling task.
        let task_id = crate::task::table::task_id_at(crate::sched::current_index());
        crate::uuid::TaskUuid::from_task_id(task_id).into_inner()
    } else {
        match read_uuid_handle(uuid_ptr) {
            Some(u) => u,
            None => return SYSCALL_ERROR,
        }
    };
    crate::perf::sample(uuid);
    0
}

fn sys_perf_read(uuid_ptr: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    let uuid = match read_uuid_handle(uuid_ptr) {
        Some(u) => u,
        None => return SYSCALL_ERROR,
    };
    let sample = match crate::perf::read(uuid) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    // Serialise as: uuid(16) + cycles(8) + instructions(8) + llc_misses(8) +
    //   branch_misses(8) + samples(4) = 52 bytes
    const RECORD_LEN: u64 = 52;
    if buf_len < RECORD_LEN {
        return SYSCALL_ERROR;
    }
    let out = match validate_user_slice_mut(buf_ptr, RECORD_LEN) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    out[0..16].copy_from_slice(&sample.task_uuid.to_bytes());
    out[16..24].copy_from_slice(&sample.cycles.to_le_bytes());
    out[24..32].copy_from_slice(&sample.instructions.to_le_bytes());
    out[32..40].copy_from_slice(&sample.llc_misses.to_le_bytes());
    out[40..48].copy_from_slice(&sample.branch_misses.to_le_bytes());
    out[48..52].copy_from_slice(&sample.samples.to_le_bytes());
    RECORD_LEN
}

// â”€â”€ Audit â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Drain security audit records into a caller-supplied buffer.
/// Only `protected_strict` (system service) tasks may call this.
fn sys_audit_read(buf_ptr: u64, buf_len: u64) -> u64 {
    // Only system-level tasks (uid == 0) may drain the audit ring.
    let current = crate::sched::current_index();
    let (uid, _) = crate::task::table::task_identity_at(current);
    if uid != 0 {
        return SYSCALL_ERROR;
    }
    // Buffer must be a non-zero multiple of RECORD_BYTES.
    if buf_len == 0 || !buf_len.is_multiple_of(crate::security::audit::RECORD_BYTES as u64) {
        return SYSCALL_ERROR;
    }
    let buf = match validate_user_slice_mut(buf_ptr, buf_len) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    crate::security::audit::drain_to_bytes(buf)
}

/// Fill caller-provided buffer with kernel-backed entropy.
fn sys_getrandom(buf_ptr: u64, buf_len: u64) -> u64 {
    if buf_len == 0 {
        return 0;
    }
    let out = match validate_user_slice_mut(buf_ptr, buf_len) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };

    let mut off = 0usize;
    while off < out.len() {
        let word = crate::arch::x86_64::cpu_init::rdrand_entropy().to_le_bytes();
        let n = (out.len() - off).min(word.len());
        out[off..off + n].copy_from_slice(&word[..n]);
        off += n;
    }
    out.len() as u64
}

// â”€â”€ Wi-Fi â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn sys_wifi_scan() -> u64 {
    if crate::drivers::net::wifi::start_scan() {
        1
    } else {
        0
    }
}

fn sys_wifi_connect(ssid_ptr: u64, pass_ptr: u64) -> u64 {
    let ssid = match validate_user_cstr(ssid_ptr, 32) {
        Some(s) if !s.is_empty() => s,
        _ => return SYSCALL_ERROR,
    };
    let pass = match validate_user_cstr(pass_ptr, 63) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    if crate::drivers::net::wifi::associate(ssid, pass) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_wifi_state() -> u64 {
    crate::drivers::net::wifi::state() as u64
}

// â”€â”€ Bluetooth â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn sys_bt_scan() -> u64 {
    if crate::drivers::bt::start_scan() {
        1
    } else {
        SYSCALL_ERROR
    }
}

fn sys_bt_connect(acl_handle: u64, psm: u64) -> u64 {
    let cid = crate::drivers::bt::l2cap_connect(acl_handle as u16, psm as u16);
    if cid == 0 { SYSCALL_ERROR } else { cid as u64 }
}

fn sys_bt_send(cid: u64, buf_ptr: u64, buf_len: u64) -> u64 {
    if buf_len == 0 || buf_len > 1024 {
        return SYSCALL_ERROR;
    }
    let payload = match validate_user_slice(buf_ptr, buf_len) {
        Some(s) => s,
        None => return SYSCALL_ERROR,
    };
    if crate::drivers::bt::l2cap_send(cid as u16, payload) {
        0
    } else {
        SYSCALL_ERROR
    }
}

fn sys_bt_close(cid: u64) -> u64 {
    crate::drivers::bt::l2cap_close(cid as u16);
    0
}

fn sys_cognitive_index(doc_ptr: u64, doc_len: u64, doc_type: u64, creator_node: u64) -> u64 {
    let input = match validate_user_slice(doc_ptr, doc_len) {
        Some(s) if !s.is_empty() => s,
        _ => {
            serial::write_line(b"[syscall] cognitive_index: invalid doc buffer");
            return SYSCALL_ERROR;
        }
    };
    let stats = crate::cognitive::engine::index_document(input, doc_type as u8, creator_node);
    let lo = (stats.chunk_count as u64) & 0xFFFF_FFFF;
    let hi = (stats.terms_indexed as u64) << 32;
    lo | hi
}

fn sys_cognitive_query(query_ptr: u64, query_len: u64, fingerprint: u64) -> u64 {
    let query = match validate_user_slice(query_ptr, query_len) {
        Some(s) if !s.is_empty() => s,
        _ => {
            serial::write_line(b"[syscall] cognitive_query: invalid query buffer");
            return SYSCALL_ERROR;
        }
    };
    let result = crate::cognitive::engine::run_query(query, fingerprint);
    let phase = (result.phase_reached as u64) & 0xFF;
    let evidence = ((result.evidence_count as u64) & 0xFF) << 8;
    let confidence = ((result.confidence >> 8) as u64 & 0xFFFF) << 16;
    let strategy = ((result.strategy as u64) & 0xFF) << 32;
    phase | evidence | confidence | strategy
}

fn sys_cognitive_redact(in_ptr: u64, in_len: u64, out_ptr: u64, out_len: u64) -> u64 {
    let input = match validate_user_slice(in_ptr, in_len) {
        Some(s) if !s.is_empty() => s,
        _ => {
            serial::write_line(b"[syscall] redact: invalid input buffer");
            return SYSCALL_ERROR;
        }
    };
    let output = match validate_user_slice_mut(out_ptr, out_len) {
        Some(s) if !s.is_empty() => s,
        _ => {
            serial::write_line(b"[syscall] redact: invalid output buffer");
            return SYSCALL_ERROR;
        }
    };
    let r = crate::cognitive::redact::redact(input, output);
    let lo = (r.output_len as u64) & 0xFFFF_FFFF;
    let hi = (r.redaction_count as u64) << 32;
    lo | hi
}

// â”€â”€ Phase J GPU compositor syscalls â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn sys_surface_commit(surface_id_raw: u64) -> u64 {
    let surface_id = surface_id_raw as u32;
    let attempt = SURFACE_COMMIT_ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
    // Validate ownership using the existing surface_table.
    let task = crate::sched::current_index();
    let owner = crate::wm::surface_table::surface_owner(surface_id);
    let trace_n = SURFACE_COMMIT_PATH_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if trace_n < 64 {
        serial::write_bytes(b"[syscall] surface_commit entry sid=");
        serial::write_u64_dec_inline(surface_id as u64);
        serial::write_bytes(b" task=");
        serial::write_u64_dec_inline(task as u64);
        serial::write_bytes(b" attempt=");
        serial::write_u64_dec_inline(attempt as u64);
        serial::write_bytes(b" owner=");
        match owner {
            Some(o) => serial::write_u64_dec_inline(o as u64),
            None => serial::write_bytes(b"none"),
        }
        serial::write_bytes(b" runtime_claimed=");
        if crate::ui::desktop::runtime_display_claimed() {
            serial::write_bytes(b"1");
        } else {
            serial::write_bytes(b"0");
        }
        serial::write_bytes(b" compositor_declared=");
        if crate::userland::manifest_declares_service(b"compositor") {
            serial::write_bytes(b"1\n");
        } else {
            serial::write_bytes(b"0\n");
        }
    }
    if let Some(owner) = owner {
        if owner != task {
            SURFACE_COMMIT_FAIL_OWNER.fetch_add(1, Ordering::Relaxed);
            serial::write_bytes(b"[syscall] surface_commit owner mismatch sid=");
            serial::write_u64_dec_inline(surface_id as u64);
            serial::write_bytes(b" task=");
            serial::write_u64_dec_inline(task as u64);
            serial::write_bytes(b" owner=");
            serial::write_u64_dec(owner as u64);
            return SYSCALL_ERROR;
        }
    } else {
        SURFACE_COMMIT_FAIL_MISSING.fetch_add(1, Ordering::Relaxed);
        serial::write_bytes(b"[syscall] surface_commit missing surface sid=");
        serial::write_u64_dec_inline(surface_id as u64);
        serial::write_bytes(b" task=");
        serial::write_u64_dec(task as u64);
        return SYSCALL_ERROR;
    }

    // Direct-present desktop mode: when no compositor service is declared,
    // let the first committed surface claim runtime scanout so frame commits
    // become visible without requiring a ring-3 compositor handoff.
    if crate::ui::direct_present_policy::should_auto_claim_runtime_display(
        crate::ui::desktop::runtime_display_claimed(),
        crate::userland::manifest_declares_service(b"compositor"),
    ) {
        serial::write_bytes(b"[syscall] surface_commit: auto-claim runtime display sid=");
        serial::write_u64_dec(surface_id as u64);
        if !crate::ui::desktop::claim_runtime_display(surface_id) {
            serial::write_line(b"[syscall] surface_commit: runtime auto-claim failed");
        }
    }

    match surface_commit_and_wake(surface_id) {
        Ok(counter) => counter as u64,
        Err(_) => {
            SURFACE_COMMIT_FAIL_PRESENT.fetch_add(1, Ordering::Relaxed);
            serial::write_bytes(b"[syscall] surface_commit queue/full sid=");
            serial::write_u64_dec_inline(surface_id as u64);
            serial::write_bytes(b" task=");
            serial::write_u64_dec(task as u64);
            SYSCALL_ERROR
        }
    }
}

fn sys_expose_toggle(window_w_raw: u64, window_h_raw: u64) -> u64 {
    let w = (window_w_raw as u32).max(1);
    let h = (window_h_raw as u32).max(1);
    crate::wm::gpu::toggle_expose(w, h);
    use core::sync::atomic::Ordering;
    // Return 1 if ExposÃ© is now active, 0 if now inactive.
    if crate::wm::gpu::EXPOSE_MODE.load(Ordering::Acquire) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::should_auto_claim_runtime_display;

    #[test]
    fn auto_claim_when_unclaimed_and_no_compositor() {
        assert!(should_auto_claim_runtime_display(false, false));
    }

    #[test]
    fn no_auto_claim_when_already_claimed() {
        assert!(!should_auto_claim_runtime_display(true, false));
    }

    #[test]
    fn no_auto_claim_when_compositor_declared() {
        assert!(!should_auto_claim_runtime_display(false, true));
    }

    #[test]
    fn no_auto_claim_when_claimed_and_compositor_declared() {
        assert!(!should_auto_claim_runtime_display(true, true));
    }
}

fn sys_surface_transform(surface_id_raw: u64, out_ptr: u64) -> u64 {
    let surface_id = surface_id_raw as u32;
    let Some(out) = validate_user_slice_mut(out_ptr, 4 * core::mem::size_of::<i32>() as u64) else {
        return SYSCALL_ERROR;
    };
    match crate::wm::gpu::surface_transform(surface_id) {
        Some((x, y, scale, opacity)) => {
            out[0..4].copy_from_slice(&x.to_le_bytes());
            out[4..8].copy_from_slice(&y.to_le_bytes());
            out[8..12].copy_from_slice(&scale.to_le_bytes());
            out[12..16].copy_from_slice(&opacity.to_le_bytes());
            0
        }
        None => SYSCALL_ERROR,
    }
}

// â”€â”€ OTA update â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// SYS_FETCH_UPDATE: fetch an update bundle via HTTP GET and stage it.
///
/// The 32 MiB staging buffer is statically allocated to avoid heap use.
/// One fetch at a time (protected by the update module's internal Mutex).
fn sys_fetch_update(url_ptr: u64, url_len: u64) -> u64 {
    if url_len == 0 || url_len > 2048 {
        serial::write_line(b"[syscall] fetch_update: bad url_len");
        return SYSCALL_ERROR;
    }

    // Security gate: refuse to fetch over cleartext until the TLS layer
    // is available.  This prevents downgrade attacks where a network-level
    // adversary intercepts the bundle fetch even though the bundle itself
    // is ed25519-signed â€” defence in depth requires transport encryption.
    if !crate::net::tls::is_available() {
        serial::write_line(
            b"[syscall] fetch_update: blocked - TLS not yet available (fail-closed)",
        );
        return SYSCALL_ERROR;
    }

    let Some(url) = validate_user_slice(url_ptr, url_len) else {
        return SYSCALL_ERROR;
    };

    // Static staging buffer â€” 32 MiB max bundle size.
    // Safety: only one sys_fetch_update call at a time (serialised by caller
    // convention; kernel is single-core today).
    static mut BUNDLE_BUF: [u8; 32 * 1024 * 1024] = [0u8; 32 * 1024 * 1024];
    // SAFETY: kernel is single-threaded (no SMP today); the buffer is only
    // written by this function and read by stage_update before this returns.
    let buf_ptr = core::ptr::addr_of_mut!(BUNDLE_BUF);
    let buf = unsafe { &mut *buf_ptr };

    let task_index = crate::sched::current_index();
    let result = crate::net::http::http_get(task_index, url, buf);

    match result {
        crate::net::http::HttpGetResult::Ok(n) => {
            serial::write_bytes(b"[update] fetched bytes=");
            serial::write_hex(n as u64);
            serial::write_line(b"");
            if crate::update::stage_update(&buf[..n]) {
                0
            } else {
                serial::write_line(b"[update] stage_update rejected bundle");
                SYSCALL_ERROR
            }
        }
        crate::net::http::HttpGetResult::BadUrl => {
            serial::write_line(b"[update] fetch: bad url");
            SYSCALL_ERROR
        }
        crate::net::http::HttpGetResult::ConnectFailed => {
            serial::write_line(b"[update] fetch: connect failed");
            SYSCALL_ERROR
        }
        crate::net::http::HttpGetResult::SendFailed => {
            serial::write_line(b"[update] fetch: send failed");
            SYSCALL_ERROR
        }
        crate::net::http::HttpGetResult::RecvFailed => {
            serial::write_line(b"[update] fetch: recv failed");
            SYSCALL_ERROR
        }
        crate::net::http::HttpGetResult::BadStatus(code) => {
            serial::write_bytes(b"[update] fetch: http status=");
            serial::write_hex(code as u64);
            serial::write_line(b"");
            SYSCALL_ERROR
        }
        crate::net::http::HttpGetResult::BodyTooLarge => {
            serial::write_line(b"[update] fetch: body too large");
            SYSCALL_ERROR
        }
    }
}

fn sys_tls_set_available() -> u64 {
    let task_idx = crate::sched::current_index();
    if !crate::security::seccomp::is_protected_strict(task_idx) {
        serial::write_line(
            b"[syscall] tls_set_available: permission denied (not protected_strict)",
        );
        return SYSCALL_ERROR;
    }
    crate::net::tls::set_available();
    0
}

fn sys_tls_set_unavailable() -> u64 {
    let task_idx = crate::sched::current_index();
    if !crate::security::seccomp::is_protected_strict(task_idx) {
        serial::write_line(b"[syscall] tls_set_unavailable: permission denied");
        return SYSCALL_ERROR;
    }
    crate::net::tls::set_unavailable();
    0
}

// ====================================================================
// GPU resource syscalls (0x500â€“0x504)
// ====================================================================

/// Gate: only the registered compositor task may call GPU resource syscalls.
fn is_compositor_caller() -> bool {
    let caller = crate::sched::current_index();
    let compositor = COMPOSITOR_TASK_INDEX.load(Ordering::Acquire);
    compositor != usize::MAX && caller == compositor
}

/// SYS_COMPOSITOR_CLAIM_DISPLAY â€” hand scanout ownership to the compositor and
/// bind a fullscreen surface as the desktop background.
fn sys_compositor_claim_display(surface_id: u32) -> u64 {
    serial::write_line(b"[claim_display] ENTRY");

    if !is_compositor_caller() {
        serial::write_line(b"[syscall] compositor_claim_display: EPERM");
        return SYSCALL_ERROR;
    }

    serial::write_line(b"[claim_display] caller check PASSED");

    let caller = crate::sched::current_index();
    match crate::wm::surface_table::surface_owner(surface_id) {
        Some(owner) if owner == caller => {}
        Some(_) => {
            serial::write_line(b"[syscall] compositor_claim_display: caller does not own surface");
            return SYSCALL_ERROR;
        }
        None => {
            serial::write_line(b"[syscall] compositor_claim_display: surface not found");
            return SYSCALL_ERROR;
        }
    }

    serial::write_line(b"[claim_display] surface ownership VERIFIED");
    serial::write_line(b"[claim_display] CALLING claim_runtime_display()");

    if !crate::ui::desktop::claim_runtime_display(surface_id) {
        serial::write_line(b"[syscall] compositor_claim_display: runtime handoff failed");
        return SYSCALL_ERROR;
    }

    serial::write_line(b"[claim_display] claim_runtime_display() COMPLETED");

    crate::sched::notify_desktop_activity();
    crate::sched::wake_desktop_task();

    serial::write_line(b"[claim_display] RETURNING success");
    0
}
///
/// Write GPU capabilities into the user-provided output struct.
///
/// Layout must match `userspace/gfx/src/device.rs::RawCaps`.
fn sys_gpu_query_caps(out_ptr: u64) -> u64 {
    #[repr(C)]
    struct RawCaps {
        flags: u32,
        screen_w: u32,
        screen_h: u32,
        _pad: u32,
    }

    const CAPS_F_PRESENT_2D: u32 = 1 << 0;
    const CAPS_F_HW_FILL: u32 = 1 << 1;
    const CAPS_F_HW_BLUR: u32 = 1 << 2;
    const CAPS_F_DEPTH_3D: u32 = 1 << 3;
    const CAPS_F_SHADERS: u32 = 1 << 4;

    let Some(out) = validate_user_slice_mut(out_ptr, core::mem::size_of::<RawCaps>() as u64) else {
        serial::write_line(b"[syscall] gpu_query_caps: bad out ptr");
        return SYSCALL_ERROR;
    };

    let (screen_w, screen_h) = crate::drivers::gpu::virtio_gpu::resolution();
    let mut flags = 0u32;

    if crate::drivers::gpu::virtio_gpu::is_present() {
        flags |= CAPS_F_PRESENT_2D;
    }

    // Keep capability shape stable for userspace while the backend remains
    // software-raster + virtio present.
    flags |= CAPS_F_HW_FILL | CAPS_F_HW_BLUR | CAPS_F_DEPTH_3D | CAPS_F_SHADERS;

    let raw = RawCaps {
        flags,
        screen_w,
        screen_h,
        _pad: 0,
    };

    out.copy_from_slice(unsafe {
        core::slice::from_raw_parts(
            &raw as *const RawCaps as *const u8,
            core::mem::size_of::<RawCaps>(),
        )
    });
    0
}

/// SYS_GPU_RESOURCE_CREATE â€” create a GPU resource.
fn sys_gpu_resource_create(req_ptr: u64) -> u64 {
    #[repr(C)]
    struct Req {
        w: u32,
        h: u32,
        fmt: u8,
        kind: u8,
        _pad: [u8; 2],
    }

    if !is_compositor_caller() {
        serial::write_line(b"[syscall] gpu_resource_create: EPERM");
        return SYSCALL_ERROR;
    }
    let Some(raw) = validate_user_slice(req_ptr, core::mem::size_of::<Req>() as u64) else {
        serial::write_line(b"[syscall] gpu_resource_create: bad request ptr");
        return SYSCALL_ERROR;
    };
    let req = unsafe { &*(raw.as_ptr() as *const Req) };
    let width = req.w;
    let height = req.h;
    let format = req.fmt;
    let kind = req.kind;
    let is_buffer = kind & 0x80 != 0;
    if is_buffer {
        let buffer_kind = kind & 0x7F;
        let size = width as usize;
        if size == 0 || size > 16 * 1024 * 1024 {
            serial::write_line(b"[syscall] gpu_resource_create: bad buffer size");
            return SYSCALL_ERROR;
        }
        match crate::wm::gpu_exec::alloc_buffer_resource(buffer_kind, size) {
            Some(id) => id as u64,
            None => {
                serial::write_line(b"[syscall] gpu_resource_create: buffer alloc failed");
                SYSCALL_ERROR
            }
        }
    } else {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            serial::write_line(b"[syscall] gpu_resource_create: bad dimensions");
            return SYSCALL_ERROR;
        }
        if format > 3 {
            serial::write_line(b"[syscall] gpu_resource_create: unknown format");
            return SYSCALL_ERROR;
        }
        match crate::wm::gpu_exec::alloc_image_resource(width, height, format, kind) {
            Some(id) => id as u64,
            None => {
                serial::write_line(b"[syscall] gpu_resource_create: driver failed");
                SYSCALL_ERROR
            }
        }
    }
}

/// SYS_GPU_RESOURCE_DESTROY â€” free a GPU texture resource.
fn sys_gpu_resource_destroy(resource_id: u32) -> u64 {
    if !is_compositor_caller() {
        serial::write_line(b"[syscall] gpu_resource_destroy: EPERM");
        return SYSCALL_ERROR;
    }
    crate::wm::gpu_exec::free_resource(resource_id);
    0
}

/// Maximum size of a GPU command buffer submission (512 KiB).
const GPU_SUBMIT_MAX_BYTES: u64 = 512 * 1024;

/// SYS_GPU_SUBMIT_3D â€” deprecated legacy entrypoint.
///
/// GraphOS-native command submission must use SYS_GPU_SUBMIT.
fn sys_gpu_submit_3d(cmd_ptr: u64, cmd_len: u64) -> u64 {
    let _ = (cmd_ptr, cmd_len);
    serial::write_line(b"[syscall] gpu_submit_3d: deprecated; use SYS_GPU_SUBMIT");
    SYSCALL_ERROR
}

/// SYS_GPU_SURFACE_IMPORT â€” import a surface backing page into a GPU resource.
///
/// Binds the surface's backing physical page to an existing GPU resource so
/// the compositor can read it as a texture without a CPU copy.
fn sys_gpu_surface_import(surface_id: u32, resource_id: u32) -> u64 {
    if !is_compositor_caller() {
        serial::write_line(b"[syscall] gpu_surface_import: EPERM");
        return SYSCALL_ERROR;
    }
    // Look up the surface backing page.
    let Some(phys_addr) = crate::wm::surface_table::surface_phys_addr(surface_id) else {
        serial::write_line(b"[syscall] gpu_surface_import: surface not found");
        return SYSCALL_ERROR;
    };
    match crate::drivers::gpu::virtio_gpu::resource_attach_backing(resource_id, phys_addr) {
        true => 0,
        false => {
            serial::write_line(b"[syscall] gpu_surface_import: driver failed");
            SYSCALL_ERROR
        }
    }
}

// â”€â”€ GraphOS-native GPU command submission (0x505â€“0x508) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// GPU fence table: up to 256 live fences.
///
/// Each entry: bit0=allocated, bit1=signalled.
static GPU_FENCES: spin::Mutex<[u8; 256]> = spin::Mutex::new([0u8; 256]);

/// SYS_GPU_FENCE_ALLOC â€” allocate a GPU timeline fence.
fn sys_gpu_fence_alloc() -> u64 {
    if !is_compositor_caller() {
        serial::write_line(b"[syscall] gpu_fence_alloc: EPERM");
        return SYSCALL_ERROR;
    }
    let mut fences = GPU_FENCES.lock();
    for (i, slot) in fences.iter_mut().enumerate().skip(1) {
        if *slot == 0 {
            *slot = 1; // allocated, not signalled
            return i as u64;
        }
    }
    serial::write_line(b"[syscall] gpu_fence_alloc: table full");
    SYSCALL_ERROR
}

/// SYS_GPU_FENCE_WAIT â€” block until a fence is signalled.
fn sys_gpu_fence_wait(fence_id: u64, _timeout_ticks: u64) -> u64 {
    if !is_compositor_caller() {
        serial::write_line(b"[syscall] gpu_fence_wait: EPERM");
        return SYSCALL_ERROR;
    }
    if fence_id == 0 || fence_id >= 256 {
        return SYSCALL_ERROR;
    }
    // Spin until signalled (Phase 1: single-CPU, no preemption on this path).
    let mut spins = 0u32;
    loop {
        let state = {
            let fences = GPU_FENCES.lock();
            fences[fence_id as usize]
        };
        if state == 0 {
            return SYSCALL_ERROR; // not allocated
        }
        if state & 2 != 0 {
            return 0; // signalled
        }
        spins += 1;
        if spins > 100_000 {
            return 1; // timeout
        }
        core::hint::spin_loop();
    }
}

/// SYS_GPU_FENCE_POLL â€” non-blocking poll of a fence.
fn sys_gpu_fence_poll(fence_id: u64) -> u64 {
    if !is_compositor_caller() {
        return SYSCALL_ERROR;
    }
    if fence_id == 0 || fence_id >= 256 {
        return SYSCALL_ERROR;
    }
    let fences = GPU_FENCES.lock();
    if fences[fence_id as usize] & 2 != 0 {
        1
    } else {
        0
    }
}

/// Signal a GPU fence from within the kernel (called by the GPU executor after
/// completing a submitted command buffer).
#[allow(dead_code)]
pub fn gpu_fence_signal(fence_id: u32) {
    if fence_id == 0 || fence_id >= 256 {
        return;
    }
    let mut fences = GPU_FENCES.lock();
    fences[fence_id as usize] |= 2;
}

/// SYS_GPU_SUBMIT â€” submit a GraphOS-native GPU command buffer.
///
/// Validates the user pointer, decodes the wire format, and dispatches each
/// command to the Phase 1 virtio-gpu 2D executor.
fn sys_gpu_submit(cmd_ptr: u64, cmd_len: u64) -> u64 {
    if !is_compositor_caller() {
        serial::write_line(b"[syscall] gpu_submit: EPERM");
        return SYSCALL_ERROR;
    }
    if cmd_len == 0 || cmd_len > GPU_SUBMIT_MAX_BYTES {
        serial::write_bytes(b"[syscall] gpu_submit: bad cmd_len=");
        serial::write_hex(cmd_len);
        serial::write_line(b"");
        return SYSCALL_ERROR;
    }
    let Some(buf) = validate_user_slice(cmd_ptr, cmd_len) else {
        serial::write_line(b"[syscall] gpu_submit: invalid user ptr");
        return SYSCALL_ERROR;
    };
    crate::wm::gpu_exec::execute(buf);
    0
}
