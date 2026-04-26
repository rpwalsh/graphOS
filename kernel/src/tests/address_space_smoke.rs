// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Regression tests for early user-address-space bring-up.

use crate::arch::interrupts;
use crate::diag;
use crate::mm::{address_space, frame_alloc, page_table};

pub fn run_tests() -> u32 {
    let mut failures = 0;
    if !test_address_space_bootstrap() {
        failures += 1;
    }
    if !test_elf_loader_and_stack_growth() {
        failures += 1;
    }
    if !test_file_mmap_and_cow() {
        failures += 1;
    }
    if !test_anon_mmap_and_swap() {
        failures += 1;
    }
    if !test_kernel_irq_cr3_guard() {
        failures += 1;
    }
    failures
}

fn test_address_space_bootstrap() -> bool {
    let kernel_pml4 = page_table::active_pml4();
    if kernel_pml4 == 0 {
        diag::test_fail(b"addrspace: kernel CR3 not active");
        return false;
    }

    let frames_before = frame_alloc::available_frames();

    let ok = {
        let mut space = match address_space::AddressSpace::new(kernel_pml4) {
            Some(space) => space,
            None => {
                diag::test_fail(b"addrspace: failed to allocate new space");
                return false;
            }
        };

        let image = [0x90u8, 0x90, 0xC3];
        let Some((entry, pages)) = space.load_user_image(&image) else {
            diag::test_fail(b"addrspace: failed to map user image");
            return false;
        };

        if !space.map_user_stack() {
            diag::test_fail(b"addrspace: failed to map user stack");
            return false;
        }

        entry == address_space::USER_CODE_START
            && pages == 1
            && space.cr3() != 0
            && space.user_stack_pointer() == address_space::USER_STACK_TOP
            && space.is_mapped(address_space::USER_CODE_START)
            && space.is_mapped(address_space::USER_STACK_BASE)
            && space.is_mapped(address_space::USER_STACK_TOP - 4096)
            && !space.is_mapped(address_space::USER_STACK_GUARD_BASE)
    };

    let frames_after = frame_alloc::available_frames();
    if !ok {
        diag::test_fail(b"addrspace: incorrect mapping state");
        return false;
    }

    if frames_before != frames_after {
        diag::test_fail(b"addrspace: frames leaked on teardown");
        return false;
    }

    diag::test_pass(b"addrspace: bootstrap image + guard stack");
    true
}

fn test_elf_loader_and_stack_growth() -> bool {
    let kernel_pml4 = page_table::active_pml4();
    if kernel_pml4 == 0 {
        diag::test_fail(b"addrspace: kernel CR3 not active for elf");
        return false;
    }

    let frames_before = frame_alloc::available_frames();

    let ok = {
        let mut space = match address_space::AddressSpace::new(kernel_pml4) {
            Some(space) => space,
            None => {
                diag::test_fail(b"addrspace: failed to allocate elf space");
                return false;
            }
        };

        let Some(image) = crate::userland::image_for_named_service(b"graphd") else {
            diag::test_fail(b"addrspace: failed to load protected service elf");
            return false;
        };
        let Some(info) = space.load_elf_image(&image) else {
            diag::test_fail(b"addrspace: failed to load user elf");
            return false;
        };
        if !space.map_user_stack() {
            diag::test_fail(b"addrspace: failed to map demand stack");
            return false;
        }

        let demand_page = address_space::USER_STACK_RESERVED_BASE;
        let grew = space.handle_page_fault(demand_page, 1 << 1);

        info.entry >= address_space::USER_CODE_START
            && info.mapped_end > info.entry
            && space.is_mapped(info.entry)
            && grew
            && space.is_mapped(demand_page)
            && !space.is_mapped(address_space::USER_STACK_GUARD_BASE)
    };

    let frames_after = frame_alloc::available_frames();
    if !ok {
        diag::test_fail(b"addrspace: elf load or stack growth incorrect");
        return false;
    }

    if frames_before != frames_after {
        diag::test_fail(b"addrspace: elf path leaked frames");
        return false;
    }

    diag::test_pass(b"addrspace: elf load + demand stack growth");
    true
}

fn test_file_mmap_and_cow() -> bool {
    let kernel_pml4 = page_table::active_pml4();
    if kernel_pml4 == 0 {
        diag::test_fail(b"addrspace: kernel CR3 not active for mmap");
        return false;
    }

    let frames_before = frame_alloc::available_frames();

    let ok = {
        let mut left = match address_space::AddressSpace::new(kernel_pml4) {
            Some(space) => space,
            None => {
                diag::test_fail(b"addrspace: failed to allocate left mmap space");
                return false;
            }
        };
        let mut right = match address_space::AddressSpace::new(kernel_pml4) {
            Some(space) => space,
            None => {
                diag::test_fail(b"addrspace: failed to allocate right mmap space");
                return false;
            }
        };

        let Some(left_addr) = left.mmap_file(
            b"/pkg/config/services.txt",
            4096,
            address_space::MMAP_PROT_READ | address_space::MMAP_PROT_WRITE,
            address_space::MMAP_FLAG_PRIVATE,
            0,
        ) else {
            diag::test_fail(b"addrspace: failed to create left file mmap");
            return false;
        };
        let Some(right_addr) = right.mmap_file(
            b"/pkg/config/services.txt",
            4096,
            address_space::MMAP_PROT_READ,
            address_space::MMAP_FLAG_PRIVATE,
            0,
        ) else {
            diag::test_fail(b"addrspace: failed to create right file mmap");
            return false;
        };

        let left_fault = left.handle_page_fault(left_addr, 0);
        let right_fault = right.handle_page_fault(right_addr, 0);
        let shared_left = left.mapped_phys(left_addr).unwrap_or(0);
        let shared_right = right.mapped_phys(right_addr).unwrap_or(0);
        let cow_fault = left.handle_page_fault(left_addr, (1 << 0) | (1 << 1));
        let private_left = left.mapped_phys(left_addr).unwrap_or(0);

        left_fault
            && right_fault
            && shared_left != 0
            && shared_left == shared_right
            && cow_fault
            && private_left != 0
            && private_left != shared_left
    };

    let frames_after = frame_alloc::available_frames();
    if !ok {
        diag::test_fail(b"addrspace: file mmap or cow incorrect");
        return false;
    }
    if frames_before != frames_after {
        diag::test_fail(b"addrspace: file mmap path leaked frames");
        return false;
    }

    diag::test_pass(b"addrspace: file mmap + cow");
    true
}

fn test_anon_mmap_and_swap() -> bool {
    let kernel_pml4 = page_table::active_pml4();
    if kernel_pml4 == 0 {
        diag::test_fail(b"addrspace: kernel CR3 not active for swap");
        return false;
    }

    let frames_before = frame_alloc::available_frames();

    let ok = {
        let mut space = match address_space::AddressSpace::new(kernel_pml4) {
            Some(space) => space,
            None => {
                diag::test_fail(b"addrspace: failed to allocate swap space");
                return false;
            }
        };

        let Some(addr) = space.mmap_anon(
            4096,
            address_space::MMAP_PROT_READ | address_space::MMAP_PROT_WRITE,
        ) else {
            diag::test_fail(b"addrspace: failed to create anon mmap");
            return false;
        };

        let mapped = space.handle_page_fault(addr, 1 << 1);
        let frame_before_swap = space.mapped_phys(addr).unwrap_or(0);
        if frame_before_swap != 0 {
            unsafe {
                (frame_before_swap as *mut u8).write_volatile(0x5A);
            }
        }

        let evicted = space.evict_one_swappable_page();
        let swapped = !space.is_mapped(addr) && space.is_swapped(addr);
        let restored = space.handle_page_fault(addr, 1 << 1);
        let frame_after_swap = space.mapped_phys(addr).unwrap_or(0);
        let byte_ok = frame_after_swap != 0
            && unsafe { (frame_after_swap as *const u8).read_volatile() } == 0x5A;

        mapped && evicted && swapped && restored && byte_ok
    };

    let frames_after = frame_alloc::available_frames();
    if !ok {
        diag::test_fail(b"addrspace: anon mmap or swap incorrect");
        return false;
    }
    if frames_before != frames_after {
        diag::test_fail(b"addrspace: swap path leaked frames");
        return false;
    }

    diag::test_pass(b"addrspace: anon mmap + swap reload");
    true
}

fn test_kernel_irq_cr3_guard() -> bool {
    let kernel_pml4 = page_table::active_pml4();
    if kernel_pml4 == 0 {
        diag::test_fail(b"addrspace: kernel CR3 not active for irq guard");
        return false;
    }

    let frames_before = frame_alloc::available_frames();

    let ok = {
        let space = match address_space::AddressSpace::new(kernel_pml4) {
            Some(space) => space,
            None => {
                diag::test_fail(b"addrspace: failed to allocate irq-guard space");
                return false;
            }
        };

        let user_cr3 = space.cr3();
        if user_cr3 == 0 || user_cr3 == kernel_pml4 {
            diag::test_fail(b"addrspace: invalid user CR3 for irq guard");
            return false;
        }

        interrupts::without_interrupts(|| {
            let starting_root = page_table::current_pml4();
            if starting_root != kernel_pml4 {
                return false;
            }

            unsafe { page_table::load_address_space(user_cr3) };
            let switched_to_user = page_table::current_pml4() == user_cr3;
            let saw_kernel_root =
                page_table::with_kernel_address_space(|| page_table::current_pml4() == kernel_pml4);
            let restored_to_user = page_table::current_pml4() == user_cr3;
            unsafe { page_table::load_address_space(kernel_pml4) };
            let restored_to_kernel = page_table::current_pml4() == kernel_pml4;

            switched_to_user && saw_kernel_root && restored_to_user && restored_to_kernel
        })
    };

    let frames_after = frame_alloc::available_frames();
    if !ok {
        diag::test_fail(b"addrspace: kernel IRQ CR3 guard incorrect");
        return false;
    }
    if frames_before != frames_after {
        diag::test_fail(b"addrspace: irq guard path leaked frames");
        return false;
    }

    diag::test_pass(b"addrspace: kernel IRQ CR3 guard");
    true
}
