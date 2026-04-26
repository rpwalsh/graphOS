// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Structured boot diagnostics.
//!
//! Provides consistent logging with categories and severity levels.
//! All output goes to the serial console with a fixed format:
//!
//!     [CATEGORY] LEVEL: message
//!
//! Categories:
//!   BOOT  - boot sequence milestones
//!   MM    - memory management (heap, frames, paging)
//!   SCHED - scheduler and task management
//!   IPC   - inter-process communication
//!   GRAPH - graph subsystem (arena, edges, twin)
//!   COG   - cognitive subsystem (BM25, LSH, etc.)
//!   TEST  - regression test results
//!
//! Levels:
//!   INFO  - normal progress
//!   WARN  - recoverable issue
//!   ERROR - failed operation, system continues
//!   FATAL - unrecoverable, system halts
//!   PASS  - test passed
//!   FAIL  - test failed

use crate::arch::serial;

/// Diagnostic category.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Category {
    Boot,
    Mm,
    Sched,
    Ipc,
    Graph,
    Cog,
    Test,
    Svc,
}

impl Category {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Category::Boot => b"BOOT ",
            Category::Mm => b"MM   ",
            Category::Sched => b"SCHED",
            Category::Ipc => b"IPC  ",
            Category::Graph => b"GRAPH",
            Category::Cog => b"COG  ",
            Category::Test => b"TEST ",
            Category::Svc => b"SVC  ",
        }
    }
}

/// Severity level.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Level {
    Info,
    Warn,
    Error,
    Fatal,
    Pass,
    Fail,
}

impl Level {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Level::Info => b"INFO ",
            Level::Warn => b"WARN ",
            Level::Error => b"ERROR",
            Level::Fatal => b"FATAL",
            Level::Pass => b"PASS ",
            Level::Fail => b"FAIL ",
        }
    }
}

/// Emit a diagnostic message.
///
/// Format: `[CATEGORY] LEVEL: message`
#[inline]
pub fn emit(cat: Category, level: Level, msg: &[u8]) {
    serial::write_bytes(b"[");
    serial::write_bytes(cat.as_bytes());
    serial::write_bytes(b"] ");
    serial::write_bytes(level.as_bytes());
    serial::write_bytes(b": ");
    serial::write_line(msg);
}

/// Emit a diagnostic with a u64 value.
///
/// Format: `[CATEGORY] LEVEL: message value`
#[inline]
pub fn emit_val(cat: Category, level: Level, msg: &[u8], value: u64) {
    serial::write_bytes(b"[");
    serial::write_bytes(cat.as_bytes());
    serial::write_bytes(b"] ");
    serial::write_bytes(level.as_bytes());
    serial::write_bytes(b": ");
    serial::write_bytes(msg);
    serial::write_u64_dec(value);
}

/// Emit a diagnostic with a hex value.
///
/// Format: `[CATEGORY] LEVEL: message 0xvalue`
#[inline]
pub fn emit_hex(cat: Category, level: Level, msg: &[u8], value: u64) {
    serial::write_bytes(b"[");
    serial::write_bytes(cat.as_bytes());
    serial::write_bytes(b"] ");
    serial::write_bytes(level.as_bytes());
    serial::write_bytes(b": ");
    serial::write_bytes(msg);
    serial::write_bytes(b"0x");
    serial::write_hex(value);
}

/// Emit a diagnostic with two labeled values.
///
/// Format: `[CATEGORY] LEVEL: msg label1=v1 label2=v2`
#[inline]
pub fn emit_kv2(cat: Category, level: Level, msg: &[u8], k1: &[u8], v1: u64, k2: &[u8], v2: u64) {
    serial::write_bytes(b"[");
    serial::write_bytes(cat.as_bytes());
    serial::write_bytes(b"] ");
    serial::write_bytes(level.as_bytes());
    serial::write_bytes(b": ");
    serial::write_bytes(msg);
    serial::write_bytes(k1);
    serial::write_bytes(b"=");
    serial::write_u64_dec_inline(v1);
    serial::write_bytes(b" ");
    serial::write_bytes(k2);
    serial::write_bytes(b"=");
    serial::write_u64_dec(v2);
}

// ────────────────────────────────────────────────────────────────────
// Convenience helpers
// ────────────────────────────────────────────────────────────────────

/// Generic helpers for any category.
#[inline]
pub fn info(cat: Category, msg: &[u8]) {
    emit(cat, Level::Info, msg);
}
#[inline]
pub fn warn(cat: Category, msg: &[u8]) {
    emit(cat, Level::Warn, msg);
}
#[inline]
pub fn error(cat: Category, msg: &[u8]) {
    emit(cat, Level::Error, msg);
}
#[inline]
pub fn fatal(cat: Category, msg: &[u8]) {
    emit(cat, Level::Fatal, msg);
}

/// Category-specific helpers.
#[inline]
pub fn boot_info(msg: &[u8]) {
    emit(Category::Boot, Level::Info, msg);
}
#[inline]
pub fn boot_warn(msg: &[u8]) {
    emit(Category::Boot, Level::Warn, msg);
}
#[inline]
pub fn boot_error(msg: &[u8]) {
    emit(Category::Boot, Level::Error, msg);
}
#[inline]
pub fn boot_fatal(msg: &[u8]) {
    emit(Category::Boot, Level::Fatal, msg);
}

#[inline]
pub fn mm_info(msg: &[u8]) {
    emit(Category::Mm, Level::Info, msg);
}
#[inline]
pub fn mm_warn(msg: &[u8]) {
    emit(Category::Mm, Level::Warn, msg);
}
#[inline]
pub fn mm_error(msg: &[u8]) {
    emit(Category::Mm, Level::Error, msg);
}

#[inline]
pub fn sched_info(msg: &[u8]) {
    emit(Category::Sched, Level::Info, msg);
}
#[inline]
pub fn sched_warn(msg: &[u8]) {
    emit(Category::Sched, Level::Warn, msg);
}
#[inline]
pub fn sched_error(msg: &[u8]) {
    emit(Category::Sched, Level::Error, msg);
}

#[inline]
pub fn ipc_info(msg: &[u8]) {
    emit(Category::Ipc, Level::Info, msg);
}
#[inline]
pub fn ipc_warn(msg: &[u8]) {
    emit(Category::Ipc, Level::Warn, msg);
}
#[inline]
pub fn ipc_error(msg: &[u8]) {
    emit(Category::Ipc, Level::Error, msg);
}

#[inline]
pub fn graph_info(msg: &[u8]) {
    emit(Category::Graph, Level::Info, msg);
}
#[inline]
pub fn graph_warn(msg: &[u8]) {
    emit(Category::Graph, Level::Warn, msg);
}
#[inline]
pub fn graph_error(msg: &[u8]) {
    emit(Category::Graph, Level::Error, msg);
}

#[inline]
pub fn cog_info(msg: &[u8]) {
    emit(Category::Cog, Level::Info, msg);
}
#[inline]
pub fn cog_warn(msg: &[u8]) {
    emit(Category::Cog, Level::Warn, msg);
}
#[inline]
pub fn cog_error(msg: &[u8]) {
    emit(Category::Cog, Level::Error, msg);
}

#[inline]
pub fn test_pass(msg: &[u8]) {
    emit(Category::Test, Level::Pass, msg);
}
#[inline]
pub fn test_fail(msg: &[u8]) {
    emit(Category::Test, Level::Fail, msg);
}
#[inline]
pub fn test_info(msg: &[u8]) {
    emit(Category::Test, Level::Info, msg);
}
