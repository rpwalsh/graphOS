// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Shared helpers for GraphOS benchmark tooling.
//!
//! The `bench` binary remains the executable entrypoint; this library provides
//! lightweight metadata and constants used by scripts/CI.

/// Default benchmark iteration count used by `bench`.
pub const DEFAULT_ITERATIONS: u64 = 100_000;

/// Canonical benchmark targets exposed by the suite.
pub const TARGETS: &[&str] = &["syscall", "vfs", "ipc"];

/// Returns true if `name` is a supported benchmark target.
pub fn is_valid_target(name: &str) -> bool {
    TARGETS.iter().any(|t| *t == name)
}

/// Normalized bytes/sec to MB/s conversion helper.
pub fn bytes_per_sec_to_mib_per_sec(bytes_per_sec: f64) -> f64 {
    bytes_per_sec / 1_048_576.0
}
