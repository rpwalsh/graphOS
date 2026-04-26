// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Security subsystem host tests.
//!
//! Run with: `cargo test --package graphos-kernel --features host-test --lib`

#![cfg(feature = "host-test")]

use crate::security::seccomp;
use crate::syscall::numbers;

// ─── seccomp policy tests ─────────────────────────────────────────────────────

/// A freshly-allocated task slot must be denied every syscall (default APP_STRICT).
#[test]
fn default_policy_is_app_strict() {
    // Task slot 0 is always BSP/idle — use a high index that won't conflict.
    const IDX: usize = 250;
    // Reset to clean state.
    seccomp::clear_policy(IDX);
    // APP_STRICT denies privileged syscalls.
    assert!(
        !seccomp::is_allowed(IDX, numbers::SYS_SETUID),
        "APP_STRICT must deny SYS_SETUID"
    );
    assert!(
        !seccomp::is_allowed(IDX, numbers::SYS_DRIVER_INSTALL),
        "APP_STRICT must deny SYS_DRIVER_INSTALL"
    );
}

/// APP_STRICT must allow unprivileged core syscalls.
#[test]
fn app_strict_allows_basic_syscalls() {
    const IDX: usize = 251;
    seccomp::clear_policy(IDX);
    assert!(seccomp::is_allowed(IDX, numbers::SYS_EXIT));
    assert!(seccomp::is_allowed(IDX, numbers::SYS_YIELD));
    assert!(seccomp::is_allowed(IDX, numbers::SYS_WRITE));
    assert!(seccomp::is_allowed(IDX, numbers::SYS_GETRANDOM));
    assert!(seccomp::is_allowed(IDX, numbers::SYS_GRAPH_EM_STATS));
}

/// After a task exits and its slot is recycled, the new occupant must not
/// inherit the previous task's PROTECTED_STRICT privileges.
#[test]
fn policy_not_inherited_after_exit() {
    const IDX: usize = 252;
    seccomp::set_protected_strict(IDX);
    assert!(
        seccomp::is_protected_strict(IDX),
        "should be protected_strict"
    );
    // Simulate exit: clear policy.
    seccomp::clear_policy(IDX);
    // New task must not be protected_strict.
    assert!(
        !seccomp::is_protected_strict(IDX),
        "cleared slot must not be protected_strict"
    );
    // New task must not be allowed privileged syscalls.
    assert!(
        !seccomp::is_allowed(IDX, numbers::SYS_SETUID),
        "recycled slot must not allow SYS_SETUID"
    );
}

/// `is_protected_strict` must return false for a task with APP_STRICT.
#[test]
fn app_strict_is_not_protected_strict() {
    const IDX: usize = 253;
    seccomp::set_app_strict(IDX);
    assert!(!seccomp::is_protected_strict(IDX));
}

/// Protected services must be allowed to claim the runtime display and
/// subscribe to the frame clock for the ring-3 compositor handoff.
#[test]
fn protected_strict_allows_compositor_handoff_syscalls() {
    const IDX: usize = 254;
    seccomp::set_protected_strict(IDX);
    assert!(seccomp::is_allowed(
        IDX,
        numbers::SYS_COMPOSITOR_CLAIM_DISPLAY
    ));
    assert!(seccomp::is_allowed(IDX, numbers::SYS_FRAME_TICK_SUBSCRIBE));
}
