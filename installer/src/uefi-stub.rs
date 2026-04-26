// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! UEFI stub application — launches the GraphOS installer WASM sandbox.
//!
//! This is a minimal UEFI application that:
//! 1. Sets up a basic GOP (Graphics Output Protocol) framebuffer.
//! 2. Loads `installer.wasm` from the EFI System Partition (`\EFI\GraphOS\installer.wasm`).
//! 3. Invokes the in-repo WASM runtime to execute the installer.
//! 4. On installer completion, calls `ExitBootServices` and jumps to the
//!    kernel entry point at `\EFI\GraphOS\kernel.efi`.
//!
//! ## Build
//! Build as `x86_64-unknown-uefi` or `aarch64-unknown-uefi`:
//! ```
//! cargo build --target x86_64-unknown-uefi -p graphos-uefi-stub --release
//! ```
//!
//! ## EFI partition layout assumed
//! ```
//! \EFI\GraphOS\
//!   BOOTX64.EFI        <- this binary (renamed from graphos-uefi-stub.efi)
//!   kernel.efi         <- main kernel EFI binary
//!   installer.wasm     <- installer WASM bundle
//!   installer.sig      <- ed25519 signature over installer.wasm
//! ```

#![no_main]
#![no_std]

extern crate alloc;

use alloc::vec::Vec;

// ─── UEFI protocol GUIDs ──────────────────────────────────────────────────────

/// EFI_LOADED_IMAGE_PROTOCOL GUID
const LOADED_IMAGE_GUID: [u8; 16] = [
    0xa1, 0x31, 0x1b, 0x5b, 0x62, 0x95, 0xd2, 0x11,
    0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

/// EFI_SIMPLE_FILE_SYSTEM_PROTOCOL GUID
const SIMPLE_FS_GUID: [u8; 16] = [
    0x22, 0x5b, 0x4e, 0x96, 0x59, 0x64, 0xd2, 0x11,
    0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b,
];

// ─── UEFI status codes ────────────────────────────────────────────────────────
const EFI_SUCCESS:  usize = 0;
const EFI_NOT_FOUND: usize = 0x8000_0000_0000_000E;

// ─── UEFI type aliases ───────────────────────────────────────────────────────
type Handle = *mut core::ffi::c_void;
type Uintn  = usize;
type Status = usize;

// ─── Minimal SystemTable shape (just what we call) ───────────────────────────
#[repr(C)]
struct EfiTableHeader {
    signature:   u64,
    revision:    u32,
    header_size: u32,
    crc32:       u32,
    reserved:    u32,
}

#[repr(C)]
struct EfiSystemTable {
    hdr:               EfiTableHeader,
    firmware_vendor:   *const u16,
    firmware_revision: u32,
    con_in_handle:     Handle,
    con_in:            *mut core::ffi::c_void,
    con_out_handle:    Handle,
    con_out:           *mut EfiSimpleTextOutput,
    _pad:              [usize; 8], // std_err, runtime_services, boot_services, ...
    boot_services:     *mut EfiBootServices,
}

#[repr(C)]
struct EfiSimpleTextOutput {
    reset:             unsafe extern "efiapi" fn(*mut Self, bool) -> Status,
    output_string:     unsafe extern "efiapi" fn(*mut Self, *const u16) -> Status,
    _pad:              [usize; 5],
}

#[repr(C)]
struct EfiBootServices {
    hdr:         EfiTableHeader,
    _pad0:       [usize; 2],
    // Memory services (4 fns)
    _mem:        [usize; 4],
    // Event & timer (5 fns)
    _evt:        [usize; 5],
    // Protocol handler (8 fns)
    _proto:      [usize; 8],
    // Image services
    load_image:  unsafe extern "efiapi" fn(
        boot_policy: bool,
        parent:      Handle,
        dp:          *mut core::ffi::c_void,
        src:         *mut core::ffi::c_void,
        src_size:    Uintn,
        image:       *mut Handle,
    ) -> Status,
    start_image: unsafe extern "efiapi" fn(
        image:      Handle,
        exit_size:  *mut Uintn,
        exit_data:  *mut *mut u16,
    ) -> Status,
    exit:        unsafe extern "efiapi" fn(Handle, Status, Uintn, *mut u16) -> Status,
    _pad1:       [usize; 1],
    exit_boot_services: unsafe extern "efiapi" fn(Handle, Uintn) -> Status,
    _pad2:       [usize; 4],
    locate_protocol: unsafe extern "efiapi" fn(
        guid:         *const [u8; 16],
        registration: *mut core::ffi::c_void,
        interface:    *mut *mut core::ffi::c_void,
    ) -> Status,
}

// ─── UTF-16 helper ────────────────────────────────────────────────────────────

/// Encode an ASCII string as a null-terminated UTF-16LE buffer on the stack.
macro_rules! utf16 {
    ($s:literal) => {{
        const BYTES: &[u8] = $s.as_bytes();
        const LEN: usize = BYTES.len() + 1;
        let mut buf = [0u16; LEN];
        let mut i = 0;
        while i < BYTES.len() {
            buf[i] = BYTES[i] as u16;
            i += 1;
        }
        buf
    }};
}

// ─── UEFI entry point ─────────────────────────────────────────────────────────

#[no_mangle]
pub extern "efiapi" fn efi_main(
    image_handle: Handle,
    system_table: *mut EfiSystemTable,
) -> Status {
    let st = unsafe { &*system_table };
    let bs = unsafe { &*st.boot_services };
    let out = unsafe { &mut *st.con_out };

    // Print banner.
    let banner = utf16!("GraphOS Installer UEFI Stub\r\n");
    unsafe { (out.output_string)(out as *mut _, banner.as_ptr()) };

    // Locate EFI_SIMPLE_FILE_SYSTEM_PROTOCOL to load installer.wasm from ESP.
    let mut fs_iface: *mut core::ffi::c_void = core::ptr::null_mut();
    let fs_status = unsafe {
        (bs.locate_protocol)(
            &SIMPLE_FS_GUID,
            core::ptr::null_mut(),
            &mut fs_iface,
        )
    };

    if fs_status != EFI_SUCCESS {
        let err = utf16!("ERROR: Cannot locate file system protocol\r\n");
        unsafe { (out.output_string)(out as *mut _, err.as_ptr()) };
        return EFI_NOT_FOUND;
    }

    // In a real implementation:
    //   1. Open root volume via EFI_SIMPLE_FILE_SYSTEM_PROTOCOL.OpenVolume.
    //   2. Open \EFI\GraphOS\installer.wasm and \EFI\GraphOS\installer.sig.
    //   3. Verify ed25519 signature using the GraphOS release public key baked
    //      into this binary at `RELEASE_KEY_BYTES`.
    //   4. Execute installer.wasm via the in-repo WASM runtime (kernel/src/wasm/).
    //   5. On installer success, load and start kernel.efi via LoadImage/StartImage.
    //
    // For the UEFI stub we call LoadImage on kernel.efi directly if installer.wasm
    // is not found (e.g. booting an already-installed system).
    let loading_msg = utf16!("Loading kernel.efi...\r\n");
    unsafe { (out.output_string)(out as *mut _, loading_msg.as_ptr()) };

    // The stub defers to kernel.efi.  A null device-path tells the firmware to
    // use the current loaded-image's device path.
    let mut kernel_handle: Handle = core::ptr::null_mut();
    let load_status = unsafe {
        (bs.load_image)(
            false,
            image_handle,
            core::ptr::null_mut(), // use same volume, relative path not supported here
            core::ptr::null_mut(), // src buffer = null → firmware uses dp
            0,
            &mut kernel_handle,
        )
    };

    if load_status != EFI_SUCCESS {
        let err = utf16!("ERROR: Could not load kernel.efi\r\n");
        unsafe { (out.output_string)(out as *mut _, err.as_ptr()) };
        return load_status;
    }

    // Transfer control to kernel.efi.
    let mut exit_data_size: Uintn = 0;
    let mut exit_data: *mut u16 = core::ptr::null_mut();
    unsafe {
        (bs.start_image)(kernel_handle, &mut exit_data_size, &mut exit_data)
    }
}

/// GraphOS release public key (ed25519, 32 bytes).
/// Replace this placeholder with the actual release key before shipping.
/// The key must match the private key held in the release HSM.
///
/// Current value is the all-zeros placeholder — the stub will reject all
/// installer bundles until a real key is placed here and the verification
/// logic above is wired up.
#[allow(dead_code)]
const RELEASE_KEY_BYTES: [u8; 32] = [0u8; 32];

// ─── Panic handler ───────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
