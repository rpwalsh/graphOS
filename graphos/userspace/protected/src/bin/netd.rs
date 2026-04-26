// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! netd — GraphOS network daemon.
//!
//! Responsibilities:
//! - Register the "netd" service binding in the kernel registry.
//! - Periodically emit heartbeats to keep the service watchdog satisfied.
//! - Drive WTG EM calibration: every `EM_INTERVAL_TICKS` iterations, call
//!   `SYS_GRAPH_EM_STEP` to fit per-type-pair (lambda, p, q) walk parameters
//!   from accumulated walk observations.  The kernel accumulates samples
//!   automatically as the graph arena is traversed; netd triggers the
//!   M-step on behalf of the whole system.
//! - Report network diagnostic statistics to the serial port.
//!
//! ## Service identity
//! Name: `"netd"` — registered under the graph service registry.
//! Required UID: 0 (root — needed for `SYS_GRAPH_EM_STEP`).
//!
//! ## Loop cadence
//! ~50 ms per iteration (SYS_YIELD × YIELDS_PER_ITER).
//! EM step every 300 iterations ≈ every 15 s.
//! Heartbeat every iteration.

#![no_std]
#![no_main]
#![forbid(unsafe_op_in_unsafe_fn)]

#[path = "../runtime.rs"]
mod runtime;

use core::panic::PanicInfo;

// ────────────────────────────────────────────────────────────────────────────
// Timing constants
// ────────────────────────────────────────────────────────────────────────────

/// SYS_YIELD calls per main loop iteration (each yield ≈ 1 scheduler quantum).
const YIELDS_PER_ITER: u32 = 50;

/// Loop iterations between EM M-step triggers.
/// 300 × 50 ms ≈ 15 s between EM fits.
const EM_INTERVAL_ITERS: u32 = 300;

/// Loop iterations between diagnostic stat prints.
/// 120 × 50 ms ≈ 6 s between stat lines.
const STAT_INTERVAL_ITERS: u32 = 120;

// ────────────────────────────────────────────────────────────────────────────
// Extended syscall numbers (mirror of numbers.rs)
// ────────────────────────────────────────────────────────────────────────────

const SYS_NET_STATS: u64 = 0x128;
const SYS_GRAPH_EM_STEP: u64 = 0x30D;
const SYS_GRAPH_EM_STATS: u64 = 0x30E;

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

fn em_step() -> u64 {
    runtime::raw_syscall(SYS_GRAPH_EM_STEP, 0, 0, 0, 0)
}

fn em_stats(src_kind: u16, dst_kind: u16) -> (u32, u32) {
    let packed = runtime::raw_syscall(
        SYS_GRAPH_EM_STATS,
        src_kind as u64,
        dst_kind as u64,
        0,
        0,
    );
    if packed == u64::MAX {
        return (0, 0);
    }
    let transitions = (packed & 0xFFFF_FFFF) as u32;
    let epoch = (packed >> 32) as u32;
    (transitions, epoch)
}

fn net_stats() -> (u64, u64, bool) {
    // Returns (tx_packets, rx_packets, link_ready).
    // SYS_NET_STATS packs: [0]=link_ready(1), [8..48]=tx(40b), [48..64]=..
    // Actual packing from kernel: bits[63]=link_ready, bits[0..32]=tx, bits[32..63]=rx
    let packed = runtime::raw_syscall(SYS_NET_STATS, 0, 0, 0, 0);
    if packed == u64::MAX {
        return (0, 0, false);
    }
    let tx = packed & 0xFFFF_FFFF;
    let rx = (packed >> 32) & 0x7FFF_FFFF;
    let link_ready = (packed >> 63) != 0;
    (tx, rx, link_ready)
}

// ────────────────────────────────────────────────────────────────────────────
// Entry point
// ────────────────────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Self-register so other services can locate netd.
    // We need a channel to register; create one.
    let channel_alias = match runtime::channel_create(256) {
        Some(a) => a,
        None => {
            runtime::write_line(b"[netd] channel_create failed - exiting");
            runtime::exit(1);
        }
    };

    if !runtime::registry_register(b"netd", channel_alias) {
        runtime::write_line(b"[netd] registry_register failed - running without binding");
    } else {
        runtime::write_line(b"[netd] registered as netd");
    }

    let mut iter: u32 = 0;

    loop {
        // ── Yield to the scheduler ──
        for _ in 0..YIELDS_PER_ITER {
            runtime::yield_now();
        }

        // ── Heartbeat ──
        runtime::heartbeat();

        iter = iter.wrapping_add(1);

        // ── Periodic EM calibration ──
        if iter % EM_INTERVAL_ITERS == 0 {
            let epoch = em_step();
            if epoch != u64::MAX {
                runtime::write(1, b"[netd] em_step epoch=");
                write_u32(epoch as u32);
                runtime::write_line(b"");

                // Spot-check: Task→Task pair (kinds 1,1 are the most active).
                let (transitions, ep) = em_stats(1, 1);
                if ep == epoch as u32 && transitions > 0 {
                    runtime::write(1, b"[netd]   Task->Task calibrated: transitions=");
                    write_u32(transitions);
                    runtime::write_line(b"");
                }
            }
        }

        // ── Periodic network stats ──
        if iter % STAT_INTERVAL_ITERS == 0 {
            let (tx, rx, link) = net_stats();
            runtime::write(1, if link { b"[netd] link=UP " } else { b"[netd] link=DOWN " });
            runtime::write(1, b"tx=");
            write_u32(tx as u32);
            runtime::write(1, b" rx=");
            write_u32(rx as u32);
            runtime::write_line(b"");
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tiny decimal formatter (no alloc, no fmt)
// ────────────────────────────────────────────────────────────────────────────

fn write_u32(mut n: u32) {
    let mut buf = [0u8; 10];
    let mut len = 0usize;
    if n == 0 {
        runtime::write(1, b"0");
        return;
    }
    while n > 0 {
        buf[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    // Reverse in place.
    let mut i = 0;
    let mut j = len - 1;
    while i < j {
        buf.swap(i, j);
        i += 1;
        j -= 1;
    }
    runtime::write(1, &buf[..len]);
}

// ────────────────────────────────────────────────────────────────────────────
// Panic handler
// ────────────────────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    runtime::write_line(b"[netd] PANIC - exiting");
    runtime::exit(2);
}
