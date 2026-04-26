#![allow(dead_code)]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_EXIT: u64 = 0x001;
const SYS_YIELD: u64 = 0x002;
const SYS_SPAWN: u64 = 0x003;
const SYS_WRITE: u64 = 0x100;
const SYS_CHANNEL_SEND: u64 = 0x102;
const SYS_CHANNEL_CREATE: u64 = 0x101;
const SYS_CHANNEL_RECV: u64 = 0x103;
const SYS_VFS_OPEN: u64 = 0x104;
const SYS_VFS_READ: u64 = 0x105;
const SYS_VFS_CLOSE: u64 = 0x106;
const SYS_VFS_WRITE: u64 = 0x107;
const SYS_VFS_CREATE: u64 = 0x108;
const SYS_VFS_RENAME: u64 = 0x109;
const SYS_VFS_MKDIR: u64 = 0x10B;
const SYS_VFS_UNLINK: u64 = 0x10C;
const SYS_SOCKET: u64 = 0x120;
const SYS_BIND: u64 = 0x121;
const SYS_CONNECT: u64 = 0x122;
const SYS_SEND: u64 = 0x123;
const SYS_RECV: u64 = 0x124;
const SYS_CLOSE_SOCK: u64 = 0x125;
const SYS_LISTEN: u64 = 0x126;
const SYS_ACCEPT: u64 = 0x127;
const SYS_GETUID: u64 = 0x130;
const SYS_GETGID: u64 = 0x131;
const SYS_SETUID: u64 = 0x132;
const SYS_LOGIN: u64 = 0x133;
const SYS_LOGOUT: u64 = 0x134;
const SYS_PTY_ALLOC: u64 = 0x135;
const SYS_SESSION_ATTACH: u64 = 0x136;
const SYS_PTY_WRITE: u64 = 0x137;
const SYS_PTY_READ: u64 = 0x138;
const SYS_GETRANDOM: u64 = 0x163;
const SYS_DRIVER_PROBE: u64 = 0x140;
const SYS_DRIVER_INSTALL: u64 = 0x141;
const SYS_CRYPTO_ED25519_VERIFY: u64 = 0x145; // kernel crypto primitive: Ed25519 verify
const SYS_HEARTBEAT: u64 = 0x150;
const SYS_MMAP: u64 = 0x200;
const SYS_MUNMAP: u64 = 0x201;
const SYS_GRAPH_SERVICE_LOOKUP: u64 = 0x306;
const SYS_REGISTRY_LOOKUP: u64 = 0x308;
const SYS_REGISTRY_REGISTER: u64 = 0x309;
const SYS_REGISTRY_SUBSCRIBE: u64 = 0x30A;
const SYS_IPC_CAP_GRANT: u64 = 0x30B;
const SYS_IPC_CAP_REVOKE: u64 = 0x30C;
const SYS_INPUT_SET_FOCUS: u64 = 0x410;
const SYS_INPUT_REGISTER_WINDOW: u64 = 0x411;
pub const SYS_GRAPH_EM_STEP: u64 = 0x30D;
pub const SYS_GRAPH_EM_STATS: u64 = 0x30E;
const SYSCALL_ERROR: u64 = u64::MAX;
const USER_SLOT_BASE: u64 = 0x0000_0080_0000_0000;
const CHANNEL_RECV_NONBLOCK: u64 = 1;

pub const PROT_READ: u64 = 1 << 0;
pub const PROT_WRITE: u64 = 1 << 1;
pub const PROT_EXEC: u64 = 1 << 2;
pub const MAP_PRIVATE: u64 = 1 << 0;
pub const MAP_SHARED: u64 = 1 << 1;
pub const MAP_ANON: u64 = 1 << 2;

pub const TAG_SERVICE_STATUS: u8 = 0x31;

#[derive(Clone, Copy, Debug)]
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

#[derive(Clone, Copy)]
pub struct RegistryLookup {
    pub service_uuid: [u8; 16],
    pub channel_uuid: [u8; 16],
    pub task_uuid: [u8; 16],
    pub channel_alias: u32,
    pub health: u8,
}

#[derive(Clone, Copy)]
pub struct SocketHandle {
    pub uuid: [u8; 16],
}

#[derive(Clone, Copy)]
pub struct DriverPackageHandle {
    pub uuid: [u8; 16],
}

#[inline(always)]
fn syscall6(nr: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") arg0,
            in("rsi") arg1,
            in("rdx") arg2,
            in("r10") arg3,
            in("r8") arg4,
            in("r9") arg5,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }
    ret
}

/// Generic syscall wrapper for custom/extended syscall numbers not yet wrapped.
#[inline(always)]
pub fn raw_syscall(nr: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    syscall6(nr, arg0, arg1, arg2, arg3, 0, 0)
}

#[inline(always)]
fn trap6(nr: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "int 0x80",
            inlateout("rax") nr => ret,
            in("rdi") arg0,
            in("rsi") arg1,
            in("rdx") arg2,
            in("r10") arg3,
            in("r8") arg4,
            in("r9") arg5,
            lateout("rcx") _,
            lateout("r11") _,
        );
    }
    ret
}

#[inline(always)]
fn rebase_ptr(ptr: *const u8) -> u64 {
    let raw = ptr as u64;
    if raw >= USER_SLOT_BASE || raw == 0 {
        raw
    } else {
        raw.wrapping_add(USER_SLOT_BASE)
    }
}

pub fn write(fd: u64, bytes: &[u8]) -> u64 {
    trap6(
        SYS_WRITE,
        fd,
        rebase_ptr(bytes.as_ptr()),
        bytes.len() as u64,
        0,
        0,
        0,
    )
}

pub fn write_line(bytes: &[u8]) {
    let _ = write(1, bytes);
}

pub fn yield_now() {
    // Cooperative reschedule is routed through the trap gate so the kernel can
    // switch directly into another task without inheriting fast-syscall GS state.
    let _ = trap6(SYS_YIELD, 0, 0, 0, 0, 0, 0);
    // Auto-heartbeat: signal the kernel watchdog every 256 yields so services
    // do not need to call `heartbeat()` explicitly in their idle loops.
    static YIELD_COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    let prev = YIELD_COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if prev & 0xFF == 0 {
        let _ = trap6(SYS_HEARTBEAT, 0, 0, 0, 0, 0, 0);
    }
}

/// Explicitly notify the kernel watchdog that this service is alive.
/// Most services do not need to call this directly because `yield_now()`
/// auto-heartbeats every 256 yields.
pub fn heartbeat() {
    let _ = trap6(SYS_HEARTBEAT, 0, 0, 0, 0, 0, 0);
}

pub fn yield_cycles(count: usize) {
    for _ in 0..count {
        yield_now();
    }
}

pub fn random_fill(out: &mut [u8]) -> bool {
    if out.is_empty() {
        return true;
    }
    let rc = trap6(
        SYS_GETRANDOM,
        rebase_ptr(out.as_mut_ptr()),
        out.len() as u64,
        0,
        0,
        0,
        0,
    );
    rc == out.len() as u64
}

pub fn spawn_named(name: &[u8]) -> u64 {
    let mut name_buf = [0u8; 64];
    if write_cstr_name(name, &mut name_buf).is_none() {
        return SYSCALL_ERROR;
    }
    trap6(SYS_SPAWN, rebase_ptr(name_buf.as_ptr()), 0, 0, 0, 0, 0)
}

pub fn spawn_named_checked(name: &[u8]) -> bool {
    spawn_named(name) != SYSCALL_ERROR
}

pub fn vfs_open(path: &[u8]) -> u64 {
    trap6(SYS_VFS_OPEN, rebase_ptr(path.as_ptr()), 0, 0, 0, 0, 0)
}

pub fn vfs_read(fd: u64, buf: &mut [u8]) -> u64 {
    trap6(
        SYS_VFS_READ,
        fd,
        rebase_ptr(buf.as_mut_ptr()),
        buf.len() as u64,
        0,
        0,
        0,
    )
}

pub fn vfs_close(fd: u64) -> bool {
    trap6(SYS_VFS_CLOSE, fd, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Create or truncate a file at `path` and return a writable file descriptor.
/// Returns `u64::MAX` on error.
pub fn vfs_create(path: &[u8]) -> u64 {
    trap6(SYS_VFS_CREATE, rebase_ptr(path.as_ptr()), 0, 0, 0, 0, 0)
}

/// Write `data` to an open file descriptor.  Returns bytes written.
pub fn vfs_write(fd: u64, data: &[u8]) -> u64 {
    trap6(
        SYS_VFS_WRITE,
        fd,
        rebase_ptr(data.as_ptr()),
        data.len() as u64,
        0,
        0,
        0,
    )
}

/// Rename `src_path` to `dst_path` (atomic on the VFS layer).
/// Returns `true` on success.
pub fn vfs_rename(src: &[u8], dst: &[u8]) -> bool {
    trap6(
        SYS_VFS_RENAME,
        rebase_ptr(src.as_ptr()),
        rebase_ptr(dst.as_ptr()),
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn vfs_mkdir(path: &[u8]) -> bool {
    trap6(SYS_VFS_MKDIR, rebase_ptr(path.as_ptr()), 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

pub fn vfs_unlink(path: &[u8]) -> bool {
    trap6(SYS_VFS_UNLINK, rebase_ptr(path.as_ptr()), 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Receive up to `out.len()` bytes from a connected socket, looping until the
/// connection closes or the buffer is full.  Returns total bytes received.
pub fn socket_recv_all(handle: &SocketHandle, out: &mut [u8]) -> usize {
    let mut total = 0usize;
    while total < out.len() {
        match socket_recv(handle, &mut out[total..]) {
            Some(0) | None => break,
            Some(n) => total += n,
        }
    }
    total
}

/// Verify an Ed25519 signature via the kernel crypto syscall (SYS_CRYPTO_ED25519_VERIFY = 0x145).
/// Returns `true` iff `sig[0..64]` is a valid Ed25519 signature over `msg` by `pub_key[0..32]`.
/// Kernel handler implementation is a v1.1 work item; the syscall returns 0 (false) until then.
pub fn ed25519_verify(pub_key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let ret = trap6(
        SYS_CRYPTO_ED25519_VERIFY,
        rebase_ptr(pub_key.as_ptr()),
        rebase_ptr(msg.as_ptr()),
        msg.len() as u64,
        rebase_ptr(sig.as_ptr()),
        0,
        0,
    );
    ret == 1
}

/// Create a new IPC channel and return its ID.
/// The kernel automatically grants CAP_SEND | CAP_RECV | CAP_MANAGE
/// to the calling task for the new channel.
pub fn channel_create(max_msg: u32) -> Option<u32> {
    let ret = trap6(SYS_CHANNEL_CREATE, max_msg as u64, 0, 0, 0, 0, 0);
    if ret == SYSCALL_ERROR {
        None
    } else {
        Some(ret as u32)
    }
}

pub fn socket_open() -> Option<SocketHandle> {
    let mut raw = [0u8; 16];
    let ret = trap6(
        SYS_SOCKET,
        rebase_ptr(raw.as_mut_ptr()),
        raw.len() as u64,
        0,
        0,
        0,
        0,
    );
    if ret == SYSCALL_ERROR {
        return None;
    }
    Some(SocketHandle { uuid: raw })
}

pub fn socket_bind(handle: &SocketHandle, local_port: u16) -> bool {
    trap6(
        SYS_BIND,
        rebase_ptr(handle.uuid.as_ptr()),
        local_port as u64,
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn socket_connect(handle: &SocketHandle, remote_ipv4: u32, remote_port: u16) -> bool {
    trap6(
        SYS_CONNECT,
        rebase_ptr(handle.uuid.as_ptr()),
        remote_ipv4 as u64,
        remote_port as u64,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn socket_send(handle: &SocketHandle, payload: &[u8]) -> Option<usize> {
    let raw = trap6(
        SYS_SEND,
        rebase_ptr(handle.uuid.as_ptr()),
        rebase_ptr(payload.as_ptr()),
        payload.len() as u64,
        0,
        0,
        0,
    );
    if raw == SYSCALL_ERROR {
        None
    } else {
        Some(raw as usize)
    }
}

pub fn socket_recv(handle: &SocketHandle, out: &mut [u8]) -> Option<usize> {
    let raw = trap6(
        SYS_RECV,
        rebase_ptr(handle.uuid.as_ptr()),
        rebase_ptr(out.as_mut_ptr()),
        out.len() as u64,
        0,
        0,
        0,
    );
    if raw == SYSCALL_ERROR {
        None
    } else {
        Some(raw as usize)
    }
}

pub fn socket_close(handle: &SocketHandle) -> bool {
    trap6(
        SYS_CLOSE_SOCK,
        rebase_ptr(handle.uuid.as_ptr()),
        0,
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

/// Put a bound socket into passive-listen mode for inbound TCP connections.
pub fn socket_listen(handle: &SocketHandle) -> bool {
    trap6(SYS_LISTEN, rebase_ptr(handle.uuid.as_ptr()), 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Accept one pending inbound connection on a listening socket.
/// Returns `Some((accepted_handle, remote_ip, remote_port))` or `None`.
pub fn socket_accept(handle: &SocketHandle) -> Option<(SocketHandle, u32, u16)> {
    let mut raw = [0u8; 16];
    let ret = trap6(
        SYS_ACCEPT,
        rebase_ptr(handle.uuid.as_ptr()),
        rebase_ptr(raw.as_mut_ptr()),
        0,
        0,
        0,
        0,
    );
    if ret == SYSCALL_ERROR {
        return None;
    }
    let remote_ip = (ret >> 16) as u32;
    let remote_port = (ret & 0xFFFF) as u16;
    Some((SocketHandle { uuid: raw }, remote_ip, remote_port))
}

pub fn getuid() -> Option<u32> {
    let raw = trap6(SYS_GETUID, 0, 0, 0, 0, 0, 0);
    if raw == SYSCALL_ERROR {
        None
    } else {
        Some(raw as u32)
    }
}

pub fn getgid() -> Option<u32> {
    let raw = trap6(SYS_GETGID, 0, 0, 0, 0, 0, 0);
    if raw == SYSCALL_ERROR {
        None
    } else {
        Some(raw as u32)
    }
}

pub fn setuid(uid: u32, gid: u32) -> bool {
    trap6(SYS_SETUID, uid as u64, gid as u64, 0, 0, 0, 0) != SYSCALL_ERROR
}

pub fn login(username: &[u8], password: &[u8]) -> bool {
    trap6(
        SYS_LOGIN,
        rebase_ptr(username.as_ptr()),
        rebase_ptr(password.as_ptr()),
        username.len() as u64,
        password.len() as u64,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn logout() -> bool {
    trap6(SYS_LOGOUT, 0, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Allocate a pseudo-terminal (PTY) for the current session.
/// Returns the tty index on success, or `None` on error.
pub fn pty_alloc() -> Option<u32> {
    let r = trap6(SYS_PTY_ALLOC, 0, 0, 0, 0, 0, 0);
    if r == SYSCALL_ERROR {
        None
    } else {
        Some(r as u32)
    }
}

/// Write bytes to the PTY's input ring (data flowing from SSH client → shell).
/// Returns number of bytes written.
pub fn pty_write(tty: u32, data: &[u8]) -> usize {
    let r = trap6(
        SYS_PTY_WRITE,
        tty as u64,
        rebase_ptr(data.as_ptr()),
        data.len() as u64,
        0,
        0,
        0,
    );
    if r == SYSCALL_ERROR { 0 } else { r as usize }
}

/// Non-blocking read from the PTY's output ring (data flowing from shell → SSH client).
/// Returns number of bytes read (0 if empty).
pub fn pty_read(tty: u32, buf: &mut [u8]) -> usize {
    let r = trap6(
        SYS_PTY_READ,
        tty as u64,
        rebase_ptr(buf.as_ptr()),
        buf.len() as u64,
        0,
        0,
        0,
    );
    if r == SYSCALL_ERROR { 0 } else { r as usize }
}

/// Attach the current task to an existing session identified by its UUID bytes.
pub fn session_attach(session_uuid: &[u8; 16]) -> bool {
    trap6(
        SYS_SESSION_ATTACH,
        rebase_ptr(session_uuid.as_ptr()),
        0,
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn driver_probe(device_uuid: &[u8; 16]) -> Option<DriverPackageHandle> {
    let mut driver_uuid = [0u8; 16];
    let raw = trap6(
        SYS_DRIVER_PROBE,
        rebase_ptr(device_uuid.as_ptr()),
        rebase_ptr(driver_uuid.as_mut_ptr()),
        0,
        0,
        0,
        0,
    );
    if raw == SYSCALL_ERROR {
        None
    } else if raw == 0 {
        Some(DriverPackageHandle { uuid: [0u8; 16] })
    } else {
        Some(DriverPackageHandle { uuid: driver_uuid })
    }
}

pub fn driver_install(
    package_uuid: &[u8; 16],
    device_uuid: &[u8; 16],
    manifest: &[u8],
    signature: &[u8; 64],
) -> bool {
    if manifest.is_empty() || manifest.len() > u32::MAX as usize {
        return false;
    }
    let manifest_ptr = rebase_ptr(manifest.as_ptr());
    let manifest_arg = manifest_ptr | ((manifest.len() as u64) << 32);
    trap6(
        SYS_DRIVER_INSTALL,
        rebase_ptr(package_uuid.as_ptr()),
        rebase_ptr(device_uuid.as_ptr()),
        manifest_arg,
        rebase_ptr(signature.as_ptr()),
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn mmap_anon(len: u64, prot: u64) -> u64 {
    trap6(SYS_MMAP, 0, len, prot, MAP_PRIVATE | MAP_ANON, 0, 0)
}

pub fn mmap_file(path: &[u8], len: u64, prot: u64, flags: u64, offset: u64) -> u64 {
    trap6(
        SYS_MMAP,
        rebase_ptr(path.as_ptr()),
        len,
        prot,
        flags,
        offset,
        0,
    )
}

pub fn munmap(addr: u64, len: u64) -> bool {
    trap6(SYS_MUNMAP, addr, len, 0, 0, 0, 0) != SYSCALL_ERROR
}

fn channel_recv_flags(channel: u32, buf: &mut [u8], flags: u64) -> u64 {
    trap6(
        SYS_CHANNEL_RECV,
        channel as u64,
        rebase_ptr(buf.as_mut_ptr()),
        buf.len() as u64,
        flags,
        0,
        0,
    )
}

pub fn channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    // Receive is part of the control plane: blocking waits are routed
    // through the trap gate so the kernel can deschedule this task.
    channel_recv_flags(channel, buf, 0)
}

pub fn channel_send(channel: u32, payload: &[u8], tag: u8) -> u64 {
    trap6(
        SYS_CHANNEL_SEND,
        channel as u64,
        rebase_ptr(payload.as_ptr()),
        payload.len() as u64,
        tag as u64,
        0,
        0,
    )
}

pub fn try_recv(channel: u32, buf: &mut [u8]) -> Option<RecvMeta> {
    let raw = channel_recv_flags(channel, buf, CHANNEL_RECV_NONBLOCK);
    if raw == 0 || raw == SYSCALL_ERROR {
        return None;
    }

    Some(RecvMeta {
        payload_len: (raw & 0xFFFF) as usize,
        tag: ((raw >> 16) & 0xFF) as u8,
        reply_endpoint: (raw >> 24) as u32,
    })
}

pub fn claim_inbox(channel: u32) {
    let mut probe = [0u8; 1];
    let _ = channel_recv_flags(channel, &mut probe, CHANNEL_RECV_NONBLOCK);
}

pub fn announce_service_ready(name: &'static [u8]) {
    if let Some(service_mgr) = service_inbox(b"servicemgr") {
        let _ = channel_send(service_mgr, name, TAG_SERVICE_STATUS);
    }
}

pub fn bootstrap_status(payload: &[u8]) -> bool {
    if let Some(ch) = service_inbox(b"bootstrap") {
        channel_send(ch, payload, TAG_SERVICE_STATUS) != SYSCALL_ERROR
    } else {
        false
    }
}

pub fn bootstrap_named_status(prefix: &[u8], name: &[u8]) -> bool {
    let mut buf = [0u8; 48];
    if prefix.len() + name.len() > buf.len() {
        return false;
    }
    buf[..prefix.len()].copy_from_slice(prefix);
    buf[prefix.len()..prefix.len() + name.len()].copy_from_slice(name);
    bootstrap_status(&buf[..prefix.len() + name.len()])
}

fn write_cstr_name(name: &[u8], out: &mut [u8; 64]) -> Option<usize> {
    if name.is_empty() {
        return None;
    }

    let mut len = 0usize;
    while len < name.len() {
        let b = name[len];
        if b == 0 {
            break;
        }
        len += 1;
    }

    if len == 0 || len >= out.len() {
        return None;
    }

    out[..len].copy_from_slice(&name[..len]);
    out[len] = 0;
    Some(len)
}

pub fn graph_service_lookup(name: &[u8]) -> Option<(u16, u64)> {
    let mut name_buf = [0u8; 64];
    write_cstr_name(name, &mut name_buf)?;

    let raw = trap6(
        SYS_GRAPH_SERVICE_LOOKUP,
        rebase_ptr(name_buf.as_ptr()),
        0,
        0,
        0,
        0,
        0,
    );
    if raw == SYSCALL_ERROR {
        return None;
    }
    Some(((raw >> 48) as u16, raw & 0x0000_FFFF_FFFF_FFFF))
}

pub fn registry_lookup(name: &[u8]) -> Option<RegistryLookup> {
    let mut name_buf = [0u8; 64];
    write_cstr_name(name, &mut name_buf)?;

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

pub fn registry_register(name: &[u8], channel_alias: u32) -> bool {
    let mut name_buf = [0u8; 64];
    let Some(_) = write_cstr_name(name, &mut name_buf) else {
        return false;
    };

    trap6(
        SYS_REGISTRY_REGISTER,
        rebase_ptr(name_buf.as_ptr()),
        channel_alias as u64,
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn registry_subscribe(last_seen_generation: u64) -> u64 {
    trap6(SYS_REGISTRY_SUBSCRIBE, last_seen_generation, 0, 0, 0, 0, 0)
}

pub fn graph_em_stats(src_kind: u16, dst_kind: u16) -> Option<(u32, u32)> {
    let packed = raw_syscall(SYS_GRAPH_EM_STATS, src_kind as u64, dst_kind as u64, 0, 0);
    if packed == SYSCALL_ERROR {
        None
    } else {
        Some(((packed & 0xFFFF_FFFF) as u32, (packed >> 32) as u32))
    }
}

pub fn ipc_cap_grant(task_id: u64, channel_alias: u32, perms: u8) -> bool {
    trap6(
        SYS_IPC_CAP_GRANT,
        task_id,
        channel_alias as u64,
        perms as u64,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn ipc_cap_revoke(task_id: u64, channel_alias: u32, perms: u8) -> bool {
    trap6(
        SYS_IPC_CAP_REVOKE,
        task_id,
        channel_alias as u64,
        perms as u64,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

pub fn service_inbox(name: &[u8]) -> Option<u32> {
    registry_lookup(name).map(|entry| entry.channel_alias)
}

pub fn service_inbox_or(name: &[u8], fallback: u32) -> u32 {
    service_inbox(name).unwrap_or(fallback)
}

pub fn service_inbox_or_die(name: &[u8]) -> u32 {
    if let Some(ch) = service_inbox(name) {
        return ch;
    }
    write_line(b"[ring3] missing service inbox registration\n");
    exit(0xEF)
}

pub fn exit(code: u64) -> ! {
    let _ = syscall6(SYS_EXIT, code, 0, 0, 0, 0, 0);
    loop {
        unsafe { asm!("ud2", options(nomem, nostack)) };
    }
}

// ---- Surface syscalls (0x400) ----

const SYS_SURFACE_CREATE: u64 = 0x400;
const SYS_SURFACE_PRESENT: u64 = 0x401;
const SYS_SURFACE_DESTROY: u64 = 0x402;
const SYS_SURFACE_QUERY_PENDING: u64 = 0x403;
const SYS_SURFACE_FLUSH: u64 = 0x404;
const SYS_SURFACE_COMMIT: u64 = 0x405;
const SYS_FRAME_TICK_SUBSCRIBE: u64 = 0x413;
const SYS_COMPOSITOR_CLAIM_DISPLAY: u64 = 0x509;

/// Create a shared pixel surface.
///
/// Returns `(surface_id, mapped_vaddr)` on success, `None` on error.
/// The pixel buffer is immediately accessible at `mapped_vaddr`.
pub fn surface_create(width: u16, height: u16) -> Option<(u32, u64)> {
    let ret = trap6(SYS_SURFACE_CREATE, width as u64, height as u64, 0, 0, 0, 0);
    if ret == SYSCALL_ERROR {
        return None;
    }
    let surface_id = (ret & 0xFFFF_FFFF) as u32;
    let vaddr_low = (ret >> 32) & 0xFFFF_FFFF;
    // The kernel packs only the low 32 bits of vaddr in bits [63:32].
    // All ring-3 mappings live in USER_SLOT_BASE (PML4 slot 1), so restore
    // the upper bits here before handing the pointer to callers.
    let vaddr = (USER_SLOT_BASE & 0xFFFF_FFFF_0000_0000) | vaddr_low;
    Some((surface_id, vaddr))
}

/// Present a surface to the compositor.
pub fn surface_present(surface_id: u32) -> bool {
    surface_commit(surface_id)
}

/// Commit a surface to the Phase J compositor path.
pub fn surface_commit(surface_id: u32) -> bool {
    trap6(SYS_SURFACE_COMMIT, surface_id as u64, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Destroy a surface.
pub fn surface_destroy(surface_id: u32) -> bool {
    trap6(SYS_SURFACE_DESTROY, surface_id as u64, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Returns true if the present queue is non-empty.
///
/// The compositor calls this after being woken to determine whether to flush.
pub fn surface_pending() -> bool {
    trap6(SYS_SURFACE_QUERY_PENDING, 0, 0, 0, 0, 0, 0) == 1
}

pub fn surface_flush() -> bool {
    trap6(SYS_SURFACE_FLUSH, 0, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

/// Subscribe a service inbox to display-system frame-tick messages.
pub fn frame_tick_subscribe(channel: u32) -> bool {
    trap6(SYS_FRAME_TICK_SUBSCRIBE, channel as u64, 0, 0, 0, 0, 0) != SYSCALL_ERROR
}

pub fn input_set_focus(channel: u32) {
    let _ = trap6(SYS_INPUT_SET_FOCUS, channel as u64, 0, 0, 0, 0, 0);
}

pub fn input_register_window(x: i16, y: i16, w: u16, h: u16, channel: u32) -> bool {
    let xy = (x as u16 as u64) | ((y as u16 as u64) << 16);
    let wh = (w as u64) | ((h as u64) << 16);
    trap6(SYS_INPUT_REGISTER_WINDOW, xy, wh, channel as u64, 0, 0, 0) != SYSCALL_ERROR
}

/// Claim runtime display ownership for the compositor and register the given
/// surface as the fullscreen desktop background.
pub fn compositor_claim_display(surface_id: u32) -> bool {
    trap6(
        SYS_COMPOSITOR_CLAIM_DISPLAY,
        surface_id as u64,
        0,
        0,
        0,
        0,
        0,
    ) != SYSCALL_ERROR
}

#[unsafe(no_mangle)]
pub static mut __stack_chk_guard: u64 = 0xD00D_F00D_CAFE_BABE;

#[unsafe(no_mangle)]
pub extern "C" fn __stack_chk_fail() -> ! {
    write_line(b"[ring3] stack smashing detected\n");
    exit(0xFD)
}

pub fn panic(info: &PanicInfo<'_>) -> ! {
    let _ = info;
    write_line(b"[ring3] protected service panic\n");
    exit(0xEE)
}

pub fn leaf_service(banner: &'static [u8], ready_name: &'static [u8]) -> ! {
    let resolved_channel = service_inbox_or_die(ready_name);

    claim_inbox(resolved_channel);
    write_line(banner);
    let _ = bootstrap_named_status(b"service-ready:", ready_name);
    let _ = bootstrap_named_status(b"service-bound:", ready_name);
    announce_service_ready(ready_name);
    let mut inbox = [0u8; 64];
    loop {
        let raw = channel_recv(resolved_channel, &mut inbox);
        if raw == SYSCALL_ERROR {
            yield_now();
            continue;
        }

        let meta = RecvMeta {
            payload_len: (raw & 0xFFFF) as usize,
            tag: ((raw >> 16) & 0xFF) as u8,
            reply_endpoint: (raw >> 24) as u32,
        };
        let payload = &inbox[..meta.payload_len];

        if payload == b"shutdown" {
            let _ = bootstrap_named_status(b"service-stop:", ready_name);
            write_line(b"[ring3] service shutdown\n");
            exit(0);
        }

        if meta.reply_endpoint != 0 {
            let _ = channel_send(meta.reply_endpoint, b"ack", TAG_SERVICE_STATUS);
        }
    }
}
