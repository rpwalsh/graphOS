// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[cfg(target_arch = "x86_64")]
pub fn without_interrupts<F: FnOnce() -> R, R>(f: F) -> R {
    // delegate to the external x86_64 crate's interrupt-safe wrapper
    ::x86_64::instructions::interrupts::without_interrupts(f)
}

#[cfg(target_arch = "aarch64")]
pub fn without_interrupts<F: FnOnce() -> R, R>(f: F) -> R {
    let daif: u64;
    unsafe {
        core::arch::asm!("mrs {0}, daif", out(reg) daif, options(nomem, nostack, preserves_flags));
        core::arch::asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }
    let out = f();
    unsafe {
        core::arch::asm!("msr daif, {0}", in(reg) daif, options(nomem, nostack, preserves_flags));
    }
    out
}
