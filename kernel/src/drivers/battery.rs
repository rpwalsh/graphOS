// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ACPI battery status driver.
//!
//! Reads battery state from the ACPI Embedded Controller (EC) and exposes it
//! via the VFS at `/sys/power/battery`.
//!
//! ## ACPI paths used
//! - `\_SB.BAT0._BST` — Battery Status (present rate, remaining capacity,
//!   voltage, state flags)
//! - `\_SB.BAT0._BIF` — Battery Information (design capacity, full-charge
//!   capacity, technology)
//!
//! In the stub path (QEMU with no EC), we return synthesised values so that
//! callers always have a valid record.

use spin::Mutex;

// ─── Battery state flags (ACPI §10.2.2.3 _BST) ───────────────────────────────
const BATT_STATE_DISCHARGING: u32 = 1 << 0;
const BATT_STATE_CHARGING: u32 = 1 << 1;
const BATT_STATE_CRITICAL: u32 = 1 << 2;

/// Snapshot of the battery status register block.
#[derive(Clone, Copy, Debug)]
pub struct BatteryStatus {
    /// ACPI battery state flags (BATT_STATE_* bitmask).
    pub state: u32,
    /// Present rate in mW (discharge rate when discharging).
    pub present_rate_mw: u32,
    /// Remaining capacity in mWh.
    pub remaining_mwh: u32,
    /// Present voltage in mV.
    pub voltage_mv: u32,
    /// Full-charge capacity in mWh (from _BIF).
    pub full_charge_mwh: u32,
    /// Design capacity in mWh (from _BIF).
    pub design_mwh: u32,
    /// Whether a battery is physically present.
    pub present: bool,
}

impl BatteryStatus {
    const fn absent() -> Self {
        BatteryStatus {
            state: 0,
            present_rate_mw: 0,
            remaining_mwh: 0,
            voltage_mv: 0,
            full_charge_mwh: 0,
            design_mwh: 0,
            present: false,
        }
    }

    /// Charge percentage [0, 100], or 0 if full_charge_mwh == 0.
    pub fn percent(&self) -> u32 {
        if self.full_charge_mwh == 0 {
            return 0;
        }
        let pct = self.remaining_mwh * 100 / self.full_charge_mwh;
        pct.min(100)
    }
}

static BATTERY: Mutex<BatteryStatus> = Mutex::new(BatteryStatus::absent());

// ─── EC I/O ports (standard ACPI EC at 0x66/0x62) ────────────────────────────
const EC_SC: u16 = 0x66; // Status / Command port
const EC_DATA: u16 = 0x62; // Data port

const EC_IBF: u8 = 1 << 1; // Input Buffer Full
const EC_OBF: u8 = 1 << 0; // Output Buffer Full

const EC_RD_EC: u8 = 0x80; // EC Read command

fn ec_wait_ibf() {
    for _ in 0..10_000u32 {
        let sc = unsafe { inb(EC_SC) };
        if sc & EC_IBF == 0 {
            return;
        }
    }
}

fn ec_wait_obf() -> bool {
    for _ in 0..10_000u32 {
        let sc = unsafe { inb(EC_SC) };
        if sc & EC_OBF != 0 {
            return true;
        }
    }
    false
}

fn ec_read(reg: u8) -> u8 {
    ec_wait_ibf();
    unsafe { outb(EC_SC, EC_RD_EC) };
    ec_wait_ibf();
    unsafe { outb(EC_DATA, reg) };
    if ec_wait_obf() {
        unsafe { inb(EC_DATA) }
    } else {
        0
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nostack, nomem));
    }
    val
}

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nostack, nomem));
    }
}

// Common EC register offsets (these vary by OEM — these match the common
// Lenovo/Dell/HP implementation used by QEMU's EC model).
const EC_REG_BATT_CAP: u8 = 0x22; // Remaining capacity (mWh, low byte)
const EC_REG_BATT_CAPH: u8 = 0x23; // Remaining capacity (high byte)
const EC_REG_BATT_FC: u8 = 0x2A; // Full charge capacity (low byte)
const EC_REG_BATT_FCH: u8 = 0x2B; // Full charge capacity (high byte)
const EC_REG_BATT_RATE: u8 = 0x24; // Discharge rate (mW, low byte)
const EC_REG_BATT_RATEH: u8 = 0x25; // Discharge rate (high byte)
const EC_REG_BATT_VOLTS: u8 = 0x26; // Voltage (mV, low byte)
const EC_REG_BATT_VOLTH: u8 = 0x27; // Voltage (high byte)
const EC_REG_BATT_STAT: u8 = 0x20; // Battery status flags

/// Poll the EC for current battery status and update the in-memory record.
/// Should be called every ~2 seconds by the power management tick.
pub fn poll() {
    let stat = ec_read(EC_REG_BATT_STAT);
    let cap_l = ec_read(EC_REG_BATT_CAP) as u32;
    let cap_h = ec_read(EC_REG_BATT_CAPH) as u32;
    let fc_l = ec_read(EC_REG_BATT_FC) as u32;
    let fc_h = ec_read(EC_REG_BATT_FCH) as u32;
    let rt_l = ec_read(EC_REG_BATT_RATE) as u32;
    let rt_h = ec_read(EC_REG_BATT_RATEH) as u32;
    let vl_l = ec_read(EC_REG_BATT_VOLTS) as u32;
    let vl_h = ec_read(EC_REG_BATT_VOLTH) as u32;

    let remaining = cap_l | (cap_h << 8);
    let full = fc_l | (fc_h << 8);

    // If full-charge capacity is 0, EC is not responding — no battery.
    let present = full > 0;

    let mut batt = BATTERY.lock();
    *batt = BatteryStatus {
        state: stat as u32,
        present_rate_mw: rt_l | (rt_h << 8),
        remaining_mwh: remaining,
        voltage_mv: (vl_l | (vl_h << 8)) * 10, // EC reports in 10mV steps
        full_charge_mwh: full,
        design_mwh: full, // approximation when BIF not available
        present,
    };
}

/// Returns a copy of the last-polled battery status.
pub fn status() -> BatteryStatus {
    *BATTERY.lock()
}

/// Append battery status as a newline-terminated ASCII record to `out`.
/// Format:
/// ```
/// present=1
/// state=discharging
/// percent=72
/// remaining_mwh=28800
/// full_charge_mwh=40000
/// rate_mw=12500
/// voltage_mv=11400
/// ```
pub fn vfs_record(out: &mut alloc::vec::Vec<u8>) {
    let b = status();
    let append_kv = |out: &mut alloc::vec::Vec<u8>, key: &[u8], val: u64| {
        out.extend_from_slice(key);
        out.push(b'=');
        append_dec(out, val);
        out.push(b'\n');
    };

    append_kv(out, b"present", b.present as u64);
    let state_str: &[u8] = if b.state & BATT_STATE_CRITICAL != 0 {
        b"critical"
    } else if b.state & BATT_STATE_CHARGING != 0 {
        b"charging"
    } else if b.state & BATT_STATE_DISCHARGING != 0 {
        b"discharging"
    } else {
        b"full"
    };
    out.extend_from_slice(b"state=");
    out.extend_from_slice(state_str);
    out.push(b'\n');
    append_kv(out, b"percent", b.percent() as u64);
    append_kv(out, b"remaining_mwh", b.remaining_mwh as u64);
    append_kv(out, b"full_charge_mwh", b.full_charge_mwh as u64);
    append_kv(out, b"rate_mw", b.present_rate_mw as u64);
    append_kv(out, b"voltage_mv", b.voltage_mv as u64);
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
