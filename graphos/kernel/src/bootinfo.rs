// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Boot information ABI contract between the UEFI loader and the kernel.
//!
//! This structure must remain ABI-compatible across both sides.
//! Any change here must be versioned explicitly.
//!
//! ## Version history
//! - v1: Initial — framebuffer, memory map, RSDP
//! - v2: Added bootinfo_version, bootinfo_size, kernel_phys_start, kernel_phys_end
//! - v3: Added boot-module descriptors for bootfs-backed ring-3 payloads
//! - v4: Added a persistent package-store image handoff for pkgfs-backed ring 3
//! - v5: Added GOP framebuffer pixel-format handoff

/// Current BootInfo ABI version. Bump when fields are added or changed.
pub const BOOTINFO_VERSION: u32 = 5;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramebufferFormat {
    Rgb = 0,
    Bgr = 1,
    Bitmask = 2,
    BltOnly = 3,
    Unknown = u32::MAX,
}

impl FramebufferFormat {
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Rgb,
            1 => Self::Bgr,
            2 => Self::Bitmask,
            3 => Self::BltOnly,
            _ => Self::Unknown,
        }
    }

    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Rgb => b"rgb",
            Self::Bgr => b"bgr",
            Self::Bitmask => b"bitmask",
            Self::BltOnly => b"blt-only",
            Self::Unknown => b"unknown",
        }
    }
}

/// Describes the kind of a physical memory region.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegionKind {
    /// Free RAM available for general use.
    Usable = 0,
    /// Reserved by firmware or hardware.
    Reserved = 1,
    /// ACPI tables that can be reclaimed after parsing.
    AcpiReclaim = 2,
    /// ACPI non-volatile storage.
    AcpiNvs = 3,
    /// Memory-mapped I/O.
    Mmio = 4,
    /// Unrecognised type — treat as reserved.
    Unknown = 255,
}

/// A contiguous physical memory region.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    /// Physical start address (byte-aligned).
    pub start: u64,
    /// Length in bytes.
    pub length: u64,
    /// Region classification.
    pub kind: MemoryRegionKind,
    /// Padding for alignment.
    pub _pad: u32,
}

/// A loader-provided boot module that should survive into kernel runtime.
///
/// `path` is a null-terminated UTF-8 path relative to the bootfs root,
/// such as `/services/init.elf`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BootModule {
    /// Null-terminated bootfs path.
    pub path: [u8; 32],
    /// Physical start address of the module payload.
    pub phys_start: u64,
    /// Payload length in bytes.
    pub size: u64,
}

impl BootModule {
    /// Return the module path bytes up to the first null terminator.
    pub fn path_bytes(&self) -> &[u8] {
        let end = self
            .path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.path.len());
        &self.path[..end]
    }
}

/// Information passed from the UEFI loader to the kernel at entry.
///
/// # ABI stability
/// This structure is `#[repr(C)]`. Fields must not be reordered.
/// New fields are appended at the end. `bootinfo_version` and `bootinfo_size`
/// allow the kernel to detect mismatches.
#[repr(C)]
pub struct BootInfo {
    // ---- ABI envelope (v2+) ----
    /// ABI version number. Must match `BOOTINFO_VERSION`.
    pub bootinfo_version: u32,
    /// Size of this structure in bytes, as written by the loader.
    pub bootinfo_size: u32,

    // ---- Framebuffer ----
    /// Physical address of the linear framebuffer.
    pub framebuffer_addr: u64,
    /// Horizontal resolution in pixels.
    pub framebuffer_width: u32,
    /// Vertical resolution in pixels.
    pub framebuffer_height: u32,
    /// Pixels per scanline (may be > width due to padding).
    pub framebuffer_stride: u32,
    pub framebuffer_format: FramebufferFormat,

    // ---- Memory map ----
    /// Physical address of the `MemoryRegion` array.
    pub memory_regions_ptr: u64,
    /// Number of valid entries in the memory region array.
    pub memory_regions_count: u64,

    // ---- ACPI ----
    /// Physical address of the ACPI RSDP, or 0 if not found.
    pub rsdp_addr: u64,

    // ---- Kernel image location (v2+) ----
    /// Physical start address of the loaded kernel image.
    pub kernel_phys_start: u64,
    /// Physical end address (exclusive) of the loaded kernel image.
    pub kernel_phys_end: u64,

    // ---- Boot modules (v3+) ----
    /// Physical address of the `BootModule` descriptor array.
    pub boot_modules_ptr: u64,
    /// Number of boot-module descriptors.
    pub boot_modules_count: u64,

    // ---- Persistent package store (v4+) ----
    /// Physical address of the package-store image staged by the loader.
    pub package_store_ptr: u64,
    /// Size in bytes of the package-store image.
    pub package_store_size: u64,
}

impl BootInfo {
    /// Validate the BootInfo envelope. Returns `true` if version and size look sane.
    pub fn validate(&self) -> bool {
        self.bootinfo_version == BOOTINFO_VERSION
            && self.bootinfo_size as usize == core::mem::size_of::<BootInfo>()
    }

    /// Extended validation: checks structural sanity beyond version/size.
    ///
    /// Returns a bitfield of diagnostic flags. Zero means fully healthy.
    /// Callers can log individual flags for debugging.
    pub fn validate_extended(&self) -> BootInfoDiag {
        let mut diag = BootInfoDiag::empty();

        if self.bootinfo_version != BOOTINFO_VERSION {
            diag |= BootInfoDiag::VERSION_MISMATCH;
        }
        if self.bootinfo_size as usize != core::mem::size_of::<BootInfo>() {
            diag |= BootInfoDiag::SIZE_MISMATCH;
        }
        if self.framebuffer_addr == 0 {
            diag |= BootInfoDiag::NO_FRAMEBUFFER;
        }
        if self.framebuffer_width == 0 || self.framebuffer_height == 0 {
            diag |= BootInfoDiag::FB_ZERO_DIMENSION;
        }
        if self.framebuffer_stride < self.framebuffer_width {
            diag |= BootInfoDiag::FB_STRIDE_UNDERFLOW;
        }
        if self.memory_regions_ptr == 0 || self.memory_regions_count == 0 {
            diag |= BootInfoDiag::NO_MEMORY_MAP;
        }
        if self.memory_regions_count > 512 {
            diag |= BootInfoDiag::MEMORY_MAP_SUSPECT;
        }
        if self.rsdp_addr == 0 {
            diag |= BootInfoDiag::NO_RSDP;
        }
        // Kernel extent: if v2 fields are present but nonsensical.
        if self.kernel_phys_start != 0
            && self.kernel_phys_end != 0
            && self.kernel_phys_start >= self.kernel_phys_end
        {
            diag |= BootInfoDiag::KERNEL_EXTENT_INVERTED;
        }
        if (self.boot_modules_ptr == 0 && self.boot_modules_count != 0)
            || self.boot_modules_count > 32
        {
            diag |= BootInfoDiag::BOOT_MODULES_INVALID;
        }
        if (self.package_store_ptr == 0) != (self.package_store_size == 0)
            || (self.package_store_ptr != 0 && self.package_store_size < 32)
        {
            diag |= BootInfoDiag::PACKAGE_STORE_INVALID;
        }

        diag
    }

    /// Framebuffer size in bytes (stride × height × 4 bytes per pixel).
    pub fn framebuffer_size_bytes(&self) -> u64 {
        self.framebuffer_stride as u64 * self.framebuffer_height as u64 * 4
    }

    /// Return a slice over the memory regions provided by the loader.
    ///
    /// # Safety
    /// The caller must ensure that `memory_regions_ptr` and `memory_regions_count`
    /// are valid and that the pointed-to memory will not be reclaimed.
    pub unsafe fn memory_regions(&self) -> &[MemoryRegion] {
        if self.memory_regions_count == 0 || self.memory_regions_ptr == 0 {
            return &[];
        }
        // SAFETY: Loader guarantees the pointer and count are valid.
        unsafe {
            core::slice::from_raw_parts(
                self.memory_regions_ptr as *const MemoryRegion,
                self.memory_regions_count as usize,
            )
        }
    }

    /// Return a slice over the boot-module descriptors provided by the loader.
    ///
    /// # Safety
    /// The caller must ensure `boot_modules_ptr` and `boot_modules_count`
    /// reference live loader memory that survived into kernel runtime.
    pub unsafe fn boot_modules(&self) -> &[BootModule] {
        if self.boot_modules_count == 0 || self.boot_modules_ptr == 0 {
            return &[];
        }
        unsafe {
            core::slice::from_raw_parts(
                self.boot_modules_ptr as *const BootModule,
                self.boot_modules_count as usize,
            )
        }
    }

    /// Return the loader-staged persistent package-store image.
    ///
    /// # Safety
    /// The caller must ensure `package_store_ptr` and `package_store_size`
    /// reference live loader memory that survived into kernel runtime.
    pub unsafe fn package_store(&self) -> &[u8] {
        if self.package_store_ptr == 0 || self.package_store_size == 0 {
            return &[];
        }
        unsafe {
            core::slice::from_raw_parts(
                self.package_store_ptr as *const u8,
                self.package_store_size as usize,
            )
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// Extended diagnostics
// ────────────────────────────────────────────────────────────────────

bitflags::bitflags! {
    /// Diagnostic flags from `BootInfo::validate_extended()`.
    ///
    /// Each flag indicates a structural concern. Some are fatal (version
    /// mismatch), others are warnings (no RSDP). The kernel logs all
    /// active flags during early boot.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BootInfoDiag: u32 {
        const VERSION_MISMATCH      = 1 << 0;
        const SIZE_MISMATCH         = 1 << 1;
        const NO_FRAMEBUFFER        = 1 << 2;
        const FB_ZERO_DIMENSION     = 1 << 3;
        const FB_STRIDE_UNDERFLOW   = 1 << 4;
        const NO_MEMORY_MAP         = 1 << 5;
        const MEMORY_MAP_SUSPECT    = 1 << 6;
        const NO_RSDP               = 1 << 7;
        const KERNEL_EXTENT_INVERTED = 1 << 8;
        const BOOT_MODULES_INVALID  = 1 << 9;
        const PACKAGE_STORE_INVALID = 1 << 10;
    }
}
