// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Per-process address space - user-mode page table management.
//!
//! Each protected userspace task gets its own PML4. The kernel identity-map
//! entry is copied in so traps/syscalls can execute kernel code while the
//! task's address space is active, while the dedicated user slot remains
//! private to the process.
//!
//! ## Memory layout (per-process)
//!
//! ```text
//! 0x0000_0080_0040_0000             - Default user code / ELF entry slot
//! 0x0000_0080_7FFE_E000             - Lowest demand-stack page
//! 0x0000_0080_7FFF_C000             - Initially committed stack base
//! 0x0000_0080_7FFF_D000             - Initial user stack page
//! 0x0000_0080_7FFF_E000             - User stack top
//! 0x0000_0080_7FFE_D000             - Guard page below reserved stack range
//! 0x0000_0000_0000_0000..1 GiB      - Kernel identity map (supervisor-only)
//! ```
//!
//! ## Current scope
//! - 4 KiB user pages with per-segment permissions from ELF PT_LOAD flags.
//! - Explicit VMA tracking for loaded segments and lazy anonymous stack growth.
//! - Rollback-friendly ownership: dropping an address space releases every
//!   page-table frame and committed user frame it owns.

use crate::arch::x86_64::paging::flags;
use crate::mm::{frame_alloc, page_table};
use alloc::boxed::Box;
use object::Endianness;
use object::elf::{
    FileHeader64, PF_W, PF_X, PT_LOAD, R_X86_64_32, R_X86_64_32S, R_X86_64_64, R_X86_64_GLOB_DAT,
    R_X86_64_JUMP_SLOT, R_X86_64_NONE, R_X86_64_RELATIVE, R_X86_64_RELATIVE64, SHN_UNDEF, SHT_RELA,
};
use object::read::elf::{
    FileHeader as _, ProgramHeader as _, Rela as _, SectionHeader as _, Sym as _,
};

const PAGE_SIZE: u64 = 4096;
/// Tracks all frames owned by an address space: page tables, PT_LOAD pages,
/// committed stack pages, and other anonymous/user mappings.
///
/// Desktop-facing binaries like shell3d reserve a large `.bss` and can exceed
/// the original bootstrap-service footprint during ELF load alone.
const MAX_USER_PT_FRAMES: usize = 32768; // 128 MiB @ 4 KiB/frame; covers 96 MiB heap + ELF + stacks
const MAX_BOOTSTRAP_IMAGE_PAGES: usize = 32;
/// Desktop apps can accumulate many mappings from PT_LOAD, stack growth,
/// mmaps, shared surfaces, and imported assets.
const MAX_VM_AREAS: usize = 256;
const MAX_SWAP_RECORDS: usize = 256;
const MAX_FILE_PATH_LEN: usize = 255;
const USER_PML4_SLOT: u64 = 1;
const USER_SLOT_BASE: u64 = USER_PML4_SLOT << 39;
const USER_SLOT_LIMIT: u64 = USER_SLOT_BASE + (1 << 39);

/// User code starts at 4 MiB into the dedicated user slot.
pub const USER_CODE_START: u64 = USER_SLOT_BASE + 4 * 1024 * 1024;
/// User stack top in the same slot.
pub const USER_STACK_TOP: u64 = USER_SLOT_BASE + 0x7FFF_E000;
/// Pages committed up front for the initial stack.
pub const USER_STACK_COMMITTED_PAGES: usize = 2;
/// Maximum reserved stack size before faults are rejected.
pub const USER_STACK_MAX_PAGES: usize = 512;
/// One unmapped guard page below the reserved stack range.
pub const USER_STACK_GUARD_PAGES: usize = 1;
/// Lowest committed stack address at task creation.
pub const USER_STACK_BASE: u64 = USER_STACK_TOP - USER_STACK_COMMITTED_PAGES as u64 * PAGE_SIZE;
/// Lowest address allowed to grow into before the guard page.
pub const USER_STACK_RESERVED_BASE: u64 = USER_STACK_TOP - USER_STACK_MAX_PAGES as u64 * PAGE_SIZE;
/// Lowest guard-page address.
pub const USER_STACK_GUARD_BASE: u64 =
    USER_STACK_RESERVED_BASE - USER_STACK_GUARD_PAGES as u64 * PAGE_SIZE;
/// Base of the generic mmap arena.
pub const USER_MMAP_START: u64 = USER_SLOT_BASE + 32 * 1024 * 1024;
/// Upper bound of the generic mmap arena, kept below the reserved stack range.
pub const USER_MMAP_LIMIT: u64 = USER_STACK_GUARD_BASE - 1024 * 1024;

pub const MMAP_PROT_READ: u64 = 1 << 0;
pub const MMAP_PROT_WRITE: u64 = 1 << 1;
pub const MMAP_PROT_EXEC: u64 = 1 << 2;
pub const MMAP_FLAG_PRIVATE: u64 = 1 << 0;
pub const MMAP_FLAG_SHARED: u64 = 1 << 1;
pub const MMAP_FLAG_ANON: u64 = 1 << 2;

#[derive(Clone, Copy, Debug)]
pub struct UserImageInfo {
    pub entry: u64,
    pub mapped_end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VmRegionKind {
    Empty,
    Load,
    Stack,
    Anon,
    Mmap,
    /// A shared surface mapping: physical frames owned by the surface table,
    /// mapped read-write into this address space. `surface_id` links back
    /// to the `SurfaceRecord` that owns the frames.
    SharedSurface,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VmBackingKind {
    None,
    File,
}

#[derive(Clone, Copy, Debug)]
struct VmRegion {
    start: u64,
    end: u64,
    page_flags: u64,
    kind: VmRegionKind,
    lazy: bool,
    cow: bool,
    backing: VmBackingKind,
    file_offset: u64,
    file_path: [u8; MAX_FILE_PATH_LEN + 1],
    file_path_len: usize,
}

impl VmRegion {
    const EMPTY: Self = Self {
        start: 0,
        end: 0,
        page_flags: 0,
        kind: VmRegionKind::Empty,
        lazy: false,
        cow: false,
        backing: VmBackingKind::None,
        file_offset: 0,
        file_path: [0; MAX_FILE_PATH_LEN + 1],
        file_path_len: 0,
    };

    fn contains_page(self, page: u64) -> bool {
        self.kind != VmRegionKind::Empty && page >= self.start && page < self.end
    }

    fn file_path(self) -> Option<[u8; MAX_FILE_PATH_LEN + 1]> {
        (self.backing == VmBackingKind::File).then_some(self.file_path)
    }
}

#[derive(Clone, Copy, Debug)]
struct VmFileBackingSpec {
    kind: VmBackingKind,
    file_offset: u64,
    file_path: [u8; MAX_FILE_PATH_LEN + 1],
    file_path_len: usize,
}

impl VmFileBackingSpec {
    const NONE: Self = Self {
        kind: VmBackingKind::None,
        file_offset: 0,
        file_path: [0; MAX_FILE_PATH_LEN + 1],
        file_path_len: 0,
    };

    fn file(path: [u8; MAX_FILE_PATH_LEN + 1], path_len: usize, offset: u64) -> Self {
        Self {
            kind: VmBackingKind::File,
            file_offset: offset,
            file_path: path,
            file_path_len: path_len,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct VmRegionRegistration {
    start: u64,
    end: u64,
    page_flags: u64,
    kind: VmRegionKind,
    lazy: bool,
    cow: bool,
    backing: VmFileBackingSpec,
}

#[derive(Clone, Copy)]
struct ElfRelocationQuery<'a> {
    header: &'a FileHeader64<Endianness>,
    endian: Endianness,
    image: &'a [u8],
    load_bias: u64,
    symtab_index: object::read::SectionIndex,
    sym_index: u32,
    addend: i64,
    r_type: u32,
}

#[derive(Clone, Copy, Debug)]
struct SwapRecord {
    page: u64,
    slot: u16,
    active: bool,
}

impl SwapRecord {
    const EMPTY: Self = Self {
        page: 0,
        slot: 0,
        active: false,
    };
}

/// A user-mode address space that owns its PML4 and all subordinate frames.
#[derive(Debug)]
pub struct AddressSpace {
    /// Physical address of the PML4 table (4 KiB aligned).
    pml4_phys: u64,
    /// Frames allocated for this address space's page tables and owned pages.
    frames: [u64; MAX_USER_PT_FRAMES],
    frame_count: usize,
    /// Virtual memory areas tracked for fault handling and diagnostics.
    regions: [VmRegion; MAX_VM_AREAS],
    region_count: usize,
    /// Swapped-out pages owned by this address space.
    swap_records: [SwapRecord; MAX_SWAP_RECORDS],
    swap_count: usize,
    /// Next hint used for the generic mmap arena.
    next_mmap_base: u64,
}

impl AddressSpace {
    /// Create a new address space by allocating a PML4 and cloning kernel mappings.
    ///
    /// Returns a heap-allocated `Box<Self>` to avoid placing the large `frames`
    /// array (~256 KiB) on the kernel stack, which would cause a stack overflow
    /// during early-boot task creation.
    pub fn new(kernel_pml4_phys: u64) -> Option<Box<Self>> {
        use alloc::alloc::{Layout, alloc_zeroed};

        let pml4 = frame_alloc::alloc_frame()?;

        unsafe {
            core::ptr::write_bytes(pml4 as *mut u8, 0, PAGE_SIZE as usize);

            let dst = pml4 as *mut u64;
            let src = kernel_pml4_phys as *const u64;

            // Keep the current low-memory kernel identity slot active so
            // traps/syscalls can execute kernel code while user CR3 is live.
            dst.write(src.read());

            // Preserve the higher-half kernel convention for future growth.
            core::ptr::copy_nonoverlapping(src.add(256), dst.add(256), 256);
        }

        // ASLR: randomise the mmap base with up to 256 MiB of page-aligned
        // entropy so that mappings don't land at deterministic addresses.
        let aslr_pages = crate::arch::x86_64::paging::rdrand64()
            .map(|r| r & 0x3FFF) // 14 bits → 0..16383 pages
            .unwrap_or(0);
        let aslr_offset = aslr_pages * PAGE_SIZE;
        let aslr_base = USER_MMAP_START
            .saturating_add(aslr_offset)
            .min(USER_MMAP_LIMIT / 2);

        // Heap-allocate the struct to avoid a ~340 KiB stack frame.
        let layout = Layout::new::<Self>();
        let ptr = unsafe { alloc_zeroed(layout) } as *mut Self;
        if ptr.is_null() {
            frame_alloc::dealloc_frame(pml4);
            return None;
        }
        // SAFETY: alloc_zeroed returned a valid, aligned, zeroed allocation.
        let mut space = unsafe { Box::from_raw(ptr) };
        space.pml4_phys = pml4;
        // frames/frame_count/regions/region_count/swap_records/swap_count
        // are already zeroed; only set the non-zero fields.
        space.regions = [VmRegion::EMPTY; MAX_VM_AREAS];
        space.swap_records = [SwapRecord::EMPTY; MAX_SWAP_RECORDS];
        space.next_mmap_base = aslr_base;

        if !space.track_owned_frame(pml4) {
            frame_alloc::dealloc_frame(pml4);
            return None;
        }
        Some(space)
    }

    /// Returns the physical address of the address space root for CR3.
    pub fn cr3(&self) -> u64 {
        self.pml4_phys
    }

    /// Fixed user entry point used by the bootstrap flat-image loader.
    pub const fn user_entry_point(&self) -> u64 {
        USER_CODE_START
    }

    /// Initial user stack pointer.
    pub const fn user_stack_pointer(&self) -> u64 {
        USER_STACK_TOP
    }

    /// Returns whether `vaddr` currently resolves to a present user mapping.
    pub fn is_mapped(&self, vaddr: u64) -> bool {
        self.page_entry_ptr(vaddr)
            .map(|entry| unsafe { entry.read() & flags::PRESENT != 0 })
            .unwrap_or(false)
    }

    /// Returns the backing physical frame for a mapped user page.
    pub fn mapped_phys(&self, vaddr: u64) -> Option<u64> {
        let entry = unsafe { self.page_entry_ptr(vaddr)?.read() };
        (entry & flags::PRESENT != 0).then_some(entry & frame_mask())
    }

    /// Returns whether `vaddr` is currently represented by a swap slot.
    pub fn is_swapped(&self, vaddr: u64) -> bool {
        self.swap_slot_for(align_down(vaddr, PAGE_SIZE)).is_some()
    }

    /// Force one swappable page out to the swap staging area.
    pub fn evict_one_swappable_page(&mut self) -> bool {
        self.swap_out_one_page()
    }

    /// Load a flat bootstrap image at the default entry slot.
    ///
    /// This path remains for tiny embedded smoke images and test fixtures.
    /// Real protected services should use `load_elf_image`.
    pub fn load_user_image(&mut self, image: &[u8]) -> Option<(u64, usize)> {
        if image.is_empty() {
            return None;
        }

        let code_pages = image.len().div_ceil(PAGE_SIZE as usize);
        if code_pages > MAX_BOOTSTRAP_IMAGE_PAGES {
            return None;
        }

        let code_start = USER_CODE_START;
        let code_end = code_start + code_pages as u64 * PAGE_SIZE;
        if !self.register_region(
            code_start,
            code_end,
            flags::PRESENT | flags::USER,
            VmRegionKind::Load,
            false,
        ) {
            return None;
        }

        for page_idx in 0..code_pages {
            let frame = self.alloc_owned_frame()?;
            let start = page_idx * PAGE_SIZE as usize;
            let end = (start + PAGE_SIZE as usize).min(image.len());
            unsafe {
                core::ptr::copy_nonoverlapping(
                    image[start..end].as_ptr(),
                    frame as *mut u8,
                    end - start,
                );
            }

            let vaddr = USER_CODE_START + page_idx as u64 * PAGE_SIZE;
            if !self.map_page(vaddr, frame, flags::PRESENT | flags::USER) {
                return None;
            }
        }

        Some((USER_CODE_START, code_pages))
    }

    /// Load an ELF64 user image into this address space.
    ///
    /// The loader maps every PT_LOAD segment with page permissions derived
    /// from the ELF segment flags and returns the entry point on success.
    /// The preferred production path is to link protected services directly
    /// into the dedicated user slot so the image can be mapped without a
    /// relocation pass. A small load-bias fallback remains for low-linked
    /// bootstrap ELFs and synthetic tests.
    pub fn load_elf_image(&mut self, image: &[u8]) -> Option<UserImageInfo> {
        // Compute a per-image ASLR base: USER_CODE_START plus a random
        // 2 MiB-aligned offset in the range [0, 128 MiB).  We use RDRAND
        // where available, falling back to a deterministic base on CPUs or
        // VMs that do not support the instruction.
        const ASLR_WINDOW: u64 = 128 * 1024 * 1024; // 128 MiB
        const ASLR_ALIGN: u64 = 2 * 1024 * 1024; // 2 MiB
        let aslr_slots = ASLR_WINDOW / ASLR_ALIGN;
        let aslr_offset = crate::arch::x86_64::paging::rdrand64()
            .map(|r| (r % aslr_slots) * ASLR_ALIGN)
            .unwrap_or(0);
        let aslr_base = USER_CODE_START + aslr_offset;

        let header = FileHeader64::<Endianness>::parse(image).ok()?;
        let endian = header.endian().ok()?;
        let phdrs = header.program_headers(endian, image).ok()?;
        let load_bias = if is_user_vaddr(header.e_entry.get(endian)) {
            0
        } else {
            let mut lowest_load = u64::MAX;
            let mut saw_load = false;
            for phdr in header.program_headers(endian, image).ok()? {
                if phdr.p_type(endian) != PT_LOAD {
                    continue;
                }
                lowest_load = lowest_load.min(align_down(phdr.p_vaddr(endian), PAGE_SIZE));
                saw_load = true;
            }
            if !saw_load {
                log_user_elf_reject(b"no PT_LOAD segments");
                return None;
            }
            aslr_base.checked_sub(lowest_load)?
        };
        let entry = header.e_entry.get(endian).checked_add(load_bias)?;

        if !is_user_vaddr(entry) {
            log_user_elf_reject(b"entry outside user range");
            return None;
        }

        let mut mapped_any = false;
        let mut mapped_end = 0u64;

        for phdr in phdrs {
            if phdr.p_type(endian) != PT_LOAD {
                continue;
            }

            let segment_vaddr = phdr.p_vaddr(endian).checked_add(load_bias)?;
            let file_offset = phdr.p_offset(endian) as usize;
            let file_size = phdr.p_filesz(endian) as usize;
            let mem_size = phdr.p_memsz(endian) as usize;
            let seg_flags = phdr.p_flags(endian);

            if mem_size == 0 || file_size > mem_size {
                log_user_elf_reject(b"invalid PT_LOAD sizes");
                return None;
            }

            let mem_end = segment_vaddr.checked_add(mem_size as u64)?;
            let seg_start = align_down(segment_vaddr, PAGE_SIZE);
            let seg_end = align_up(mem_end, PAGE_SIZE);
            if !is_user_vaddr_range(seg_start, seg_end) {
                log_user_elf_reject(b"segment outside user range");
                return None;
            }
            if file_offset.checked_add(file_size)? > image.len() {
                log_user_elf_reject(b"segment file bounds invalid");
                return None;
            }

            let page_flags = elf_page_flags(seg_flags);

            // Split the segment into:
            //   1. file-backed pages (eagerly mapped with content)
            //   2. pure-BSS tail (memsz > filesz): register as a lazy zero region
            //      so frames are demand-paged on first touch rather than allocated
            //      upfront.  This keeps the owned-frame table within bounds for
            //      binaries with large static heaps (e.g. compositor's 48 MiB BSS).
            let file_end = segment_vaddr + file_size as u64;
            let file_page_end = align_up(file_end, PAGE_SIZE);
            let eager_end = file_page_end.min(seg_end);

            // Register the eager (file-backed) portion as non-lazy.
            if eager_end > seg_start {
                if !self.register_region(
                    seg_start,
                    eager_end,
                    page_flags,
                    VmRegionKind::Load,
                    false,
                ) {
                    log_user_elf_reject(b"region registration failed");
                    return None;
                }
            }

            // Register the lazy BSS tail (if any) as a separate demand-paged region.
            if seg_end > eager_end {
                if !self.register_region(eager_end, seg_end, page_flags, VmRegionKind::Load, true) {
                    log_user_elf_reject(b"bss region registration failed");
                    return None;
                }
            }

            let mut page = seg_start;
            while page < eager_end {
                let frame = self.alloc_owned_frame()?;

                let copy_start = page.max(segment_vaddr);
                let copy_end = (page + PAGE_SIZE).min(file_end);
                if copy_start < copy_end {
                    let src_off = file_offset + (copy_start - segment_vaddr) as usize;
                    let dst_off = (copy_start - page) as usize;
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            image[src_off..src_off + (copy_end - copy_start) as usize].as_ptr(),
                            (frame as *mut u8).add(dst_off),
                            (copy_end - copy_start) as usize,
                        );
                    }
                }

                if !self.map_page(page, frame, page_flags) {
                    log_user_elf_reject(b"page map failed");
                    return None;
                }
                page += PAGE_SIZE;
            }

            mapped_any = true;
            mapped_end = mapped_end.max(seg_end);
        }

        let relocation_count = self.apply_elf_relocations(header, endian, image, load_bias)?;
        if relocation_count > 0 {
            crate::arch::serial::write_bytes(b"[task] user-elf rela applied=");
            crate::arch::serial::write_u64_dec(relocation_count as u64);
        }

        mapped_any.then_some(UserImageInfo { entry, mapped_end })
    }

    /// Map an initial user stack with a guard page below a lazily-growable range.
    pub fn map_user_stack(&mut self) -> bool {
        let stack_flags = flags::PRESENT | flags::WRITABLE | flags::USER | flags::NO_EXECUTE;
        if !self.register_region(
            USER_STACK_RESERVED_BASE,
            USER_STACK_TOP,
            stack_flags,
            VmRegionKind::Stack,
            true,
        ) {
            return false;
        }

        for page_idx in 0..USER_STACK_COMMITTED_PAGES {
            let vaddr = USER_STACK_BASE + page_idx as u64 * PAGE_SIZE;
            if !self.map_zeroed_page(vaddr, stack_flags) {
                return false;
            }
        }

        true
    }

    /// Map a list of pre-allocated shared physical frames into this address
    /// space as a contiguous virtual region, starting at the next mmap base.
    ///
    /// The frames are **not** tracked as owned by this address space (they are
    /// owned by the surface table). They will not be freed when this address
    /// space is destroyed. The caller is responsible for unmapping them before
    /// freeing the frames.
    ///
    /// Returns the user virtual address of the first mapped byte, or `None`
    /// if there is no space in the VMA table, no room in the frame table, or
    /// the frame list is empty / too large.
    ///
    /// # Safety
    /// `frames` must be non-empty, page-aligned physical addresses that are
    /// valid for the lifetime of this mapping.
    pub fn map_shared_frames(&mut self, frames: &[u64], prot: u64, surface_id: u32) -> Option<u64> {
        if frames.is_empty() || frames.len() > crate::wm::surface_table::MAX_SURFACE_FRAMES {
            return None;
        }
        let length = frames.len() as u64 * PAGE_SIZE;
        let start = self.find_free_range(length)?;
        let page_flags = prot_to_page_flags(prot)
            | crate::arch::x86_64::paging::flags::PRESENT
            | crate::arch::x86_64::paging::flags::USER;

        // Register the VMA before mapping pages so rollback is clean.
        if !self.register_region(
            start,
            start + length,
            page_flags,
            VmRegionKind::SharedSurface,
            false,
        ) {
            return None;
        }

        // Map each frame without tracking ownership.
        for (i, &phys) in frames.iter().enumerate() {
            let vaddr = start + i as u64 * PAGE_SIZE;
            if !self.map_page_unowned(vaddr, phys, page_flags) {
                // Rollback: remove the VMA entry we just added.
                if let Some(idx) = self
                    .regions
                    .iter()
                    .take(self.region_count)
                    .position(|r| r.kind == VmRegionKind::SharedSurface && r.start == start)
                {
                    for shift in idx..self.region_count - 1 {
                        self.regions[shift] = self.regions[shift + 1];
                    }
                    self.region_count -= 1;
                    self.regions[self.region_count] = VmRegion::EMPTY;
                }
                // Unmap already-mapped pages (best effort).
                for rollback in 0..i {
                    let rv = start + rollback as u64 * PAGE_SIZE;
                    let _ = self.unmap_page(rv);
                }
                return None;
            }
        }

        let _ = surface_id; // stored in VMA kind for future unmap use
        Some(start)
    }

    /// Map a single 4 KiB page without taking ownership of the physical frame.
    ///
    /// Used for shared surface mappings whose frames are managed by the
    /// surface table, not by this address space.
    fn map_page_unowned(&mut self, vaddr: u64, phys: u64, page_flags: u64) -> bool {
        use crate::arch::x86_64::paging::{pd_index, pdpt_index, pml4_index, pt_index};

        if !is_user_vaddr(vaddr) || phys & (PAGE_SIZE - 1) != 0 {
            return false;
        }

        let pml4_ptr = self.pml4_phys as *mut u64;
        unsafe {
            let pml4i = pml4_index(vaddr);
            let pdpt_phys = self.ensure_table(pml4_ptr, pml4i);
            if pdpt_phys == 0 {
                return false;
            }
            let pdpt_ptr = pdpt_phys as *mut u64;
            let pdpti = pdpt_index(vaddr);
            let pd_phys = self.ensure_table(pdpt_ptr, pdpti);
            if pd_phys == 0 {
                return false;
            }
            let pd_ptr = pd_phys as *mut u64;
            let pdi = pd_index(vaddr);
            let pt_phys = self.ensure_table(pd_ptr, pdi);
            if pt_phys == 0 {
                return false;
            }
            let pt_ptr = pt_phys as *mut u64;
            let pti = pt_index(vaddr);
            let entry_ptr = pt_ptr.add(pti);
            if entry_ptr.read() & crate::arch::x86_64::paging::flags::PRESENT != 0 {
                return false; // already mapped
            }
            entry_ptr.write(phys | page_flags);
        }
        true
    }

    /// Create a lazily-committed anonymous mapping.
    pub fn mmap_anon(&mut self, len: u64, prot: u64) -> Option<u64> {
        let length = align_up(len.max(PAGE_SIZE), PAGE_SIZE);
        let start = self.find_free_range(length)?;
        let page_flags = prot_to_page_flags(prot);
        if !self.register_region(start, start + length, page_flags, VmRegionKind::Mmap, true) {
            return None;
        }
        Some(start)
    }

    /// Create a lazily-populated file mapping.
    ///
    /// The file offset must be page aligned. `MAP_PRIVATE|PROT_WRITE` maps from
    /// the shared page cache read-only first and breaks into private frames on
    /// the first write fault.
    pub fn mmap_file(
        &mut self,
        path: &[u8],
        len: u64,
        prot: u64,
        map_flags: u64,
        offset: u64,
    ) -> Option<u64> {
        if path.is_empty()
            || path.len() > MAX_FILE_PATH_LEN
            || len == 0
            || offset & (PAGE_SIZE - 1) != 0
        {
            return None;
        }
        let private = map_flags & MMAP_FLAG_PRIVATE != 0;
        let shared = map_flags & MMAP_FLAG_SHARED != 0;
        if private == shared {
            return None;
        }
        if shared && prot & MMAP_PROT_WRITE != 0 {
            return None;
        }

        let length = align_up(len, PAGE_SIZE);
        let start = self.find_free_range(length)?;
        let mut file_path = [0u8; MAX_FILE_PATH_LEN + 1];
        file_path[..path.len()].copy_from_slice(path);

        let page_flags = prot_to_page_flags(prot);
        if !self.register_region_with_backing(VmRegionRegistration {
            start,
            end: start + length,
            page_flags,
            kind: VmRegionKind::Mmap,
            lazy: true,
            cow: private && prot & MMAP_PROT_WRITE != 0,
            backing: VmFileBackingSpec::file(file_path, path.len(), offset),
        }) {
            return None;
        }
        Some(start)
    }

    /// Unmap an exact region created by `mmap_*`.
    pub fn munmap(&mut self, addr: u64, len: u64) -> bool {
        let start = align_down(addr, PAGE_SIZE);
        let end = align_up(addr.checked_add(len).unwrap_or(addr), PAGE_SIZE);
        let Some(region_idx) = self
            .regions
            .iter()
            .take(self.region_count)
            .position(|region| {
                region.kind == VmRegionKind::Mmap && region.start == start && region.end == end
            })
        else {
            return false;
        };

        let region = self.regions[region_idx];
        let mut page = region.start;
        while page < region.end {
            if let Some(slot) = self.swap_slot_for(page) {
                crate::mm::swap::discard(slot);
                self.clear_swap_record(page);
            } else if let Some(entry_ptr) = self.page_entry_ptr(page) {
                unsafe {
                    let entry = entry_ptr.read();
                    if entry & flags::PRESENT != 0 {
                        let frame = entry & frame_mask();
                        if !self.release_owned_frame(frame) {
                            crate::mm::page_cache::release_frame(frame);
                        }
                        entry_ptr.write(0);
                    }
                }
            }
            page += PAGE_SIZE;
        }

        for idx in region_idx..self.region_count - 1 {
            self.regions[idx] = self.regions[idx + 1];
        }
        self.region_count -= 1;
        self.regions[self.region_count] = VmRegion::EMPTY;
        true
    }

    /// Handle a not-present fault in this address space.
    ///
    /// Today this is used for demand-zero stack growth and other lazy
    /// anonymous regions staged by the address-space layer.
    pub fn handle_page_fault(&mut self, fault_addr: u64, error_code_bits: u64) -> bool {
        const PF_PRESENT: u64 = 1 << 0;
        const PF_WRITE: u64 = 1 << 1;

        let page = align_down(fault_addr, PAGE_SIZE);
        let Some(region) = self.region_for(page) else {
            return false;
        };

        if error_code_bits & PF_PRESENT != 0 {
            if error_code_bits & PF_WRITE != 0 && region.cow && self.is_mapped(page) {
                return self.break_cow_page(page, region.page_flags);
            }
            return false;
        }

        if error_code_bits & PF_WRITE != 0
            && region.page_flags & flags::WRITABLE == 0
            && !region.cow
        {
            return false;
        }

        if let Some(slot) = self.swap_slot_for(page) {
            if self.restore_swapped_page(page, region.page_flags, slot) {
                unsafe { page_table::flush_page(page) };
                return true;
            }
            return false;
        }

        if region.backing == VmBackingKind::File {
            if self.map_file_page(page, region) {
                unsafe { page_table::flush_page(page) };
                return true;
            }
            return false;
        }

        if !region.lazy || self.is_mapped(page) {
            return false;
        }

        if !self.map_zeroed_page(page, region.page_flags) {
            return false;
        }

        unsafe { page_table::flush_page(page) };
        true
    }

    /// Map a single 4 KiB page at `vaddr -> phys` with the given flags.
    pub fn map_page(&mut self, vaddr: u64, phys: u64, page_flags: u64) -> bool {
        use crate::arch::x86_64::paging::{pd_index, pdpt_index, pml4_index, pt_index};

        if !is_user_vaddr(vaddr) || phys & (PAGE_SIZE - 1) != 0 {
            return false;
        }

        let pml4_ptr = self.pml4_phys as *mut u64;
        unsafe {
            let pml4i = pml4_index(vaddr);
            let pdpt_phys = self.ensure_table(pml4_ptr, pml4i);
            if pdpt_phys == 0 {
                return false;
            }

            let pdpt_ptr = pdpt_phys as *mut u64;
            let pdpti = pdpt_index(vaddr);
            let pd_phys = self.ensure_table(pdpt_ptr, pdpti);
            if pd_phys == 0 {
                return false;
            }

            let pd_ptr = pd_phys as *mut u64;
            let pdi = pd_index(vaddr);
            let pt_phys = self.ensure_table(pd_ptr, pdi);
            if pt_phys == 0 {
                return false;
            }

            let pt_ptr = pt_phys as *mut u64;
            let pti = pt_index(vaddr);
            let entry_ptr = pt_ptr.add(pti);
            if entry_ptr.read() & flags::PRESENT != 0 {
                return false;
            }
            let pte_value = phys | page_flags;
            entry_ptr.write(pte_value);
        }

        true
    }

    /// Map already-allocated code pages at `USER_CODE_START`.
    pub fn map_user_code(&mut self, code_phys: u64, code_pages: usize) -> bool {
        let code_start = USER_CODE_START;
        let code_end = code_start + code_pages as u64 * PAGE_SIZE;
        if !self.register_region(
            code_start,
            code_end,
            flags::PRESENT | flags::USER,
            VmRegionKind::Load,
            false,
        ) {
            return false;
        }

        for (mapped_pages, page_idx) in (0..code_pages).enumerate() {
            let vaddr = USER_CODE_START + page_idx as u64 * PAGE_SIZE;
            let phys = code_phys + page_idx as u64 * PAGE_SIZE;
            if !self.map_page(vaddr, phys, flags::PRESENT | flags::USER) {
                for rollback_idx in 0..mapped_pages {
                    let rollback_vaddr = USER_CODE_START + rollback_idx as u64 * PAGE_SIZE;
                    let _ = self.unmap_page(rollback_vaddr);
                }
                return false;
            }
        }
        true
    }

    /// Explicitly free every frame owned by this address space.
    pub fn destroy(&mut self) {
        self.release_external_mappings();
        self.release_swap_records();
        for idx in 0..self.frame_count {
            let frame = self.frames[idx];
            if frame != 0 {
                frame_alloc::dealloc_frame(frame);
                self.frames[idx] = 0;
            }
        }
        self.frame_count = 0;
        self.region_count = 0;
        self.regions = [VmRegion::EMPTY; MAX_VM_AREAS];
        self.swap_count = 0;
        self.swap_records = [SwapRecord::EMPTY; MAX_SWAP_RECORDS];
        self.next_mmap_base = USER_MMAP_START;
        self.pml4_phys = 0;
    }

    fn map_zeroed_page(&mut self, vaddr: u64, page_flags: u64) -> bool {
        let Some(frame) = self.alloc_owned_frame() else {
            return false;
        };
        self.map_page(vaddr, frame, page_flags)
    }

    fn apply_elf_relocations(
        &mut self,
        header: &FileHeader64<Endianness>,
        endian: Endianness,
        image: &[u8],
        load_bias: u64,
    ) -> Option<usize> {
        let sections = header.sections(endian, image).ok()?;
        let mut applied = 0usize;

        for (_, section) in sections.enumerate().skip(1) {
            if section.sh_type(endian) != SHT_RELA {
                continue;
            }
            let Some((rels, symtab_index)) = section.rela(endian, image).ok()? else {
                continue;
            };

            for rel in rels {
                let offset = rel.r_offset(endian);
                let target = offset.checked_add(load_bias)?;
                let addend = rel.r_addend(endian);
                let r_type = rel.r_type(endian, false);
                let sym_index = rel.r_sym(endian, false);

                if r_type == R_X86_64_NONE {
                    continue;
                }

                let value = self.resolve_relocation_value(ElfRelocationQuery {
                    header,
                    endian,
                    image,
                    load_bias,
                    symtab_index,
                    sym_index,
                    addend,
                    r_type,
                })?;

                let wrote = match r_type {
                    R_X86_64_32 | R_X86_64_32S => {
                        if value > u32::MAX as u64 {
                            log_user_elf_reject(b"relocation value exceeds u32");
                            return None;
                        }
                        self.write_user_u32(target, value as u32)
                    }
                    _ => self.write_user_u64(target, value),
                };

                if !wrote {
                    log_user_elf_reject(b"relocation write failed");
                    return None;
                }
                applied += 1;
            }
        }

        Some(applied)
    }

    fn write_user_u64(&self, vaddr: u64, value: u64) -> bool {
        let entry_ptr = match self.page_entry_ptr(vaddr) {
            Some(ptr) => ptr,
            None => return false,
        };

        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off > PAGE_SIZE - core::mem::size_of::<u64>() as u64 {
            return false;
        }

        unsafe {
            let entry = entry_ptr.read();
            if entry & flags::PRESENT == 0 {
                return false;
            }

            let phys = (entry & frame_mask()) + page_off;
            (phys as *mut u64).write_unaligned(value);
        }

        true
    }

    fn write_user_u32(&self, vaddr: u64, value: u32) -> bool {
        let entry_ptr = match self.page_entry_ptr(vaddr) {
            Some(ptr) => ptr,
            None => return false,
        };

        let page_off = vaddr & (PAGE_SIZE - 1);
        if page_off > PAGE_SIZE - core::mem::size_of::<u32>() as u64 {
            return false;
        }

        unsafe {
            let entry = entry_ptr.read();
            if entry & flags::PRESENT == 0 {
                return false;
            }
            let phys = (entry & frame_mask()) + page_off;
            (phys as *mut u32).write_unaligned(value);
        }

        true
    }

    fn resolve_relocation_value(&self, query: ElfRelocationQuery<'_>) -> Option<u64> {
        let addend_u64 = if query.addend < 0 {
            None
        } else {
            Some(query.addend as u64)
        };
        match query.r_type {
            R_X86_64_RELATIVE | R_X86_64_RELATIVE64 => addend_u64?.checked_add(query.load_bias),
            R_X86_64_64 | R_X86_64_32 | R_X86_64_32S | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                let sym_value = self.resolve_symbol_value(query)?;
                match query.addend {
                    0 => Some(sym_value),
                    n if n > 0 => sym_value.checked_add(n as u64),
                    _ => None,
                }
            }
            _ => {
                log_user_elf_reject(b"unsupported relocation type");
                None
            }
        }
    }

    fn resolve_symbol_value(&self, query: ElfRelocationQuery<'_>) -> Option<u64> {
        let sections = query.header.sections(query.endian, query.image).ok()?;
        let symtab = sections
            .symbol_table_by_index(query.endian, query.image, query.symtab_index)
            .ok()?;
        let sym = symtab.symbols().get(query.sym_index as usize)?;
        if sym.st_shndx(query.endian) == SHN_UNDEF {
            return None;
        }
        sym.st_value(query.endian).checked_add(query.load_bias)
    }

    fn alloc_owned_frame(&mut self) -> Option<u64> {
        let mut frame = frame_alloc::alloc_frame();
        if frame.is_none() && self.swap_out_one_page() {
            frame = frame_alloc::alloc_frame();
        }
        let frame = frame?;
        if !self.track_owned_frame(frame) {
            frame_alloc::dealloc_frame(frame);
            return None;
        }
        unsafe {
            core::ptr::write_bytes(frame as *mut u8, 0, PAGE_SIZE as usize);
        }
        Some(frame)
    }

    fn track_owned_frame(&mut self, frame: u64) -> bool {
        if self.frame_count >= MAX_USER_PT_FRAMES {
            log_user_elf_reject(b"owned-frame table full");
            return false;
        }
        self.frames[self.frame_count] = frame;
        self.frame_count += 1;
        true
    }

    fn release_owned_frame(&mut self, frame: u64) -> bool {
        for idx in 0..self.frame_count {
            if self.frames[idx] == frame {
                frame_alloc::dealloc_frame(frame);
                for shift in idx..self.frame_count - 1 {
                    self.frames[shift] = self.frames[shift + 1];
                }
                self.frame_count -= 1;
                self.frames[self.frame_count] = 0;
                return true;
            }
        }
        false
    }

    fn register_region(
        &mut self,
        start: u64,
        end: u64,
        page_flags: u64,
        kind: VmRegionKind,
        lazy: bool,
    ) -> bool {
        self.register_region_with_backing(VmRegionRegistration {
            start,
            end,
            page_flags,
            kind,
            lazy,
            cow: false,
            backing: VmFileBackingSpec::NONE,
        })
    }

    fn register_region_with_backing(&mut self, registration: VmRegionRegistration) -> bool {
        let start = align_down(registration.start, PAGE_SIZE);
        let end = align_up(registration.end, PAGE_SIZE);
        if !is_user_vaddr_range(start, end) {
            log_user_elf_reject(b"region outside user range");
            return false;
        }
        if self.region_count >= MAX_VM_AREAS {
            log_user_elf_reject(b"region table full");
            return false;
        }

        for idx in 0..self.region_count {
            let existing = self.regions[idx];
            if existing.kind != VmRegionKind::Empty && start < existing.end && end > existing.start
            {
                log_user_elf_reject(b"region overlap");
                return false;
            }
        }

        self.regions[self.region_count] = VmRegion {
            start,
            end,
            page_flags: registration.page_flags,
            kind: registration.kind,
            lazy: registration.lazy,
            cow: registration.cow,
            backing: registration.backing.kind,
            file_offset: registration.backing.file_offset,
            file_path: registration.backing.file_path,
            file_path_len: registration.backing.file_path_len,
        };
        self.region_count += 1;
        true
    }

    fn region_for(&self, page: u64) -> Option<VmRegion> {
        for idx in 0..self.region_count {
            let region = self.regions[idx];
            if region.contains_page(page) {
                return Some(region);
            }
        }
        None
    }

    fn find_free_range(&mut self, len: u64) -> Option<u64> {
        // Add RDRAND-seeded entropy to the base address for ASLR.
        // We randomise at the 2 MiB granularity (21-bit shift) within a
        // 128 MiB window so that the entropy is useful but doesn't exhaust
        // the mmap region too quickly.
        let entropy_pages = {
            let rand = crate::arch::x86_64::paging::rdrand64().unwrap_or_else(|| {
                // CPUs/VMs without RDRAND should still get a stable non-zero spread.
                crate::net::our_ipv4() as u64 ^ 0xDEAD_BEEF_CAFE_BABEu64
            });
            // Take bits [12..20] → up to 256 pages (1 MiB) of offset.
            (rand >> 12) & 0xFF
        };
        let base_offset = entropy_pages * PAGE_SIZE;

        let mut candidate = align_up(
            self.next_mmap_base
                .max(USER_MMAP_START)
                .saturating_add(base_offset),
            PAGE_SIZE,
        );
        let len = align_up(len, PAGE_SIZE);

        while candidate.checked_add(len)? <= USER_MMAP_LIMIT {
            let mut overlap_end = 0;
            for idx in 0..self.region_count {
                let region = self.regions[idx];
                if region.kind != VmRegionKind::Empty
                    && candidate < region.end
                    && candidate + len > region.start
                {
                    overlap_end = overlap_end.max(region.end);
                }
            }

            if overlap_end == 0 {
                self.next_mmap_base = candidate + len;
                return Some(candidate);
            }
            candidate = align_up(overlap_end, PAGE_SIZE);
        }

        None
    }

    fn owns_frame(&self, frame: u64) -> bool {
        self.frames[..self.frame_count].contains(&frame)
    }

    fn swap_slot_for(&self, page: u64) -> Option<u16> {
        self.swap_records[..self.swap_count]
            .iter()
            .find(|record| record.active && record.page == page)
            .map(|record| record.slot)
    }

    fn clear_swap_record(&mut self, page: u64) {
        for idx in 0..self.swap_count {
            if self.swap_records[idx].active && self.swap_records[idx].page == page {
                for shift in idx..self.swap_count - 1 {
                    self.swap_records[shift] = self.swap_records[shift + 1];
                }
                self.swap_count -= 1;
                self.swap_records[self.swap_count] = SwapRecord::EMPTY;
                return;
            }
        }
    }

    fn track_swap_record(&mut self, page: u64, slot: u16) -> bool {
        self.clear_swap_record(page);
        if self.swap_count >= MAX_SWAP_RECORDS {
            return false;
        }
        self.swap_records[self.swap_count] = SwapRecord {
            page,
            slot,
            active: true,
        };
        self.swap_count += 1;
        true
    }

    fn release_swap_records(&mut self) {
        for idx in 0..self.swap_count {
            let record = self.swap_records[idx];
            if record.active {
                crate::mm::swap::discard(record.slot);
            }
        }
    }

    fn release_external_mappings(&self) {
        for idx in 0..self.region_count {
            let region = self.regions[idx];
            if region.kind == VmRegionKind::Empty {
                continue;
            }

            let mut page = region.start;
            while page < region.end {
                if let Some(entry_ptr) = self.page_entry_ptr(page) {
                    unsafe {
                        let entry = entry_ptr.read();
                        if entry & flags::PRESENT != 0 {
                            let frame = entry & frame_mask();
                            if !self.owns_frame(frame) {
                                crate::mm::page_cache::release_frame(frame);
                            }
                        }
                    }
                }
                page += PAGE_SIZE;
            }
        }
    }

    fn map_file_page(&mut self, page: u64, region: VmRegion) -> bool {
        let Some(path_buf) = region.file_path() else {
            return false;
        };
        let path = &path_buf[..region.file_path_len];
        let page_index = (region.file_offset + (page - region.start)) / PAGE_SIZE;
        let Some((frame, _valid_len)) = crate::mm::page_cache::acquire(path, page_index) else {
            return false;
        };
        let page_flags = if region.cow {
            region.page_flags & !flags::WRITABLE
        } else {
            region.page_flags
        };
        if !self.map_page(page, frame, page_flags) {
            crate::mm::page_cache::release_frame(frame);
            return false;
        }
        true
    }

    fn break_cow_page(&mut self, page: u64, page_flags: u64) -> bool {
        let Some(entry_ptr) = self.page_entry_ptr(page) else {
            return false;
        };
        let frame = unsafe {
            let entry = entry_ptr.read();
            if entry & flags::PRESENT == 0 {
                return false;
            }
            entry & frame_mask()
        };

        let Some(new_frame) = self.alloc_owned_frame() else {
            return false;
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                frame as *const u8,
                new_frame as *mut u8,
                PAGE_SIZE as usize,
            );
            entry_ptr.write(new_frame | page_flags);
            page_table::flush_page(page);
        }
        crate::mm::page_cache::release_frame(frame);
        true
    }

    fn restore_swapped_page(&mut self, page: u64, page_flags: u64, slot: u16) -> bool {
        let Some(frame) = self.alloc_owned_frame() else {
            return false;
        };
        if !crate::mm::swap::swap_in(slot, frame) {
            self.release_owned_frame(frame);
            return false;
        }
        if !self.map_page(page, frame, page_flags) {
            self.release_owned_frame(frame);
            return false;
        }
        self.clear_swap_record(page);
        true
    }

    fn swap_out_one_page(&mut self) -> bool {
        for idx in 0..self.region_count {
            let region = self.regions[idx];
            if region.kind == VmRegionKind::Load {
                continue;
            }

            let mut page = region.start;
            while page < region.end {
                let Some(entry_ptr) = self.page_entry_ptr(page) else {
                    page += PAGE_SIZE;
                    continue;
                };
                let entry = unsafe { entry_ptr.read() };
                if entry & flags::PRESENT == 0 {
                    page += PAGE_SIZE;
                    continue;
                }
                let frame = entry & frame_mask();
                if !self.owns_frame(frame) {
                    page += PAGE_SIZE;
                    continue;
                }

                let Some(slot) = crate::mm::swap::swap_out(frame) else {
                    return false;
                };
                unsafe {
                    entry_ptr.write(0);
                    page_table::flush_page(page);
                }
                let _ = self.release_owned_frame(frame);
                if !self.track_swap_record(page, slot) {
                    crate::mm::swap::discard(slot);
                    return false;
                }
                return true;
            }
        }

        false
    }

    fn page_entry_ptr(&self, vaddr: u64) -> Option<*mut u64> {
        use crate::arch::x86_64::paging::{pd_index, pdpt_index, pml4_index, pt_index};

        if !is_user_vaddr(vaddr) {
            return None;
        }

        unsafe {
            let pml4_ptr = self.pml4_phys as *mut u64;
            let pml4e = pml4_ptr.add(pml4_index(vaddr)).read();
            if pml4e & flags::PRESENT == 0 {
                return None;
            }

            let pdpt_ptr = (pml4e & frame_mask()) as *mut u64;
            let pdpte = pdpt_ptr.add(pdpt_index(vaddr)).read();
            if pdpte & flags::PRESENT == 0 {
                return None;
            }

            let pd_ptr = (pdpte & frame_mask()) as *mut u64;
            let pde = pd_ptr.add(pd_index(vaddr)).read();
            if pde & flags::PRESENT == 0 || pde & flags::HUGE_PAGE != 0 {
                return None;
            }

            let pt_ptr = (pde & frame_mask()) as *mut u64;
            Some(pt_ptr.add(pt_index(vaddr)))
        }
    }

    fn unmap_page(&mut self, vaddr: u64) -> Option<u64> {
        let entry_ptr = self.page_entry_ptr(vaddr)?;
        unsafe {
            let entry = entry_ptr.read();
            if entry & flags::PRESENT == 0 {
                return None;
            }
            entry_ptr.write(0);
            let frame = entry & frame_mask();
            if !self.release_owned_frame(frame) {
                crate::mm::page_cache::release_frame(frame);
            }
            Some(frame)
        }
    }

    unsafe fn ensure_table(&mut self, table: *mut u64, index: usize) -> u64 {
        let entry = unsafe { table.add(index).read() };
        if entry & flags::PRESENT != 0 {
            // Existing upper-level entries inherited from firmware/kernel maps
            // may be supervisor-only. Ensure the user walk path is marked USER.
            let mut updated = entry;
            if updated & flags::USER == 0 {
                updated |= flags::USER;
            }
            if updated & flags::WRITABLE == 0 {
                updated |= flags::WRITABLE;
            }
            if updated != entry {
                unsafe { table.add(index).write(updated) };
            }
            return updated & frame_mask();
        }

        let frame = match self.alloc_owned_frame() {
            Some(frame) => frame,
            None => return 0,
        };

        unsafe {
            table
                .add(index)
                .write(frame | flags::PRESENT | flags::WRITABLE | flags::USER);
        }
        frame
    }
}

fn log_user_elf_reject(reason: &[u8]) {
    crate::arch::serial::write_bytes(b"[task] user-elf reject ");
    crate::arch::serial::write_line(reason);
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn elf_page_flags(seg_flags: u32) -> u64 {
    // ELF segment flags:
    // PF_X (1 = 0x01): executable
    // PF_W (2 = 0x02): writable
    // PF_R (4 = 0x04): readable

    let mut page_flags = flags::PRESENT | flags::USER;

    // Map ELF write flag to page table write flag
    if seg_flags & PF_W != 0 {
        page_flags |= flags::WRITABLE;
    }

    // Handle execute permission: critical for code segment mapping
    let is_executable = (seg_flags & PF_X) != 0;
    if is_executable {
        // Segment IS executable - ensure NO_EXECUTE is NOT set
        // Make sure bit 63 is cleared (even though it shouldn't be set from initialization)
        page_flags &= !flags::NO_EXECUTE;
    } else {
        // Segment is NOT executable - enforce NX bit
        page_flags |= flags::NO_EXECUTE;
    }

    page_flags
}

fn prot_to_page_flags(prot: u64) -> u64 {
    let mut page_flags = flags::PRESENT | flags::USER;
    if prot & MMAP_PROT_WRITE != 0 {
        page_flags |= flags::WRITABLE;
    }
    if prot & MMAP_PROT_EXEC == 0 {
        page_flags |= flags::NO_EXECUTE;
    }
    page_flags
}

fn frame_mask() -> u64 {
    0x000F_FFFF_FFFF_F000
}

fn is_user_vaddr(vaddr: u64) -> bool {
    (USER_SLOT_BASE..USER_SLOT_LIMIT).contains(&vaddr)
}

fn is_user_vaddr_range(start: u64, end: u64) -> bool {
    start >= USER_SLOT_BASE && end > start && end <= USER_SLOT_LIMIT
}

fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}
