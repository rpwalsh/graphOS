// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Memory management subsystem.

pub mod address_space;
pub mod frame_alloc;
pub mod heap;
pub mod kaslr;
pub mod page_cache;
pub mod page_table;
pub mod phys;
pub mod reserved;
pub mod swap;
