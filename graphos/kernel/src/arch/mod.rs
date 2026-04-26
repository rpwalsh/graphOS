// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Architecture-specific code.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

pub mod interrupts;
pub mod machine;
pub mod serial;
pub mod timer;
