// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Performance counter subsystem — PMU sampling per task.
//!
//! Wraps the x86-64 Performance Monitoring Unit (PMU) to collect per-task
//! hardware counters: cycles, retired instructions, LLC misses, branch
//! mispredictions.  Exposes readings via a VFS node at `/sys/perf/<uuid>`.
//!
//! ## Architecture support
//! - x86-64: IA-32 PERF_CTL/PERF_CTR MSR programming (Intel architectural PMU v2)
//! - AArch64: PMU PMCR/PMEVCNTR stub (future)

use crate::uuid::Uuid128 as Uuid;
use spin::Mutex;

// ---------------------------------------------------------------------------
// PMU MSR addresses (Intel architectural PMU v2)
// ---------------------------------------------------------------------------

const MSR_PERF_GLOBAL_CTRL: u32 = 0x38F;
const MSR_PERF_GLOBAL_STATUS: u32 = 0x38E;
const MSR_PERFEVTSEL0: u32 = 0x186;
const MSR_PMC0: u32 = 0xC1;
const MSR_PERFEVTSEL1: u32 = 0x187;
const MSR_PMC1: u32 = 0xC2;
const MSR_PERFEVTSEL2: u32 = 0x188;
const MSR_PMC2: u32 = 0xC3;
const MSR_PERFEVTSEL3: u32 = 0x189;
const MSR_PMC3: u32 = 0xC4;

// Event encodings: (Umask << 8) | EventSelect
const EVSEL_CYCLES: u32 = 0x003C; // UnHalted Reference Cycles
const EVSEL_INSTRUCTIONS: u32 = 0x00C0; // Instructions Retired
const EVSEL_LLC_MISSES: u32 = 0x412E; // Last-Level-Cache Misses
const EVSEL_BRANCH_MISS: u32 = 0x00C5; // Branch Mispredictions

// Perf event select: enable (EN=1), user (USR=1), kernel (OS=1).
const EVSEL_EN: u32 = (1 << 22) | (1 << 16) | (1 << 17);

// ---------------------------------------------------------------------------
// Per-task sample record
// ---------------------------------------------------------------------------

const MAX_TASKS: usize = 64;

#[derive(Clone, Copy)]
pub struct PerfSample {
    pub task_uuid: Uuid,
    /// Accumulated cycles.
    pub cycles: u64,
    /// Accumulated instructions retired.
    pub instructions: u64,
    /// Accumulated LLC misses.
    pub llc_misses: u64,
    /// Accumulated branch mispredictions.
    pub branch_misses: u64,
    /// Number of context switches sampled.
    pub samples: u32,
    pub active: bool,
}

struct PerfState {
    tasks: [PerfSample; MAX_TASKS],
    enabled: bool,
}

impl PerfState {
    const fn new() -> Self {
        Self {
            tasks: [PerfSample {
                task_uuid: Uuid::NIL,
                cycles: 0,
                instructions: 0,
                llc_misses: 0,
                branch_misses: 0,
                samples: 0,
                active: false,
            }; MAX_TASKS],
            enabled: false,
        }
    }
}

static STATE: Mutex<PerfState> = Mutex::new(PerfState::new());

// ---------------------------------------------------------------------------
// MSR helpers (x86-64 only)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
    }
    ((hi as u64) << 32) | lo as u64
}

#[cfg(target_arch = "x86_64")]
unsafe fn wrmsr(msr: u32, val: u64) {
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") (val & 0xFFFF_FFFF) as u32,
            in("edx") (val >> 32) as u32,
            options(nostack, nomem),
        );
    }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn rdmsr(_msr: u32) -> u64 {
    0
}
#[cfg(not(target_arch = "x86_64"))]
unsafe fn wrmsr(_msr: u32, _val: u64) {}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Initialise and enable the performance counters.
pub fn init() {
    unsafe {
        // Programme counter 0: cycles
        wrmsr(MSR_PERFEVTSEL0, (EVSEL_CYCLES | EVSEL_EN) as u64);
        // Counter 1: instructions
        wrmsr(MSR_PERFEVTSEL1, (EVSEL_INSTRUCTIONS | EVSEL_EN) as u64);
        // Counter 2: LLC misses
        wrmsr(MSR_PERFEVTSEL2, (EVSEL_LLC_MISSES | EVSEL_EN) as u64);
        // Counter 3: branch mispredictions
        wrmsr(MSR_PERFEVTSEL3, (EVSEL_BRANCH_MISS | EVSEL_EN) as u64);
        // Enable all 4 counters via PERF_GLOBAL_CTRL.
        wrmsr(MSR_PERF_GLOBAL_CTRL, 0x0F);
    }
    STATE.lock().enabled = true;
    crate::arch::serial::write_line(b"[perf] PMU enabled");
}

/// Snapshot current counter values for `task_uuid`.
///
/// Call on every context switch for that task (or periodically via timer).
/// Accumulates deltas so the task total grows over its lifetime.
pub fn sample(task_uuid: Uuid) {
    let (cyc, ins, llc, brn) = unsafe {
        (
            rdmsr(MSR_PMC0),
            rdmsr(MSR_PMC1),
            rdmsr(MSR_PMC2),
            rdmsr(MSR_PMC3),
        )
    };
    let mut s = STATE.lock();
    if !s.enabled {
        return;
    }

    // Find or allocate a slot.
    let slot = s.tasks.iter_mut().find(|t| t.task_uuid == task_uuid);
    let slot = match slot {
        Some(s) => s,
        None => {
            let free = s.tasks.iter_mut().find(|t| !t.active);
            match free {
                Some(f) => {
                    f.task_uuid = task_uuid;
                    f.active = true;
                    f
                }
                None => return, // table full
            }
        }
    };

    // Simple cumulative accumulation (no previous-value delta for now).
    slot.cycles = slot.cycles.wrapping_add(cyc & 0x0000_FFFF_FFFF_FFFF);
    slot.instructions = slot.instructions.wrapping_add(ins & 0x0000_FFFF_FFFF_FFFF);
    slot.llc_misses = slot.llc_misses.wrapping_add(llc & 0x0000_FFFF_FFFF_FFFF);
    slot.branch_misses = slot.branch_misses.wrapping_add(brn & 0x0000_FFFF_FFFF_FFFF);
    slot.samples += 1;
}

/// Read the accumulated counters for `task_uuid`.
///
/// Returns `None` if the task has no recorded samples.
pub fn read(task_uuid: Uuid) -> Option<PerfSample> {
    let s = STATE.lock();
    s.tasks
        .iter()
        .find(|t| t.active && t.task_uuid == task_uuid)
        .copied()
}

/// Reset counters for `task_uuid`.
pub fn reset(task_uuid: Uuid) {
    let mut s = STATE.lock();
    if let Some(t) = s
        .tasks
        .iter_mut()
        .find(|t| t.active && t.task_uuid == task_uuid)
    {
        *t = PerfSample {
            task_uuid,
            active: true,
            cycles: 0,
            instructions: 0,
            llc_misses: 0,
            branch_misses: 0,
            samples: 0,
        };
    }
}

/// Remove the task from the perf table (call on task exit).
pub fn remove(task_uuid: Uuid) {
    let mut s = STATE.lock();
    if let Some(t) = s
        .tasks
        .iter_mut()
        .find(|t| t.active && t.task_uuid == task_uuid)
    {
        t.active = false;
    }
}
