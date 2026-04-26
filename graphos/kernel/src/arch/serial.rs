// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
#[cfg(target_arch = "x86_64")]
#[allow(unused_imports)]
pub use crate::arch::x86_64::serial::{
    init, try_read_byte, write_byte, write_bytes, write_bytes_raw, write_hex, write_hex_inline,
    write_line, write_line_raw, write_u64_dec, write_u64_dec_inline, write_u64_dec_inline_raw,
};

#[cfg(target_arch = "aarch64")]
pub use crate::arch::aarch64::serial::{
    init, try_read_byte, write_byte, write_bytes, write_bytes_raw, write_hex, write_hex_inline,
    write_line, write_line_raw, write_u64_dec, write_u64_dec_inline, write_u64_dec_inline_raw,
};
