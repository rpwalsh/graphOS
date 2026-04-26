// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use x86_64::{PrivilegeLevel, VirtAddr};

use crate::arch::x86_64::{pic, serial, timer};
use crate::sched;

static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

const TIMER_VEC: usize = pic::PIC1_OFFSET as usize;
const KEYBOARD_VEC: usize = pic::PIC1_OFFSET as usize + 1;
const PCI_IRQ5_VEC: usize = pic::PIC1_OFFSET as usize + 5;
const PCI_IRQ9_VEC: usize = pic::PIC2_OFFSET as usize + 1;
const PCI_IRQ10_VEC: usize = pic::PIC2_OFFSET as usize + 2;
const PCI_IRQ11_VEC: usize = pic::PIC2_OFFSET as usize + 3;
const MOUSE_IRQ12_VEC: usize = pic::PIC2_OFFSET as usize + 4;
const SPURIOUS_MASTER_VEC: usize = pic::PIC1_OFFSET as usize + 7;
const SPURIOUS_SLAVE_VEC: usize = pic::PIC2_OFFSET as usize + 7;
const USER_SYSCALL_VEC: usize = 0x80;

pub fn init() {
    unsafe {
        let idt = &mut *core::ptr::addr_of_mut!(IDT);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(super::gdt::DOUBLE_FAULT_IST_INDEX);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.general_protection_fault
            .set_handler_fn(general_protection_handler);
        idt.stack_segment_fault
            .set_handler_fn(stack_segment_handler);
        idt[TIMER_VEC as u8].set_handler_fn(timer_interrupt_handler);
        idt[KEYBOARD_VEC as u8].set_handler_fn(keyboard_interrupt_handler);
        idt[PCI_IRQ5_VEC as u8].set_handler_fn(pci_irq5_handler);
        idt[PCI_IRQ9_VEC as u8].set_handler_fn(pci_irq9_handler);
        idt[PCI_IRQ10_VEC as u8].set_handler_fn(pci_irq10_handler);
        idt[PCI_IRQ11_VEC as u8].set_handler_fn(pci_irq11_handler);
        idt[MOUSE_IRQ12_VEC as u8].set_handler_fn(mouse_irq12_handler);
        idt[SPURIOUS_MASTER_VEC as u8].set_handler_fn(spurious_master_handler);
        idt[SPURIOUS_SLAVE_VEC as u8].set_handler_fn(spurious_slave_handler);
        idt[USER_SYSCALL_VEC as u8]
            .set_handler_addr(VirtAddr::new(super::ring3::int80_entry_addr()))
            .set_privilege_level(PrivilegeLevel::Ring3);
        idt.load();
    }

    serial::write_line(
        b"[idt] IDT loaded: exceptions + IRQ 0 (timer), 1 (kbd), 5/9/10/11/12 (pci+mouse), 7/15 (spurious), 0x80 (ring3 trap)",
    );
}

extern "x86-interrupt" fn breakpoint_handler(frame: InterruptStackFrame) {
    serial::write_line(b"[exc] Breakpoint");
    serial::write_hex(frame.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn double_fault_handler(frame: InterruptStackFrame, _error_code: u64) -> ! {
    serial::write_line(b"[exc] DOUBLE FAULT");
    serial::write_hex(frame.instruction_pointer.as_u64());
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

extern "x86-interrupt" fn page_fault_handler(
    frame: InterruptStackFrame,
    error_code: x86_64::structures::idt::PageFaultErrorCode,
) {
    let fault_addr: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) fault_addr, options(nomem, nostack));
    }

    let current = sched::current_index();
    if current != 0
        && crate::task::table::is_user_task(current)
        && crate::task::table::handle_user_page_fault(current, fault_addr, error_code.bits())
    {
        return;
    }

    serial::write_line(b"[exc] PAGE FAULT");
    serial::write_hex(frame.instruction_pointer.as_u64());
    serial::write_bytes(b"  cr2=");
    serial::write_hex_inline(fault_addr);
    serial::write_bytes(b"  error=");
    serial::write_hex(error_code.bits());
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

extern "x86-interrupt" fn invalid_opcode_handler(frame: InterruptStackFrame) {
    serial::write_line(b"[exc] INVALID OPCODE");
    serial::write_hex(frame.instruction_pointer.as_u64());
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

extern "x86-interrupt" fn general_protection_handler(frame: InterruptStackFrame, error_code: u64) {
    serial::write_line(b"[exc] GENERAL PROTECTION FAULT");
    serial::write_hex(frame.instruction_pointer.as_u64());
    serial::write_bytes(b"  error=");
    serial::write_hex(error_code);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

extern "x86-interrupt" fn stack_segment_handler(frame: InterruptStackFrame, error_code: u64) {
    serial::write_line(b"[exc] STACK-SEGMENT FAULT");
    serial::write_hex(frame.instruction_pointer.as_u64());
    serial::write_bytes(b"  error=");
    serial::write_hex(error_code);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(_frame: InterruptStackFrame) {
    let is_ap_timer = sched::percpu::is_active() && sched::percpu::current_cpu_index() != 0;

    let should_preempt = if is_ap_timer {
        true
    } else {
        let should_preempt = timer::tick();
        // Advance timed-sleep expirations on every BSP tick (before EOI so any
        // newly-ready task can preempt in the same handler invocation).
        sched::tick_advance();
        // Watchdog check: runs every 500 ticks (~0.5 s) to amortise lock cost.
        // watchdog_check() uses try_lock internally — safe to call in ISR context.
        let t = timer::ticks();
        if t.is_multiple_of(500) {
            crate::svc::watchdog_check();
            crate::svc::drain_restart_queue();
        }
        // Token-based watchdog: check every 1 000 ticks (~1 s).
        // Converts PIT tick count to milliseconds (1 tick ≈ 1 ms).
        if t.is_multiple_of(1000) {
            let now_ms = t; // 1 PIT tick ≈ 1 ms
            crate::watchdog::tick(now_ms, |task_id| {
                // Kill the expired task and queue its service for restart.
                // If the service is critical this will trigger a hardware reboot.
                if let Some(idx) = crate::task::table::task_index_by_id(task_id) {
                    crate::task::table::mark_dead(idx);
                }
                crate::svc::queue_restart_by_task_id(task_id);
            });

            // Liveness heartbeat: surfaces the current task and per-sid commit
            // totals once per second so a render-side stall is visible without
            // relying on capped per-call traces.
            heartbeat_trace(t);
        }
        crate::ui::desktop::pump_frame_clock_from_timer(t);
        // Poll virtio input queue every tick as IRQ delivery fallback.
        // Wrapped in kernel address space because notify MMIO is above 512 MiB.
        crate::mm::page_table::with_kernel_address_space(|| {
            crate::input::pointer::poll_input();
        });
        should_preempt
    };

    // BSP: PIC delivered IRQ0 — only PIC EOI is needed.
    // AP:  LAPIC delivered vector 32 — only LAPIC EOI is needed.
    if is_ap_timer {
        crate::arch::x86_64::lapic::eoi();
    } else {
        unsafe { pic::end_of_interrupt(0) };
    }
    if should_preempt {
        unsafe { sched::preempt() };
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_frame: InterruptStackFrame) {
    let scancode: u8;
    unsafe {
        core::arch::asm!(
            "in al, dx",
            in("dx") 0x60u16,
            out("al") scancode,
            options(nomem, nostack, preserves_flags),
        );
    }
    super::keyboard::push_scancode(scancode);
    unsafe { pic::end_of_interrupt(1) };
}

extern "x86-interrupt" fn pci_irq5_handler(_frame: InterruptStackFrame) {
    dispatch_pci_irq(5);
}

extern "x86-interrupt" fn pci_irq9_handler(_frame: InterruptStackFrame) {
    dispatch_pci_irq(9);
}

extern "x86-interrupt" fn pci_irq10_handler(_frame: InterruptStackFrame) {
    dispatch_pci_irq(10);
}

extern "x86-interrupt" fn pci_irq11_handler(_frame: InterruptStackFrame) {
    dispatch_pci_irq(11);
}

extern "x86-interrupt" fn mouse_irq12_handler(_frame: InterruptStackFrame) {
    dispatch_pci_irq(12);
}

extern "x86-interrupt" fn spurious_master_handler(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn spurious_slave_handler(_frame: InterruptStackFrame) {
    unsafe { pic::end_of_interrupt(0) };
}

fn dispatch_pci_irq(irq: u8) {
    let handled = crate::mm::page_table::with_kernel_address_space(|| {
        let pointer_handled = crate::input::pointer::handle_irq(irq);
        let driver_handled = crate::drivers::dispatch_irq(irq);
        pointer_handled || driver_handled
    });

    unsafe { pic::end_of_interrupt(irq) };

    if handled {
        unsafe { sched::preempt() };
    }
}

/// Periodic liveness trace.  Called once per second from the BSP timer ISR.
///
/// Emits a single line containing:
///   - current scheduler tick
///   - current task index
///   - per-surface commit totals for the first few surfaces (sid 0..7)
///
/// This survives even when the per-call commit log budget is exhausted, so
/// a render-side stall is visible at one-second granularity instead of
/// silently disappearing after the 64-line cap.
fn heartbeat_trace(now_ms: u64) {
    let mut totals = [0usize; crate::syscall::SURFACE_COMMIT_TOTAL_SLOTS];
    crate::syscall::surface_commit_totals_snapshot(&mut totals);
    let stats = crate::syscall::surface_commit_stats_snapshot();
    let pointer = crate::input::router::pointer_route_stats_snapshot();
    let display_idx = crate::sched::desktop_task_index();
    let compositor_idx = crate::syscall::COMPOSITOR_TASK_INDEX.load(core::sync::atomic::Ordering::Acquire);
    let display_state = if display_idx == usize::MAX {
        b'-'
    } else {
        crate::task::table::state_code(display_idx)
    };
    let compositor_state = if compositor_idx == usize::MAX {
        b'-'
    } else {
        crate::task::table::state_code(compositor_idx)
    };

    serial::write_bytes(b"[hb] t=");
    serial::write_u64_dec_inline(now_ms);
    serial::write_bytes(b"ms cur=");
    serial::write_u64_dec_inline(crate::sched::current_index() as u64);
    serial::write_bytes(b" disp=");
    serial::write_bytes(&[display_state]);
    serial::write_bytes(b" comp=");
    serial::write_bytes(&[compositor_state]);
    serial::write_bytes(b" commits sid1=");
    serial::write_u64_dec_inline(totals[1] as u64);
    serial::write_bytes(b" sid2=");
    serial::write_u64_dec_inline(totals[2] as u64);
    serial::write_bytes(b" sid3=");
    serial::write_u64_dec_inline(totals[3] as u64);
    serial::write_bytes(b" attempts=");
    serial::write_u64_dec_inline(stats.attempts as u64);
    serial::write_bytes(b" fail_owner=");
    serial::write_u64_dec_inline(stats.fail_owner as u64);
    serial::write_bytes(b" fail_missing=");
    serial::write_u64_dec_inline(stats.fail_missing as u64);
    serial::write_bytes(b" fail_present=");
    serial::write_u64_dec_inline(stats.fail_present as u64);
    serial::write_bytes(b" ptr_total=");
    serial::write_u64_dec_inline(pointer.total as u64);
    serial::write_bytes(b" ptr_comp=");
    serial::write_u64_dec_inline(pointer.to_compositor as u64);
    serial::write_bytes(b" ptr_win=");
    serial::write_u64_dec_inline(pointer.to_window as u64);
    serial::write_bytes(b"\n");
}
