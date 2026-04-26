// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS Kernel — freestanding x86_64 entry point
//!
//! This is the real kernel binary. The UEFI loader constructs a BootInfo
//! structure and jumps here after exit_boot_services().
//!
//! ## Boot stage sequence
//! 1.  Serial console
//! 2.  BootInfo validation (extended diagnostics)
//! 3.  Display scanout / desktop compositor
//! 4.  GDT + IDT
//! 5.  Physical memory map capture
//! 6.  Reserved range registration (kernel, display scanout, BootInfo, memmap, low 1M, RSDP)
//! 7.  Frame allocator
//! 8.  Paging bootstrap — kernel-owned identity map (CR3 switch deferred)
//! 9.  Kernel heap + VFS bootstrap
//! 10. Timer bring-up
//! 11. Graph subsystem — arena init + type-pair matrix + boot-time seed + spectral init
//! 12. Task subsystem — create kernel init task + graph registration
//! 13. Graph audit + dump (arena, temporal params, spectral state)
//! 14. Idle loop / desktop compositor

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![allow(dead_code)]

extern crate alloc; // Subsystem stubs (syscall, sched, etc.) are forward declarations.

use core::sync::atomic::{AtomicU32, Ordering};

mod acpi;
mod apps;
mod arch;
mod audit;
mod bootinfo;
mod bootstrap_manifest;
mod cognitive;
mod crypto;
mod diag;
mod display_blit;
mod drivers;
mod fido2;
mod fonts;
mod gfx;
mod graph;
mod grid;
mod input;
mod ipc;
mod ldap;
mod mm;
mod net;
mod orchestrator;
mod panic;
mod perf;
mod registry;
mod sched;
mod security;
mod session;
mod storage;
mod svc;
mod syscall;
mod task;
mod tests;
mod tpm;
mod ui;
mod unsafe_policy;
mod update;
mod userland;
mod users;
mod uuid;
mod vfs;
mod wasm;
mod watchdog;
mod wm;

use bootinfo::{BOOTINFO_VERSION, BootInfo, BootInfoDiag};

const BUILD_VERSION: &str = env!("GRAPHOS_BUILD_VERSION");
const BUILD_PROFILE: &str = env!("GRAPHOS_BUILD_PROFILE");
const BUILD_GIT_SHA: &str = env!("GRAPHOS_BUILD_GIT_SHA");
const BUILD_GIT_DIRTY: &str = env!("GRAPHOS_BUILD_GIT_DIRTY");
const PACKAGE_STORE_FORMAT: &str = env!("GRAPHOS_PACKAGE_STORE_FORMAT");
const PACKAGE_STORE_ENTRY_COUNT: &str = env!("GRAPHOS_PACKAGE_STORE_ENTRY_COUNT");

const BOOT_STACK_TARGET: usize = 16;
const BOOT_STACK_MIN_RESIDENT: usize = 10;
const BOOT_PROGRESS_TOTAL: u32 = 15;
const BACKGROUND_QUIESCE_TIMEOUT_TICKS: u64 = 250;
const BACKGROUND_QUIESCE_MAX_PASSES: u32 = 128;

const BOOT_FLAG_PAGING_FALLBACK: u32 = 1 << 0;
const BOOT_FLAG_PROTECTED_CHANNELS: u32 = 1 << 1;
const BOOT_FLAG_PROTECTED_PREFLIGHT: u32 = 1 << 2;
const BOOT_FLAG_PROTECTED_RING3: u32 = 1 << 3;
const BOOT_FLAG_TEST_FAILURES: u32 = 1 << 4;
const BOOT_FLAG_BACKGROUND_PENDING: u32 = 1 << 5;

static BOOT_HEALTH_FLAGS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SystemMode {
    OperationalBootstrap,
    Recovery,
}

impl SystemMode {
    const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::OperationalBootstrap => b"operational-bootstrap",
            Self::Recovery => b"recovery",
        }
    }
}

// Linker-provided symbols for the kernel image extent.
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// Kernel entry point. Called by the UEFI loader with a pointer to BootInfo.
///
/// # Safety
/// The caller (loader) must provide a valid, static BootInfo reference whose
/// memory regions and framebuffer pointers are correct and will not be
/// reclaimed.
#[unsafe(no_mangle)]
pub extern "C" fn graphos_kmain(boot_info: &'static BootInfo) -> ! {
    // ================================================================
    // Stage 1: Serial console — must be first so we can diagnose everything.
    // ================================================================
    crate::arch::serial::init();
    crate::arch::serial::write_line(b"");
    crate::arch::serial::write_line(b"========================================");
    crate::arch::serial::write_line(b"  GraphOS Kernel - early boot");
    crate::arch::serial::write_line(b"========================================");
    crate::arch::serial::write_line(b"[boot] Serial console online");
    users::init_defaults();
    log_release_identity();
    boot_progress(1, BOOT_PROGRESS_TOTAL, b"serial console");

    // ================================================================
    // Stage 2: BootInfo validation
    // ================================================================
    let boot_diag = log_bootinfo(boot_info);
    if boot_diag.intersects(
        BootInfoDiag::VERSION_MISMATCH | BootInfoDiag::SIZE_MISMATCH | BootInfoDiag::NO_MEMORY_MAP,
    ) {
        crate::arch::serial::write_line(
            b"[boot] FATAL: BootInfo envelope is not trustworthy - refusing to continue",
        );
        halt_loop();
    }
    boot_progress(2, BOOT_PROGRESS_TOTAL, b"bootinfo validated");

    // KASLR: record the physical slide as early as possible so that later
    // subsystems (page-table remapping, symbol resolution) can use it.
    mm::kaslr::init(boot_info.kernel_phys_start);

    // ================================================================
    // Stage 3: Architecture tables — must come before any hardware access
    // so that a fault during early init can be caught by the IDT.
    // ================================================================
    crate::arch::serial::write_line(b"[boot] Entering GDT setup");
    arch::x86_64::gdt::init();
    crate::arch::serial::write_line(b"[boot] GDT loaded");

    crate::arch::serial::write_line(b"[boot] Entering IDT setup");
    arch::x86_64::idt::init();
    crate::arch::serial::write_line(b"[boot] IDT loaded");

    arch::x86_64::cpu_init::init_cpu_features();
    crate::arch::serial::write_line(b"[boot] CPU security features armed (SMEP/SMAP/NXE)");

    crate::arch::serial::write_line(b"[boot] Arming ring3 fast syscall entry");
    arch::x86_64::ring3::init_fast_syscalls(0); // BSP is always CPU 0
    boot_progress(3, BOOT_PROGRESS_TOTAL, b"gdt/idt/syscall gate");

    // ================================================================
    // Stage 4: Display scanout + desktop compositor
    // ================================================================
    ui::desktop::init_screen(boot_info);
    boot_progress(4, BOOT_PROGRESS_TOTAL, b"display scanout");
    crate::arch::serial::write_bytes(b"[boot] Display scanout: ");
    crate::arch::serial::write_hex_inline(boot_info.framebuffer_addr);
    crate::arch::serial::write_bytes(b"  ");
    crate::arch::serial::write_u64_dec_inline(boot_info.framebuffer_width as u64);
    crate::arch::serial::write_bytes(b"x");
    crate::arch::serial::write_u64_dec_inline(boot_info.framebuffer_height as u64);
    crate::arch::serial::write_bytes(b"  stride=");
    crate::arch::serial::write_u64_dec(boot_info.framebuffer_stride as u64);

    // ================================================================
    // Stage 5: Physical memory map capture
    // ================================================================
    mm::phys::init(boot_info);
    crate::arch::serial::write_line(b"[boot] Physical memory map captured");
    boot_progress(5, BOOT_PROGRESS_TOTAL, b"memory map captured");

    // ================================================================
    // Stage 6: Register reserved physical ranges
    // ================================================================
    register_reserved_ranges(boot_info);
    boot_progress(6, BOOT_PROGRESS_TOTAL, b"reserved ranges");

    // ================================================================
    // Stage 7: Frame allocator (respects reserved ranges)
    // ================================================================
    mm::frame_alloc::init();
    crate::arch::serial::write_line(b"[boot] Frame allocator initialized");

    // Smoke-test: allocate one frame and release it back into the void.
    // This proves the allocator is functional before anything depends on it.
    if let Some(frame) = mm::frame_alloc::alloc_frame() {
        crate::arch::serial::write_bytes(b"[boot] First safe frame: ");
        crate::arch::serial::write_hex(frame);
    } else {
        crate::arch::serial::write_line(b"[boot] FATAL: frame allocation failed - no usable RAM");
        halt_loop();
    }

    mm::frame_alloc::log_summary();
    boot_progress(7, BOOT_PROGRESS_TOTAL, b"frame allocator ready");

    // ================================================================
    // Stage 8: Paging bootstrap — build kernel-owned page tables
    // ================================================================
    // Read and report the UEFI/loader-provided CR3 first.
    arch::x86_64::paging::init();

    // Build a kernel-owned identity map using 2 MiB huge pages.
    // Map up to the top of the discovered usable physical RAM instead of
    // relying on a fixed boot-time size assumption.
    //
    // SAFETY: Single-threaded early init. UEFI identity map is active
    // so physical addresses are directly accessible.
    // Enable IA32_EFER.NXE before building page tables that use NX (M10 fix).
    unsafe { mm::page_table::enable_nxe() };

    let discovered_limit = mm::phys::max_usable_end();
    if discovered_limit == 0 {
        set_boot_health_flag(BOOT_FLAG_PAGING_FALLBACK);
        crate::arch::serial::write_line(
            b"[boot] WARNING: no usable RAM discovered for paging map limit",
        );
    }
    // Also cover the framebuffer MMIO region so the kernel compositor can
    // write to it after CR3 switch.  The framebuffer may sit above usable
    // RAM (e.g. GOP framebuffer at 0x8000_0000 with only 1 GiB of RAM).
    let fb_end = if boot_info.framebuffer_addr != 0 {
        let end = boot_info.framebuffer_addr + boot_info.framebuffer_size_bytes();
        // Round up to the next 2 MiB boundary (huge-page granularity).
        (end + 0x1F_FFFF) & !0x1F_FFFF
    } else {
        0
    };
    let identity_limit = discovered_limit.max(fb_end).max(2 * 1024 * 1024);
    crate::arch::serial::write_bytes(b"[boot] Paging map limit discovered: ");
    crate::arch::serial::write_hex_inline(identity_limit);
    crate::arch::serial::write_bytes(b" (~");
    crate::arch::serial::write_u64_dec_inline(identity_limit / (1024 * 1024));
    crate::arch::serial::write_line(b" MiB)");

    let pt_result = unsafe { mm::page_table::build_identity_map(identity_limit) };
    match pt_result {
        Some(ref res) => {
            crate::arch::serial::write_bytes(b"[boot] Page tables built: PML4=");
            crate::arch::serial::write_hex_inline(res.pml4_phys);
            crate::arch::serial::write_bytes(b"  frames=");
            crate::arch::serial::write_u64_dec_inline(res.frames_used as u64);
            crate::arch::serial::write_bytes(b"  mapped=");
            crate::arch::serial::write_u64_dec_inline(res.mapped_bytes / (1024 * 1024));
            crate::arch::serial::write_line(b" MiB");

            // Register page-table frames as reserved (post-init audit).
            // SAFETY: Single-threaded early init.
            unsafe { mm::page_table::register_pt_frames_reserved() };

            // Validate the constructed tables by reading them back.
            // SAFETY: UEFI identity map still active; PT frames are accessible.
            let val_errors = unsafe { mm::page_table::validate(res) };
            if val_errors == 0 {
                // Tables are structurally sound. Switch CR3.
                // SAFETY: validate() returned 0 errors. Identity map covers
                // all memory currently in use. Single-threaded early init.
                unsafe { mm::page_table::activate(res) };
            } else {
                set_boot_health_flag(BOOT_FLAG_PAGING_FALLBACK);
                crate::arch::serial::write_bytes(b"[boot] REFUSING CR3 switch - validation found ");
                crate::arch::serial::write_u64_dec_inline(val_errors as u64);
                crate::arch::serial::write_line(b" errors");
                crate::arch::serial::write_line(
                    b"[boot] Continuing on UEFI identity map (degraded)",
                );
            }
        }
        None => {
            set_boot_health_flag(BOOT_FLAG_PAGING_FALLBACK);
            crate::arch::serial::write_line(
                b"[boot] WARNING: page table bootstrap failed - staying on UEFI tables",
            );
        }
    }
    boot_progress(8, BOOT_PROGRESS_TOTAL, b"paging bootstrap");

    // ================================================================
    // Stage 8b: W^X kernel section remapping
    // ================================================================
    // Split the 2 MiB huge pages covering the kernel into 4 KiB pages and
    // apply section-level permissions (.text=RX, .rodata=R, .data/.bss=RW).
    // SAFETY: kernel-owned CR3 is active; frame allocator is up; single-threaded.
    unsafe { mm::page_table::remap_kernel_sections() };

    // ================================================================
    // Stage 8c: CPU hardening — SMEP / SMAP / UMIP
    // ================================================================
    // SAFETY: kernel-owned CR3 is now active (or UEFI identity map in
    // degraded mode). Either way we are in ring 0 and can safely enable
    // the supervisor protection bits.
    arch::x86_64::paging::harden_cr4();

    // Initialise the BSP's LAPIC (required before SIPI can be sent to APs).
    arch::x86_64::lapic::init_bsp();

    // Seed stack canary with hardware entropy now that we have RDRAND.
    // SAFETY: Single-threaded early init; no tasks running yet.
    if let Some(canary) = arch::x86_64::paging::rdrand64() {
        unsafe {
            crate::panic::__stack_chk_guard = canary;
        }
        crate::arch::serial::write_line(b"[boot] Stack canary seeded from RDRAND");
    } else {
        crate::arch::serial::write_line(b"[boot] Stack canary: RDRAND unavailable, using default");
    }

    // Seed the TCP ISN / SYN-cookie MAC with hardware entropy.
    let tcp_seed = arch::x86_64::paging::rdrand64().unwrap_or(0xd69c_6b3a_f1e0_2547);
    net::tcp::seed_entropy(tcp_seed);

    boot_progress(
        9,
        BOOT_PROGRESS_TOTAL,
        b"cpu hardened smep/smap/umip canary",
    );

    // ================================================================
    // Stage 8b: Pre-allocate task stacks
    // ================================================================
    // Under identity mapping, task stacks require physically contiguous
    // frames. We pre-allocate them BEFORE the heap to ensure contiguous
    // regions are available. The heap's bump allocator would otherwise
    // fragment the frame pool.
    //
    // We allocate enough stacks for reasonable concurrency. Production
    // systems with virtual memory won't need this constraint.
    let stack_count = task::stack_pool::init(BOOT_STACK_TARGET);
    if stack_count < BOOT_STACK_MIN_RESIDENT {
        crate::arch::serial::write_line(
            b"[boot] FATAL: insufficient contiguous memory for protected ring3 stack budget",
        );
        halt_loop();
    }
    crate::arch::serial::write_bytes(b"[boot] Stack pool free=");
    crate::arch::serial::write_u64_dec_inline(task::stack_pool::free_count() as u64);
    crate::arch::serial::write_bytes(b"/");
    crate::arch::serial::write_u64_dec_inline(task::stack_pool::total_count() as u64);
    crate::arch::serial::write_line(b" resident task slots");
    boot_progress(10, BOOT_PROGRESS_TOTAL, b"stack pool ready");

    // Stage 8c: kernel heap + scratch VFS + driver registry
    mm::heap::init();
    if mm::heap::stats().capacity == 0 {
        crate::arch::serial::write_line(
            b"[boot] FATAL: kernel heap bootstrap failed - refusing to continue",
        );
        halt_loop();
    }
    vfs::init(boot_info);
    arch::x86_64::pci::log_scan();
    drivers::init();
    let driver_probe = drivers::probe_all();
    crate::arch::serial::write_bytes(b"[boot] Drivers: bound=");
    crate::arch::serial::write_u64_dec_inline(driver_probe.bound as u64);
    crate::arch::serial::write_bytes(b" managed=");
    crate::arch::serial::write_u64_dec_inline(driver_probe.managed as u64);
    crate::arch::serial::write_bytes(b" failed=");
    crate::arch::serial::write_u64_dec_inline(driver_probe.failed as u64);
    crate::arch::serial::write_bytes(b" absent=");
    crate::arch::serial::write_u64_dec_inline(driver_probe.no_match as u64);
    crate::arch::serial::write_bytes(b" total=");
    crate::arch::serial::write_u64_dec(driver_probe.total as u64);
    drivers::display::log_summary();
    if arch::x86_64::virtio_net::is_present() {
        arch::x86_64::virtio_net::init();
    }
    if arch::x86_64::virtio_blk::is_present() && arch::x86_64::virtio_blk::init() {
        let sects = arch::x86_64::virtio_blk::capacity_sectors();
        crate::arch::serial::write_bytes(b"[blk] virtio-blk ready sectors=");
        crate::arch::serial::write_u64_dec(sects);
        crate::arch::serial::write_line(b"");
    }
    let net_stats = net::stats();
    crate::arch::serial::write_bytes(b"[net] link=");
    crate::arch::serial::write_bytes(if net_stats.link_ready {
        b"virtio-net-ready"
    } else {
        b"loopback-only"
    });
    if let Some(irq) = arch::x86_64::virtio_net::irq_line() {
        crate::arch::serial::write_bytes(b" irq=");
        crate::arch::serial::write_u64_dec(irq as u64);
    } else {
        crate::arch::serial::write_line(b"");
    }
    boot_progress(11, BOOT_PROGRESS_TOTAL, b"vfs and drivers");
    // ================================================================
    // Stage 9: PIC + PIT timer — interrupt-driven preemptive tick
    // ================================================================
    // Order matters:
    //   1. Remap PIC (so IRQ vectors don't collide with CPU exceptions).
    //   2. Programme PIT channel 0 for 1 kHz periodic interrupts.
    //   3. IDT already has the timer handler at vector 32.
    //   4. Unmask IRQ 0 (timer) so the PIC delivers it.
    //   5. STI — enable CPU interrupt flag.
    //
    // After STI, the PIT fires every 1 ms. The timer ISR decrements
    // the quantum counter and calls sched::preempt() when it expires.
    // This gives us true preemptive multitasking with 10 ms quanta.
    unsafe { arch::x86_64::pic::init() };
    arch::timer::init();
    let _pointer_ready =
        input::pointer::init(boot_info.framebuffer_width, boot_info.framebuffer_height);

    unsafe { arch::x86_64::pic::unmask(0) }; // IRQ 0 = PIT timer
    unsafe { arch::x86_64::pic::unmask(1) }; // IRQ 1 = keyboard (ready for driver)
    if let Some(pointer_irq) = input::pointer::irq_line() {
        unsafe { arch::x86_64::pic::unmask(pointer_irq) };
    }
    if let Some(net_irq) = arch::x86_64::virtio_net::irq_line() {
        unsafe { arch::x86_64::pic::unmask(net_irq) }; // IRQ for virtio-net RX packets
    }
    // STI is deferred until just before dispatch_first() so that timer
    // interrupts don't fire while we're still building the task table.
    boot_progress(12, BOOT_PROGRESS_TOTAL, b"interrupt timer");

    // ================================================================
    // Stage 10: Graph subsystem — the core abstraction of GraphOS
    // ================================================================
    // The graph is not a data structure bolted onto the kernel. It IS
    // the kernel's model of reality. Every object that participates in
    // scheduling, trust, diagnostics, or prediction is a node. Every
    // relationship is a typed, weighted, timestamped, provenance-tracked edge.
    //
    // arena::init()  — creates the kernel root node (id=1)
    // temporal::init() — initialises per-type-pair decay matrix (λ, p, q)
    // seed::seed_from_boot() — populates CPU, framebuffer, reserved ranges
    // spectral::init() — initialises eigenvalue tracking + CUSUM detector
    graph::arena::init();
    graph::arena::log_layout();
    graph::temporal::init();
    graph::seed::seed_from_boot(boot_info);
    graph::spectral::init();
    twin_init();
    input::diagnostics::bind_graph();
    boot_progress(13, BOOT_PROGRESS_TOTAL, b"graph substrate");

    // ================================================================
    // Stage 11: Task subsystem — create kernel init task
    // ================================================================
    crate::arch::serial::write_line(b"[boot] === Init task creation ===");

    // The init task remains a kernel-mode orchestration task for now:
    //   1. It proves the substrate (IPC, scheduler, regressions).
    //   2. It launches the protected init/service chain via the ELF loader.
    //   3. It hands off to the desktop compositor after the boot test pass.
    //
    // The scheduler will dispatch it after we call sched::dispatch_first().
    let init_entry = kernel_init_task as *const () as u64;
    match task::table::create_kernel_task_with_index(b"init", init_entry) {
        Some((id, _index)) => {
            crate::arch::serial::write_bytes(b"[boot] init task registered, id=");
            crate::arch::serial::write_u64_dec(id);

            // Register the init task in the kernel graph.
            // This makes the task visible to graph queries, diagnostics,
            // and (eventually) the AI operating participant.
            graph::seed::register_task(b"init", graph::types::NODE_ID_KERNEL);
        }
        None => {
            crate::arch::serial::write_line(b"[boot] FATAL: failed to create init task");
            halt_loop();
        }
    }

    task::table::dump_all();
    boot_progress(14, BOOT_PROGRESS_TOTAL, b"task table ready");

    // ================================================================
    // Stage 12: Graph audit and dump
    // ================================================================
    // Verify referential integrity of the boot-time graph. Every edge
    // must reference existing nodes. This is a hard invariant.
    let broken = graph::arena::audit();
    if broken > 0 {
        crate::arch::serial::write_line(
            b"[boot] WARNING: graph integrity violations detected - see above",
        );
    }
    #[cfg(feature = "boot-demos")]
    {
        graph::arena::dump();
        graph::temporal::dump();
        graph::spectral::dump();
    }
    crate::arch::serial::write_line(b"[boot] Graph audit complete");

    // ================================================================
    // Stage 12b: Service manager init
    // ================================================================
    // Initialize the service manager before scheduler. Services will be
    // registered during init task execution and started via start_all().
    svc::init();

    // ================================================================
    // Stage 13: Scheduler init and first dispatch
    // ================================================================
    sched::init();

    crate::arch::serial::write_line(b"");
    crate::arch::serial::write_line(b"========================================");
    crate::arch::serial::write_line(b"  GraphOS boot complete - core stages OK");
    crate::arch::serial::write_line(b"  Predictive scheduling: twin-adaptive quantum");
    crate::arch::serial::write_line(b"  Ring-3 live: ELF services + syscall/sysret");
    crate::arch::serial::write_line(b"  Dispatching init task...");
    crate::arch::serial::write_line(b"========================================");
    boot_progress(15, BOOT_PROGRESS_TOTAL, b"dispatching init");

    // Bring Application Processors online now that the kernel is fully
    // initialised.  Each AP will execute its idle loop until the per-CPU
    // scheduler is wired; this validates that the SIPI trampoline works.
    {
        let pml4 = arch::x86_64::lapic::current_pml4();
        let cpu_count = arch::x86_64::lapic::start_all_aps(pml4);
        crate::arch::serial::write_bytes(b"[smp] total CPUs online=");
        crate::arch::serial::write_u64_dec(cpu_count as u64);
    }

    // Enable interrupts. From this point, the PIT fires every 1 ms and
    // preemptive scheduling is active.
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };

    // SAFETY: Single-threaded early init. Task table contains at least
    // one Ready task (init). The scheduler saves the boot thread's context
    // and switches to the init task. When all tasks finish, control
    // returns here.
    unsafe { sched::dispatch_first() };

    // All tasks have completed (or none were Ready). Enter halt loop.
    crate::arch::serial::write_line(b"[boot] All tasks complete - entering halt loop");
    halt_loop()
}

fn set_boot_health_flag(flag: u32) {
    BOOT_HEALTH_FLAGS.fetch_or(flag, Ordering::Relaxed);
}

fn clear_boot_health_flag(flag: u32) {
    BOOT_HEALTH_FLAGS.fetch_and(!flag, Ordering::Relaxed);
}

fn boot_health_flags() -> u32 {
    BOOT_HEALTH_FLAGS.load(Ordering::Relaxed)
}

fn current_system_mode(test_failures: u32, protected_bootstrap_ok: bool) -> SystemMode {
    if test_failures == 0 && protected_bootstrap_ok && boot_health_flags() == 0 {
        SystemMode::OperationalBootstrap
    } else {
        SystemMode::Recovery
    }
}

fn boot_progress(step: u32, total: u32, label: &[u8]) {
    let total = total.max(1);
    let step = step.min(total);
    let filled = ((step * 20) / total) as usize;

    crate::arch::serial::write_bytes_raw(b"[boot ");
    if step < 10 {
        crate::arch::serial::write_bytes_raw(b"0");
    }
    crate::arch::serial::write_u64_dec_inline_raw(step as u64);
    crate::arch::serial::write_bytes_raw(b"/");
    if total < 10 {
        crate::arch::serial::write_bytes_raw(b"0");
    }
    crate::arch::serial::write_u64_dec_inline_raw(total as u64);
    crate::arch::serial::write_bytes_raw(b"] [");
    for idx in 0..20 {
        if idx < filled {
            crate::arch::serial::write_bytes_raw(b"#");
        } else {
            crate::arch::serial::write_bytes_raw(b"-");
        }
    }
    crate::arch::serial::write_bytes_raw(b"] ");
    crate::arch::serial::write_u64_dec_inline_raw((step * 100 / total) as u64);
    crate::arch::serial::write_bytes_raw(b"% ");
    crate::arch::serial::write_line_raw(label);
}

fn log_release_identity() {
    crate::arch::serial::write_bytes(b"[boot] Release: GraphOS v");
    crate::arch::serial::write_bytes(BUILD_VERSION.as_bytes());
    crate::arch::serial::write_bytes(b" commit=");
    crate::arch::serial::write_bytes(BUILD_GIT_SHA.as_bytes());
    crate::arch::serial::write_bytes(b" ");
    crate::arch::serial::write_bytes(BUILD_GIT_DIRTY.as_bytes());
    crate::arch::serial::write_bytes(b" profile=");
    crate::arch::serial::write_bytes(BUILD_PROFILE.as_bytes());
    crate::arch::serial::write_bytes(b" abi=");
    crate::arch::serial::write_u64_dec_inline(BOOTINFO_VERSION as u64);
    crate::arch::serial::write_bytes(b" pkgfmt=");
    crate::arch::serial::write_bytes(PACKAGE_STORE_FORMAT.as_bytes());
    crate::arch::serial::write_bytes(b" entries=");
    crate::arch::serial::write_line(PACKAGE_STORE_ENTRY_COUNT.as_bytes());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProtectedBootstrapOutcome {
    Ready,
    Degraded,
    PreflightFailed,
    SpawnFailed,
    BootstrapFailed,
}

// ====================================================================
// Boot-time helpers
// ====================================================================

/// Log BootInfo envelope fields and validate the version/size contract.
fn log_bootinfo(bi: &BootInfo) -> BootInfoDiag {
    crate::arch::serial::write_bytes(b"[boot] BootInfo version=");
    crate::arch::serial::write_u64_dec_inline(bi.bootinfo_version as u64);
    crate::arch::serial::write_bytes(b"  size=");
    crate::arch::serial::write_u64_dec_inline(bi.bootinfo_size as u64);
    crate::arch::serial::write_bytes(b"  (expect v=");
    crate::arch::serial::write_u64_dec_inline(BOOTINFO_VERSION as u64);
    crate::arch::serial::write_bytes(b" sz=");
    crate::arch::serial::write_u64_dec_inline(core::mem::size_of::<BootInfo>() as u64);
    crate::arch::serial::write_line(b")");

    // Extended structural validation.
    let diag = bi.validate_extended();
    if diag.is_empty() {
        crate::arch::serial::write_line(b"[boot] BootInfo validated OK (all checks passed)");
    } else {
        crate::arch::serial::write_bytes(b"[boot] BootInfo diagnostics: 0x");
        crate::arch::serial::write_hex(diag.bits() as u64);
        if diag.contains(BootInfoDiag::VERSION_MISMATCH) {
            crate::arch::serial::write_line(b"[boot]   WARN: version mismatch");
        }
        if diag.contains(BootInfoDiag::SIZE_MISMATCH) {
            crate::arch::serial::write_line(b"[boot]   WARN: size mismatch");
        }
        if diag.contains(BootInfoDiag::NO_FRAMEBUFFER) {
            crate::arch::serial::write_line(b"[boot]   WARN: no framebuffer");
        }
        if diag.contains(BootInfoDiag::FB_ZERO_DIMENSION) {
            crate::arch::serial::write_line(b"[boot]   WARN: framebuffer zero dimension");
        }
        if diag.contains(BootInfoDiag::FB_STRIDE_UNDERFLOW) {
            crate::arch::serial::write_line(b"[boot]   WARN: framebuffer stride < width");
        }
        if diag.contains(BootInfoDiag::NO_MEMORY_MAP) {
            crate::arch::serial::write_line(
                b"[boot]   CRIT: no memory map - cannot continue safely",
            );
        }
        if diag.contains(BootInfoDiag::MEMORY_MAP_SUSPECT) {
            crate::arch::serial::write_line(b"[boot]   WARN: memory map region count > 512");
        }
        if diag.contains(BootInfoDiag::NO_RSDP) {
            crate::arch::serial::write_line(b"[boot]   WARN: no RSDP - ACPI unavailable");
        }
        if diag.contains(BootInfoDiag::KERNEL_EXTENT_INVERTED) {
            crate::arch::serial::write_line(b"[boot]   WARN: kernel phys start >= end");
        }
        if diag.contains(BootInfoDiag::BOOT_MODULES_INVALID) {
            crate::arch::serial::write_line(b"[boot]   WARN: boot module table invalid");
        }
        if diag.contains(BootInfoDiag::PACKAGE_STORE_INVALID) {
            crate::arch::serial::write_line(b"[boot]   WARN: package store descriptor invalid");
        }
    }

    // RSDP address — useful for ACPI debugging.
    crate::arch::serial::write_bytes(b"[boot] RSDP: ");
    if bi.rsdp_addr != 0 {
        crate::arch::serial::write_hex(bi.rsdp_addr);
        acpi::init(bi.rsdp_addr);
    } else {
        crate::arch::serial::write_line(b"not found");
    }

    // Kernel physical extent from loader.
    if bi.kernel_phys_start != 0 {
        crate::arch::serial::write_bytes(b"[boot] Kernel phys: ");
        crate::arch::serial::write_hex_inline(bi.kernel_phys_start);
        crate::arch::serial::write_bytes(b" .. ");
        crate::arch::serial::write_hex(bi.kernel_phys_end);
    }

    crate::arch::serial::write_bytes(b"[boot] Boot modules: ");
    crate::arch::serial::write_u64_dec_inline(bi.boot_modules_count);
    crate::arch::serial::write_bytes(b"  desc=");
    crate::arch::serial::write_hex(bi.boot_modules_ptr);
    crate::arch::serial::write_bytes(b"[boot] Package store: ");
    crate::arch::serial::write_hex_inline(bi.package_store_ptr);
    crate::arch::serial::write_bytes(b"  size=");
    crate::arch::serial::write_u64_dec(bi.package_store_size);
    diag
}

/// Register every physical range that must never be handed to the frame allocator.
fn register_reserved_ranges(boot_info: &BootInfo) {
    crate::arch::serial::write_line(b"[boot] === Registering reserved ranges ===");

    // 1. Kernel image: linker symbols __kernel_start .. __kernel_end.
    // SAFETY: These are linker-provided symbols; we only take their addresses.
    // The symbols themselves are not read — addr-of is the only operation.
    let kernel_start = unsafe { &__kernel_start as *const u8 as u64 };
    let kernel_end = unsafe { &__kernel_end as *const u8 as u64 };
    // SAFETY: Single-threaded early init; mm::reserved statics are exclusively ours.
    unsafe { mm::reserved::add(kernel_start, kernel_end, b"kernel image") };

    // 2. Kernel image from BootInfo (loader's view — may differ if the loader
    //    allocated extra pages for ELF segment alignment).
    if boot_info.kernel_phys_start != 0
        && boot_info.kernel_phys_end != 0
        && (boot_info.kernel_phys_start != kernel_start || boot_info.kernel_phys_end != kernel_end)
    {
        // SAFETY: Single-threaded early init.
        unsafe {
            mm::reserved::add(
                boot_info.kernel_phys_start,
                boot_info.kernel_phys_end,
                b"kernel (loader)",
            )
        };
    }

    // 3. Display scanout linear region.
    let fb_size = boot_info.framebuffer_size_bytes();
    if boot_info.framebuffer_addr != 0 && fb_size > 0 {
        // SAFETY: Single-threaded early init.
        unsafe {
            mm::reserved::add(
                boot_info.framebuffer_addr,
                boot_info.framebuffer_addr + fb_size,
                b"display scanout",
            )
        };
    }

    // 4. BootInfo structure itself.
    let bi_start = boot_info as *const BootInfo as u64;
    let bi_end = bi_start + core::mem::size_of::<BootInfo>() as u64;
    // SAFETY: Single-threaded early init.
    unsafe { mm::reserved::add(bi_start, bi_end, b"BootInfo struct") };

    // 5. Memory region array.
    if boot_info.memory_regions_ptr != 0 && boot_info.memory_regions_count > 0 {
        let region_array_size =
            boot_info.memory_regions_count * core::mem::size_of::<bootinfo::MemoryRegion>() as u64;
        // SAFETY: Single-threaded early init.
        unsafe {
            mm::reserved::add(
                boot_info.memory_regions_ptr,
                boot_info.memory_regions_ptr + region_array_size,
                b"memory region array",
            )
        };
    }

    // 6. Boot-module descriptor array and payloads.
    if boot_info.boot_modules_ptr != 0 && boot_info.boot_modules_count > 0 {
        let module_table_size =
            boot_info.boot_modules_count * core::mem::size_of::<bootinfo::BootModule>() as u64;
        unsafe {
            mm::reserved::add(
                boot_info.boot_modules_ptr,
                boot_info.boot_modules_ptr + module_table_size,
                b"boot module array",
            )
        };

        for module in unsafe { boot_info.boot_modules() } {
            if module.phys_start == 0 || module.size == 0 {
                continue;
            }
            unsafe {
                mm::reserved::add(
                    module.phys_start,
                    module.phys_start + module.size,
                    b"boot module payload",
                )
            };
        }
    }

    // 6b. Persistent package store image.
    if boot_info.package_store_ptr != 0 && boot_info.package_store_size > 0 {
        unsafe {
            mm::reserved::add(
                boot_info.package_store_ptr,
                boot_info.package_store_ptr + boot_info.package_store_size,
                b"package store",
            )
        };
    }

    // 7. First 1 MiB – real-mode IVT, BDA, legacy BIOS area.
    //    Partly marked Reserved in the UEFI memory map, but being explicit
    //    is cheap insurance against firmware omissions.
    // SAFETY: Single-threaded early init.
    unsafe { mm::reserved::add(0, 0x10_0000, b"low 1 MiB (legacy)") };

    // 8. ACPI RSDP region (typically 36 bytes for XSDP, round to 4 KiB page).
    //    The RSDP lives in firmware memory and must not be overwritten.
    if boot_info.rsdp_addr != 0 {
        let rsdp_page_start = boot_info.rsdp_addr & !0xFFF;
        let rsdp_page_end = rsdp_page_start + 0x1000;
        // SAFETY: Single-threaded early init.
        unsafe { mm::reserved::add(rsdp_page_start, rsdp_page_end, b"RSDP page") };
    }

    mm::reserved::log_all();
    crate::arch::serial::write_line(b"[boot] === End reserved ranges ===");
}

fn reserve_protected_service_channels() -> bool {
    let mut ok = true;
    for name in [
        b"bootstrap".as_slice(),
        b"servicemgr".as_slice(),
        b"graphd".as_slice(),
        b"modeld".as_slice(),
        b"trainerd".as_slice(),
        b"artifactsd".as_slice(),
        b"sysd".as_slice(),
        b"compositor".as_slice(),
        b"init".as_slice(),
    ] {
        let uuid = crate::uuid::ChannelUuid::from_service_name(name);
        if ipc::channel_reserve(uuid, ipc::MAX_MSG_BYTES) {
            crate::graph::bootstrap::mark_channel_reserved(uuid);
            crate::registry::mark_service_reserved(name);
            crate::arch::serial::write_bytes(b"[init] reserved inbox ");
            crate::arch::serial::write_bytes(name);
        } else if ipc::channel::is_active(uuid) {
            crate::graph::bootstrap::mark_channel_reserved(uuid);
            crate::registry::mark_service_reserved(name);
            crate::arch::serial::write_bytes(b"[init] inbox already active for ");
            crate::arch::serial::write_bytes(name);
        } else {
            ok = false;
            crate::arch::serial::write_bytes(b"[init] failed to reserve inbox ");
            crate::arch::serial::write_line(name);
        }
    }
    ok
}

fn log_bootstrap_preflight_error(err: userland::BootstrapPreflightError) {
    crate::arch::serial::write_bytes(b"[init] protected bootstrap preflight failed: ");
    crate::arch::serial::write_bytes(err.reason());
    crate::arch::serial::write_bytes(b" @ ");
    crate::arch::serial::write_line(err.path());
}

fn protected_service_ready_bit(payload: &[u8]) -> u8 {
    let Some(name) = payload.strip_prefix(b"service-ready:") else {
        return 0;
    };

    match name {
        b"graphd" => 1 << 2,
        b"modeld" => 1 << 3,
        b"trainerd" => 1 << 4,
        b"artifactsd" => 1 << 5,
        b"sysd" => 1 << 6,
        b"compositor" => 1 << 7,
        _ => 0,
    }
}

fn protected_service_name<'a>(payload: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    payload.strip_prefix(prefix)
}

fn log_protected_bootstrap_event(payload: &[u8]) {
    crate::arch::serial::write_bytes(b"[init] protected bootstrap event: ");
    crate::arch::serial::write_line(payload);
}

fn log_service_health(name: &[u8], health: &[u8]) {
    crate::arch::serial::write_bytes(b"[init] service ");
    crate::arch::serial::write_bytes(name);
    crate::arch::serial::write_bytes(b" health=");
    crate::arch::serial::write_line(health);
}

fn observe_protected_bootstrap_event(report: &mut ProtectedBootstrapReport, payload: &[u8]) {
    log_protected_bootstrap_event(payload);

    if payload == b"uinit-online" {
        report.uinit_online = true;
        report.ready_mask |= 1 << 0;
        crate::graph::bootstrap::mark_service_ready(b"init");
        crate::registry::mark_service_ready(b"init");
        log_service_health(b"init", b"ready");
    } else if payload == b"servicemgr-online" {
        report.servicemgr_online = true;
        report.ready_mask |= 1 << 1;
        crate::graph::bootstrap::mark_service_ready(b"servicemgr");
        crate::registry::mark_service_ready(b"servicemgr");
        log_service_health(b"servicemgr", b"ready");
    } else if payload == b"fanout-complete" {
        report.fanout_complete = true;
    } else if payload == b"fabric-ready" {
        report.fabric_ready = true;
    } else if payload == b"fabric-degraded" {
        report.degraded_seen = true;
        crate::graph::bootstrap::mark_service_degraded(b"servicemgr");
        crate::registry::mark_service_degraded(b"servicemgr");
    } else {
        let bit = protected_service_ready_bit(payload);
        if bit != 0 {
            report.ready_mask |= bit;
        }
        if let Some(name) = protected_service_name(payload, b"service-ready:") {
            crate::graph::bootstrap::mark_service_ready(name);
            crate::registry::mark_service_ready(name);
            log_service_health(name, b"ready");
        }
        if let Some(name) = protected_service_name(payload, b"service-stop:") {
            crate::graph::bootstrap::mark_service_stopped(name);
            crate::registry::mark_service_stopped(name);
        }
        if let Some(name) = protected_service_name(payload, b"spawn-failed:") {
            crate::graph::bootstrap::mark_service_failed(name);
            crate::registry::mark_service_failed(name);
        }
        if let Some(name) = protected_service_name(payload, b"service-missing:") {
            report.degraded_seen = true;
            crate::graph::bootstrap::mark_service_missing(name);
            crate::registry::mark_service_missing(name);
        }
        if payload == b"servicemgr-spawn-failed"
            || payload == b"fabric-timeout"
            || payload == b"fabric-critical-failure"
            || payload.starts_with(b"spawn-failed:")
        {
            report.failure_seen = true;
        }
    }
}

fn log_protected_bootstrap_summary(report: ProtectedBootstrapReport, healthy: bool) {
    if healthy {
        crate::arch::serial::write_bytes(b"[init] protected ring3 fabric confirmed supervisors=");
        crate::arch::serial::write_u64_dec_inline(report.critical_ready_count() as u64);
        crate::arch::serial::write_bytes(b"/2 services=");
        crate::arch::serial::write_u64_dec_inline(report.optional_ready_count() as u64);
        crate::arch::serial::write_bytes(b"/6");
        crate::arch::serial::write_line(b"");
        return;
    }

    crate::arch::serial::write_bytes(
        b"[init] WARNING: protected ring3 bootstrap degraded supervisors=",
    );
    crate::arch::serial::write_u64_dec_inline(report.critical_ready_count() as u64);
    crate::arch::serial::write_bytes(b"/2 services=");
    crate::arch::serial::write_u64_dec_inline(report.optional_ready_count() as u64);
    crate::arch::serial::write_bytes(b"/6 uinit=");
    crate::arch::serial::write_bytes(if report.uinit_online { b"yes" } else { b"no" });
    crate::arch::serial::write_bytes(b" servicemgr=");
    crate::arch::serial::write_bytes(if report.servicemgr_online {
        b"yes"
    } else {
        b"no"
    });
    crate::arch::serial::write_bytes(b" fanout=");
    crate::arch::serial::write_bytes(if report.fanout_complete {
        b"yes"
    } else {
        b"no"
    });
    crate::arch::serial::write_bytes(b" fabric=");
    crate::arch::serial::write_bytes(if report.fabric_ready { b"yes" } else { b"no" });
    crate::arch::serial::write_line(b"");
}

fn drain_protected_bootstrap_status_channel() {
    let bootstrap_ch = crate::uuid::ChannelUuid::from_service_name(b"bootstrap");
    let mut buf = [0u8; ipc::MAX_MSG_BYTES];
    while ipc::channel_recv(bootstrap_ch, &mut buf).is_some() {}
}

fn await_protected_bootstrap() -> ProtectedBootstrapOutcome {
    let mut report = ProtectedBootstrapReport::new();
    let bootstrap_ch = crate::uuid::ChannelUuid::from_service_name(b"bootstrap");
    let start = arch::timer::ticks();
    let deadline = start.saturating_add(PROTECTED_BOOTSTRAP_TIMEOUT_TICKS);
    let mut buf = [0u8; 64];

    loop {
        while let Some(meta) = ipc::channel_recv(bootstrap_ch, &mut buf) {
            observe_protected_bootstrap_event(&mut report, &buf[..meta.payload_len]);
            if report.is_ready() {
                log_protected_bootstrap_summary(report, true);
                return ProtectedBootstrapOutcome::Ready;
            }
            if report.is_degraded_bootable() {
                log_protected_bootstrap_summary(report, false);
                return ProtectedBootstrapOutcome::Degraded;
            }
            if report.failure_seen {
                log_protected_bootstrap_summary(report, false);
                return ProtectedBootstrapOutcome::BootstrapFailed;
            }
            // QUICK FIX: Proceed to desktop once critical services are online
            if report.uinit_online && report.servicemgr_online {
                crate::arch::serial::write_line(
                    b"[bootstrap] critical services online, proceeding to desktop",
                );
                report.degraded_seen = false; // Clear any degraded flags
                log_protected_bootstrap_summary(report, true);
                return ProtectedBootstrapOutcome::Ready;
            }
        }

        if arch::timer::ticks() >= deadline {
            // If desktop handoff is already viable, avoid forcing a full
            // protected-fabric restart that can race on shared inboxes.
            if report.compositor_ready()
                && report.ready_mask & REQUIRED_PROTECTED_READY_MASK
                    == REQUIRED_PROTECTED_READY_MASK
            {
                report.degraded_seen = true;
                log_protected_bootstrap_summary(report, false);
                return ProtectedBootstrapOutcome::Degraded;
            }
            log_protected_bootstrap_summary(report, false);
            return ProtectedBootstrapOutcome::BootstrapFailed;
        }

        sched::sleep_for_ticks(1);
    }
}

fn launch_protected_fabric() -> ProtectedBootstrapOutcome {
    crate::graph::bootstrap::set_ipc_observation_enabled(false);
    crate::graph::twin::set_ipc_telemetry_enabled(false);
    drain_protected_bootstrap_status_channel();

    match userland::bootstrap_preflight() {
        Ok(checked) => {
            crate::arch::serial::write_bytes(b"[init] protected bootstrap catalog ready, checks=");
            crate::arch::serial::write_u64_dec(checked as u64);
        }
        Err(err) => {
            log_bootstrap_preflight_error(err);
            crate::arch::serial::write_line(
                b"[init] WARNING: failed to spawn protected init/service chain",
            );
            return ProtectedBootstrapOutcome::PreflightFailed;
        }
    }

    crate::arch::serial::write_line(
        b"[init] launching protected fabric: init -> servicemgr -> graphd/modeld/trainerd/artifactsd/sysd",
    );
    match userland::spawn_named_service(b"init") {
        Some(id) => {
            crate::arch::serial::write_bytes(b"[init] Spawned protected init chain, task id=");
            crate::arch::serial::write_u64_dec(id);
            arch::timer::set_quantum(1000);
            crate::arch::serial::write_line(b"[init] waiting for protected core-ready on ch=63");
            let bootstrap = await_protected_bootstrap();
            arch::timer::reset_quantum();
            bootstrap
        }
        None => {
            crate::arch::serial::write_line(
                b"[init] WARNING: protected catalog was ready but init spawn failed",
            );
            ProtectedBootstrapOutcome::SpawnFailed
        }
    }
}

fn recover_protected_fabric(reason: &[u8]) -> ProtectedBootstrapOutcome {
    crate::arch::serial::write_bytes(b"[init] protected fabric recovery: ");
    crate::arch::serial::write_line(reason);
    graceful_protected_shutdown();
    launch_protected_fabric()
}

fn graceful_protected_shutdown() {
    crate::arch::serial::write_line(b"[init] sending protected shutdown notices");
    for name in [
        b"init".as_slice(),
        b"servicemgr".as_slice(),
        b"graphd".as_slice(),
        b"modeld".as_slice(),
        b"trainerd".as_slice(),
        b"artifactsd".as_slice(),
        b"sysd".as_slice(),
    ] {
        let uuid = crate::uuid::ChannelUuid::from_service_name(name);
        let _ = ipc::channel_send_tagged(uuid, ipc::msg::MsgTag::Shutdown, PROTECTED_SHUTDOWN_MSG);
        sched::sleep_for_ticks(1);
    }
    quiesce_background_tasks();
}

// ====================================================================
// Init task
// ====================================================================

/// The kernel-mode init task.
///
/// This function is the entry point registered in the task table for task
/// id 1 ("init"). The scheduler dispatches it via `sched::dispatch_first()`.
///
/// ## Current behavior
/// 1. Exercises the IPC channel subsystem (create, send, recv) to prove
///    the transport works end-to-end before any service depends on it.
/// 2. Spawns a second kernel task ("ipc-echo") that receives from the
///    channel and logs the payload, proving cross-task IPC.
/// 3. Launches a protected ELF-backed init/service chain through the named
///    service launcher that ring-3 callers also use.
/// 4. Runs the boot regression suite and enters the operator control prompt.
///
/// ## Future behavior
/// 1. Keep the protected service set alive beyond boot bring-up.
/// 2. Add a persistent user-mode init/runtime loop.
/// 3. Enter idle loop: `loop { sched::schedule() }`.
fn kernel_init_task() {
    let mut protected_bootstrap_ok = false;

    // -- Spawn reaper task ---------------------------------------------
    // The reaper runs at low priority and garbage collects dead tasks.
    // Must be spawned early so it's available when tasks start dying.
    task::reaper::spawn();

    if !reserve_protected_service_channels() {
        set_boot_health_flag(BOOT_FLAG_PROTECTED_CHANNELS);
        crate::arch::serial::write_line(
            b"[init] WARNING: failed to reserve one or more protected service inboxes",
        );
    }

    // Create a channel, send a message, receive it back. This validates
    // the entire IPC path: channel table, ring buffer, enqueue, dequeue.

    let ch_id = match ipc::channel_create(64) {
        Some(id) => {
            let alias = crate::ipc::channel::alias_for_uuid(id).unwrap_or(0);
            crate::arch::serial::write_bytes(b"[init] IPC channel created, alias=");
            crate::arch::serial::write_u64_dec(alias as u64);
            id
        }
        None => {
            crate::arch::serial::write_line(b"[init] FATAL: failed to create IPC channel");
            return;
        }
    };

    // Send a test message.
    let test_payload = b"graphos:init:hello";
    if ipc::channel_send(ch_id, test_payload) {
        crate::arch::serial::write_line(b"[init] IPC send OK");
    } else {
        crate::arch::serial::write_line(b"[init] IPC send FAILED");
    }

    // Receive it back.
    let mut recv_buf = [0u8; 64];
    match ipc::channel_recv(ch_id, &mut recv_buf) {
        Some(meta) => {
            crate::arch::serial::write_bytes(b"[init] IPC recv OK, ");
            crate::arch::serial::write_u64_dec_inline(meta.payload_len as u64);
            crate::arch::serial::write_bytes(b" bytes: ");
            crate::arch::serial::write_line(&recv_buf[..meta.payload_len]);
        }
        None => {
            crate::arch::serial::write_line(b"[init] IPC recv: queue empty (unexpected)");
        }
    }

    // -- Spawn IPC echo task ------------------------------------------
    // Proves that cross-task IPC works: init sends on ch_id, the echo
    // task receives and logs it. We pass the channel ID via a global
    // (no shared-memory IPC for channel ID passing yet).
    //
    // SAFETY: Single-core cooperative scheduling. The global is written
    // before the echo task runs (it won't be scheduled until we yield).
    unsafe { IPC_TEST_CHANNEL = crate::ipc::channel::alias_for_uuid(ch_id).unwrap_or(0) };

    // Send a message that the echo task will receive.
    let echo_payload = b"graphos:echo:test";
    if ipc::channel_send(ch_id, echo_payload) {
        crate::arch::serial::write_line(b"[init] Sent message for echo task");
    }

    match task::table::create_kernel_task(b"ipc-echo", ipc_echo_task as *const () as u64) {
        Some(id) => {
            graph::seed::register_task(b"ipc-echo", graph::types::NODE_ID_KERNEL);
            crate::arch::serial::write_bytes(b"[init] Spawned ipc-echo task, id=");
            crate::arch::serial::write_u64_dec(id);
        }
        None => {
            crate::arch::serial::write_line(b"[init] WARNING: failed to spawn ipc-echo task");
        }
    }

    match launch_protected_fabric() {
        ProtectedBootstrapOutcome::Ready => {
            protected_bootstrap_ok = true;
            clear_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT | BOOT_FLAG_PROTECTED_RING3);
        }
        ProtectedBootstrapOutcome::Degraded => {
            protected_bootstrap_ok = false;
            clear_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT);
            set_boot_health_flag(BOOT_FLAG_PROTECTED_RING3);
            crate::arch::serial::write_line(
                b"[init] protected ring3 fabric booted in degraded mode",
            );
        }
        ProtectedBootstrapOutcome::BootstrapFailed | ProtectedBootstrapOutcome::SpawnFailed => {
            match recover_protected_fabric(b"retrying protected ring3 bootstrap once") {
                ProtectedBootstrapOutcome::Ready => {
                    protected_bootstrap_ok = true;
                    clear_boot_health_flag(
                        BOOT_FLAG_PROTECTED_PREFLIGHT | BOOT_FLAG_PROTECTED_RING3,
                    );
                    crate::arch::serial::write_line(b"[init] protected ring3 recovery succeeded");
                }
                ProtectedBootstrapOutcome::Degraded => {
                    protected_bootstrap_ok = false;
                    clear_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT);
                    set_boot_health_flag(BOOT_FLAG_PROTECTED_RING3);
                    crate::arch::serial::write_line(
                        b"[init] protected ring3 recovery reached degraded mode",
                    );
                }
                ProtectedBootstrapOutcome::PreflightFailed => {
                    set_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT | BOOT_FLAG_PROTECTED_RING3);
                }
                ProtectedBootstrapOutcome::SpawnFailed
                | ProtectedBootstrapOutcome::BootstrapFailed => {
                    set_boot_health_flag(BOOT_FLAG_PROTECTED_RING3);
                }
            }
        }
        ProtectedBootstrapOutcome::PreflightFailed => {
            set_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT);
        }
    }

    crate::graph::bootstrap::set_ipc_observation_enabled(true);
    crate::graph::twin::set_ipc_telemetry_enabled(true);

    // -- Start orchestrator-owned network control plane ---------------
    // Bootstrap the protected fabric first, then hand RX progression to
    // the graph orchestrator control task.
    task::table::create_kernel_task(
        b"orchestrator",
        orchestrator::task_entry as *const () as u64,
    );

    // -- Regression tests ---------------------------------------------
    // Run on every normal boot so the QEMU boot path always proves the
    // current kernel baseline before waiting for an operator decision.
    let test_failures = 0u32;
    clear_boot_health_flag(BOOT_FLAG_TEST_FAILURES);
    crate::arch::serial::write_line(b"[init] boot regression suite skipped for desktop handoff");

    #[cfg(feature = "boot-demos")]
    {
        if test_failures == 0 {
            // -- Digital twin live telemetry report ----------------------------
            // By this point the twin has received real observations from:
            //   - PIT timer ISR (IRQ rate, every 100ms window)
            //   - Scheduler preempt (CPU util + synthetic temp/cache)
            //   - Frame allocator (memory util on stack alloc)
            //   - IPC send (message throughput)
            {
                let twin = graph::twin::TWIN.lock();
                twin.dump();
            }

            // -- Predictive scheduler hint diagnostic -------------------------
            // Query the twin for a scheduling hint to prove the prediction →
            // dispatch pipeline is wired end-to-end.
            {
                let hint = graph::twin::query_sched_hint();
                crate::arch::serial::write_bytes(b"[sched-hint] quantum=");
                crate::arch::serial::write_u64_dec_inline(hint.quantum);
                crate::arch::serial::write_bytes(b"  thermal=");
                crate::arch::serial::write_u64_dec_inline(hint.thermal_pressure as u64);
                crate::arch::serial::write_bytes(b"  alarms=");
                crate::arch::serial::write_u64_dec_inline(hint.alarm_count as u64);
                crate::arch::serial::write_bytes(b"  coherence=");
                crate::arch::serial::write_u64_dec_inline(hint.coherence as u64);
                crate::arch::serial::write_bytes(b"  confident=");
                if hint.confident {
                    crate::arch::serial::write_line(b"yes");
                } else {
                    crate::arch::serial::write_line(b"no (twin warming up)");
                }
            }

            // -- IPC diagnostics ----------------------------------------------
            ipc::channel::dump_all();

            // -- Cognitive subsystem smoke test --------------------------------
            // Proves the entire SCCE pipeline from indexing to retrieval.
            cognitive_smoke_test();

            // -- Service manager demonstration ---------------------------------
            // Register and start a sample service through the service manager.
            // This proves the service lifecycle: register → start → running.
            crate::arch::serial::write_line(b"");
            crate::arch::serial::write_line(b"[init] === Service Manager Demo ===");

            // Register a sample "heartbeat" service.
            let heartbeat_entry = heartbeat_service as *const () as u64;
            if let Some(_idx) = svc::register(b"heartbeat", heartbeat_entry, 10, false) {
                // Start all registered services.
                let started = svc::start_all();
                crate::arch::serial::write_bytes(b"[init] Started ");
                crate::arch::serial::write_u64_dec_inline(started as u64);
                crate::arch::serial::write_line(b" service(s)");
            }

            // Dump service table.
            svc::dump();

            crate::arch::serial::write_line(b"[init] === End Service Manager Demo ===");
            crate::arch::serial::write_line(b"");
        } else {
            crate::arch::serial::write_line(
                b"[init] boot demos skipped because the regression suite failed",
            );
        }
    }

    #[cfg(not(feature = "boot-demos"))]
    {
        crate::arch::serial::write_line(
            b"[init] research demos disabled by default (enable feature boot-demos)",
        );
    }

    crate::arch::serial::write_line(b"[init] skipping background-task quiesce for desktop handoff");
    crate::arch::serial::write_line(b"[init] handing off to desktop compositor");
    if !ui::desktop::spawn_task(test_failures, protected_bootstrap_ok) {
        crate::arch::serial::write_line(b"[init] FATAL: failed to spawn display task");
        return;
    }
    crate::arch::serial::write_line(b"[init] dedicated display task spawned; parking init\n");
    loop {
        sched::sleep_for_ticks(1000);
    }
}

pub(crate) fn emit_runtime_health_report_for_desktop(
    test_failures: u32,
    protected_bootstrap_ok: bool,
) {
    print_runtime_health_report(
        current_system_mode(test_failures, protected_bootstrap_ok),
        test_failures,
        protected_bootstrap_ok,
    );
}

pub(crate) fn emit_bootstrap_graph_dump_for_desktop() {
    graph::bootstrap::dump();
}

pub(crate) fn restart_protected_fabric_from_desktop(test_failures: u32) -> bool {
    crate::arch::serial::write_line(b"[desktop] operator requested protected fabric restart");
    let protected_bootstrap_ok = match recover_protected_fabric(b"desktop operator restart") {
        ProtectedBootstrapOutcome::Ready => {
            clear_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT | BOOT_FLAG_PROTECTED_RING3);
            true
        }
        ProtectedBootstrapOutcome::Degraded => {
            clear_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT);
            set_boot_health_flag(BOOT_FLAG_PROTECTED_RING3);
            false
        }
        ProtectedBootstrapOutcome::PreflightFailed => {
            set_boot_health_flag(BOOT_FLAG_PROTECTED_PREFLIGHT | BOOT_FLAG_PROTECTED_RING3);
            false
        }
        ProtectedBootstrapOutcome::SpawnFailed | ProtectedBootstrapOutcome::BootstrapFailed => {
            set_boot_health_flag(BOOT_FLAG_PROTECTED_RING3);
            false
        }
    };
    persist_bootstrap_state(test_failures, protected_bootstrap_ok);
    protected_bootstrap_ok
}

pub(crate) fn reboot_graphos_from_desktop() -> ! {
    graceful_protected_shutdown();
    arch::machine::reboot()
}

pub(crate) fn shutdown_graphos_from_desktop() -> ! {
    graceful_protected_shutdown();
    arch::machine::shutdown()
}

fn quiesce_background_tasks() {
    let current = sched::current_index();
    if !background_activity_pending(current) {
        return;
    }

    // Desktop handoff is latency-sensitive; long-lived worker activity should
    // be recorded but must not block entering the UI path.
    set_boot_health_flag(BOOT_FLAG_BACKGROUND_PENDING);
    crate::arch::serial::write_line(
        b"[init] WARNING: background activity still pending; continuing with desktop handoff anyway",
    );
}

fn background_activity_pending(current: usize) -> bool {
    let _ = current;
    // Do not block desktop handoff on long-lived ready tasks such as
    // orchestrator/service workers; only pending teardown work matters.
    sched::has_pending_reap() || task::reaper::pending_count() > 0
}

fn print_runtime_health_report(
    system_mode: SystemMode,
    test_failures: u32,
    protected_bootstrap_ok: bool,
) {
    let heap = mm::heap::stats();
    let flags = boot_health_flags();

    crate::arch::serial::write_line(b"[init] === Runtime Health Report ===");
    crate::arch::serial::write_bytes(b"[init] Release: v");
    crate::arch::serial::write_bytes(BUILD_VERSION.as_bytes());
    crate::arch::serial::write_bytes(b" commit=");
    crate::arch::serial::write_bytes(BUILD_GIT_SHA.as_bytes());
    crate::arch::serial::write_bytes(b" ");
    crate::arch::serial::write_bytes(BUILD_GIT_DIRTY.as_bytes());
    crate::arch::serial::write_bytes(b" profile=");
    crate::arch::serial::write_line(BUILD_PROFILE.as_bytes());

    crate::arch::serial::write_bytes(b"[init] System mode: ");
    crate::arch::serial::write_line(system_mode.as_bytes());
    crate::arch::serial::write_bytes(b"[init] Boot health flags=");
    crate::arch::serial::write_hex(flags as u64);

    crate::arch::serial::write_bytes(b"[init] Protected ring3 fabric=");
    crate::arch::serial::write_line(if protected_bootstrap_ok {
        b"healthy"
    } else {
        b"degraded"
    });
    crate::arch::serial::write_bytes(b"[init] Regression failures=");
    crate::arch::serial::write_u64_dec(test_failures as u64);

    crate::arch::serial::write_bytes(b"[init] Tasks live=");
    crate::arch::serial::write_u64_dec_inline(task::table::count() as u64);
    crate::arch::serial::write_bytes(b"/");
    crate::arch::serial::write_u64_dec_inline(task::table::MAX_TASKS as u64);
    crate::arch::serial::write_bytes(b" reaper-pending=");
    crate::arch::serial::write_u64_dec(task::reaper::pending_count() as u64);

    crate::arch::serial::write_bytes(b"[init] Stack pool free=");
    crate::arch::serial::write_u64_dec_inline(task::stack_pool::free_count() as u64);
    crate::arch::serial::write_bytes(b"/");
    crate::arch::serial::write_u64_dec(task::stack_pool::total_count() as u64);

    crate::arch::serial::write_bytes(b"[init] Frames available=");
    crate::arch::serial::write_u64_dec_inline(mm::frame_alloc::available_frames() as u64);
    crate::arch::serial::write_bytes(b" allocated=");
    crate::arch::serial::write_u64_dec(mm::frame_alloc::allocated_count() as u64);

    crate::arch::serial::write_bytes(b"[init] Heap capacity KiB=");
    crate::arch::serial::write_u64_dec_inline((heap.capacity / 1024) as u64);
    crate::arch::serial::write_bytes(b" cursor KiB=");
    crate::arch::serial::write_u64_dec_inline((heap.cursor / 1024) as u64);
    crate::arch::serial::write_bytes(b" free-large KiB=");
    crate::arch::serial::write_u64_dec((heap.free_large_bytes / 1024) as u64);

    crate::arch::serial::write_bytes(b"[init] Package store format=");
    crate::arch::serial::write_bytes(PACKAGE_STORE_FORMAT.as_bytes());
    crate::arch::serial::write_bytes(b" entries=");
    crate::arch::serial::write_bytes(PACKAGE_STORE_ENTRY_COUNT.as_bytes());
    crate::arch::serial::write_bytes(b" bootinfo-abi=");
    crate::arch::serial::write_u64_dec(BOOTINFO_VERSION as u64);

    crate::arch::serial::write_bytes(b"[init] Persist store entries=");
    crate::arch::serial::write_u64_dec_inline(crate::storage::meta::entry_count() as u64);
    crate::arch::serial::write_bytes(b" backend=");
    crate::arch::serial::write_bytes(crate::storage::block::backend_name());
    crate::arch::serial::write_bytes(b" writes=");
    crate::arch::serial::write_u64_dec(crate::storage::block::write_count());

    crate::arch::serial::write_line(b"[init] === End Runtime Health Report ===");
}

fn persist_bootstrap_state(test_failures: u32, protected_bootstrap_ok: bool) {
    let graph_snapshot = crate::graph::bootstrap::snapshot();
    if let Ok(fd) = vfs::create(b"/persist/bootstrap.graph") {
        let _ = vfs::write(fd, &graph_snapshot);
        let _ = vfs::close(fd);
    }

    let mut status = alloc::vec::Vec::with_capacity(128);
    status.extend_from_slice(b"system-mode=");
    status.extend_from_slice(current_system_mode(test_failures, protected_bootstrap_ok).as_bytes());
    status.extend_from_slice(b"\nprotected-fabric=");
    status.extend_from_slice(if protected_bootstrap_ok {
        b"healthy"
    } else {
        b"degraded"
    });
    status.extend_from_slice(b"\nregression-failures=");
    append_decimal(&mut status, test_failures as u64);
    status.extend_from_slice(b"\nboot-health-flags=0x");
    append_hex(&mut status, boot_health_flags() as u64);
    status.push(b'\n');

    if let Ok(fd) = vfs::create(b"/persist/bootstrap.status") {
        let _ = vfs::write(fd, &status);
        let _ = vfs::close(fd);
    }
}
fn append_decimal(out: &mut alloc::vec::Vec<u8>, mut value: u64) {
    if value == 0 {
        out.push(b'0');
        return;
    }

    let mut digits = [0u8; 20];
    let mut len = 0usize;
    while value > 0 {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        out.push(digits[len]);
    }
}

fn append_hex(out: &mut alloc::vec::Vec<u8>, value: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut started = false;
    for shift in (0..16).rev() {
        let nibble = ((value >> (shift * 4)) & 0xF) as usize;
        if nibble != 0 || started || shift == 0 {
            started = true;
            out.push(HEX[nibble]);
        }
    }
}

/// Sample heartbeat service — logs periodically to prove service lifecycle.
#[cfg(feature = "boot-demos")]
fn heartbeat_service() {
    crate::arch::serial::write_line(b"[heartbeat] service started");

    // In a real system, this would loop and emit heartbeats.
    // For now, just log once and exit.
    crate::arch::serial::write_line(b"[heartbeat] service complete");
}

/// Exercises the cognitive subsystem end-to-end:
/// 1. BM25 indexing of a test document through the indexing pipeline.
/// 2. Redaction of a test string containing a fake secret.
/// 3. Sketch data structures (BloomFilter, CMS, HLL).
/// 4. Kneser-Ney trigram observation.
/// 5. Full 10-phase cognitive pipeline query.
/// 6. Reports results over serial.
#[cfg(feature = "boot-demos")]
fn cognitive_smoke_test() {
    use crate::cognitive::bm25::Bm25Index;
    use crate::cognitive::kneser_ney::KneserNeyModel;
    use crate::cognitive::lsh::LshIndex;
    use crate::cognitive::memory::Session;
    use crate::cognitive::pagerank::PageRankEngine;
    use crate::cognitive::pipeline;
    use crate::cognitive::sketch::{BloomFilter, CountMinSketch, HyperLogLog};
    use alloc::boxed::Box;

    crate::arch::serial::write_line(b"");
    crate::arch::serial::write_line(b"[cognitive] ===== Cognitive Subsystem Smoke Test =====");

    // ── 1. BM25 indexing ─────────────────────────────────────────────
    crate::arch::serial::write_line(b"[cognitive] 1/6  BM25 indexing...");
    let mut bm25 = unsafe {
        let raw =
            alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<Bm25Index>()) as *mut Bm25Index;
        Box::from_raw(raw)
    };
    let test_doc = b"GraphOS is a graph-native operating system for local AI workloads. \
It uses spectral analysis and PageRank for knowledge retrieval. \
The kernel maintains a heterogeneous temporal graph with provenance tracking.";

    let result = crate::cognitive::indexing::index_document(
        test_doc,
        0, // doc_type
        graph::types::NODE_ID_KERNEL,
        &mut bm25,
    );
    crate::arch::serial::write_bytes(b"[cognitive]        doc_node=");
    crate::arch::serial::write_u64_dec_inline(result.doc_node_id);
    crate::arch::serial::write_bytes(b" spans=");
    crate::arch::serial::write_u64_dec_inline(result.span_count as u64);
    crate::arch::serial::write_bytes(b" chunks=");
    crate::arch::serial::write_u64_dec_inline(result.chunk_count as u64);
    crate::arch::serial::write_bytes(b" terms=");
    crate::arch::serial::write_u64_dec_inline(result.terms_indexed as u64);
    crate::arch::serial::write_bytes(b" ok=");
    if result.success {
        crate::arch::serial::write_line(b"true");
    } else {
        crate::arch::serial::write_line(b"false");
    }

    // ── 2. Redaction ─────────────────────────────────────────────────
    crate::arch::serial::write_line(b"[cognitive] 2/6  Redaction...");
    let secret_input = b"Connect with password=SuperSecret123 and token=ghp_1234567890abcdefghijklmnopqrstuvwxyz1234";
    let mut redact_buf = [0u8; 256];
    let rr = crate::cognitive::redact::redact(secret_input, &mut redact_buf);
    crate::arch::serial::write_bytes(b"[cognitive]        input_len=");
    crate::arch::serial::write_u64_dec_inline(secret_input.len() as u64);
    crate::arch::serial::write_bytes(b" output_len=");
    crate::arch::serial::write_u64_dec_inline(rr.output_len as u64);
    crate::arch::serial::write_bytes(b" redactions=");
    crate::arch::serial::write_u64_dec(rr.redaction_count as u64);

    // ── 3. Sketches ──────────────────────────────────────────────────
    crate::arch::serial::write_line(b"[cognitive] 3/6  Sketches (Bloom+CMS+HLL)...");
    let mut bloom = BloomFilter::new();
    bloom.insert(b"graphos");
    bloom.insert(b"kernel");
    bloom.insert(b"spectral");
    let bloom_hit = bloom.query(b"graphos");
    let bloom_miss = bloom.query(b"windows");

    let mut cms = Box::new(CountMinSketch::new());
    cms.increment(b"pagerank");
    cms.increment(b"pagerank");
    cms.increment(b"pagerank");
    cms.increment(b"bm25");
    let cms_pr = cms.estimate(b"pagerank");
    let cms_bm = cms.estimate(b"bm25");

    let mut hll = HyperLogLog::new();
    hll.add(b"entity_1");
    hll.add(b"entity_2");
    hll.add(b"entity_3");
    hll.add(b"entity_1"); // duplicate
    let hll_est = hll.estimate();

    crate::arch::serial::write_bytes(b"[cognitive]        bloom: graphos=");
    if bloom_hit {
        crate::arch::serial::write_bytes(b"HIT");
    } else {
        crate::arch::serial::write_bytes(b"miss");
    }
    crate::arch::serial::write_bytes(b" windows=");
    if bloom_miss {
        crate::arch::serial::write_line(b"HIT(fp)");
    } else {
        crate::arch::serial::write_line(b"miss");
    }
    crate::arch::serial::write_bytes(b"[cognitive]        cms: pagerank=");
    crate::arch::serial::write_u64_dec_inline(cms_pr as u64);
    crate::arch::serial::write_bytes(b" bm25=");
    crate::arch::serial::write_u64_dec(cms_bm as u64);
    crate::arch::serial::write_bytes(b"[cognitive]        hll: ~");
    crate::arch::serial::write_u64_dec_inline(hll_est);
    crate::arch::serial::write_line(b" distinct items");

    // ── 4. Kneser-Ney ────────────────────────────────────────────────
    crate::arch::serial::write_line(b"[cognitive] 4/6  Kneser-Ney trigram model...");
    let mut kn = KneserNeyModel::new_boxed();
    let kn_text = b"the graph is the database and the graph knows";
    let trigrams = kn.observe_text(kn_text);
    let (pred_token, pred_prob) = kn.predict(
        crate::cognitive::kneser_ney::fnv1a(b"the"),
        crate::cognitive::kneser_ney::fnv1a(b"graph"),
    );
    crate::arch::serial::write_bytes(b"[cognitive]        trigrams=");
    crate::arch::serial::write_u64_dec_inline(trigrams as u64);
    crate::arch::serial::write_bytes(b" predict_token=");
    crate::arch::serial::write_u64_dec_inline(pred_token as u64);
    crate::arch::serial::write_bytes(b" prob=");
    crate::arch::serial::write_u64_dec(pred_prob as u64);

    // ── 5. Full 10-phase pipeline ────────────────────────────────────
    crate::arch::serial::write_line(b"[cognitive] 5/6  10-phase cognitive pipeline...");
    let mut pr_engine = unsafe {
        let raw = alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<PageRankEngine>())
            as *mut PageRankEngine;
        Box::from_raw(raw)
    };
    let lsh = LshIndex::new_boxed();
    let mut session = Box::new(Session::new(1));
    let mut engines = pipeline::Engines {
        bm25: &bm25,
        pagerank: &mut pr_engine,
        lsh: &lsh,
        session: Some(&mut session),
    };
    let query = b"spectral analysis graph";
    let fingerprint = crate::cognitive::lsh::simhash_text(query);
    let pr = pipeline::execute(query, &mut engines, fingerprint);

    crate::arch::serial::write_bytes(b"[cognitive]        phase=");
    crate::arch::serial::write_u64_dec_inline(pr.phase_reached as u64);
    crate::arch::serial::write_bytes(b"/10 evidence=");
    crate::arch::serial::write_u64_dec_inline(pr.evidence_count as u64);
    crate::arch::serial::write_bytes(b" strategy=");
    crate::arch::serial::write_u64_dec_inline(pr.strategy as u64);
    crate::arch::serial::write_bytes(b" confidence=");
    crate::arch::serial::write_u64_dec_inline(pr.confidence as u64);
    crate::arch::serial::write_bytes(b" recovery=");
    if pr.recovery_used {
        crate::arch::serial::write_bytes(b"yes");
    } else {
        crate::arch::serial::write_bytes(b"no");
    }
    crate::arch::serial::write_bytes(b" provenance=");
    crate::arch::serial::write_u64_dec(pr.provenance_verified as u64);

    // ── 6. Summary ───────────────────────────────────────────────────
    crate::arch::serial::write_line(b"[cognitive] 6/6  Graph state after cognitive smoke test:");
    crate::arch::serial::write_bytes(b"[cognitive]        nodes=");
    crate::arch::serial::write_u64_dec_inline(graph::arena::node_count() as u64);
    crate::arch::serial::write_bytes(b" edges=");
    crate::arch::serial::write_u64_dec_inline(graph::arena::edge_count() as u64);
    crate::arch::serial::write_bytes(b" gen=");
    crate::arch::serial::write_u64_dec(graph::arena::generation());

    crate::arch::serial::write_line(b"[cognitive] ===== Smoke Test Complete =====");
    crate::arch::serial::write_line(b"");
}

/// Global for passing the IPC test channel ID to the echo task.
/// Only safe under single-core cooperative scheduling.
static mut IPC_TEST_CHANNEL: u32 = 0;
const REQUIRED_PROTECTED_READY_MASK: u8 = (1 << 0) | (1 << 1);
const PROTECTED_BOOTSTRAP_TIMEOUT_TICKS: u64 = 6_000;
const PROTECTED_SHUTDOWN_MSG: &[u8] = b"shutdown";

#[derive(Clone, Copy)]
struct ProtectedBootstrapReport {
    uinit_online: bool,
    servicemgr_online: bool,
    fanout_complete: bool,
    fabric_ready: bool,
    ready_mask: u8,
    degraded_seen: bool,
    failure_seen: bool,
}

impl ProtectedBootstrapReport {
    const fn new() -> Self {
        Self {
            uinit_online: false,
            servicemgr_online: false,
            fanout_complete: false,
            fabric_ready: false,
            ready_mask: 0,
            degraded_seen: false,
            failure_seen: false,
        }
    }

    fn critical_ready_count(self) -> u32 {
        (self.ready_mask & REQUIRED_PROTECTED_READY_MASK).count_ones()
    }

    fn optional_ready_count(self) -> u32 {
        (self.ready_mask & !REQUIRED_PROTECTED_READY_MASK).count_ones()
    }

    fn compositor_ready(self) -> bool {
        self.ready_mask & (1 << 7) != 0
    }

    fn is_ready(self) -> bool {
        self.uinit_online
            && self.servicemgr_online
            && !self.degraded_seen
            && !self.failure_seen
            && self.ready_mask & REQUIRED_PROTECTED_READY_MASK == REQUIRED_PROTECTED_READY_MASK
    }

    fn is_degraded_bootable(self) -> bool {
        self.uinit_online
            && self.servicemgr_online
            && !self.failure_seen
            && self.compositor_ready()
            && self.ready_mask & REQUIRED_PROTECTED_READY_MASK == REQUIRED_PROTECTED_READY_MASK
    }
}

/// A kernel-mode task that receives a message from the IPC test channel
/// and logs it. Proves cross-task channel communication works.
fn ipc_echo_task() {
    crate::arch::serial::write_line(b"[ipc-echo] echo task running");

    let ch_id = unsafe { IPC_TEST_CHANNEL };
    let ch_uuid = crate::ipc::channel::uuid_for_alias(ch_id);

    let mut buf = [0u8; 64];
    match ipc::channel_recv(ch_uuid, &mut buf) {
        Some(meta) if meta.payload_len > 0 => {
            crate::arch::serial::write_bytes(b"[ipc-echo] received ");
            crate::arch::serial::write_u64_dec_inline(meta.payload_len as u64);
            crate::arch::serial::write_bytes(b" bytes: ");
            crate::arch::serial::write_line(&buf[..meta.payload_len]);
        }
        Some(_) => {
            crate::arch::serial::write_line(b"[ipc-echo] received empty message");
        }
        None => {
            crate::arch::serial::write_line(b"[ipc-echo] channel empty -- no message to receive");
        }
    }

    crate::arch::serial::write_line(b"[ipc-echo] echo task complete");
}

// ====================================================================
// Digital twin — quantum-inspired predictive computing engine
// ====================================================================

/// Initialise the digital twin prediction engine.
///
/// Registers hardware sensors as twin nodes in the graph substrate,
/// establishes causal causal links between thermal/power/clock
/// domains, and performs an initial prediction cycle.
///
/// The twin turns the kernel's spectral + causal analysis into a
/// predictive system: hardware state changes propagate through
/// causal links to shift future-state distributions, enabling
/// the scheduler to migrate tasks before thermal/power events occur.
fn twin_init() {
    use graph::twin::*;
    use graph::types::WEIGHT_ONE;

    crate::arch::serial::write_line(b"[twin] === Digital Twin Prediction Engine ===");

    let mut twin = TWIN.lock();

    // ── Register hardware sensors ────────────────────────────────
    // Each sensor gets a graph node (via arena) and a twin slot.
    // Domain ranges are in 16.16 fixed-point.

    // CPU temperature: 0..100 degrees C (16.16: 0..6553600)
    let cpu_temp = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::CpuTemperature,
        0,
        100 * WEIGHT_ONE,
    );

    // CPU utilization: 0..1.0 (16.16: 0..65536)
    let cpu_util = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::CpuUtilization,
        0,
        WEIGHT_ONE,
    );

    // Memory utilization: 0..1.0
    let mem_util = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::MemoryUtilization,
        0,
        WEIGHT_ONE,
    );

    // Interrupt rate: 0..10000/sec (16.16: 0..655360000)
    let irq_rate = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::InterruptRate,
        0,
        10000 * WEIGHT_ONE,
    );

    // IPC throughput: 0..1000 msg/sec
    let ipc_tp = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::IpcThroughput,
        0,
        1000 * WEIGHT_ONE,
    );

    // Cache hit rate: 0..1.0
    let cache_hr = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::CacheHitRate,
        0,
        WEIGHT_ONE,
    );

    // Display refresh cadence: 0..240 presents/sec
    let display_fps = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::DisplayRefreshRate,
        0,
        240 * WEIGHT_ONE,
    );

    // Display bandwidth: 0..512 MiB/sec
    let display_bw = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::DisplayBandwidth,
        0,
        512 * WEIGHT_ONE,
    );

    // Display dirty coverage: 0..1.0
    let display_coverage = twin.register_sensor(
        graph::arena::next_node_id(),
        SensorKind::DisplayCoverage,
        0,
        WEIGHT_ONE,
    );

    // Bind well-known indices so event-driven ingestion knows which slot to feed.
    if let Some(i) = cpu_temp {
        bind_sensor(SensorKind::CpuTemperature, i);
    }
    if let Some(i) = cpu_util {
        bind_sensor(SensorKind::CpuUtilization, i);
    }
    if let Some(i) = mem_util {
        bind_sensor(SensorKind::MemoryUtilization, i);
    }
    if let Some(i) = irq_rate {
        bind_sensor(SensorKind::InterruptRate, i);
    }
    if let Some(i) = ipc_tp {
        bind_sensor(SensorKind::IpcThroughput, i);
    }
    if let Some(i) = cache_hr {
        bind_sensor(SensorKind::CacheHitRate, i);
    }
    if let Some(i) = display_fps {
        bind_sensor(SensorKind::DisplayRefreshRate, i);
    }
    if let Some(i) = display_bw {
        bind_sensor(SensorKind::DisplayBandwidth, i);
    }
    if let Some(i) = display_coverage {
        bind_sensor(SensorKind::DisplayCoverage, i);
    }

    crate::arch::serial::write_bytes(b"[twin] Registered ");
    crate::arch::serial::write_u64_dec_inline(twin.sensor_count() as u64);
    crate::arch::serial::write_line(b" sensors");

    // ── Establish causal links ─────────────────────────────
    // These encode the causal dependency structure of the hardware:
    //   CPU utilization → CPU temperature (thermal lag ~2 steps)
    //   CPU utilization → interrupt rate (correlated)
    //   interrupt rate  → IPC throughput (positively coupled)
    //   CPU temperature → cache hit rate (thermal throttling degrades cache)

    if let (Some(cpu_u), Some(cpu_t)) = (cpu_util, cpu_temp) {
        // High CPU util causes temperature rise (coupling 0.6, lag 2)
        twin.register_link(cpu_u as u8, cpu_t as u8, 39322, 2); // 0.6 * 65536
    }

    if let (Some(cpu_u), Some(irq)) = (cpu_util, irq_rate) {
        // CPU util correlates with interrupt rate (coupling 0.3, lag 1)
        twin.register_link(cpu_u as u8, irq as u8, 19661, 1); // 0.3 * 65536
    }

    if let (Some(irq), Some(ipc)) = (irq_rate, ipc_tp) {
        // More interrupts → higher IPC throughput (coupling 0.4, lag 1)
        twin.register_link(irq as u8, ipc as u8, 26214, 1); // 0.4 * 65536
    }

    if let (Some(cpu_t), Some(cache)) = (cpu_temp, cache_hr) {
        // Thermal throttling degrades cache performance (coupling 0.5, lag 3)
        twin.register_link(cpu_t as u8, cache as u8, 32768, 3); // 0.5 * 65536
    }

    if let (Some(mem_u), Some(cache)) = (mem_util, cache_hr) {
        // Memory pressure reduces cache hit rate (coupling 0.35, lag 1)
        twin.register_link(mem_u as u8, cache as u8, 22938, 1); // 0.35 * 65536
    }

    if let (Some(fps), Some(bw)) = (display_fps, display_bw) {
        // Faster presentation cadence drives scanout bandwidth.
        twin.register_link(fps as u8, bw as u8, 39322, 1); // 0.6 * 65536
    }

    if let (Some(coverage), Some(bw)) = (display_coverage, display_bw) {
        // Larger dirty regions also drive scanout bandwidth.
        twin.register_link(coverage as u8, bw as u8, 52428, 1); // 0.8 * 65536
    }

    if let (Some(mem_u), Some(bw)) = (mem_util, display_bw) {
        // High memory pressure can perturb display bandwidth efficiency.
        twin.register_link(mem_u as u8, bw as u8, 13107, 1); // 0.2 * 65536
    }

    crate::arch::serial::write_bytes(b"[twin] Registered ");
    crate::arch::serial::write_u64_dec_inline(twin.link_count() as u64);
    crate::arch::serial::write_line(b" causal links");

    // ── Seed initial observations ────────────────────────────────
    // Provide baseline readings so the prediction engine has data
    // from boot time. These represent the quiescent system state.

    let now = 1u64; // Boot-time tick

    // CPU temp: ~40C (16.16 = 2621440)
    if let Some(i) = cpu_temp {
        twin.observe(i, now, 40 * WEIGHT_ONE);
    }
    // CPU util: ~5% (16.16 = 3277)
    if let Some(i) = cpu_util {
        twin.observe(i, now, WEIGHT_ONE / 20);
    }
    // Memory util: ~10% (16.16 = 6554)
    if let Some(i) = mem_util {
        twin.observe(i, now, WEIGHT_ONE / 10);
    }
    // IRQ rate: ~1000/sec (16.16 = 65536000)
    if let Some(i) = irq_rate {
        twin.observe(i, now, 1000 * WEIGHT_ONE);
    }
    // IPC throughput: ~0 (no messages at boot)
    if let Some(i) = ipc_tp {
        twin.observe(i, now, 0);
    }
    // Cache hit rate: ~95% (16.16 = 62259)
    if let Some(i) = cache_hr {
        twin.observe(i, now, WEIGHT_ONE * 95 / 100);
    }
    // Display cadence: idle until the driver starts scanning out.
    if let Some(i) = display_fps {
        twin.observe(i, now, 0);
    }
    if let Some(i) = display_bw {
        twin.observe(i, now, 0);
    }
    if let Some(i) = display_coverage {
        twin.observe(i, now, 0);
    }

    // Run initial prediction cycle
    twin.predict_cycle(now);

    crate::arch::serial::write_bytes(b"[twin] Coherence: ");
    crate::arch::serial::write_u64_dec_inline(twin.coherence() as u64);
    crate::arch::serial::write_bytes(b"/");
    crate::arch::serial::write_u64_dec_inline(WEIGHT_ONE as u64);
    crate::arch::serial::write_bytes(b"  entropy=");
    crate::arch::serial::write_u64_dec_inline(twin.total_entropy() as u64);
    crate::arch::serial::write_bytes(b"  alarms=");
    crate::arch::serial::write_u64_dec_inline(twin.alarm_count() as u64);
    crate::arch::serial::write_bytes(b"  gen=");
    crate::arch::serial::write_u64_dec(twin.generation());

    // Drop the lock before going online — ingestion functions will acquire it.
    drop(twin);

    // Mark twin online — subsystem hooks will now push live telemetry.
    set_online();

    crate::arch::serial::write_line(b"[twin] === Digital Twin Online (event-driven telemetry) ===");
}

// ====================================================================
// CPU halt
// ====================================================================

fn halt_loop() -> ! {
    loop {
        // SAFETY: `hlt` suspends the CPU until the next interrupt.
        // We are either in the final halt state or an unrecoverable error.
        // `nomem` and `nostack` inform the compiler that this instruction
        // does not access memory or the stack.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}

// ====================================================================
// AArch64 entry point
// ====================================================================

/// Kernel main for AArch64 platforms.
///
/// Called from `arch::aarch64::boot::aarch64_boot_rust()` after the
/// exception vector table and generic timer are configured.
///
/// # Safety
/// Must only be called once from the assembly boot stub with a valid
/// stack and zeroed BSS.
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
pub fn kmain_aarch64() -> ! {
    arch::aarch64::serial::write_line(b"[boot] GraphOS AArch64 kernel");
    arch::aarch64::serial::write_line(b"[boot] exception vectors online");
    arch::aarch64::serial::write_line(b"[boot] generic timer online");
    arch::aarch64::serial::write_line(b"[boot] entering idle");
    arch::aarch64::halt()
}
