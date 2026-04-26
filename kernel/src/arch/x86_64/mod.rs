// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! x86_64-specific kernel subsystems.

pub mod cpu_init;
pub mod gdt;
pub mod idt;
pub mod keyboard;
pub mod lapic;
pub mod machine;
pub mod mouse;
pub mod paging;
pub mod pci;
pub mod pic;
pub mod ring3;
pub mod serial;
pub mod smp;
pub mod timer;
pub mod virtio_blk;
pub mod virtio_input;
pub mod virtio_net;
