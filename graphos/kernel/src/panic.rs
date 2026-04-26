// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Panic handler for the freestanding kernel.
//!
//! Prints the panic location and message to the serial console, then halts.
//! If serial is not yet initialized, output is silently lost — there is no
//! earlier output channel available on x86_64.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::serial;
use crate::uuid::Uuid128Gen;

// ====================================================================
// Stack canary (Phase G)
// ====================================================================

/// Stack canary value used by `-Z stack-protector=strong`.
/// Initialised to a non-zero constant; reseeded from RDRAND during boot
/// Stage 8c before any user code runs.  The compiler inserts a load of
/// this symbol at function entry and a compare at return; a mismatch
/// triggers `__stack_chk_fail`.
#[unsafe(no_mangle)]
pub static mut __stack_chk_guard: u64 = 0xDEAD_BEEF_1337_C0DE;

// ====================================================================
// Crash dump helper (Phase E)
// ====================================================================

/// Prevents recursive panics from re-entering crash-dump logic.
static DUMP_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

fn write_hex_u64(dst: &mut [u8], mut pos: usize, value: u64) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut shift = 60u32;
    loop {
        let nib = ((value >> shift) & 0xF) as usize;
        dst[pos] = HEX[nib];
        pos += 1;
        if shift == 0 {
            break;
        }
        shift -= 4;
    }
    pos
}

/// Build `/var/crash/<uuid>.dump` into `dst` and return the path length.
fn build_crash_dump_path(dst: &mut [u8; 64]) -> usize {
    const PREFIX: &[u8] = b"/var/crash/";
    const SUFFIX: &[u8] = b".dump";

    dst[..PREFIX.len()].copy_from_slice(PREFIX);
    let dump_id = Uuid128Gen::v4();
    let (hi, lo) = dump_id.to_u64_pair();

    let mut pos = PREFIX.len();
    pos = write_hex_u64(dst, pos, hi);
    pos = write_hex_u64(dst, pos, lo);
    dst[pos..pos + SUFFIX.len()].copy_from_slice(SUFFIX);
    pos + SUFFIX.len()
}

/// Attempt to persist a minimal crash record to `/var/crash/<uuid>.dump`.
/// Silently no-ops if VFS is not yet available or if a dump is already
/// in progress (recursive panic guard).
fn try_write_crash_dump(info: &PanicInfo) {
    let mut path = [0u8; 64];
    let path_len = build_crash_dump_path(&mut path);
    let path = &path[..path_len];
    // `vfs::create` returns an fd; if VFS is uninitialised it returns Err.
    let Ok(fd) = crate::vfs::create(path) else {
        return;
    };
    let _ = crate::vfs::write(fd, b"GRAPHOS_CRASH_DUMP_V1\n");
    let _ = crate::vfs::write(fd, b"path: ");
    let _ = crate::vfs::write(fd, path);
    let _ = crate::vfs::write(fd, b"\n");
    let _ = crate::vfs::write(fd, b"build: v");
    let _ = crate::vfs::write(fd, env!("GRAPHOS_BUILD_VERSION").as_bytes());
    let _ = crate::vfs::write(fd, b" commit=");
    let _ = crate::vfs::write(fd, env!("GRAPHOS_BUILD_GIT_SHA").as_bytes());
    let _ = crate::vfs::write(fd, b"\n");
    if let Some(location) = info.location() {
        let _ = crate::vfs::write(fd, b"at: ");
        let _ = crate::vfs::write(fd, location.file().as_bytes());
        let _ = crate::vfs::write(fd, b":");
        // Write line number as decimal bytes.
        let line = location.line();
        let mut buf = [0u8; 12];
        let mut pos = 12usize;
        let mut n = line;
        loop {
            pos -= 1;
            buf[pos] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        let _ = crate::vfs::write(fd, &buf[pos..]);
        let _ = crate::vfs::write(fd, b"\n");
    }
    if let Some(msg) = info.message().as_str() {
        let _ = crate::vfs::write(fd, b"msg: ");
        let _ = crate::vfs::write(fd, msg.as_bytes());
        let _ = crate::vfs::write(fd, b"\n");
    }
    let _ = crate::vfs::close(fd);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    const BUILD_VERSION: &str = env!("GRAPHOS_BUILD_VERSION");
    const BUILD_PROFILE: &str = env!("GRAPHOS_BUILD_PROFILE");
    const BUILD_GIT_SHA: &str = env!("GRAPHOS_BUILD_GIT_SHA");
    const BUILD_GIT_DIRTY: &str = env!("GRAPHOS_BUILD_GIT_DIRTY");

    serial::write_line(b"");
    serial::write_line(b"!!! KERNEL PANIC !!!");
    serial::write_bytes(b"  build: v");
    serial::write_bytes(BUILD_VERSION.as_bytes());
    serial::write_bytes(b" commit=");
    serial::write_bytes(BUILD_GIT_SHA.as_bytes());
    serial::write_bytes(b" ");
    serial::write_bytes(BUILD_GIT_DIRTY.as_bytes());
    serial::write_bytes(b" profile=");
    serial::write_line(BUILD_PROFILE.as_bytes());

    if let Some(location) = info.location() {
        serial::write_bytes(b"  at: ");
        serial::write_bytes(location.file().as_bytes());
        serial::write_bytes(b":");
        serial::write_u64_dec(location.line() as u64);
    }

    if let Some(msg) = info.message().as_str() {
        serial::write_bytes(b"  msg: ");
        serial::write_line(msg.as_bytes());
    }

    // Phase E: attempt crash dump to VFS (best-effort; guarded against recursion).
    if !DUMP_IN_PROGRESS.swap(true, Ordering::Relaxed) {
        try_write_crash_dump(info);
    }

    loop {
        // SAFETY: hlt waits for the next interrupt. We are in an
        // unrecoverable state and must not return.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
