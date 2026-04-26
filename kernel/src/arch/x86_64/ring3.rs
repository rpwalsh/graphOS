// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Protected-userspace entry helpers.
//!
//! This module now provides both:
//! - `enter_user_mode()` via `iretq` into ring 3.
//! - A DPL3 `int 0x80` trap gate kept as a compatibility/debug fallback.
//! - A real `syscall/sysret` fast path with a dedicated GS-backed kernel
//!   stack handoff for protected userspace tasks.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::x86_64::{gdt, serial};
use crate::syscall;

const IA32_GS_BASE: u32 = 0xC000_0101;
const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

#[repr(C)]
pub struct Int80Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
}

#[repr(C)]
pub struct FastSyscallFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rcx: u64,
    pub r11: u64,
    pub user_rsp: u64,
}

use crate::sched::percpu::MAX_CPUS;

#[repr(C)]
struct SyscallCpuState {
    kernel_rsp: u64,
    user_rsp: u64,
    in_fast_path: u64,
}

// One slot per logical CPU so each CPU's SYSCALL/SYSRET fast path has its
// own state.  Each CPU sets IA32_KERNEL_GS_BASE to &SYSCALL_CPU_STATES[cpu_idx]
// during `init_fast_syscalls(cpu_idx)`.  Access is safe because a CPU only
// ever touches its own slot (selected via swapgs + GS-relative addressing).
static mut SYSCALL_CPU_STATES: [SyscallCpuState; MAX_CPUS] = {
    // Manual zero-init required: SyscallCpuState is not Copy.
    const ZERO: SyscallCpuState = SyscallCpuState {
        kernel_rsp: 0,
        user_rsp: 0,
        in_fast_path: 0,
    };
    [ZERO; MAX_CPUS]
};
static FAST_SYSCALL_READY: AtomicBool = AtomicBool::new(false);
static DISPATCH_TRACE_BUDGET: AtomicU64 = AtomicU64::new(64);

global_asm!(
    r#"
    .global graphos_user_int80_entry
graphos_user_int80_entry:
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    // Temporarily allow supervisor access to user pages while dispatching
    // syscall pointer arguments (SMAP-safe without requiring STAC/CLAC).
    pushfq
    pop rax
    mov rbx, rax
    or rax, 0x40000
    push rax
    popfq
    sub rsp, 8
    call {dispatch_int80}
    add rsp, 8
    push rbx
    popfq
    mov [rsp + 112], rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

    .global graphos_user_syscall_entry
graphos_user_syscall_entry:
    swapgs
    mov qword ptr gs:[16], 1
    mov gs:[8], rsp
    mov rsp, gs:[0]
    push qword ptr gs:[8]
    push r11
    push rcx
    push rax
    push rbx
    push rbp
    push rdi
    push rsi
    push rdx
    push r8
    push r9
    push r10
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    // Temporarily allow supervisor access to user pages while dispatching
    // syscall pointer arguments (SMAP-safe without requiring STAC/CLAC).
    pushfq
    pop rax
    mov rbx, rax
    or rax, 0x40000
    push rax
    popfq
    sub rsp, 8
    call {dispatch_fast}
    add rsp, 8
    push rbx
    popfq
    cli
    mov qword ptr gs:[16], 0
    mov [rsp + 96], rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r10
    pop r9
    pop r8
    pop rdx
    pop rsi
    pop rdi
    pop rbp
    pop rbx
    pop rax
    pop rcx
    pop r11
    mov rsp, [rsp]
    swapgs
    sysretq
    "#,
    dispatch_int80 = sym graphos_dispatch_int80_syscall,
    dispatch_fast = sym graphos_dispatch_fast_syscall,
);

unsafe extern "C" {
    fn graphos_user_int80_entry();
    fn graphos_user_syscall_entry();
}

pub fn int80_entry_addr() -> u64 {
    graphos_user_int80_entry as *const () as u64
}

pub fn syscall_entry_addr() -> u64 {
    graphos_user_syscall_entry as *const () as u64
}

pub fn fast_syscall_ready() -> bool {
    FAST_SYSCALL_READY.load(Ordering::Acquire)
}

/// Initialise the fast syscall path for the calling CPU.
///
/// `cpu_idx` is the sequential CPU index (0 = BSP) returned by
/// `sched::percpu::register_cpu()`.  Each CPU must call this once during
/// bring-up so that its `IA32_KERNEL_GS_BASE` points to its own
/// `SyscallCpuState` slot.
pub fn init_fast_syscalls(cpu_idx: usize) {
    let idx = if cpu_idx < MAX_CPUS { cpu_idx } else { 0 };
    unsafe {
        write_msr(
            IA32_KERNEL_GS_BASE,
            core::ptr::addr_of!(SYSCALL_CPU_STATES[idx]) as u64,
        );
        write_msr(IA32_GS_BASE, 0);
    }
    gdt::init_syscall_msrs(syscall_entry_addr());
    gdt::enable_sce();
    FAST_SYSCALL_READY.store(true, Ordering::Release);
    serial::write_line(b"[ring3] syscall/sysret fast path armed");
}

pub fn set_syscall_kernel_stack(rsp0: u64) {
    let idx = crate::sched::percpu::current_cpu_index();
    let safe_idx = if idx < MAX_CPUS { idx } else { 0 };
    unsafe {
        let state = &mut *core::ptr::addr_of_mut!(SYSCALL_CPU_STATES[safe_idx]);
        state.kernel_rsp = rsp0;
    }
}

pub fn unwind_nonreturning_fast_syscall() {
    let idx = crate::sched::percpu::current_cpu_index();
    let safe_idx = if idx < MAX_CPUS { idx } else { 0 };
    unsafe {
        let state = &mut *core::ptr::addr_of_mut!(SYSCALL_CPU_STATES[safe_idx]);
        if state.in_fast_path != 0 {
            state.in_fast_path = 0;
            asm!("swapgs", options(nostack, preserves_flags));
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn graphos_dispatch_int80_syscall(frame: &mut Int80Frame) -> u64 {
    dispatch_common(
        frame.rax, frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    )
}

#[unsafe(no_mangle)]
extern "C" fn graphos_dispatch_fast_syscall(frame: &mut FastSyscallFrame) -> u64 {
    dispatch_common(
        frame.rax, frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    )
}

fn dispatch_common(
    nr: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> u64 {
    // Trace the first few ring3 syscalls after boot to prove user execution
    // crossed into kernel dispatch.
    let remaining = DISPATCH_TRACE_BUDGET.load(Ordering::Relaxed);
    if remaining > 0
        && DISPATCH_TRACE_BUDGET
            .compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
    {
        let cur = crate::sched::current_index();
        let tid = crate::task::table::task_id_at(cur);
        serial::write_bytes(b"[ring3] dispatch nr=");
        serial::write_hex_inline(nr);
        serial::write_bytes(b" task=");
        serial::write_u64_dec_inline(tid);
        serial::write_bytes(b" idx=");
        serial::write_u64_dec(cur as u64);
    }

    let args = [arg0, arg1, arg2, arg3, arg4, arg5];
    syscall::dispatch(nr, &args)
}

/// Enter ring 3 at `user_rip` with stack pointer `user_rsp`.
///
/// # Safety
/// - The current task must have a valid `TSS.rsp0` configured.
/// - `user_rip` and `user_rsp` must point to mapped user pages.
pub unsafe fn enter_user_mode(user_rip: u64, user_rsp: u64) -> ! {
    unsafe {
        asm!(
            "mov eax, {user_ds}",
            "mov ds, ax",
            "mov es, ax",
            "mov rax, {user_ds}",
            "push rax",
            "push {user_rsp}",
            "pushfq",
            "pop rax",
            "or rax, 0x200",
            "push rax",
            "mov rax, {user_cs}",
            "push rax",
            "push {user_rip}",
            "iretq",
            user_ds = const crate::arch::x86_64::gdt::USER_DS as u64,
            user_cs = const crate::arch::x86_64::gdt::USER_CS as u64,
            user_rsp = in(reg) user_rsp,
            user_rip = in(reg) user_rip,
            options(noreturn),
        )
    }
}

/// Enter ring-3 at `user_rip`/`user_rsp` with `arg` pre-loaded into rdi
/// (the first System V AMD64 integer argument register).
///
/// Used for user threads created via `SYS_THREAD_SPAWN` that need an
/// argument delivered without a full stack frame.
///
/// # Safety
/// Same contract as `enter_user_mode`.
pub unsafe fn enter_user_mode_with_arg(user_rip: u64, user_rsp: u64, arg: u64) -> ! {
    unsafe {
        asm!(
            "mov eax, {user_ds}",
            "mov ds, ax",
            "mov es, ax",
            "mov rax, {user_ds}",
            "push rax",
            "push {user_rsp}",
            "pushfq",
            "pop rax",
            "or rax, 0x200",
            "push rax",
            "mov rax, {user_cs}",
            "push rax",
            "push {user_rip}",
            // Load arg into rdi before iretq so ring-3 sees it as the
            // first function argument.
            "mov rdi, {arg}",
            "iretq",
            user_ds = const crate::arch::x86_64::gdt::USER_DS as u64,
            user_cs = const crate::arch::x86_64::gdt::USER_CS as u64,
            user_rsp = in(reg) user_rsp,
            user_rip = in(reg) user_rip,
            arg = in(reg) arg,
            options(noreturn),
        )
    }
}

unsafe fn write_msr(msr: u32, value: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("edx") (value >> 32) as u32,
            in("eax") value as u32,
            options(nomem, nostack),
        );
    }
}
