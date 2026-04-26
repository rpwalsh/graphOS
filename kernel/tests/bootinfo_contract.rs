// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use graphos_kernel::bootinfo::{BOOTINFO_VERSION, BootInfo, BootInfoDiag, FramebufferFormat};

fn sample_bootinfo() -> BootInfo {
    BootInfo {
        bootinfo_version: BOOTINFO_VERSION,
        bootinfo_size: core::mem::size_of::<BootInfo>() as u32,
        framebuffer_addr: 0xE000_0000,
        framebuffer_width: 1280,
        framebuffer_height: 720,
        framebuffer_stride: 1280,
        framebuffer_format: FramebufferFormat::Bgr,
        memory_regions_ptr: 0x9000,
        memory_regions_count: 4,
        rsdp_addr: 0xF0000,
        kernel_phys_start: 0x10_0000,
        kernel_phys_end: 0x14_0000,
        boot_modules_ptr: 0xA000,
        boot_modules_count: 2,
        package_store_ptr: 0xB000,
        package_store_size: 0x4000,
    }
}

#[test]
fn bootinfo_contract_valid_envelope_has_no_diag_flags() {
    let bootinfo = sample_bootinfo();
    assert!(bootinfo.validate_extended().is_empty());
}

#[test]
fn bootinfo_contract_invalid_envelope_sets_expected_flags() {
    let mut bootinfo = sample_bootinfo();
    bootinfo.framebuffer_width = 0;
    bootinfo.memory_regions_ptr = 0;
    bootinfo.memory_regions_count = 1;

    let diag = bootinfo.validate_extended();
    assert!(diag.contains(BootInfoDiag::FB_ZERO_DIMENSION));
    assert!(diag.contains(BootInfoDiag::NO_MEMORY_MAP));
}
