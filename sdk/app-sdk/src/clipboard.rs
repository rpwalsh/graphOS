// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! System clipboard API for ring-3 GraphOS applications.
//!
//! The clipboard is a kernel-managed 4096-byte ring-3 accessible buffer.
//! Only plain-text (UTF-8) content is supported in this initial revision.

use crate::sys;

/// Write `text` to the system clipboard.
///
/// At most 4096 bytes are transferred.  Returns `true` on success.
pub fn write(text: &[u8]) -> bool {
    sys::clipboard_write(text)
}

/// Read the current clipboard contents into `buf`.
///
/// Returns the number of bytes written.  Returns `0` if the clipboard is
/// empty or an error occurred.
pub fn read(buf: &mut [u8]) -> usize {
    sys::clipboard_read(buf)
}
