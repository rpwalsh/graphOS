// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS init — planned first userspace process.
//!
//! This host-build scaffold describes the eventual PID 1 lifecycle.
//! Protected userspace is not live yet, so the kernel still owns the real
//! boot/runtime orchestration path.
//!
//! ## Boot sequence
//!
//! 1. init starts as PID 1.
//! 2. Spawns servicemgr (which owns service lifecycle and channel allocation).
//! 3. Enters a supervision loop: if servicemgr exits, respawn it.
//! 4. On Shutdown signal: sends SIGTERM-equivalent to servicemgr, waits, exits.
//!
//! ## Syscall usage
//!
//! - SYS_SPAWN (0x003): spawn a child process from a path.
//! - SYS_YIELD (0x002): yield CPU while waiting.
//! - SYS_WRITE (0x100): write to serial console.
//! - SYS_EXIT  (0x001): exit the process.
//! - SYS_CHANNEL_RECV (0x103): receive IPC messages (for shutdown signal).

// ════════════════════════════════════════════════════════════════════
// Host-mode syscall shims until ring-3 exists
// ════════════════════════════════════════════════════════════════════

#[path = "../../common/host_sys.rs"]
mod host_sys;

/// Spawn a new process from a named binary. Returns PID or 0 on error.
fn sys_spawn(name: &[u8]) -> u64 {
    host_sys::spawn(name)
}

/// Yield CPU to the scheduler.
fn sys_yield() {
    host_sys::yield_now();
}

/// Write bytes to kernel serial console.
fn sys_write(fd: u32, data: &[u8]) -> usize {
    host_sys::write(fd, data)
}

/// Exit the current process.
fn sys_exit(code: u32) -> ! {
    host_sys::exit(code)
}

/// Receive a message from an IPC channel.
fn sys_channel_recv(channel: u32, buf: &mut [u8]) -> u64 {
    host_sys::channel_recv(channel, buf)
}

#[path = "../../common/ipc.rs"]
mod ipc;

// ════════════════════════════════════════════════════════════════════
// Constants
// ════════════════════════════════════════════════════════════════════

/// Well-known IPC channel for init to receive kernel signals.
const INIT_CHANNEL: u32 = 63;

/// Maximum supervision restarts before giving up.
const MAX_RESTARTS: u32 = 5;

// ════════════════════════════════════════════════════════════════════
// Init process
// ════════════════════════════════════════════════════════════════════

fn log(msg: &[u8]) {
    sys_write(0, msg);
}

fn main() {
    log(b"[init] GraphOS init starting as PID 1\n");

    let mut restart_count: u32 = 0;

    loop {
        // Spawn servicemgr.
        log(b"[init] Spawning servicemgr...\n");
        let pid = sys_spawn(b"servicemgr");

        if pid == 0 {
            log(b"[init] ERROR: Failed to spawn servicemgr\n");
            restart_count += 1;
            if restart_count >= MAX_RESTARTS {
                log(b"[init] FATAL: Max restarts exceeded, halting\n");
                sys_exit(1);
            }
            // Backoff: yield many times before retry.
            for _ in 0..1000 {
                sys_yield();
            }
            continue;
        }

        log(b"[init] servicemgr spawned, entering supervision loop\n");

        // Supervision loop: poll for signals while servicemgr runs.
        let mut idle_ticks: u64 = 0;
        let mut recv_buf = [0u8; 64];
        let mut shutdown_requested = false;

        loop {
            // Check for kernel shutdown signal on init channel.
            let result = sys_channel_recv(INIT_CHANNEL, &mut recv_buf);
            if let Some(msg) = ipc::decode_recv_result(result) {
                if msg.tag == 0x04 {
                    // Shutdown signal from kernel.
                    log(b"[init] Received shutdown signal\n");
                    shutdown_requested = true;
                    break;
                }
            }

            if !host_sys::process_alive(pid) {
                log(b"[init] servicemgr exited\n");
                break;
            }

            idle_ticks += 1;
            sys_yield();

            // Periodic heartbeat log (every ~65536 yields).
            if idle_ticks & 0xFFFF == 0 {
                log(b"[init] heartbeat\n");
            }
        }

        if shutdown_requested {
            log(b"[init] Shutdown complete\n");
            sys_exit(0);
        }

        // servicemgr exited unexpectedly — restart.
        restart_count += 1;
        if restart_count >= MAX_RESTARTS {
            log(b"[init] FATAL: Max restarts exceeded, halting\n");
            sys_exit(1);
        }
        log(b"[init] servicemgr exited, restarting...\n");
        // Brief backoff.
        for _ in 0..100 {
            sys_yield();
        }
    }
}
