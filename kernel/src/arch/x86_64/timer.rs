// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! PIT — Programmable Interval Timer (8253/8254) driver.
//!
//! Configures PIT channel 0 in rate-generator mode (mode 2) to fire
//! IRQ 0 at a configurable frequency. The IRQ handler increments a
//! global tick counter and invokes the scheduler for preemptive
//! multitasking.
//!
//! ## Frequency
//! The PIT's base oscillator runs at 1,193,182 Hz. We divide by a
//! 16-bit reload value to get the desired tick rate. Default is 1000 Hz
//! (1 ms per tick), which provides millisecond-resolution timekeeping
//! and responsive preemption without excessive interrupt overhead.
//!
//! ## Monotonic clock
//! `TICK_COUNT` is a monotonically increasing u64 counter. At 1 kHz it
//! will not overflow for ~584 million years.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::serial;

// ════════════════════════════════════════════════════════════════════
// I/O port helpers (shared with PIC — duplicated here to keep modules
// independent; a future port_io module will unify them)
// ════════════════════════════════════════════════════════════════════

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nomem, nostack, preserves_flags),
        );
    }
}

// ════════════════════════════════════════════════════════════════════
// PIT constants
// ════════════════════════════════════════════════════════════════════

/// PIT channel 0 data port.
const PIT_CH0_DATA: u16 = 0x40;
/// PIT command/mode register.
const PIT_CMD: u16 = 0x43;
/// PIT oscillator frequency in Hz.
const PIT_BASE_HZ: u32 = 1_193_182;

/// Desired tick frequency in Hz. 1000 = 1 ms per tick.
pub const TICK_HZ: u32 = 1000;

/// Number of ticks in the default scheduler time slice. A task runs for at
/// most this many ticks before being preempted. At 1 kHz, 10 = 10 ms.
pub const SCHED_TIME_SLICE_TICKS: u64 = 10;

// ════════════════════════════════════════════════════════════════════
// Global state
// ════════════════════════════════════════════════════════════════════

/// Monotonic tick counter. Incremented by the IRQ 0 handler.
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// Ticks remaining in the current task's time slice. When this reaches 0,
/// the timer handler triggers a preemptive reschedule.
static QUANTUM_REMAINING: AtomicU64 = AtomicU64::new(SCHED_TIME_SLICE_TICKS);

// ════════════════════════════════════════════════════════════════════
// Public interface
// ════════════════════════════════════════════════════════════════════

/// Programme PIT channel 0 for periodic interrupts at `TICK_HZ`.
///
/// Does NOT unmask IRQ 0 or enable CPU interrupts — the caller (boot
/// sequence) must do that after the IDT is fully configured.
///
/// # Safety
/// Must be called during single-threaded early init.
pub fn init() {
    let divisor = PIT_BASE_HZ / TICK_HZ;
    // Clamp to u16 range (minimum divisor = 1, maximum = 65535).
    let divisor = if divisor == 0 {
        1u16
    } else if divisor > 0xFFFF {
        0xFFFFu16
    } else {
        divisor as u16
    };

    // Command byte: channel 0, lobyte/hibyte access, mode 2 (rate generator).
    // Bits: [7:6]=00 (ch0), [5:4]=11 (lo/hi), [3:1]=010 (mode 2), [0]=0 (binary).
    let cmd: u8 = 0b0011_0100;

    // SAFETY: PIT ports 0x40 and 0x43 are the standard 8254 timer ports.
    // Writing the command and divisor is a required hardware-init step.
    unsafe {
        outb(PIT_CMD, cmd);
        outb(PIT_CH0_DATA, (divisor & 0xFF) as u8); // low byte
        outb(PIT_CH0_DATA, (divisor >> 8) as u8); // high byte
    }

    let actual_hz = PIT_BASE_HZ / divisor as u32;

    serial::write_bytes(b"[timer] PIT channel 0: divisor=");
    serial::write_u64_dec_inline(divisor as u64);
    serial::write_bytes(b" freq=");
    serial::write_u64_dec_inline(actual_hz as u64);
    serial::write_bytes(b" Hz  slice=");
    serial::write_u64_dec_inline(SCHED_TIME_SLICE_TICKS);
    serial::write_line(b" ticks");
}

/// Called by the IRQ 0 handler on every PIT tick.
///
/// Returns `true` if the current task's time slice has expired and the
/// scheduler should preempt.
///
/// # Safety
/// Must be called only from the IRQ 0 interrupt handler context.
pub fn tick() -> bool {
    let t = TICK_COUNT.fetch_add(1, Ordering::Relaxed);

    // Feed the digital twin — event-driven, no polling loop.
    crate::graph::twin::ingest_irq_tick(t + 1);

    // Fire TCP retransmit timers every tick (now_ms = tick count at ~1 kHz).
    crate::net::tcp::tick_retransmits(t + 1);

    // Fire TCP keepalive / TIME_WAIT timers every 1000 ticks (~1 s).
    if (t + 1) % 1000 == 0 {
        crate::net::tcp::tick_timers(t + 1, 1000);
    }

    // Drain audit ring to /tmp/audit.log every 5000 ticks (~5 s).
    if (t + 1) % 5000 == 0 {
        crate::security::audit::drain_to_vfs_log();
    }

    // Decrement the current time slice. If it hits 0, reset and signal preemption.
    let remaining = QUANTUM_REMAINING.fetch_sub(1, Ordering::Relaxed);
    if remaining <= 1 {
        QUANTUM_REMAINING.store(SCHED_TIME_SLICE_TICKS, Ordering::Relaxed);
        return true; // preempt
    }
    false
}

/// Read the time slice remaining for the current task.
///
/// Used by the twin ingestion to compute utilization fraction.
#[inline]
pub fn quantum_remaining() -> u64 {
    QUANTUM_REMAINING.load(Ordering::Relaxed)
}

/// Reset the time-slice counter. Called when a task voluntarily yields or
/// when a new task is dispatched, to give it a fresh time slice.
pub fn reset_quantum() {
    QUANTUM_REMAINING.store(SCHED_TIME_SLICE_TICKS, Ordering::Relaxed);
}

/// Set a custom time slice for the next dispatch window.
///
/// The scheduler uses this for predictive slice adjustment — e.g.
/// shortening the time slice when the twin predicts a thermal event.
/// Clamped to [1, SCHED_TIME_SLICE_TICKS * 2] to prevent extremes.
#[inline]
pub fn set_quantum(ticks_val: u64) {
    let clamped = ticks_val.clamp(1, SCHED_TIME_SLICE_TICKS * 2);
    QUANTUM_REMAINING.store(clamped, Ordering::Relaxed);
}

/// Read the current monotonic tick count.
///
/// Lock-free, safe to call from any context (interrupt or task).
#[inline]
pub fn ticks() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}

/// Approximate milliseconds since boot (assumes TICK_HZ = 1000).
#[inline]
pub fn millis() -> u64 {
    ticks() // At 1 kHz, ticks == milliseconds.
}

/// Approximate seconds since boot.
#[inline]
pub fn uptime_secs() -> u64 {
    ticks() / TICK_HZ as u64
}
