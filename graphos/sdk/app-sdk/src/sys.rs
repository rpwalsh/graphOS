// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Raw syscall wrappers for ring-3 GraphOS applications.
//!
//! All functions use `int 0x80` (trap gate) which is safe from ring-3 because
//! the kernel always swaps GS via the IST path for the trap gate. The faster
//! `syscall` instruction is not used here because `int 0x80` works from both
//! kernel-mode helper tasks and true ring-3 tasks.

#![allow(missing_docs, dead_code)]

use core::arch::asm;

const SYS_EXIT: u64 = 0x001;
const SYS_YIELD: u64 = 0x002;
const SYS_WRITE: u64 = 0x100;
const SYS_THREAD_SPAWN: u64 = 0x010;
const SYS_THREAD_JOIN: u64 = 0x011;
const SYS_THREAD_EXIT: u64 = 0x012;
const SYS_CHANNEL_CREATE: u64 = 0x101;
const SYS_MMAP: u64 = 0x200;
const SYS_MUNMAP: u64 = 0x201;
const SYS_SLEEP: u64 = 0x202;
const SYS_REGISTRY_LOOKUP: u64 = 0x308;
const SYS_SURFACE_CREATE: u64 = 0x400;
const SYS_SURFACE_PRESENT: u64 = 0x401;
const SYS_SURFACE_DESTROY: u64 = 0x402;
const SYS_SURFACE_QUERY_PENDING: u64 = 0x403;
const SYS_SURFACE_COMMIT: u64 = 0x405;
const SYS_INPUT_SET_FOCUS: u64 = 0x410;
const SYS_INPUT_REGISTER_WINDOW: u64 = 0x411;
const SYS_INPUT_UNREGISTER_WINDOW: u64 = 0x412;
const SYS_FRAME_TICK_SUBSCRIBE: u64 = 0x413;
const SYS_CLIPBOARD_WRITE: u64 = 0x420;
const SYS_CLIPBOARD_READ: u64 = 0x421;
const SYS_SURFACE_MOVE: u64 = 0x430;
const SYS_SURFACE_RESIZE: u64 = 0x431;

/// Syscall error sentinel.
pub const SYSCALL_ERROR: u64 = u64::MAX;

/// User-space base address added to all pointer arguments.
/// User PML4 slot base — all ring-3 mappings live here.
/// Matches `USER_SLOT_BASE = USER_PML4_SLOT << 39 = 1 << 39` in the kernel.
const USER_SLOT_BASE: u64 = 1u64 << 39; // == 0x0000_0080_0000_0000

const CHANNEL_RECV_NONBLOCK: u64 = 1;

/// Decoded metadata returned by `SYS_CHANNEL_RECV`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecvMeta {
    pub payload_len: usize,
    pub tag: u8,
    pub reply_endpoint: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RegistryLookupOut {
    service_uuid: [u8; 16],
    channel_uuid: [u8; 16],
    task_uuid: [u8; 16],
    channel_alias: u32,
    health: u8,
    _pad: [u8; 3],
}

/// Registry lookup result for a named service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistryLookup {
    pub service_uuid: [u8; 16],
    pub channel_uuid: [u8; 16],
    pub task_uuid: [u8; 16],
    pub channel_alias: u32,
    pub health: u8,
}

pub const PROT_READ: u64 = 1 << 0;
pub const PROT_WRITE: u64 = 1 << 1;
pub const MAP_PRIVATE: u64 = 1 << 0;
pub const MAP_ANON: u64 = 1 << 2;

#[inline(always)]
fn rebase_ptr(ptr: *const u8) -> u64 {
    let raw = ptr as u64;
    if raw >= USER_SLOT_BASE || raw == 0 {
        raw
    } else {
        raw.wrapping_add(USER_SLOT_BASE)
    }
}

#[inline(always)]
pub fn trap6(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") nr => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8")  a4,
            in("r9")  a5,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }
    ret
}

// ---------------------------------------------------------------------------
// Basic I/O
// ---------------------------------------------------------------------------

pub fn write(fd: u64, buf: &[u8]) {
    let _ = trap6(
        SYS_WRITE,
        fd,
        rebase_ptr(buf.as_ptr()),
        buf.len() as u64,
        0,
        0,
        0,
    );
}

pub fn exit(code: u64) -> ! {
    let _ = trap6(SYS_EXIT, code, 0, 0, 0, 0, 0);
    loop {
        unsafe { asm!("ud2", options(nomem, nostack)) };
    }
}

pub fn yield_now() {
    let _ = trap6(SYS_YIELD, 0, 0, 0, 0, 0, 0);
}

pub fn sleep_ticks(n: u64) {
    let _ = trap6(SYS_SLEEP, n, 0, 0, 0, 0, 0);
}

// ---------------------------------------------------------------------------
// Threading
// ---------------------------------------------------------------------------

/// Thread stack size used by `thread_spawn` (64 KiB).
pub const THREAD_STACK_SIZE: u64 = 64 * 1024;

/// Spawn a new thread in the calling task's address space.
///
/// * `entry`  — ring-3 function pointer; called as `extern "C" fn(u64)`.
/// * `arg`    — value passed in the first argument register (rdi) of `entry`.
///
/// Allocates a 64 KiB stack via `SYS_MMAP`, spawns the thread, and returns
/// a `ThreadHandle` that must be passed to `thread_join` to reclaim the stack.
///
/// Returns `None` if the syscall or stack allocation fails.
/// Stub placed at the bottom of every thread stack (the return-address slot).
/// When the thread entry function returns it lands here, which calls
/// `SYS_THREAD_EXIT` so the kernel can clean up the task cleanly.
unsafe extern "C" fn thread_exit_stub(_: u64) -> ! {
    thread_exit(0);
}

pub fn thread_spawn(entry: unsafe extern "C" fn(u64), arg: u64) -> Option<ThreadHandle> {
    // Allocate anonymous private stack from the kernel's mmap.
    let stack = trap6(
        SYS_MMAP,
        0, // NULL → kernel picks address
        THREAD_STACK_SIZE,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANON,
        0,
        0,
    );
    if stack == SYSCALL_ERROR || stack == 0 {
        return None;
    }
    let user_stack_top = stack + THREAD_STACK_SIZE;
    // Write the thread-exit stub address into the return-address slot
    // (user_stack_top - 8).  The kernel adjusts RSP to user_stack_top - 8
    // so this slot is the first thing `ret` pops when the thread returns.
    // SAFETY: the page at user_stack_top - 8 is inside the mmap'd region
    // (which covers [stack, stack + THREAD_STACK_SIZE)) and is writable.
    unsafe {
        let ret_slot = (user_stack_top - 8) as *mut u64;
        ret_slot.write_volatile(thread_exit_stub as u64);
    }
    let entry_raw = entry as u64;
    let tid = trap6(SYS_THREAD_SPAWN, entry_raw, arg, user_stack_top, 0, 0, 0);
    if tid == SYSCALL_ERROR {
        let _ = trap6(SYS_MUNMAP, stack, THREAD_STACK_SIZE, 0, 0, 0, 0);
        return None;
    }
    Some(ThreadHandle {
        tid,
        stack_base: stack,
    })
}

/// Join a spawned thread and free its stack.
///
/// Blocks the caller until the thread has exited, then unmaps the thread's stack.
pub fn thread_join(handle: ThreadHandle) {
    let _ = trap6(SYS_THREAD_JOIN, handle.tid, 0, 0, 0, 0, 0);
    let _ = trap6(SYS_MUNMAP, handle.stack_base, THREAD_STACK_SIZE, 0, 0, 0, 0);
}

/// Exit the calling thread.
pub fn thread_exit(code: u64) -> ! {
    let _ = trap6(SYS_THREAD_EXIT, code, 0, 0, 0, 0, 0);
    loop {
        unsafe { asm!("ud2", options(nomem, nostack)) };
    }
}

/// Handle returned by `thread_spawn`.  Must be passed to `thread_join`.
#[derive(Clone, Copy)]
pub struct ThreadHandle {
    /// Kernel TaskId of the spawned thread.
    pub tid: u64,
    /// Base address of the thread's stack (for munmap on join).
    pub stack_base: u64,
}

// ---------------------------------------------------------------------------
// IPC
// ---------------------------------------------------------------------------

pub fn channel_recv_nonblock(channel: u32, buf: &mut [u8]) -> u64 {
    trap6(
        SYS_CHANNEL_RECV,
        channel as u64,
        rebase_ptr(buf.as_mut_ptr()),
        buf.len() as u64,
        CHANNEL_RECV_NONBLOCK,
        0,
        0,
    )
}

/// Non-blocking receive that decodes the packed ABI metadata.
pub fn channel_recv_nonblock_meta(channel: u32, buf: &mut [u8]) -> Option<RecvMeta> {
    let raw = channel_recv_nonblock(channel, buf);
    if raw == 0 || raw == SYSCALL_ERROR {
        return None;
    }
    Some(RecvMeta {
        payload_len: (raw & 0xFFFF) as usize,
        tag: ((raw >> 16) & 0xFF) as u8,
        reply_endpoint: ((raw >> 24) & 0xFFFF_FFFF) as u32,
    })
}

pub fn channel_send(channel: u32, payload: &[u8], tag: u8) -> bool {
    trap6(
        SYS_CHANNEL_SEND,
        channel as u64,
        rebase_ptr(payload.as_ptr()),
        payload.len() as u64,
        tag as u64,
        0,
        0,
    ) != SYSCALL_ERROR
}

/// Create a new IPC channel. Returns the channel ID, or `None` on failure.
/// The kernel grants the caller CAP_SEND | CAP_RECV | CAP_MANAGE on the new channel.
pub fn channel_create_with_size(max_msg_size: u32) -> Option<u32> {
    let ret = unsafe { trap6(SYS_CHANNEL_CREATE, max_msg_size as u64, 0, 0, 0, 0, 0) };
    if ret == SYSCALL_ERROR {
        None
    } else {
        Some(ret as u32)
    }
}

/// Create a new IPC channel with default message size (4096 bytes).
/// Returns the channel ID (u32 used directly by Window::open), or panics.
pub fn channel_create() -> u32 {
    let ret = unsafe { trap6(SYS_CHANNEL_CREATE, 4096u64, 0, 0, 0, 0, 0) };
    if ret == SYSCALL_ERROR { 0 } else { ret as u32 }
}

/// Resolve a service by name through the kernel registry.
pub fn registry_lookup(name: &[u8]) -> Option<RegistryLookup> {
    if name.is_empty() || name.len() >= 64 {
        return None;
    }

    let mut name_buf = [0u8; 64];
    name_buf[..name.len()].copy_from_slice(name);
    name_buf[name.len()] = 0;

    let mut out = RegistryLookupOut {
        service_uuid: [0; 16],
        channel_uuid: [0; 16],
        task_uuid: [0; 16],
        channel_alias: 0,
        health: 0,
        _pad: [0; 3],
    };

    let raw = trap6(
        SYS_REGISTRY_LOOKUP,
        rebase_ptr(name_buf.as_ptr()),
        rebase_ptr((&mut out as *mut RegistryLookupOut).cast::<u8>()),
        core::mem::size_of::<RegistryLookupOut>() as u64,
        0,
        0,
        0,
    );

    if raw == SYSCALL_ERROR {
        return None;
    }

    Some(RegistryLookup {
        service_uuid: out.service_uuid,
        channel_uuid: out.channel_uuid,
        task_uuid: out.task_uuid,
        channel_alias: out.channel_alias,
        health: out.health,
    })
}

// ---------------------------------------------------------------------------
// Memory mapping
// ---------------------------------------------------------------------------

pub fn mmap_anon(len: u64, prot: u64) -> Option<u64> {
    let ret = trap6(SYS_MMAP, 0, len, prot, MAP_PRIVATE | MAP_ANON, 0, 0);
    if ret == SYSCALL_ERROR || ret == 0 {
        None
    } else {
        Some(ret)
    }
}

pub fn munmap(addr: u64, len: u64) -> bool {
    trap6(SYS_MUNMAP, addr, len, 0, 0, 0, 0) != SYSCALL_ERROR
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Create a shared pixel surface.
///
/// Returns `(surface_id, mapped_vaddr)` where `mapped_vaddr` is the
/// user virtual address of the pixel buffer.
pub fn surface_create(width: u16, height: u16) -> Option<(u32, u64)> {
    let ret = trap6(SYS_SURFACE_CREATE, width as u64, height as u64, 0, 0, 0, 0);
    if ret == SYSCALL_ERROR {
        return None;
    }
    let surface_id = (ret & 0xFFFF_FFFF) as u32;
    let vaddr_low = (ret >> 32) & 0xFFFF_FFFF;
    // The kernel packs only the low 32 bits of the vaddr.
    // All ring-3 mmaps reside in PML4 slot 1 (USER_SLOT_BASE = 1<<39),
    // so restore the high bits by ORing in the slot base masked to 32 bits.
    // This is valid as long as mapped addresses stay below 0x80_FFFF_FFFF,
    // which holds given the small per-task surface budget.
    let vaddr = (USER_SLOT_BASE & 0xFFFF_FFFF_0000_0000) | vaddr_low;
    Some((surface_id, vaddr))
}

/// Tell the compositor this surface is ready to display.
pub fn surface_present(surface_id: u32) -> bool {
    surface_commit(surface_id)
}

/// Tell the compositor this surface is ready to display through the Phase J
/// GPU path. Returns true on success.
pub fn surface_commit(surface_id: u32) -> bool {
    trap6(SYS_SURFACE_COMMIT, surface_id as u64, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Destroy a surface. Call after unmapping.
pub fn surface_destroy(surface_id: u32) -> bool {
    trap6(SYS_SURFACE_DESTROY, surface_id as u64, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Request keyboard focus on `channel`. Pass `0` to release.
pub fn input_set_focus(channel: u32) {
    let _ = trap6(SYS_INPUT_SET_FOCUS, channel as u64, 0, 0, 0, 0, 0);
}

/// Register a window rectangle for pointer hit-testing.
pub fn input_register_window(x: i16, y: i16, w: u16, h: u16, channel: u32) -> bool {
    let xy = (x as u16 as u64) | ((y as u16 as u64) << 16);
    let wh = (w as u64) | ((h as u64) << 16);
    trap6(SYS_INPUT_REGISTER_WINDOW, xy, wh, channel as u64, 0, 0, 0) != SYSCALL_ERROR
}

/// Remove window registration for this task.
pub fn input_unregister_window() {
    let _ = trap6(SYS_INPUT_UNREGISTER_WINDOW, 0, 0, 0, 0, 0, 0);
}

/// Subscribe `channel` to receive FrameTick IPC messages from the orchestrator.
/// Returns true on success.
pub fn subscribe_frame_tick(channel: u32) -> bool {
    trap6(SYS_FRAME_TICK_SUBSCRIBE, channel as u64, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

/// Write plain-text data to the system clipboard.
///
/// Returns `true` on success.  The kernel copies up to 4096 bytes.
pub fn clipboard_write(data: &[u8]) -> bool {
    let len = data.len().min(4096) as u64;
    trap6(
        SYS_CLIPBOARD_WRITE,
        rebase_ptr(data.as_ptr()),
        len,
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

/// Read plain-text data from the system clipboard into `buf`.
///
/// Returns the number of bytes actually written to `buf`, or 0 on failure.
pub fn clipboard_read(buf: &mut [u8]) -> usize {
    let len = buf.len().min(4096) as u64;
    let ret = trap6(
        SYS_CLIPBOARD_READ,
        rebase_ptr(buf.as_ptr()),
        len,
        0,
        0,
        0,
        0,
    );
    if ret == SYSCALL_ERROR {
        0
    } else {
        ret as usize
    }
}

// ---------------------------------------------------------------------------
// Surface geometry (drag-resize)
// ---------------------------------------------------------------------------

/// Move the compositor window associated with `surface_id` to `(x, y)`.
pub fn surface_move(surface_id: u32, x: i32, y: i32) -> bool {
    trap6(
        SYS_SURFACE_MOVE,
        surface_id as u64,
        x as u64,
        y as u64,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

/// Resize the compositor window associated with `surface_id`.
/// The backing pixel buffer is NOT reallocated; `w` and `h` must fit within
/// the dimensions given at `surface_create` time.
pub fn surface_resize(surface_id: u32, w: u16, h: u16) -> bool {
    trap6(
        SYS_SURFACE_RESIZE,
        surface_id as u64,
        w as u64,
        h as u64,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

// ---------------------------------------------------------------------------
// Extended syscall constants (Sessions 19-26)
// ---------------------------------------------------------------------------

pub const SYS_VERIFY_BUNDLE: u64 = 0x500;
pub const SYS_WASM_SPAWN: u64 = 0x501;
pub const SYS_TASK_KILL: u64 = 0x502;
pub const SYS_CHANNEL_RECV: u64 = SYS_CHANNEL_RECV_inner;
pub const SYS_CHANNEL_SEND: u64 = 0x102;
pub const SYS_LOG: u64 = 0x100;
pub const SYS_FIDO2_GET_ASSERTION: u64 = 0x600;
pub const SYS_NET_HTTP_GET: u64 = 0x700;
pub const SYS_NET_HTTP_POST: u64 = 0x701;
pub const SYS_TPM_QUOTE: u64 = 0x800;

const SYS_CHANNEL_RECV_inner: u64 = 0x103;

/// Low-level 4-arg syscall.
///
/// # Safety
/// Caller must ensure syscall number and arguments are valid.
pub unsafe fn raw_syscall(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    trap6(nr, a0, a1, a2, a3, 0, 0)
}

/// Write a log message to the kernel serial console.
pub fn write_log(msg: &[u8]) {
    write(1, msg);
}

/// Fetch a URL through the kernel network path.
///
/// Returns number of bytes written to `out`, or 0 on failure.
pub fn net_http_get(url: &[u8], out: &mut [u8], flags: u64) -> usize {
    if url.is_empty() || out.is_empty() {
        return 0;
    }
    let ret = trap6(
        SYS_NET_HTTP_GET,
        rebase_ptr(url.as_ptr()),
        rebase_ptr(out.as_mut_ptr()),
        out.len() as u64,
        flags,
        0,
        0,
    );
    if ret == SYSCALL_ERROR {
        0
    } else {
        ret as usize
    }
}

// ---------------------------------------------------------------------------
// VFS syscall numbers (0x300–0x30F)
// ---------------------------------------------------------------------------
const SYS_VFS_OPEN: u64 = 0x300;
const SYS_VFS_CLOSE: u64 = 0x301;
const SYS_VFS_READ: u64 = 0x302;
const SYS_VFS_WRITE: u64 = 0x303;
const SYS_VFS_READDIR: u64 = 0x304;
const SYS_VFS_UNLINK: u64 = 0x305;
const SYS_VFS_RENAME: u64 = 0x306;
const SYS_VFS_MKDIR: u64 = 0x307;

// ---------------------------------------------------------------------------
// VFS wrappers
// ---------------------------------------------------------------------------

/// Open a file; returns fd (u64) or SYSCALL_ERROR.
pub fn vfs_open(path: &[u8], flags: u64) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_OPEN,
            rebase_ptr(path.as_ptr()),
            path.len() as u64,
            flags,
            0,
            0,
            0,
        )
    }
}

/// Close a file descriptor.
pub fn vfs_close(fd: u64) {
    unsafe {
        let _ = trap6(SYS_VFS_CLOSE, fd, 0, 0, 0, 0, 0);
    }
}

/// Read up to `len` bytes from `fd` into `buf`; returns bytes read.
pub fn vfs_read(fd: u64, buf: &mut [u8], len: usize) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_READ,
            fd,
            rebase_ptr(buf.as_ptr()),
            len as u64,
            0,
            0,
            0,
        )
    }
}

/// Write `buf` to `fd`; returns bytes written.
pub fn vfs_write(fd: u64, buf: &[u8], len: usize) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_WRITE,
            fd,
            rebase_ptr(buf.as_ptr()),
            len as u64,
            0,
            0,
            0,
        )
    }
}

/// Read directory entries from `fd` into `buf`; returns bytes written.
pub fn vfs_readdir(fd: u64, buf: &mut [u8], len: usize) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_READDIR,
            fd,
            rebase_ptr(buf.as_ptr()),
            len as u64,
            0,
            0,
            0,
        )
    }
}

/// Delete a file at `path`; returns 0 on success or SYSCALL_ERROR.
pub fn vfs_unlink(path: &[u8]) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_UNLINK,
            rebase_ptr(path.as_ptr()),
            path.len() as u64,
            0,
            0,
            0,
            0,
        )
    }
}

/// Rename a file from `from` to `to`; returns 0 on success or SYSCALL_ERROR.
pub fn vfs_rename(from: &[u8], to: &[u8]) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_RENAME,
            rebase_ptr(from.as_ptr()),
            from.len() as u64,
            rebase_ptr(to.as_ptr()),
            to.len() as u64,
            0,
            0,
        )
    }
}

/// Create a directory at `path`; returns 0 on success or SYSCALL_ERROR.
pub fn vfs_mkdir(path: &[u8]) -> u64 {
    unsafe {
        trap6(
            SYS_VFS_MKDIR,
            rebase_ptr(path.as_ptr()),
            path.len() as u64,
            0,
            0,
            0,
            0,
        )
    }
}

/// Spawn a new application by path.
pub fn spawn(path: &[u8]) {
    unsafe {
        let _ = trap6(
            SYS_WASM_SPAWN,
            rebase_ptr(path.as_ptr()),
            path.len() as u64,
            0,
            0,
            0,
            0,
        );
    }
}

/// Alias for yield_now, used by apps as yield_task.
#[inline(always)]
pub fn yield_task() {
    yield_now();
}
