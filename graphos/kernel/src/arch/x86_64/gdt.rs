// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GDT with TSS, IST, and user-mode segments for staged ring-3 support.
//!
//! Layout:
//!   0x00: Null
//!   0x08: Kernel code (ring 0, 64-bit)
//!   0x10: Kernel data (ring 0)
//!   0x18: Padding slot used as the `sysret` STAR base
//!   0x20: User data (ring 3)
//!   0x28: User code (ring 3, 64-bit)
//!   0x30: TSS low
//!   0x38: TSS high
//!
//! SYSCALL/SYSRET convention:
//!   - `IA32_STAR[47:32] = 0x08` gives `syscall` -> CS=0x08, SS=0x10
//!   - `IA32_STAR[63:48] = 0x18` gives `sysret`  -> SS=0x20, CS=0x28
//!
//! The descriptor layout and MSR helpers are now used by the live protected-
//! userspace `syscall/sysret` entry path.

use x86_64::instructions::tables::lgdt;
use x86_64::structures::DescriptorTablePointer;

/// IST stack for double-fault handler (8 KiB, 16-byte aligned).
#[repr(C, align(16))]
struct IstStack([u8; 8192]);

static mut DOUBLE_FAULT_STACK: IstStack = IstStack([0; 8192]);

/// The TSS. rsp[0] is the kernel stack for ring 3->0 transitions.
#[repr(C, packed)]
struct Tss {
    _reserved0: u32,
    /// Privilege-level stacks. rsp[0] is loaded on ring 3->0 transition.
    rsp: [u64; 3],
    _reserved1: u64,
    ist: [u64; 7],
    _reserved2: u64,
    _reserved3: u16,
    iomap_base: u16,
}

static mut TSS: Tss = Tss {
    _reserved0: 0,
    rsp: [0; 3],
    _reserved1: 0,
    ist: [0; 7],
    _reserved2: 0,
    _reserved3: 0,
    iomap_base: 104,
};

/// GDT: null, kernel code, kernel data, (pad), user data, user code, TSS lo, TSS hi.
#[repr(C, align(8))]
struct Gdt {
    entries: [u64; 8],
}

static mut GDT: Gdt = Gdt {
    entries: [
        0x0000_0000_0000_0000, // 0x00: Null
        0x00AF_9A00_0000_FFFF, // 0x08: Kernel code (64-bit, DPL 0)
        0x00CF_9200_0000_FFFF, // 0x10: Kernel data (DPL 0)
        0x0000_0000_0000_0000, // 0x18: Padding (STAR base for sysret)
        0x00CF_F200_0000_FFFF, // 0x20: User data (DPL 3)
        0x00AF_FA00_0000_FFFF, // 0x28: User code (64-bit, DPL 3)
        0,                     // 0x30: TSS low  (patched at runtime)
        0,                     // 0x38: TSS high (patched at runtime)
    ],
};

/// Kernel code segment selector.
pub const KERNEL_CS: u16 = 0x08;
/// Kernel data segment selector.
pub const KERNEL_DS: u16 = 0x10;
/// User data segment selector (with RPL 3).
pub const USER_DS: u16 = 0x20 | 3;
/// User code segment selector (with RPL 3).
pub const USER_CS: u16 = 0x28 | 3;
/// TSS selector.
const TSS_SELECTOR: u16 = 0x30;

/// IST index for double-fault handler (1-based per x86_64 spec).
pub const DOUBLE_FAULT_IST_INDEX: u16 = 1;

/// Set TSS.rsp[0] to the given kernel stack pointer.
/// Called when switching to a user-mode task so that interrupts/syscalls
/// land on the correct kernel stack.
pub fn set_kernel_stack(rsp0: u64) {
    unsafe {
        let tss = &mut *core::ptr::addr_of_mut!(TSS);
        tss.rsp[0] = rsp0;
    }
}

/// Load the GDT (with TSS) and reload segment registers.
pub fn init() {
    // Set up the double-fault IST stack pointer.
    let ist_top = {
        let base = core::ptr::addr_of!(DOUBLE_FAULT_STACK) as u64;
        base + 8192
    };
    unsafe {
        let tss = &mut *core::ptr::addr_of_mut!(TSS);
        tss.ist[0] = ist_top; // IST1
    }

    // Build the 64-bit TSS descriptor (occupies 2 GDT slots).
    let tss_addr = core::ptr::addr_of!(TSS) as u64;
    let tss_limit: u64 = (core::mem::size_of::<Tss>() - 1) as u64;

    let limit_lo = tss_limit & 0xFFFF;
    let base_lo = tss_addr & 0xFFFF;
    let base_mid = (tss_addr >> 16) & 0xFF;
    let base_hi = (tss_addr >> 24) & 0xFF;
    let limit_hi = (tss_limit >> 16) & 0xF;

    let tss_low: u64 = limit_lo
        | (base_lo << 16)
        | (base_mid << 32)
        | (0x89u64 << 40)
        | (limit_hi << 48)
        | (base_hi << 56);
    let tss_high: u64 = (tss_addr >> 32) & 0xFFFF_FFFF;

    unsafe {
        let gdt = &mut *core::ptr::addr_of_mut!(GDT);
        gdt.entries[6] = tss_low;
        gdt.entries[7] = tss_high;
    }

    let ptr = DescriptorTablePointer {
        limit: (8 * 8 - 1) as u16,
        base: x86_64::VirtAddr::new(core::ptr::addr_of!(GDT) as u64),
    };

    unsafe {
        lgdt(&ptr);
        core::arch::asm!(
            "push 0x08",
            "lea rax, [rip + 2f]",
            "push rax",
            "retfq",
            "2:",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "xor ax, ax",
            "mov fs, ax",
            "mov gs, ax",
            out("rax") _,
        );
        // Load the TSS.
        core::arch::asm!(
            "ltr ax",
            in("ax") TSS_SELECTOR,
            options(nomem, nostack),
        );
    }
}

/// Configure SYSCALL/SYSRET MSRs (IA32_STAR, IA32_LSTAR, IA32_FMASK).
///
/// After this, the `syscall` instruction in ring-3 will:
///   1. Load CS=0x08, SS=0x10 (kernel segments)
///   2. Jump to the address in IA32_LSTAR
///   3. Mask RFLAGS with IA32_FMASK (we mask IF to disable interrupts on entry)
///
/// And `sysretq` will:
///   1. Load CS=0x28|3, SS=0x20|3 (user segments)
///   2. Jump to RCX, restore RFLAGS from R11
pub fn init_syscall_msrs(handler: u64) {
    const IA32_STAR: u32 = 0xC000_0081;
    const IA32_LSTAR: u32 = 0xC000_0082;
    const IA32_FMASK: u32 = 0xC000_0084;

    // STAR: bits [63:48] = sysret CS base (0x18), bits [47:32] = syscall CS (0x08)
    let star_val: u64 = (0x0018u64 << 48) | (0x0008u64 << 32);

    // FMASK: mask IF (bit 9) on syscall entry so interrupts are disabled.
    let fmask_val: u64 = 1 << 9;

    unsafe {
        // wrmsr: ECX = MSR index, EDX:EAX = value
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_STAR,
            in("edx") (star_val >> 32) as u32,
            in("eax") star_val as u32,
            options(nomem, nostack),
        );
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_LSTAR,
            in("edx") (handler >> 32) as u32,
            in("eax") handler as u32,
            options(nomem, nostack),
        );
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_FMASK,
            in("edx") (fmask_val >> 32) as u32,
            in("eax") fmask_val as u32,
            options(nomem, nostack),
        );
    }
}

/// Enable the SCE (System Call Extensions) bit in IA32_EFER.
pub fn enable_sce() {
    const IA32_EFER: u32 = 0xC000_0080;
    unsafe {
        let mut lo: u32;
        let mut hi: u32;
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_EFER,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
        lo |= 1; // SCE = bit 0
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_EFER,
            in("edx") hi,
            in("eax") lo,
            options(nomem, nostack),
        );
    }
}
