// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! CPU thermal zone driver.
//!
//! Reads core temperature from the IA32_THERM_STATUS MSR (Intel) and the
//! AMD SMN thermal register, and exposes data via `/sys/thermal/`.
//!
//! Thermal throttle action: if any core exceeds `THROTTLE_TEMP_C`, the
//! scheduler's CPU frequency hint is reduced by 25 % (via `SCHEDULER_FREQ_SCALE`
//! atomic). The hint is advisory — a real P-state/ACPI Ppc interface lives in
//! `acpi/pm.rs`.

use core::sync::atomic::{AtomicI32, Ordering};

// ─── Throttle threshold ───────────────────────────────────────────────────────
/// Temperature (°C) above which we request throttling.
pub const THROTTLE_TEMP_C: i32 = 90;

/// Current frequency scale factor in percent [25, 100].
/// The scheduler polls this to scale time-slice lengths.
pub static SCHEDULER_FREQ_SCALE: AtomicI32 = AtomicI32::new(100);

// ─── MSR / CPUID constants ────────────────────────────────────────────────────
const IA32_THERM_STATUS: u32 = 0x019C;
const IA32_TEMPERATURE_TARGET: u32 = 0x01A2;

const THERM_STATUS_VALID: u64 = 1 << 31;
const THERM_STATUS_READING: u64 = 0x7F << 16;
const THERM_STATUS_READING_SHIFT: u64 = 16;

// ─── Temperature reading ──────────────────────────────────────────────────────

/// Read the current temperature for logical CPU 0 via MSR.
/// Returns `None` if the MSR is not supported (CPUID check fails or VM has no
/// thermal MSR access).
pub fn read_cpu0_temp_c() -> Option<i32> {
    // Check CPUID: leaf 6 bit 0 = Digital Thermal Sensor.
    let cpuid_6 = unsafe {
        let (eax, _, _, _) = cpuid(6);
        eax
    };
    if cpuid_6 & 1 == 0 {
        return None;
    }

    let status = rdmsr(IA32_THERM_STATUS);
    if status & THERM_STATUS_VALID == 0 {
        return None;
    }

    // TjMax - digital reading = current temperature.
    let tj_max = read_tj_max();
    let reading = ((status & THERM_STATUS_READING) >> THERM_STATUS_READING_SHIFT) as i32;
    Some(tj_max - reading)
}

fn read_tj_max() -> i32 {
    let val = rdmsr(IA32_TEMPERATURE_TARGET);
    let tj = ((val >> 16) & 0xFF) as i32;
    if tj > 0 { tj } else { 100 } // fallback: 100 °C
}

#[inline]
fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem)
        );
    }
    lo as u64 | ((hi as u64) << 32)
}

#[inline]
unsafe fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let eax: u32;
    let ecx: u32;
    let edx: u32;
    // ebx is reserved by LLVM; save/restore manually.
    let ebx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            in("eax") leaf,
            in("ecx") 0u32,
            lateout("eax") eax,
            ebx_out = lateout(reg) ebx,
            lateout("ecx") ecx,
            lateout("edx") edx,
            options(nostack, nomem)
        );
    }
    (eax, ebx, ecx, edx)
}

// ─── Thermal poll ────────────────────────────────────────────────────────────

/// Current temperature snapshot (°C).  Updated by `poll()`.
/// Reads as `i16::MIN` (−32768) if no reading is available.
static LAST_TEMP_C: AtomicI32 = AtomicI32::new(i32::MIN);

/// Poll CPU0 temperature and apply throttle policy.
/// Called from the ACPI thermal zone tick (every ~1 s).
pub fn poll() {
    let temp = match read_cpu0_temp_c() {
        Some(t) => t,
        None => return,
    };

    LAST_TEMP_C.store(temp, Ordering::Relaxed);

    let scale = if temp >= THROTTLE_TEMP_C {
        // Hard throttle: drop to 25 % of full speed.
        25
    } else if temp >= THROTTLE_TEMP_C - 10 {
        // Soft throttle: 75 %.
        75
    } else {
        100
    };

    let prev = SCHEDULER_FREQ_SCALE.swap(scale, Ordering::Relaxed);
    if prev != scale {
        use crate::arch::serial;
        serial::write_bytes(b"[thermal] temp=");
        serial::write_u64_dec_inline(temp as u64);
        serial::write_bytes(b"C scale=");
        serial::write_u64_dec_inline(scale as u64);
        serial::write_bytes(b"%\n");
    }
}

/// Returns the last measured temperature in °C, or `i32::MIN` if unknown.
pub fn last_temp_c() -> i32 {
    LAST_TEMP_C.load(Ordering::Relaxed)
}

/// Append thermal status as ASCII key=value lines to `out`.
pub fn vfs_record(out: &mut alloc::vec::Vec<u8>) {
    let temp = last_temp_c();
    let scale = SCHEDULER_FREQ_SCALE.load(Ordering::Relaxed);

    out.extend_from_slice(b"temp_c=");
    if temp == i32::MIN {
        out.extend_from_slice(b"unavailable\n");
    } else {
        append_dec(out, temp as u64);
        out.push(b'\n');
    }
    out.extend_from_slice(b"throttle_threshold_c=");
    append_dec(out, THROTTLE_TEMP_C as u64);
    out.push(b'\n');
    out.extend_from_slice(b"freq_scale_pct=");
    append_dec(out, scale as u64);
    out.push(b'\n');
}

fn append_dec(out: &mut alloc::vec::Vec<u8>, mut v: u64) {
    if v == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20usize;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    out.extend_from_slice(&buf[i..]);
}
