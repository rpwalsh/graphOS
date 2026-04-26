// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Shared definitions for GraphOS fuzz targets.
//!
//! Individual fuzz binaries live in `fuzz_targets/`; this library centralizes
//! target naming for harness tooling.

/// Names of all first-party fuzz targets.
pub const TARGETS: &[&str] = &[
    "fuzz_syscall_dispatch",
    "fuzz_vfs_path",
    "fuzz_capability_delegate",
    "fuzz_tcp_state",
];

/// Returns true if `name` is a registered fuzz target.
pub fn is_registered_target(name: &str) -> bool {
    TARGETS.iter().any(|t| *t == name)
}
