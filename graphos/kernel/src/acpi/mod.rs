// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! ACPI subsystem — RSDP/XSDT parser, MADT, FADT, S5 poweroff.

pub mod hpet;
pub mod pm;

use core::sync::atomic::{AtomicU16, AtomicU64, Ordering};

// ── ACPI signatures ────────────────────────────────────────────────────────────

const SIG_RSDP: &[u8; 8] = b"RSD PTR ";
const SIG_XSDT: &[u8; 4] = b"XSDT";
const SIG_RSDT: &[u8; 4] = b"RSDT";
const SIG_FADT: &[u8; 4] = b"FACP";
const SIG_MADT: &[u8; 4] = b"APIC";

// ── State ──────────────────────────────────────────────────────────────────────

/// Physical address of the FADT (0 = not found).
static FADT_PHYS: AtomicU64 = AtomicU64::new(0);

/// PM1a control I/O port (for S5 poweroff) — 0 = unknown.
static PM1A_CNT: AtomicU16 = AtomicU16::new(0);
/// PM1b control I/O port (may be 0 if unused).
static PM1B_CNT: AtomicU16 = AtomicU16::new(0);
/// SLP_TYP value for S5 in PM1a and PM1b.
static SLP_TYP_S5A: AtomicU16 = AtomicU16::new(0);
static SLP_TYP_S5B: AtomicU16 = AtomicU16::new(0);

// ── Low-level physical memory access ─────────────────────────────────────────

/// Read a `u8` from a physical identity-mapped address.
/// Safety: requires the UEFI memory map has been parsed and direct map established.
unsafe fn read_phys_u8(phys: u64) -> u8 {
    unsafe { core::ptr::read_volatile(phys as *const u8) }
}

unsafe fn read_phys_u16(phys: u64) -> u16 {
    unsafe { core::ptr::read_unaligned(phys as *const u16) }
}

unsafe fn read_phys_u32(phys: u64) -> u32 {
    unsafe { core::ptr::read_unaligned(phys as *const u32) }
}

unsafe fn read_phys_u64(phys: u64) -> u64 {
    unsafe { core::ptr::read_unaligned(phys as *const u64) }
}

unsafe fn phys_bytes(phys: u64, len: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(phys as *const u8, len) }
}

// ── ACPI checksum ──────────────────────────────────────────────────────────────

fn valid_checksum(phys: u64, len: usize) -> bool {
    let sum: u8 = unsafe { phys_bytes(phys, len) }
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    sum == 0
}

// ── RSDP ──────────────────────────────────────────────────────────────────────

/// Parse the RSDP; return the physical address of the XSDT (preferred) or
/// RSDT if XSDT is not present.
fn parse_rsdp(rsdp_phys: u64) -> Option<(u64, bool)> {
    if unsafe { phys_bytes(rsdp_phys, 8) } != SIG_RSDP {
        return None;
    }
    // RSDP v1 is 20 bytes, v2+ is 36 bytes.
    if !valid_checksum(rsdp_phys, 20) {
        return None;
    }
    let revision = unsafe { read_phys_u8(rsdp_phys + 15) };
    if revision >= 2 {
        // v2: XSDT at offset 24.
        let xsdt_addr = unsafe { read_phys_u64(rsdp_phys + 24) };
        if xsdt_addr != 0 {
            return Some((xsdt_addr, true));
        }
    }
    // Fallback: RSDT at offset 16.
    let rsdt_addr = unsafe { read_phys_u32(rsdp_phys + 16) } as u64;
    if rsdt_addr != 0 {
        Some((rsdt_addr, false))
    } else {
        None
    }
}

// ── SDT header ────────────────────────────────────────────────────────────────

fn sdt_length(phys: u64) -> u32 {
    unsafe { read_phys_u32(phys + 4) }
}

fn sdt_sig(phys: u64) -> [u8; 4] {
    let mut sig = [0u8; 4];
    sig.copy_from_slice(unsafe { phys_bytes(phys, 4) });
    sig
}

// ── XSDT/RSDT table walk ──────────────────────────────────────────────────────

fn walk_tables(sdt_phys: u64, is_xsdt: bool) -> Option<u64> {
    let len = sdt_length(sdt_phys) as u64;
    if len < 36 {
        return None;
    }
    let header_size: u64 = 36;
    let entry_size: u64 = if is_xsdt { 8 } else { 4 };
    let entries = (len - header_size) / entry_size;

    let mut fadt_phys: Option<u64> = None;

    for i in 0..entries {
        let entry_off = sdt_phys + header_size + i * entry_size;
        let table_phys: u64 = if is_xsdt {
            unsafe { read_phys_u64(entry_off) }
        } else {
            (unsafe { read_phys_u32(entry_off) }) as u64
        };
        if table_phys == 0 {
            continue;
        }
        let sig = sdt_sig(table_phys);
        if &sig == SIG_FADT {
            fadt_phys = Some(table_phys);
        }
    }
    fadt_phys
}

// ── FADT / S5 parsing ─────────────────────────────────────────────────────────

/// Parse the FADT and extract PM1a/PM1b control port addresses, then locate
/// the \_S5_ object in the DSDT to determine SLP_TYP values.
fn parse_fadt(fadt_phys: u64) {
    FADT_PHYS.store(fadt_phys, Ordering::Relaxed);

    let len = sdt_length(fadt_phys);
    if len < 116 {
        return;
    }

    // PM1a_CNT_BLK at offset 64, PM1b_CNT_BLK at offset 68 (both 4-byte I/O port).
    let pm1a_cnt = unsafe { read_phys_u32(fadt_phys + 64) } as u16;
    let pm1b_cnt = unsafe { read_phys_u32(fadt_phys + 68) } as u16;
    PM1A_CNT.store(pm1a_cnt, Ordering::Relaxed);
    PM1B_CNT.store(pm1b_cnt, Ordering::Relaxed);

    // DSDT physical address at offset 40 (32-bit pointer in older FADT).
    let dsdt_phys = if len >= 140 {
        // Extended FADT: X_DSDT at offset 132.
        let x = unsafe { read_phys_u64(fadt_phys + 132) };
        if x != 0 {
            x
        } else {
            (unsafe { read_phys_u32(fadt_phys + 40) }) as u64
        }
    } else {
        (unsafe { read_phys_u32(fadt_phys + 40) }) as u64
    };

    if dsdt_phys != 0 {
        parse_dsdt_for_s5(dsdt_phys);
    }
}

/// Scan DSDT AML bytes for the `_S5_` name object and extract SLP_TYP values.
/// We only support the simple case: `Name(_S5_, Package{a, b, ...})`.
fn parse_dsdt_for_s5(dsdt_phys: u64) {
    let dsdt_len = sdt_length(dsdt_phys) as usize;
    if dsdt_len < 36 {
        return;
    }
    let aml = unsafe { phys_bytes(dsdt_phys + 36, dsdt_len - 36) };

    // Search for the byte sequence `_S5_` (0x5F 0x53 0x35 0x5F) followed by a Package opcode.
    const S5_NAME: [u8; 4] = *b"_S5_";
    for i in 0..aml.len().saturating_sub(10) {
        if aml[i..i + 4] == S5_NAME {
            // Expect: NameOp (0x08) precedes _S5_, or we just found the raw bytes.
            // Then: PackageOp (0x12), PkgLength, NumElements, BytePrefix (0x0A), val, ...
            let off = i + 4;
            if off + 4 >= aml.len() {
                break;
            }
            // Skip any AML encoding details — look for two BytePrefix/WordPrefix values.
            let mut j = off;
            // Consume opcode 0x12 (PackageOp) if present.
            if aml[j] == 0x12 {
                j += 1;
            }
            // Consume PkgLength (1–4 bytes).
            if j >= aml.len() {
                break;
            }
            let pkg_len_byte = aml[j];
            let extra = (pkg_len_byte >> 6) as usize;
            j += 1 + extra;
            if j >= aml.len() {
                break;
            }
            // NumElements byte.
            j += 1;
            // First element: SLP_TYP_A.
            if j >= aml.len() {
                break;
            }
            let val_a = if aml[j] == 0x0A {
                j += 1;
                aml.get(j).copied().unwrap_or(0) as u16
            } else {
                aml[j] as u16
            };
            j += 1;
            // Second element: SLP_TYP_B.
            let val_b = if j < aml.len() && aml[j] == 0x0A {
                j += 1;
                aml.get(j).copied().unwrap_or(0) as u16
            } else {
                0
            };
            SLP_TYP_S5A.store((val_a & 0x07) << 10, Ordering::Relaxed);
            SLP_TYP_S5B.store((val_b & 0x07) << 10, Ordering::Relaxed);
            crate::arch::serial::write_line(b"[acpi] _S5_ SLP_TYP found");
            return;
        }
    }
    // QEMU default S5 fallback.
    SLP_TYP_S5A.store(0 << 10, Ordering::Relaxed);
    crate::arch::serial::write_line(b"[acpi] _S5_ not found; using QEMU default");
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the ACPI subsystem from the RSDP physical address provided by
/// the UEFI loader in BootInfo.
pub fn init(rsdp_phys: u64) {
    if rsdp_phys == 0 {
        crate::arch::serial::write_line(b"[acpi] no RSDP - ACPI unavailable");
        return;
    }
    let Some((sdt_phys, is_xsdt)) = parse_rsdp(rsdp_phys) else {
        crate::arch::serial::write_line(b"[acpi] RSDP parse failed");
        return;
    };
    crate::arch::serial::write_line(b"[acpi] RSDP OK");
    if let Some(fadt_phys) = walk_tables(sdt_phys, is_xsdt) {
        parse_fadt(fadt_phys);
        crate::arch::serial::write_line(b"[acpi] FADT parsed");
    } else {
        crate::arch::serial::write_line(b"[acpi] FADT not found");
    }
}

/// Return the PM1a control port (0 = not available).
pub fn pm1a_cnt_port() -> u16 {
    PM1A_CNT.load(Ordering::Relaxed)
}
/// Return the PM1b control port (0 = not used).
pub fn pm1b_cnt_port() -> u16 {
    PM1B_CNT.load(Ordering::Relaxed)
}
/// Return the SLP_TYP value for S5 on PM1a.
pub fn slp_typ_s5_a() -> u16 {
    SLP_TYP_S5A.load(Ordering::Relaxed)
}
/// Return the SLP_TYP value for S5 on PM1b.
pub fn slp_typ_s5_b() -> u16 {
    SLP_TYP_S5B.load(Ordering::Relaxed)
}
