// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! GraphOS UEFI Stage-1 Loader
//!
//! Responsibilities:
//! 1. Discover GOP framebuffer
//! 2. Read the kernel ELF image (`GRAPHOSK.BIN`) from the EFI System Partition
//! 3. Parse ELF headers and load PT_LOAD segments into physical memory
//! 4. Capture the UEFI memory map
//! 5. Find the ACPI RSDP
//! 6. Construct BootInfo
//! 7. Exit boot services
//! 8. Jump to kernel entry (graphos_kmain)
//!
//! SAFETY: This entire crate runs in UEFI boot-services context with identity-mapped
//! physical memory. After exit_boot_services(), firmware memory may be reclaimed.

#![no_main]
#![no_std]
#![allow(unused_imports)]

extern crate alloc;

use alloc::vec::Vec;
use core::slice;
use log::info;
use object::Endianness;
use object::elf::{FileHeader64, PT_LOAD};
use object::read::elf::{FileHeader as _, ProgramHeader as _};
use uefi::Identify;
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, FileType, RegularFile};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::table::boot::{AllocateType, MemoryMap, MemoryType, SearchType};
use uefi::table::cfg::ACPI2_GUID;

/// Maximum number of memory regions we pass to the kernel.
const MAX_MEMORY_REGIONS: usize = 256;

/// The kernel binary filename on the ESP, as a UCS-2 null-terminated array.
/// Each ASCII character is stored as a u16. UEFI file APIs require UCS-2.
static KERNEL_FILENAME: [u16; 13] = [
    b'G' as u16,
    b'R' as u16,
    b'A' as u16,
    b'P' as u16,
    b'H' as u16,
    b'O' as u16,
    b'S' as u16,
    b'K' as u16,
    b'.' as u16,
    b'B' as u16,
    b'I' as u16,
    b'N' as u16,
    0u16,
];

/// Memory region kind mirroring the kernel's BootInfo ABI.
#[repr(u32)]
#[derive(Clone, Copy)]
enum MemoryRegionKind {
    Usable = 0,
    Reserved = 1,
    AcpiReclaim = 2,
    AcpiNvs = 3,
    Mmio = 4,
}

/// A single memory region in the BootInfo contract.
#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryRegion {
    start: u64,
    length: u64,
    kind: MemoryRegionKind,
    _pad: u32,
}

/// The boot-information structure passed to graphos_kmain.
/// Must remain ABI-compatible with the kernel's definition.
/// Version 5 adds the GOP framebuffer pixel-format handoff.
#[repr(C)]
struct BootInfo {
    bootinfo_version: u32,
    bootinfo_size: u32,
    framebuffer_addr: u64,
    framebuffer_width: u32,
    framebuffer_height: u32,
    framebuffer_stride: u32,
    framebuffer_format: u32,
    memory_regions_ptr: u64,
    memory_regions_count: u64,
    rsdp_addr: u64,
    kernel_phys_start: u64,
    kernel_phys_end: u64,
    boot_modules_ptr: u64,
    boot_modules_count: u64,
    package_store_ptr: u64,
    package_store_size: u64,
}

/// Current BootInfo ABI version — must match the kernel.
const BOOTINFO_VERSION: u32 = 5;

/// Well-known physical address for the BootInfo handoff structure.
/// We place it at 0x90000 (576 KiB) — below the kernel at 1 MiB and above
/// the real-mode IVT/BDA area. This is conventional free memory in UEFI.
const BOOTINFO_PHYS: u64 = 0x90000;

/// Well-known physical address for the memory regions array.
/// Placed at 0x91000 — one page after BootInfo.
const REGIONS_PHYS: u64 = 0x91000;
const PACKAGE_STORE_MAX_ADDR: u64 = 0x3FFF_FFFF;
static PACKAGE_STORE_FILENAME: [u16; 13] = [
    b'G' as u16,
    b'R' as u16,
    b'A' as u16,
    b'P' as u16,
    b'H' as u16,
    b'O' as u16,
    b'S' as u16,
    b'P' as u16,
    b'.' as u16,
    b'P' as u16,
    b'K' as u16,
    b'G' as u16,
    0u16,
];
const PACKAGE_STORE_MAGIC: &[u8; 8] = b"GPKSTORE";
const PACKAGE_STORE_VERSION: u32 = 2;
const PACKAGE_STORE_HEADER_SIZE: usize = 32;
const PACKAGE_STORE_ENTRY_SIZE: usize = 88;

struct KernelFileBuffer {
    ptr: *mut u8,
    len: usize,
    pages: usize,
}

struct PackageStoreInfo {
    version: u32,
    entry_count: u32,
    checksum: u64,
}

impl KernelFileBuffer {
    fn as_slice(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }

    fn len(&self) -> usize {
        self.len
    }
}

#[entry]
fn efi_main(_image: Handle, mut st: SystemTable<Boot>) -> Status {
    uefi::helpers::init(&mut st).expect("Failed to init UEFI helpers");
    let bs = st.boot_services();

    info!("[graphos-loader] GraphOS UEFI Loader starting");

    // ---- Step 1: GOP framebuffer ----
    let (fb_addr, fb_width, fb_height, fb_stride, fb_format) = discover_gop(bs);
    info!(
        "[graphos-loader] GOP: {}x{} stride={} format={} addr={:#x}",
        fb_width,
        fb_height,
        fb_stride,
        pixel_format_name(fb_format),
        fb_addr
    );

    // ---- Step 2: Load kernel ELF from ESP ----
    let kernel_data = load_kernel_from_esp(bs);
    info!(
        "[graphos-loader] Kernel file loaded: {} bytes",
        kernel_data.len()
    );

    let (entry_addr, phys_start, phys_end) = load_elf_segments(bs, kernel_data.as_slice());
    unsafe {
        bs.free_pages(kernel_data.ptr as u64, kernel_data.pages)
            .expect("[graphos-loader] Failed to free kernel staging pages");
    }
    info!(
        "[graphos-loader] ELF loaded: entry={:#x} phys=[{:#x}..{:#x})",
        entry_addr, phys_start, phys_end
    );

    // ---- Step 3: Stage the persistent package store for pkgfs-backed ring 3 ----
    let package_store = load_package_store_from_esp(bs);
    let package_info = inspect_package_store(package_store.as_slice());
    info!(
        "[graphos-loader] Package store staged: {} bytes v{} entries={} checksum={:#x}",
        package_store.len(),
        package_info.version,
        package_info.entry_count,
        package_info.checksum
    );

    // ---- Step 4: Find RSDP ----
    let rsdp = find_rsdp(&st);
    info!("[graphos-loader] RSDP addr: {:#x}", rsdp);

    // ---- Step 4b: Allocate pages for BootInfo, REGIONS, and module descriptors ----
    // We allocate at well-known physical addresses so the data survives
    // exit_boot_services and the kernel knows where to find it.
    // SAFETY: These addresses are below 1 MiB (kernel start), in conventional
    // memory. UEFI will mark them as LOADER_DATA which persists.
    bs.allocate_pages(
        AllocateType::Address(BOOTINFO_PHYS),
        MemoryType::LOADER_DATA,
        1,
    )
    .expect("[graphos-loader] Failed to allocate BootInfo page at 0x90000");
    // Regions array: MAX_MEMORY_REGIONS * 24 bytes = 6144 bytes ~ 2 pages
    let regions_pages =
        (MAX_MEMORY_REGIONS * core::mem::size_of::<MemoryRegion>() + 0xFFF) / 0x1000;
    bs.allocate_pages(
        AllocateType::Address(REGIONS_PHYS),
        MemoryType::LOADER_DATA,
        regions_pages,
    )
    .expect("[graphos-loader] Failed to allocate REGIONS pages at 0x91000");
    info!(
        "[graphos-loader] BootInfo at {:#x}, REGIONS at {:#x}",
        BOOTINFO_PHYS, REGIONS_PHYS
    );

    // ---- Step 5: Memory map + exit boot services ----
    info!("[graphos-loader] Preparing to exit boot services");

    // SAFETY: We are still in boot-services context. exit_boot_services()
    // consumes `st`, internally allocates a buffer of type LOADER_DATA (which
    // survives after exit), retrieves the memory map, and calls the firmware's
    // ExitBootServices. The returned MemoryMap is owned and valid.
    let (_runtime, mmap) = unsafe { st.exit_boot_services(MemoryType::LOADER_DATA) };

    // ---- Step 6: Collect memory regions into the allocated page ----
    let regions_ptr = REGIONS_PHYS as *mut MemoryRegion;
    let region_count = collect_regions_to_ptr(&mmap, regions_ptr);

    // ---- Step 7: Construct BootInfo at the allocated page ----
    // SAFETY: We allocated this page above, it is zeroed by UEFI,
    // and we are single-threaded post-exit_boot_services.
    let bi_ptr = BOOTINFO_PHYS as *mut BootInfo;
    unsafe {
        (*bi_ptr).bootinfo_version = BOOTINFO_VERSION;
        (*bi_ptr).bootinfo_size = core::mem::size_of::<BootInfo>() as u32;
        (*bi_ptr).framebuffer_addr = fb_addr;
        (*bi_ptr).framebuffer_width = fb_width;
        (*bi_ptr).framebuffer_height = fb_height;
        (*bi_ptr).framebuffer_stride = fb_stride;
        (*bi_ptr).framebuffer_format = fb_format;
        (*bi_ptr).memory_regions_ptr = REGIONS_PHYS;
        (*bi_ptr).memory_regions_count = region_count as u64;
        (*bi_ptr).rsdp_addr = rsdp;
        (*bi_ptr).kernel_phys_start = phys_start;
        (*bi_ptr).kernel_phys_end = phys_end;
        (*bi_ptr).boot_modules_ptr = 0;
        (*bi_ptr).boot_modules_count = 0;
        (*bi_ptr).package_store_ptr = package_store.ptr as u64;
        (*bi_ptr).package_store_size = package_store.len as u64;
    }

    // ---- Step 8: Jump to kernel ----
    // SAFETY: entry_addr was read from the ELF e_entry field and the
    // corresponding PT_LOAD segment was loaded at its p_vaddr. The
    // kernel is linked at 1 MiB with identity mapping, so the virtual
    // address equals the physical address.
    //
    // ABI CROSSING: The UEFI loader is compiled for x86_64-unknown-uefi which
    // uses the Microsoft x64 calling convention (first arg in rcx). The kernel
    // is compiled for x86_64-unknown-none with SysV ABI (first arg in rdi).
    // We cannot use a normal Rust function call across this ABI boundary.
    // Instead we use inline assembly to:
    //   1. Load the BootInfo pointer into rdi (SysV first argument register)
    //   2. Jump to the kernel entry point
    // This is a one-way transfer — we never return.
    unsafe {
        core::arch::asm!(
            "mov rdi, {bi}",
            "jmp {entry}",
            bi = in(reg) BOOTINFO_PHYS,
            entry = in(reg) entry_addr,
            options(noreturn)
        );
    }
}

// ====================================================================
// ESP file reading
// ====================================================================

/// Read the kernel binary from the EFI System Partition root directory.
///
/// Returns the entire file contents as a `Vec<u8>`. Panics if the file
/// cannot be found or read — at this stage, there is no graceful fallback.
fn load_kernel_from_esp(bs: &BootServices) -> KernelFileBuffer {
    read_file_from_esp(bs, &KERNEL_FILENAME, "GRAPHOSK.BIN", u64::MAX)
}

fn load_package_store_from_esp(bs: &BootServices) -> KernelFileBuffer {
    read_file_from_esp(
        bs,
        &PACKAGE_STORE_FILENAME,
        "GRAPHOSP.PKG",
        PACKAGE_STORE_MAX_ADDR,
    )
}

fn inspect_package_store(data: &[u8]) -> PackageStoreInfo {
    if data.len() < PACKAGE_STORE_HEADER_SIZE || &data[..8] != PACKAGE_STORE_MAGIC {
        panic!("[graphos-loader] GRAPHOSP.PKG has invalid magic");
    }

    let version = u32::from_le_bytes(
        data[8..12]
            .try_into()
            .expect("[graphos-loader] package version field"),
    );
    if version != PACKAGE_STORE_VERSION {
        panic!("[graphos-loader] GRAPHOSP.PKG has unsupported version");
    }
    let entry_count = u32::from_le_bytes(
        data[12..16]
            .try_into()
            .expect("[graphos-loader] package entry count field"),
    );
    let image_size = u64::from_le_bytes(
        data[16..24]
            .try_into()
            .expect("[graphos-loader] package size field"),
    );
    let checksum = u64::from_le_bytes(
        data[24..32]
            .try_into()
            .expect("[graphos-loader] package checksum field"),
    );
    if image_size as usize != data.len() {
        panic!("[graphos-loader] GRAPHOSP.PKG size header mismatch");
    }
    let table_end = PACKAGE_STORE_HEADER_SIZE
        .checked_add(entry_count as usize * PACKAGE_STORE_ENTRY_SIZE)
        .expect("[graphos-loader] package entry table overflow");
    if table_end > data.len() {
        panic!("[graphos-loader] GRAPHOSP.PKG entry table out of bounds");
    }
    if checksum != package_store_checksum(data) {
        panic!("[graphos-loader] GRAPHOSP.PKG checksum mismatch");
    }

    PackageStoreInfo {
        version,
        entry_count,
        checksum,
    }
}

fn package_store_checksum(data: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    const CHECKSUM_OFFSET: usize = 24;

    let mut hash = OFFSET_BASIS;
    for (idx, &byte) in data.iter().enumerate() {
        let effective = if (CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8).contains(&idx) {
            0
        } else {
            byte
        };
        hash ^= effective as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn read_file_from_esp(
    bs: &BootServices,
    filename: &[u16],
    label: &str,
    max_address: u64,
) -> KernelFileBuffer {
    // Locate the SimpleFileSystem protocol on any handle (typically the ESP).
    let fs_handle = bs
        .locate_handle_buffer(SearchType::ByProtocol(&SimpleFileSystem::GUID))
        .expect("[graphos-loader] No SimpleFileSystem handle — is an ESP present?");

    // SAFETY: We open the first filesystem handle. UEFI guarantees the handle
    // is valid while boot services are active. Exclusive access prevents
    // concurrent opens.
    let mut sfs = bs
        .open_protocol_exclusive::<SimpleFileSystem>(fs_handle[0])
        .expect("[graphos-loader] Failed to open SimpleFileSystem");

    let mut root = sfs
        .open_volume()
        .expect("[graphos-loader] Failed to open ESP root volume");

    // Construct CStr16 from our static UCS-2 array.
    // SAFETY: `filename` is a valid null-terminated UCS-2 string of ASCII characters.
    let name = unsafe { uefi::CStr16::from_u16_with_nul_unchecked(filename) };

    // Open the file from the root of the ESP.
    let file_handle = root
        .open(name, FileMode::Read, FileAttribute::empty())
        .unwrap_or_else(|_| panic!("[graphos-loader] Failed to open {} on ESP", label));

    let mut regular: RegularFile = match file_handle.into_type().unwrap() {
        FileType::Regular(f) => f,
        FileType::Dir(_) => panic!("[graphos-loader] {} is a directory, not a file", label),
    };

    let file_info = regular
        .get_boxed_info::<FileInfo>()
        .unwrap_or_else(|_| panic!("[graphos-loader] Failed to query {} metadata", label));
    let file_size = file_info.file_size() as usize;
    if file_size == 0 {
        panic!("[graphos-loader] {} is empty", label);
    }

    let page_count = file_size.div_ceil(0x1000);
    let staging_phys = bs
        .allocate_pages(
            AllocateType::MaxAddress(max_address),
            MemoryType::LOADER_DATA,
            page_count,
        )
        .unwrap_or_else(|_| {
            panic!(
                "[graphos-loader] Failed to allocate staging pages for {}",
                label
            )
        });

    let buf = unsafe { slice::from_raw_parts_mut(staging_phys as *mut u8, file_size) };
    let mut filled = 0usize;
    while filled < file_size {
        let bytes_read = regular
            .read(&mut buf[filled..file_size])
            .unwrap_or_else(|_| panic!("[graphos-loader] Failed to read {}", label));
        if bytes_read == 0 {
            break;
        }
        filled += bytes_read;
    }

    if filled != file_size {
        panic!("[graphos-loader] Short read for {}", label);
    }

    info!("[graphos-loader] Read {}: {} bytes", label, filled);

    KernelFileBuffer {
        ptr: staging_phys as *mut u8,
        len: file_size,
        pages: page_count,
    }
}

// ====================================================================
// ELF loading
// ====================================================================

/// Parse the ELF file and load PT_LOAD segments into physical memory.
///
/// Returns `(entry_address, kernel_phys_start, kernel_phys_end)`.
///
/// The kernel is linked at a fixed physical base (`linker.ld`), and since UEFI
/// provides identity-mapped physical memory, we allocate pages at the
/// exact physical addresses specified by the ELF's p_paddr fields.
///
/// Strategy: two-pass approach. First pass computes the total physical range
/// across all PT_LOAD segments. We allocate the entire range at once (page-
/// aligned) to handle segments that are not individually page-aligned.
/// Second pass copies segment data.
fn load_elf_segments(bs: &BootServices, data: &[u8]) -> (u64, u64, u64) {
    // Parse ELF64 header using the `object` crate's typed parser.
    let header = FileHeader64::<Endianness>::parse(data)
        .expect("[graphos-loader] Failed to parse ELF64 header");
    let endian = header
        .endian()
        .expect("[graphos-loader] Failed to determine ELF endianness");

    let e_entry = header.e_entry.get(endian);

    // Read program header table.
    let phdrs = header
        .program_headers(endian, data)
        .expect("[graphos-loader] Failed to read ELF program headers");

    info!(
        "[graphos-loader] ELF: entry={:#x} phnum={}",
        e_entry,
        phdrs.len()
    );

    // ---- Pass 1: Collect PT_LOAD ranges and allocate only merged segment spans ----
    let mut phys_start: u64 = u64::MAX;
    let mut phys_end: u64 = 0;
    let mut load_ranges: Vec<(u64, u64)> = Vec::new();

    for phdr in phdrs {
        let p_type = phdr.p_type(endian);
        if p_type != PT_LOAD {
            continue;
        }

        let p_paddr = phdr.p_paddr(endian);
        let p_memsz = phdr.p_memsz(endian) as u64;

        if p_memsz == 0 {
            continue;
        }

        if p_paddr < phys_start {
            phys_start = p_paddr;
        }
        let seg_end = p_paddr + p_memsz;
        if seg_end > phys_end {
            phys_end = seg_end;
        }

        let range_start = p_paddr & !0xFFF;
        let range_end = (seg_end + 0xFFF) & !0xFFF;
        load_ranges.push((range_start, range_end));
    }

    if phys_start == u64::MAX {
        panic!("[graphos-loader] No PT_LOAD segments found in kernel ELF");
    }

    load_ranges.sort_unstable_by_key(|(start, _)| *start);

    let mut merged_ranges: Vec<(u64, u64)> = Vec::new();
    for (start, end) in load_ranges {
        if let Some((_, last_end)) = merged_ranges.last_mut() {
            if start <= *last_end {
                if end > *last_end {
                    *last_end = end;
                }
                continue;
            }
        }
        merged_ranges.push((start, end));
    }

    info!(
        "[graphos-loader] Kernel extent: [{:#x}..{:#x}), merged PT_LOAD ranges={}",
        phys_start,
        phys_end,
        merged_ranges.len()
    );

    for (range_start, range_end) in &merged_ranges {
        let num_pages = ((range_end - range_start) / 0x1000) as usize;
        info!(
            "[graphos-loader]   alloc range [{:#x}..{:#x}) = {} pages",
            range_start, range_end, num_pages
        );

        // Try to claim the range as LOADER_DATA. If the range is already owned
        // by UEFI boot-services code/data (common for the low-memory area the
        // kernel is linked at), fall through and write in-place. UEFI identity-
        // maps all physical memory and boot-services pages become available
        // after ExitBootServices, so overwriting them here is safe.
        let alloc_result = bs.allocate_pages(
            AllocateType::Address(*range_start),
            MemoryType::LOADER_DATA,
            num_pages,
        );
        if let Err(ref e) = alloc_result {
            info!(
                "[graphos-loader]   alloc at {:#x} failed ({:?}), writing in-place (boot-services fallback)",
                range_start,
                e.status()
            );
        }
        // SAFETY: UEFI identity-maps all physical memory. The range is either
        // freshly allocated LOADER_DATA, or pre-existing boot-services memory
        // that is safe to overwrite before ExitBootServices.
        unsafe { zero_phys(*range_start, num_pages * 0x1000) };
    }

    // ---- Pass 2: Copy PT_LOAD segment data ----
    for phdr in phdrs {
        let p_type = phdr.p_type(endian);
        if p_type != PT_LOAD {
            continue;
        }

        let p_paddr = phdr.p_paddr(endian);
        let p_offset = phdr.p_offset(endian) as usize;
        let p_filesz = phdr.p_filesz(endian) as usize;
        let p_memsz = phdr.p_memsz(endian) as usize;

        if p_memsz == 0 {
            continue;
        }

        info!(
            "[graphos-loader]   PT_LOAD: paddr={:#x} filesz={:#x} memsz={:#x}",
            p_paddr, p_filesz, p_memsz
        );

        // Copy file-backed portion of the segment.
        // SAFETY: The destination [p_paddr .. p_paddr + p_filesz) lies within
        // the region we allocated above. UEFI identity maps all physical memory.
        if p_filesz > 0 {
            unsafe { copy_to_phys(data.as_ptr().add(p_offset), p_paddr, p_filesz) };
        }
    }

    (e_entry, phys_start, phys_end)
}

unsafe fn zero_phys(dst_phys: u64, len: usize) {
    if len == 0 {
        return;
    }
    // Loader destinations are physical addresses and may legally include 0x0.
    unsafe {
        core::arch::asm!(
            "cld",
            "rep stosb",
            in("rdi") dst_phys as usize,
            in("al") 0u8,
            in("rcx") len,
            options(nostack, preserves_flags),
        );
    }
}

unsafe fn copy_to_phys(src: *const u8, dst_phys: u64, len: usize) {
    if len == 0 {
        return;
    }
    // Loader destinations are physical addresses and may legally include 0x0.
    unsafe {
        core::arch::asm!(
            "cld",
            "rep movsb",
            in("rsi") src,
            in("rdi") dst_phys as usize,
            in("rcx") len,
            options(nostack, preserves_flags),
        );
    }
}

// ====================================================================
// GOP / RSDP / Memory map
// ====================================================================

/// Discover the GOP framebuffer and return (addr, width, height, stride).
fn discover_gop(bs: &BootServices) -> (u64, u32, u32, u32, u32) {
    let gop_handle = bs
        .locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutput::GUID))
        .expect("No GOP handle found");
    // SAFETY: We open the first GOP handle with exclusive access as required by UEFI spec.
    let mut gop = bs
        .open_protocol_exclusive::<GraphicsOutput>(gop_handle[0])
        .expect("Failed to open GOP");
    let mode = gop.current_mode_info();
    let width = mode.resolution().0 as u32;
    let height = mode.resolution().1 as u32;
    let stride = mode.stride() as u32;
    match mode.pixel_format() {
        PixelFormat::Rgb => {
            let mut fb = gop.frame_buffer();
            (fb.as_mut_ptr() as u64, width, height, stride, 0)
        }
        PixelFormat::Bgr => {
            let mut fb = gop.frame_buffer();
            (fb.as_mut_ptr() as u64, width, height, stride, 1)
        }
        PixelFormat::Bitmask => {
            let mut fb = gop.frame_buffer();
            (fb.as_mut_ptr() as u64, width, height, stride, 2)
        }
        PixelFormat::BltOnly => {
            info!("[graphos-loader] GOP mode is BLT-only; continuing without direct framebuffer");
            (0, width, height, stride, 3)
        }
    }
}

fn pixel_format_name(format: u32) -> &'static str {
    match format {
        0 => "rgb",
        1 => "bgr",
        2 => "bitmask",
        3 => "blt-only",
        _ => "unknown",
    }
}

/// Find the ACPI RSDP address from UEFI configuration tables.
fn find_rsdp(st: &SystemTable<Boot>) -> u64 {
    for entry in st.config_table() {
        if entry.guid == ACPI2_GUID {
            return entry.address as u64;
        }
    }
    0
}

/// Convert UEFI memory descriptors into our flat MemoryRegion array.
///
/// Writes directly to the provided pointer (pre-allocated physical pages).
/// Returns the number of regions written.
fn collect_regions_to_ptr(mmap: &MemoryMap, out: *mut MemoryRegion) -> usize {
    let mut count = 0usize;
    for desc in mmap.entries() {
        if count >= MAX_MEMORY_REGIONS {
            break;
        }
        let kind = match desc.ty {
            MemoryType::CONVENTIONAL => MemoryRegionKind::Usable,
            MemoryType::BOOT_SERVICES_CODE | MemoryType::BOOT_SERVICES_DATA => {
                MemoryRegionKind::Usable
            }
            MemoryType::ACPI_RECLAIM => MemoryRegionKind::AcpiReclaim,
            MemoryType::ACPI_NON_VOLATILE => MemoryRegionKind::AcpiNvs,
            MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => MemoryRegionKind::Mmio,
            _ => MemoryRegionKind::Reserved,
        };
        // SAFETY: `out` points to pre-allocated LOADER_DATA pages with room
        // for MAX_MEMORY_REGIONS entries. Single-threaded post-exit_boot_services.
        unsafe {
            out.add(count).write(MemoryRegion {
                start: desc.phys_start,
                length: desc.page_count * 4096,
                kind,
                _pad: 0,
            });
        }
        count += 1;
    }
    count
}
