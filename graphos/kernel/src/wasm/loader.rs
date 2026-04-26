// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! WASM binary loader — section parser, start-function finder.

const SECTION_TYPE: u8 = 1;
const SECTION_FUNCTION: u8 = 3;
const SECTION_START: u8 = 8;
const SECTION_CODE: u8 = 10;

/// Parse the WASM binary and return the `start` function index if present.
pub fn find_start(bytes: &[u8]) -> Option<u32> {
    let mut pos = 8; // Skip magic + version.
    while pos + 1 < bytes.len() {
        let section_id = bytes[pos];
        pos += 1;
        let (size, len) = leb128_u32(&bytes[pos..])?;
        pos += len;
        if section_id == SECTION_START {
            let (func_idx, _) = leb128_u32(&bytes[pos..])?;
            return Some(func_idx);
        }
        pos += size as usize;
    }
    None
}

/// Decode a LEB128 unsigned 32-bit integer.
/// Returns `(value, bytes_consumed)`.
fn leb128_u32(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    for (i, &byte) in bytes.iter().enumerate() {
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if shift >= 35 {
            break;
        }
    }
    None
}
