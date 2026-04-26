// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Fuzz target: VFS path resolution / validation.
//!
//! The VFS accepts arbitrary byte slices as path strings.  This target
//! verifies that the path normalisation and component-split logic
//! (replicated from kernel/src/vfs/mod.rs) never panics on arbitrary input.

#![no_main]

use libfuzzer_sys::fuzz_target;

// ── Replicated VFS path helpers ───────────────────────────────────────────────

const MAX_PATH: usize = 255;
const MAX_COMPONENTS: usize = 32;

#[derive(Debug, PartialEq)]
enum PathError {
    TooLong,
    InvalidPath,
    TooManyComponents,
}

/// Validates and splits a path into components.
/// Mirrors the logic in `vfs::resolve()` and `fat32fs::resolve_path()`.
fn validate_and_split(path: &[u8]) -> Result<usize, PathError> {
    if path.len() > MAX_PATH {
        return Err(PathError::TooLong);
    }
    if path.is_empty() || path[0] != b'/' {
        return Err(PathError::InvalidPath);
    }
    let inner = &path[1..];
    let mut count = 0usize;
    for component in inner.split(|&b| b == b'/') {
        if component.is_empty() {
            continue;
        }
        // Component must be valid UTF-8 printable ASCII for SFN compatibility.
        for &byte in component {
            if byte < 0x20 || byte == 0x7F {
                return Err(PathError::InvalidPath);
            }
        }
        count += 1;
        if count > MAX_COMPONENTS {
            return Err(PathError::TooManyComponents);
        }
    }
    Ok(count)
}

fuzz_target!(|data: &[u8]| {
    // Should never panic — only return Ok or Err.
    let _ = validate_and_split(data);
});
