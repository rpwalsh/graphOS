// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[cfg(target_arch = "x86_64")]
pub use crate::arch::x86_64::machine::{reboot, shutdown};

#[cfg(target_arch = "aarch64")]
mod aarch64_machine {
    use crate::arch::serial;

    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

    #[inline]
    fn psci_call(fid: u64) {
        unsafe {
            core::arch::asm!(
                "hvc #0",
                in("x0") fid,
                in("x1") 0u64,
                in("x2") 0u64,
                in("x3") 0u64,
                options(nostack)
            );
        }
    }

    pub fn reboot() -> ! {
        serial::write_line(b"[power] aarch64 reboot requested");
        psci_call(PSCI_SYSTEM_RESET);
        loop {
            unsafe {
                core::arch::asm!("wfe", options(nomem, nostack));
            }
        }
    }

    pub fn shutdown() -> ! {
        serial::write_line(b"[power] aarch64 shutdown requested");
        psci_call(PSCI_SYSTEM_OFF);
        loop {
            unsafe {
                core::arch::asm!("wfe", options(nomem, nostack));
            }
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub use aarch64_machine::{reboot, shutdown};
