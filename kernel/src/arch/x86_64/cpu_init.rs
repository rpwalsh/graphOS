// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! CPU security feature initialization — SMEP, SMAP, UMIP, NXE, RDRAND entropy,
//! and the stack-smashing canary anchor.
//!
//! Call `init_cpu_features()` on the BSP after GDT and IDT are loaded but before
//! the first ring-3 transition.  Call `ap_cpu_init()` on every AP after its local
//! GDT/IDT are live.
//!
//! # CR4 bits enabled
//! * Bit 11 — UMIP  (User-Mode Instruction Prevention: SGDT/SIDT/SLDT/STR/SMSW in ring3 → #GP)
//! * Bit 16 — FSGSBASE  (RDFSBASE/RDGSBASE/WRFSBASE/WRGSBASE in ring3)
//! * Bit 20 — SMEP  (Supervisor Mode Execution Prevention: kernel executing user pages → #PF)
//! * Bit 21 — SMAP  (Supervisor Mode Access Prevention: kernel accessing user pages without STAC → #PF)
//!
//! # IA32_EFER bit enabled
//! * Bit 11 — NXE  (No-Execute Enable: PAE NX bit honoured in page tables)
//!
//! # Stack canary
//! A 64-bit random value drawn from RDRAND seeds `STACK_CANARY`.
//! `__stack_chk_fail` halts the machine if the canary is corrupted.
//! When `-Z stack-protector=strong` is passed to rustc the compiler instruments
//! function prologues/epilogues automatically.

use core::sync::atomic::{AtomicU64, Ordering};

// ── CR4 bits ──────────────────────────────────────────────────────────────────
const CR4_UMIP: u64 = 1 << 11;
const CR4_FSGSBASE: u64 = 1 << 16;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;

// ── IA32_EFER ─────────────────────────────────────────────────────────────────
const MSR_EFER: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;

// ── Stack canary ──────────────────────────────────────────────────────────────
static STACK_CANARY: AtomicU64 = AtomicU64::new(0);

// ── MSR helpers ──────────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    (hi as u64) << 32 | lo as u64
}

#[inline(always)]
unsafe fn wrmsr(msr: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") lo,
            in("edx") hi,
            options(nomem, nostack),
        );
    }
}

// ── CR4 helpers ───────────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn read_cr4() -> u64 {
    let val: u64;
    unsafe { core::arch::asm!("mov {}, cr4", out(reg) val, options(nomem, nostack)) };
    val
}

#[inline(always)]
unsafe fn write_cr4(val: u64) {
    unsafe { core::arch::asm!("mov cr4, {}", in(reg) val, options(nomem, nostack)) };
}

// ── RDRAND ────────────────────────────────────────────────────────────────────

/// Returns true if the CPU supports the RDRAND instruction.
/// CPUID.01H:ECX[bit 30].
#[inline]
fn cpu_has_rdrand() -> bool {
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "xor ecx, ecx",
            "cpuid",
            "pop rbx",
            lateout("eax") _,
            out("ecx") ecx,
            lateout("edx") _,
            options(nomem),
        );
    }
    ecx & (1 << 30) != 0
}

/// Hardware RNG via RDRAND.  Returns `None` if the CPU does not support
/// RDRAND or if the hardware RNG reports failure.
#[inline]
pub fn rdrand64() -> Option<u64> {
    if !cpu_has_rdrand() {
        return None;
    }
    let mut val: u64 = 0;
    let mut ok: u8 = 0;
    // Retry up to 10 times — RDRAND may need a few cycles to refill the FIFO.
    for _ in 0..10u32 {
        unsafe {
            core::arch::asm!(
                "rdrand {0:r}",
                "setc {1}",
                out(reg) val,
                out(reg_byte) ok,
                options(nomem, nostack),
            );
        }
        if ok != 0 {
            return Some(val);
        }
    }
    None
}

/// Mix multiple RDRAND reads into a single 64-bit value.
/// Falls back to a compile-time constant combined with the TSC if RDRAND fails.
pub fn rdrand_entropy() -> u64 {
    // Fibonacci hashing splitmix64 constant as accumulator seed.
    let mut acc: u64 = 0x9e3779b97f4a7c15;
    for i in 0u64..16 {
        let raw = rdrand64().unwrap_or_else(|| {
            // TSC fallback — not cryptographically strong but avoids a zero canary.
            let tsc: u64;
            unsafe { core::arch::asm!("rdtsc; shl rdx, 32; or rax, rdx", out("rax") tsc, options(nomem, nostack)) };
            tsc ^ (i.wrapping_mul(0x6c62272e07bb0142))
        });
        // splitmix64 mix step
        let raw = raw ^ (raw >> 30);
        let raw = raw.wrapping_mul(0xbf58476d1ce4e5b9);
        let raw = raw ^ (raw >> 27);
        let raw = raw.wrapping_mul(0x94d049bb133111eb);
        let raw = raw ^ (raw >> 31);
        acc ^= raw.wrapping_add(i);
        acc = acc.rotate_left(17);
    }
    acc
}

// ── Stack canary API ──────────────────────────────────────────────────────────

/// Seed the global stack canary from hardware entropy.
/// Must be called before any function that may be instrumented by
/// `-Z stack-protector`.
pub fn init_stack_canary() {
    let entropy = rdrand_entropy();
    // The canary must never be zero — a zero canary is trivially undetectable.
    let canary = if entropy == 0 {
        0xDEAD_BEEF_CAFE_BABE_u64
    } else {
        entropy
    };
    STACK_CANARY.store(canary, Ordering::Release);
}

/// Return the current canary value.  Used by instrumented prologues/epilogues.
#[inline(always)]
pub fn stack_canary() -> u64 {
    STACK_CANARY.load(Ordering::Acquire)
}

/// Called by `rustc`-generated stack-smashing detection instrumentation when
/// a stack canary mismatch is detected.  This is also the linker symbol expected
/// by GCC-ABI stack protection (`__stack_chk_fail`).
///
/// # Safety
/// This function never returns.  It disables interrupts and halts the core.
#[unsafe(no_mangle)]
pub extern "C" fn __stack_chk_fail() -> ! {
    // Do not use any stack-allocated buffers here — the stack is already
    // considered corrupt.  Write directly to the serial port via the
    // arch serial module.
    crate::arch::x86_64::serial::write_line(
        b"[SECURITY] *** STACK SMASHING DETECTED -- halting core ***",
    );
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack, att_syntax)) };
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Apply SMEP + SMAP + UMIP + FSGSBASE to CR4, and NXE to EFER.
/// Safe to call on both BSP and APs.
unsafe fn apply_cpu_hardening() {
    // NXE
    unsafe {
        let efer = rdmsr(MSR_EFER);
        wrmsr(MSR_EFER, efer | EFER_NXE);
    }
    // CR4 — only set bits the CPU actually supports (CPUID-gated).
    unsafe {
        let cr4 = read_cr4();
        let mut mask: u64 = 0;

        // SMEP: CPUID.07H:EBX.SMEP[bit 7]
        if cpuid_ebx7() & (1 << 7) != 0 {
            mask |= CR4_SMEP;
        }
        // SMAP: CPUID.07H:EBX.SMAP[bit 20]
        if cpuid_ebx7() & (1 << 20) != 0 {
            mask |= CR4_SMAP;
        }
        // UMIP: CPUID.07H:ECX.UMIP[bit 2]
        if cpuid_ecx7() & (1 << 2) != 0 {
            mask |= CR4_UMIP;
        }
        // FSGSBASE: CPUID.07H:EBX.FSGSBASE[bit 0]
        if cpuid_ebx7() & (1 << 0) != 0 {
            mask |= CR4_FSGSBASE;
        }

        write_cr4(cr4 | mask);
    }
}

#[inline]
fn cpuid_ebx7() -> u32 {
    let ebx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            ebx_out = out(reg) ebx,
            lateout("eax") _,
            lateout("ecx") _,
            lateout("edx") _,
            options(nomem),
        );
    }
    ebx
}

#[inline]
fn cpuid_ecx7() -> u32 {
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "pop rbx",
            lateout("eax") _,
            out("ecx") ecx,
            lateout("edx") _,
            options(nomem),
        );
    }
    ecx
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Initialize CPU security features on the BSP (Bootstrap Processor).
///
/// Call once, after GDT and IDT are loaded, before the first ring-3 transition.
pub fn init_cpu_features() {
    unsafe { apply_cpu_hardening() };
    init_stack_canary();
    crate::arch::x86_64::serial::write_line(
        b"[cpu_init] CPU hardening applied (NXE + available SMEP/SMAP/UMIP/FSGSBASE)",
    );
}

/// Initialize CPU security features on an Application Processor.
///
/// Call from the AP trampoline after the AP's GDT and IDT are live.
/// Does **not** reinitialise the canary — APs share the canary seeded by the BSP.
pub fn ap_cpu_init() {
    unsafe { apply_cpu_hardening() };
}
