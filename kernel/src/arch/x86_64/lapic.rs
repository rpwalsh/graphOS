// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! LAPIC (Local Advanced Programmable Interrupt Controller) driver
//! and SMP Application Processor (AP) boot trampoline.
//! BSP initialisation maps LAPIC MMIO, enables the spurious-vector register,
//! and then starts APs with INIT/SIPI/SIPI using a trampoline at 0x8000.
//! AP discovery is sourced from ACPI MADT entries.
//! Current limitations: identity-mapped physical memory is assumed,
//! `MAX_CPUS` is 16, I/O APIC MSI routing is not wired yet, and AP stacks are
//! fixed-size static allocations.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::x86_64::serial;

// ── constants ────────────────────────────────────────────────────────────────

pub const MAX_CPUS: usize = 16;

/// Physical address of the LAPIC MMIO registers (default; may be overridden by
/// the IA32_APIC_BASE MSR if the firmware has relocated it).
const LAPIC_DEFAULT_PHYS: u64 = 0xFEE0_0000;

/// LAPIC register offsets (each register is 32 bits at a 16-byte stride).
const LAPIC_ID: usize = 0x020;
const LAPIC_VERSION: usize = 0x030;
const LAPIC_TPR: usize = 0x080; // Task Priority Register
const LAPIC_EOI: usize = 0x0B0;
const LAPIC_SVR: usize = 0x0F0; // Spurious Vector Register
const LAPIC_ICR_LO: usize = 0x300; // Interrupt Command Register (low 32 bits)
const LAPIC_ICR_HI: usize = 0x310; // Interrupt Command Register (high 32 bits)
const LAPIC_TIMER_LVT: usize = 0x320;
const LAPIC_TIMER_INITIAL: usize = 0x380;
const LAPIC_TIMER_CURRENT: usize = 0x390;
const LAPIC_TIMER_DIVIDE: usize = 0x3E0;

const LAPIC_SVR_ENABLE: u32 = 1 << 8;
const LAPIC_SVR_VECTOR: u32 = 0xFF; // spurious vector
const LAPIC_TIMER_PERIODIC: u32 = 1 << 17;
const LAPIC_TIMER_DIVIDE_16: u32 = 0x03;
const LAPIC_TIMER_VECTOR: u32 = 32;
const AP_TIMER_INITIAL_COUNT: u32 = 250_000;

const ICR_DEST_FIELD_SHIFT: u32 = 24;
const ICR_DELIVERY_INIT: u32 = 0x500;
const ICR_DELIVERY_STARTUP: u32 = 0x600;
const ICR_ASSERT: u32 = 1 << 14;
const ICR_TRIGGER_LEVEL: u32 = 1 << 15;
const ICR_DEASSERT: u32 = 0;
const ICR_STATUS_PENDING: u32 = 1 << 12;

/// SIPI vector: the AP will start executing at physical address `vector << 12`.
/// 0x08 → 0x8000 (the conventional SMP trampoline page).
const SIPI_VECTOR: u32 = 0x08;

/// Per-AP stack size.
const AP_STACK_BYTES: usize = 64 * 1024; // 64 KiB per AP

// ── ACPI MADT constants ───────────────────────────────────────────────────────

const RSDP_SIGNATURE: [u8; 8] = *b"RSD PTR ";
const RSDT_SIGNATURE: [u8; 4] = *b"RSDT";
const XSDT_SIGNATURE: [u8; 4] = *b"XSDT";
const MADT_SIGNATURE: [u8; 4] = *b"APIC";

const MADT_TYPE_LAPIC: u8 = 0;
const MADT_TYPE_LAPIC_OVERRIDE: u8 = 5;

// ── static storage ────────────────────────────────────────────────────────────

/// Virtual (= physical in identity-mapped kernel) base address of LAPIC MMIO.
static LAPIC_BASE: AtomicU32 = AtomicU32::new(0);

/// Number of CPUs that have finished AP init and are spinning in their idle loop.
pub static AP_READY: AtomicU32 = AtomicU32::new(0);

/// Number of CPUs total (BSP + APs discovered from MADT).
pub static CPU_COUNT: AtomicU32 = AtomicU32::new(1);

/// Per-AP stack memory pool.
#[repr(align(16))]
struct ApStacks([[u8; AP_STACK_BYTES]; MAX_CPUS]);
static mut AP_STACKS: ApStacks = ApStacks([[0u8; AP_STACK_BYTES]; MAX_CPUS]);

/// LAPIC IDs discovered from MADT (index 0 = BSP).
static AP_LAPIC_IDS: spin::Mutex<[u8; MAX_CPUS]> = spin::Mutex::new([0u8; MAX_CPUS]);

// ── LAPIC MMIO read/write ─────────────────────────────────────────────────────

#[inline]
fn lapic_read(offset: usize) -> u32 {
    let base = LAPIC_BASE.load(Ordering::Relaxed) as usize;
    if base == 0 {
        return 0;
    }
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

#[inline]
fn lapic_write(offset: usize, val: u32) {
    let base = LAPIC_BASE.load(Ordering::Relaxed) as usize;
    if base == 0 {
        return;
    }
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, val) };
    core::sync::atomic::fence(Ordering::SeqCst);
}

// ── MSR helpers ──────────────────────────────────────────────────────────────

fn rdmsr(msr: u32) -> u64 {
    let (hi, lo): (u32, u32);
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi,
                         options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    unsafe {
        core::arch::asm!("wrmsr", in("ecx") msr, in("eax") lo, in("edx") hi,
                         options(nomem, nostack, preserves_flags));
    }
}

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const IA32_APIC_BASE_ENABLE: u64 = 1 << 11;

// ── public API ────────────────────────────────────────────────────────────────

/// Initialise the BSP's LAPIC.
///
/// 1. Read the physical base from IA32_APIC_BASE MSR.
/// 2. Identity-map assumption: physical address == virtual address.
/// 3. Enable the LAPIC via the Spurious Vector Register.
/// 4. Suppress all legacy PIC interrupts via the TPR.
pub fn init_bsp() {
    let apic_base_msr = rdmsr(IA32_APIC_BASE_MSR);
    // The physical base is bits 12..35 of the MSR value.
    let phys_base = apic_base_msr & 0xFFFF_F000;
    let base = if phys_base == 0 {
        LAPIC_DEFAULT_PHYS
    } else {
        phys_base
    };

    // Identity mapping: virtual == physical.
    LAPIC_BASE.store(base as u32, Ordering::Relaxed);

    // Make sure the LAPIC is globally enabled in the MSR.
    wrmsr(IA32_APIC_BASE_MSR, apic_base_msr | IA32_APIC_BASE_ENABLE);

    // Ensure the LAPIC MMIO 2 MiB region is identity-mapped with cache-disable.
    // This is needed if the initial bootstrap identity map didn't cover this
    // region or if it was mapped without the uncacheable attribute.
    crate::mm::page_table::ensure_identity_mapped_2m(base);

    // Enable LAPIC: set SVR software-enable bit; use vector 0xFF as spurious.
    lapic_write(LAPIC_SVR, LAPIC_SVR_ENABLE | LAPIC_SVR_VECTOR);
    // Accept all interrupt priorities.
    lapic_write(LAPIC_TPR, 0);

    serial::write_bytes(b"[lapic] BSP LAPIC id=");
    serial::write_hex_inline(lapic_id() as u64);
    serial::write_bytes(b" base=");
    serial::write_hex(base);
    serial::write_line(b"");
}

/// Return the LAPIC ID of the current CPU.
pub fn lapic_id() -> u8 {
    ((lapic_read(LAPIC_ID) >> 24) & 0xFF) as u8
}

/// Signal end-of-interrupt to the LAPIC.
///
/// Uses `with_kernel_address_space` so this is safe to call from interrupt
/// handlers that fire while a user task's CR3 is active.
#[inline]
pub fn eoi() {
    crate::mm::page_table::with_kernel_address_space(|| lapic_write(LAPIC_EOI, 0));
}

fn init_ap_timer() {
    // QEMU's LAPIC timer frequency varies by machine model. Use a conservative
    // periodic count that is sufficient to drive AP-side preemption without
    // replacing the BSP PIT timebase.
    lapic_write(LAPIC_TIMER_DIVIDE, LAPIC_TIMER_DIVIDE_16);
    lapic_write(LAPIC_TIMER_LVT, LAPIC_TIMER_PERIODIC | LAPIC_TIMER_VECTOR);
    lapic_write(LAPIC_TIMER_INITIAL, AP_TIMER_INITIAL_COUNT);
}

// ── ACPI table walk ───────────────────────────────────────────────────────────

/// Walk low memory (0–0xFFFFF) looking for the RSDP signature on a 16-byte
/// boundary.  Returns the RSDP physical address if found.
fn find_rsdp() -> Option<u64> {
    let mut addr = 16usize; // skip addr=0; null pointer would fail slice safety precondition
    while addr < 0x10_0000 {
        let sig = unsafe { core::slice::from_raw_parts(addr as *const u8, 8) };
        if sig == RSDP_SIGNATURE {
            return Some(addr as u64);
        }
        addr += 16;
    }
    None
}

/// Read a little-endian u32 from a raw physical (= virtual) address.
fn read_u32_at(addr: u64) -> u32 {
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

fn read_u64_at(addr: u64) -> u64 {
    unsafe { core::ptr::read_unaligned(addr as *const u64) }
}

fn read_u8_at(addr: u64) -> u8 {
    unsafe { *(addr as *const u8) }
}

/// Parse the MADT to collect LAPIC IDs.
/// Returns (lapic_ids, count).
fn parse_madt(madt_addr: u64) -> ([u8; MAX_CPUS], usize) {
    let mut ids = [0u8; MAX_CPUS];
    let mut count = 0usize;

    // MADT header: signature(4) + length(4) + revision(1) + checksum(1) + oemid(6) +
    //              oemtableid(8) + oemrevision(4) + creatorid(4) + creatorrevision(4)
    //              + lapic_addr(4) + flags(4) = 44 bytes total header.
    let table_len = read_u32_at(madt_addr + 4) as u64;
    if table_len < 44 {
        return (ids, 0);
    }

    let mut off = 44u64; // offset into MADT past fixed header
    while off + 2 <= table_len {
        let entry_type = read_u8_at(madt_addr + off);
        let entry_length = read_u8_at(madt_addr + off + 1) as u64;
        if entry_length < 2 {
            break;
        }

        if entry_type == MADT_TYPE_LAPIC {
            // Type 0: Processor Local APIC — 8 bytes total.
            // offset 2: processor_id, offset 3: apic_id, offset 4: flags (u32)
            if entry_length >= 8 {
                let apic_id = read_u8_at(madt_addr + off + 3);
                let flags = read_u32_at(madt_addr + off + 4);
                // bit 0 of flags = processor enabled
                if flags & 1 != 0 && count < MAX_CPUS {
                    ids[count] = apic_id;
                    count += 1;
                }
            }
        }
        off += entry_length;
    }
    (ids, count)
}

/// Find the MADT table by walking RSDP → RSDT/XSDT.
fn find_madt() -> Option<u64> {
    let rsdp = find_rsdp()?;
    // RSDP v2: revision at offset 15.
    let revision = read_u8_at(rsdp + 15);

    let (sdt_addr, use_xsdt) = if revision >= 2 {
        // XSDT address at offset 24 (8 bytes).
        let xsdt = read_u64_at(rsdp + 24);
        if xsdt != 0 {
            (xsdt, true)
        } else {
            (read_u32_at(rsdp + 16) as u64, false)
        }
    } else {
        (read_u32_at(rsdp + 16) as u64, false)
    };

    if sdt_addr == 0 {
        return None;
    }

    // SDT header: length at offset 4 (4 bytes). Entries start at offset 36.
    let table_len = read_u32_at(sdt_addr + 4) as u64;
    let entry_bytes: u64 = if use_xsdt { 8 } else { 4 };
    let entry_count = (table_len.saturating_sub(36)) / entry_bytes;

    for i in 0..entry_count {
        let entry_addr = sdt_addr + 36 + i * entry_bytes;
        let ptr: u64 = if use_xsdt {
            read_u64_at(entry_addr)
        } else {
            read_u32_at(entry_addr) as u64
        };
        if ptr == 0 {
            continue;
        }
        // Check the signature field (first 4 bytes of the table).
        let sig = [
            read_u8_at(ptr),
            read_u8_at(ptr + 1),
            read_u8_at(ptr + 2),
            read_u8_at(ptr + 3),
        ];
        if sig == MADT_SIGNATURE {
            return Some(ptr);
        }
    }
    None
}

// ── SIPI trampoline ───────────────────────────────────────────────────────────

/// Write a minimal 16-bit real-mode → 64-bit long-mode trampoline to physical
/// address 0x8000, then send INIT + 2× SIPI to the target LAPIC ID.
///
/// The trampoline:
/// 1. Clears interrupts.
/// 2. Loads a temporary GDT (flat 32-bit code/data descriptors).
/// 3. Sets PE in CR0 → jumps to 32-bit protected mode.
/// 4. Enables PAE in CR4, loads the BSP's PML4 from CR3, enables long mode
///    via IA32_EFER, enables paging → jumps to 64-bit mode.
/// 5. Loads the kernel GDT/IDT selectors, sets up the per-AP RSP, then calls
///    `ap_entry` (defined below).
///
/// Because the trampoline runs at real-mode address 0x8000, all 16-bit
/// relative addresses are computed as offsets from that base.
///
/// **Safety:** Writes to the low-memory physical page at 0x8000.  Only safe
/// during early boot before userspace is alive.
unsafe fn write_trampoline(ap_index: usize, pml4_phys: u64) {
    // Physical trampoline target page.
    const TRAMPOLINE_PHYS: u64 = 0x0000_8000;

    // Per-AP stack top (grows downward).
    let stack_top = unsafe { AP_STACKS.0[ap_index].as_ptr().add(AP_STACK_BYTES) as u64 };

    // 64-bit target (the `ap_entry_asm` function pointer).
    let entry64 = ap_entry_asm as *const () as u64;

    // ── Flat 32-bit GDT (temporary, lives in the trampoline page) ──────────
    // Located at TRAMPOLINE_PHYS + 0x08  (right after the jmp instruction).
    // Layout: null(8) | code32(8) | data32(8) = 24 bytes.
    const GDT_OFFSET: u64 = 0x10; // offset within trampoline page

    let gdt_phys = TRAMPOLINE_PHYS + GDT_OFFSET;
    let gdt_ptr = gdt_phys as *mut u64;
    // SAFETY: Identity-mapped early boot; writing to physical 0x8000 page.
    unsafe {
        // Null descriptor.
        gdt_ptr.write_volatile(0u64);
        // Code segment: limit=0xFFFFF, base=0, readable, execute, 32-bit.
        gdt_ptr.add(1).write_volatile(0x00CF_9A00_0000_FFFFu64);
        // Data segment: limit=0xFFFFF, base=0, writable, 32-bit.
        gdt_ptr.add(2).write_volatile(0x00CF_9200_0000_FFFFu64);
    }

    // ── 16-bit real-mode code ───────────────────────────────────────────────
    // Writes machine code bytes into TRAMPOLINE_PHYS + 0x00.
    let tramp = TRAMPOLINE_PHYS as *mut u8;

    // We write a compact trampoline in raw bytes.  The offsets are relative
    // to the start of the trampoline page (physical 0x8000).
    //
    // This is necessarily `unsafe` inline asm encoded as data.  The sequence:
    //
    //   cli
    //   lgdt [GDT descriptor at offset 0x40]
    //   mov eax, cr0 | 1 (PE bit)
    //   mov cr0, eax
    //   far jmp 0x08:trampoline_32 (CS = code32 selector, EIP = 32-bit stub)
    //
    // Then the 32-bit stub (also in this page, offset 0x50):
    //   mov ax, 0x10   ; data selector
    //   mov ds/es/ss/fs/gs, ax
    //   mov eax, cr4 | PAE | PGE
    //   mov cr4, eax
    //   mov eax, pml4_phys
    //   mov cr3, eax
    //   mov ecx, 0xC0000080 (EFER MSR)
    //   rdmsr
    //   or eax, (1<<8) | (1<<0) ; LME | SCE
    //   wrmsr
    //   mov eax, cr0 | PG | PE | WP
    //   mov cr0, eax
    //   far jmp 0x08:ap_entry64_lo (64-bit entry, encoded as u32 low bits of entry64)
    //
    // Followed by a 64-bit stub at offset 0x80:
    //   mov rsp, stack_top
    //   mov rax, entry64
    //   call rax
    //   hlt / jmp back

    // This raw encoding is simplified and uses fixed offsets.
    // All addresses are < 4 GiB (identity map).

    let pml4_lo = pml4_phys as u32;

    // GDT pseudo-descriptor at offset 0x40 (6 bytes: limit(2) + base(4)).
    const GDTD_OFFSET: u64 = 0x40;
    let gdtd_phys = TRAMPOLINE_PHYS + GDTD_OFFSET;
    unsafe {
        let gdtd = gdtd_phys as *mut u8;
        let limit = (3u16 * 8 - 1).to_le_bytes();
        gdtd.write_volatile(limit[0]);
        gdtd.add(1).write_volatile(limit[1]);
        let base = (gdt_phys as u32).to_le_bytes();
        gdtd.add(2).write_volatile(base[0]);
        gdtd.add(3).write_volatile(base[1]);
        gdtd.add(4).write_volatile(base[2]);
        gdtd.add(5).write_volatile(base[3]);
    }

    // Trampoline code bytes at offset 0.
    // cli
    let mut off = 0usize;
    let b = |tramp: *mut u8, off: &mut usize, v: u8| unsafe {
        tramp.add(*off).write_volatile(v);
        *off += 1;
    };
    let bw = |tramp: *mut u8, off: &mut usize, v: u16| {
        b(tramp, off, v as u8);
        b(tramp, off, (v >> 8) as u8);
    };
    let bd = |tramp: *mut u8, off: &mut usize, v: u32| {
        b(tramp, off, v as u8);
        b(tramp, off, (v >> 8) as u8);
        b(tramp, off, (v >> 16) as u8);
        b(tramp, off, (v >> 24) as u8);
    };
    let bq = |tramp: *mut u8, off: &mut usize, v: u64| {
        bd(tramp, off, v as u32);
        bd(tramp, off, (v >> 32) as u32);
    };

    // cli
    b(tramp, &mut off, 0xFA);
    // lgdt [0x8000 + GDTD_OFFSET] → 0F 01 16 + 4-byte address
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x01);
    b(tramp, &mut off, 0x16); // /2 mem16&32 ModRM [disp32]
    bd(tramp, &mut off, gdtd_phys as u32);
    // mov eax, cr0
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x20);
    b(tramp, &mut off, 0xC0);
    // or al, 1
    b(tramp, &mut off, 0x0C);
    b(tramp, &mut off, 0x01);
    // mov cr0, eax
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x22);
    b(tramp, &mut off, 0xC0);
    // far jmp 0x08:0x8050 → EA + 4-byte offset + 2-byte selector
    let tramp32_target = TRAMPOLINE_PHYS as u32 + 0x50;
    b(tramp, &mut off, 0xEA);
    bd(tramp, &mut off, tramp32_target);
    bw(tramp, &mut off, 0x0008); // code32 selector

    // 32-bit protected mode stub at offset 0x50.
    off = 0x50;
    // mov ax, 0x10  (data32 selector)
    b(tramp, &mut off, 0x66);
    b(tramp, &mut off, 0xB8);
    bw(tramp, &mut off, 0x0010);
    // mov ds, ax
    b(tramp, &mut off, 0x8E);
    b(tramp, &mut off, 0xD8);
    // mov ss, ax
    b(tramp, &mut off, 0x8E);
    b(tramp, &mut off, 0xD0);
    // mov cr4, (cr4 | PAE | PGE)  → read-modify-write
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x20);
    b(tramp, &mut off, 0xE0); // mov eax, cr4
    b(tramp, &mut off, 0x66);
    b(tramp, &mut off, 0x0D); // or eax, imm32
    bd(tramp, &mut off, (1 << 5) | (1 << 7)); // PAE=5, PGE=7
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x22);
    b(tramp, &mut off, 0xE0); // mov cr4, eax
    // mov eax, pml4_lo; mov cr3, eax
    b(tramp, &mut off, 0xB8);
    bd(tramp, &mut off, pml4_lo);
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x22);
    b(tramp, &mut off, 0xD8); // mov cr3, eax
    // rdmsr(0xC0000080); or eax, (LME | SCE); wrmsr
    b(tramp, &mut off, 0xB9);
    bd(tramp, &mut off, 0xC000_0080u32); // mov ecx, EFER
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x32); // rdmsr
    b(tramp, &mut off, 0x66);
    b(tramp, &mut off, 0x0D);
    bd(tramp, &mut off, (1 << 8) | (1 << 0));
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x30); // wrmsr
    // Enable paging: or cr0, PG|WP → set bits 31 and 16
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x20);
    b(tramp, &mut off, 0xC0); // mov eax, cr0
    b(tramp, &mut off, 0x66);
    b(tramp, &mut off, 0x0D);
    bd(tramp, &mut off, (1 << 31) | (1 << 16));
    b(tramp, &mut off, 0x0F);
    b(tramp, &mut off, 0x22);
    b(tramp, &mut off, 0xC0); // mov cr0, eax
    // far jmp 0x08:0x8080 (64-bit long mode stub)
    let tramp64_target = TRAMPOLINE_PHYS as u32 + 0x80;
    b(tramp, &mut off, 0xEA);
    bd(tramp, &mut off, tramp64_target);
    bw(tramp, &mut off, 0x0008);

    // 64-bit stub at offset 0x80.
    off = 0x80;
    // movabs rsp, stack_top (REX.W + B8+r)
    b(tramp, &mut off, 0x48);
    b(tramp, &mut off, 0xBC);
    bq(tramp, &mut off, stack_top);
    // movabs rax, entry64
    b(tramp, &mut off, 0x48);
    b(tramp, &mut off, 0xB8);
    bq(tramp, &mut off, entry64);
    // call rax
    b(tramp, &mut off, 0xFF);
    b(tramp, &mut off, 0xD0);
    // hlt loop
    b(tramp, &mut off, 0xF4); // hlt
    b(tramp, &mut off, 0xEB);
    b(tramp, &mut off, 0xFD); // jmp -3
}

/// 64-bit entry point for Application Processors.
/// Called by the trampoline after entering long mode.
/// The AP increments `AP_READY`, then enters its idle spin loop.
#[unsafe(no_mangle)]
pub extern "C" fn ap_entry_asm() -> ! {
    ap_entry()
}

fn ap_entry() -> ! {
    // Enable the local LAPIC on this AP.
    lapic_write(LAPIC_SVR, LAPIC_SVR_ENABLE | LAPIC_SVR_VECTOR);
    lapic_write(LAPIC_TPR, 0);
    init_ap_timer();

    let id = lapic_id();
    serial::write_bytes(b"[smp] AP online lapic_id=");
    serial::write_hex_inline(id as u64);
    serial::write_line(b"");

    // Register this AP in the per-CPU scheduler table.
    let cpu_idx = crate::sched::percpu::register_cpu(id);
    serial::write_bytes(b"[smp] AP cpu_idx=");
    serial::write_u64_dec_inline(cpu_idx as u64);
    serial::write_line(b"");

    AP_READY.fetch_add(1, Ordering::Release);

    // Enable interrupts and enter the per-CPU scheduler loop.
    // When percpu is not yet active (BSP hasn't called init_percpu yet)
    // the scheduler falls through to the legacy single-core path.
    x86_64::instructions::interrupts::enable();
    loop {
        if crate::sched::percpu::is_active() {
            // Try to run a ready task from our local queue or steal one.
            if let Some(task_idx) = crate::sched::percpu::find_next_ready_percpu() {
                crate::sched::percpu::set_current_on_cpu(task_idx);
                // Hand off to the global scheduler which will perform the
                // context switch into the task.  On return the task has
                // yielded / been preempted; loop to pick the next one.
                unsafe { crate::sched::run_on_ap(task_idx) };
            } else {
                x86_64::instructions::hlt();
            }
        } else {
            x86_64::instructions::hlt();
        }
    }
}

// ── IPI helpers ───────────────────────────────────────────────────────────────

/// Wait until the ICR delivery status bit is clear (IPI accepted by bus).
fn wait_icr_idle() {
    for _ in 0..100_000 {
        if lapic_read(LAPIC_ICR_LO) & ICR_STATUS_PENDING == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Send INIT IPI to a given LAPIC ID.
fn send_init(lapic_id: u8) {
    lapic_write(LAPIC_ICR_HI, (lapic_id as u32) << ICR_DEST_FIELD_SHIFT);
    lapic_write(
        LAPIC_ICR_LO,
        ICR_DELIVERY_INIT | ICR_ASSERT | ICR_TRIGGER_LEVEL,
    );
    wait_icr_idle();
    // Deassert INIT.
    lapic_write(LAPIC_ICR_HI, (lapic_id as u32) << ICR_DEST_FIELD_SHIFT);
    lapic_write(
        LAPIC_ICR_LO,
        ICR_DELIVERY_INIT | ICR_DEASSERT | ICR_TRIGGER_LEVEL,
    );
    wait_icr_idle();
}

/// Send SIPI to a given LAPIC ID.
fn send_sipi(lapic_id: u8) {
    lapic_write(LAPIC_ICR_HI, (lapic_id as u32) << ICR_DEST_FIELD_SHIFT);
    lapic_write(LAPIC_ICR_LO, ICR_DELIVERY_STARTUP | SIPI_VECTOR);
    wait_icr_idle();
}

// ── short delay ───────────────────────────────────────────────────────────────

/// Busy-wait for approximately `loops` iterations (calibrated for ~1 ms
/// each on a 1 GHz+ CPU; exact timing not required for the SIPI sequence).
fn delay(loops: u64) {
    for _ in 0..loops {
        core::hint::spin_loop();
    }
}

// ── public boot entry ─────────────────────────────────────────────────────────

/// Enumerate APs from ACPI MADT and bring them online.
///
/// `pml4_phys` must be the physical address of the kernel's PML4 table so
/// the AP trampoline can load CR3.
///
/// Returns the total number of CPUs (including BSP) that are online after
/// this call returns.
pub fn start_all_aps(pml4_phys: u64) -> usize {
    let bsp_id = lapic_id();
    serial::write_bytes(b"[smp] BSP lapic_id=");
    serial::write_hex_inline(bsp_id as u64);
    serial::write_line(b"");

    // Discover MADT.
    let Some(madt_addr) = find_madt() else {
        serial::write_line(b"[smp] MADT not found -- single-core only");
        return 1;
    };

    let (lap_ids, count) = parse_madt(madt_addr);
    if count == 0 {
        serial::write_line(b"[smp] no APs in MADT -- single-core only");
        return 1;
    }

    serial::write_bytes(b"[smp] MADT CPUs=");
    serial::write_u64_dec_inline(count as u64);
    serial::write_line(b"");

    CPU_COUNT.store(count as u32, Ordering::Relaxed);
    {
        let mut ids = AP_LAPIC_IDS.lock();
        *ids = lap_ids;
    }

    let mut ap_index = 0usize;
    let mut total_started = 1usize; // BSP already running

    for id_ref in lap_ids.iter().take(count) {
        let id = *id_ref;
        if id == bsp_id {
            continue;
        } // skip BSP

        serial::write_bytes(b"[smp] starting AP lapic_id=");
        serial::write_hex_inline(id as u64);
        serial::write_line(b"");

        // Write trampoline for this AP.
        unsafe { write_trampoline(ap_index, pml4_phys) };

        // INIT IPI.
        send_init(id);
        delay(10_000); // ~10 ms wait

        // SIPI × 2 (Intel spec mandates two SIPIs).
        send_sipi(id);
        delay(1_000);
        send_sipi(id);
        delay(1_000);

        // Wait up to ~50 ms for AP to check in.
        let expected = ap_index as u32 + 1;
        let mut waited = 0u32;
        while AP_READY.load(Ordering::Acquire) < expected && waited < 50_000 {
            delay(1);
            waited += 1;
        }
        if AP_READY.load(Ordering::Acquire) >= expected {
            serial::write_bytes(b"[smp] AP ");
            serial::write_hex_inline(id as u64);
            serial::write_line(b" ready");
            total_started += 1;
        } else {
            serial::write_bytes(b"[smp] AP ");
            serial::write_hex_inline(id as u64);
            serial::write_line(b" timed out -- skipping");
        }

        ap_index += 1;
        if ap_index >= MAX_CPUS - 1 {
            break;
        }
    }

    serial::write_bytes(b"[smp] online cpus=");
    serial::write_u64_dec_inline(total_started as u64);
    serial::write_line(b"");

    // Activate per-CPU scheduling now that all APs are alive.
    crate::sched::percpu::init_percpu(total_started);

    total_started
}

/// Read the current CPU's CR3 register to obtain the PML4 physical address.
pub fn current_pml4() -> u64 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
    }
    cr3 & 0xFFFF_FFFF_F000
}
