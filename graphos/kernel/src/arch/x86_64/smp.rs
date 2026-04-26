// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! SMP initialisation — start Application Processors and wire per-CPU scheduler.
//!
//! Calls LAPIC `start_all_aps()` to boot every AP via SIPI, then registers
//! each CPU with `sched::percpu::register_cpu()` and `init_percpu()`.
//!
//! Called once from `main.rs` after the BSP scheduler is running.

use crate::arch::x86_64::lapic;
use crate::sched::percpu;

/// Bring all APs online and initialise per-CPU run queues.
///
/// Returns the total CPU count (BSP + APs).
pub fn init_smp() -> usize {
    // Identify BSP.
    let bsp_lapic = lapic::lapic_id();
    percpu::register_cpu(bsp_lapic);

    // Boot APs. `start_all_aps` sends INIT + SIPI sequences and waits for
    // each AP to report ready via a shared atomic counter in the trampoline.
    let pml4 = lapic::current_pml4();
    let ap_count = lapic::start_all_aps(pml4);

    // Register APs in per-CPU table. LAPIC IDs are contiguous starting from 1.
    for i in 1..=(ap_count as u8) {
        percpu::register_cpu(i);
    }

    let total = 1 + ap_count;
    percpu::init_percpu(total);

    crate::arch::x86_64::serial::write_line(b"[smp] all APs online");
    total
}
