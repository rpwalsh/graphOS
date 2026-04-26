#![allow(dead_code)]
// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.

use crate::runtime;

pub const CONTEXT_PATH: &[u8] = b"/tmp/graph-workspace.ctx";
pub const PATH_CAP: usize = 64;
pub const SOURCE_CAP: usize = 16;

const FILE_CAP: usize = 224;

#[derive(Clone, Copy)]
pub struct WorkspaceContext {
    scope: [u8; PATH_CAP],
    scope_len: usize,
    focus: [u8; PATH_CAP],
    focus_len: usize,
    source: [u8; SOURCE_CAP],
    source_len: usize,
    pub is_dir: bool,
}

impl WorkspaceContext {
    pub const fn empty() -> Self {
        Self {
            scope: [0; PATH_CAP],
            scope_len: 0,
            focus: [0; PATH_CAP],
            focus_len: 0,
            source: [0; SOURCE_CAP],
            source_len: 0,
            is_dir: true,
        }
    }

    pub fn scope(&self) -> &[u8] {
        &self.scope[..self.scope_len]
    }

    pub fn focus(&self) -> &[u8] {
        &self.focus[..self.focus_len]
    }

    pub fn source(&self) -> &[u8] {
        &self.source[..self.source_len]
    }

    pub fn has_scope(&self) -> bool {
        self.scope_len > 0
    }

    pub fn has_focus(&self) -> bool {
        self.focus_len > 0
    }
}

pub fn read() -> Option<WorkspaceContext> {
    let fd = runtime::vfs_open(CONTEXT_PATH);
    if fd == u64::MAX {
        return None;
    }

    let mut raw = [0u8; FILE_CAP];
    let len = runtime::vfs_read(fd, &mut raw) as usize;
    runtime::vfs_close(fd);
    if len == 0 {
        return None;
    }

    let mut ctx = WorkspaceContext::empty();
    let mut start = 0usize;
    while start < len {
        let mut end = start;
        while end < len && raw[end] != b'\n' && raw[end] != 0 {
            end += 1;
        }

        let line = trim_ascii(&raw[start..end]);
        if let Some(value) = line.strip_prefix(b"scope=") {
            ctx.scope_len = copy_field(&mut ctx.scope, value);
        } else if let Some(value) = line.strip_prefix(b"focus=") {
            ctx.focus_len = copy_field(&mut ctx.focus, value);
        } else if let Some(value) = line.strip_prefix(b"source=") {
            ctx.source_len = copy_field(&mut ctx.source, value);
        } else if let Some(value) = line.strip_prefix(b"kind=") {
            ctx.is_dir = value != b"file";
        }

        start = end.saturating_add(1);
    }

    if !ctx.has_scope() && ctx.has_focus() {
        let mut parent = [0u8; PATH_CAP];
        let parent_len = copy_field(&mut parent, parent_path(ctx.focus()));
        ctx.scope_len = copy_field(&mut ctx.scope, &parent[..parent_len]);
    }
    if !ctx.has_focus() && ctx.has_scope() {
        let mut scope = [0u8; PATH_CAP];
        let scope_len = copy_field(&mut scope, ctx.scope());
        ctx.focus_len = copy_field(&mut ctx.focus, &scope[..scope_len]);
        ctx.is_dir = true;
    }

    if ctx.has_scope() || ctx.has_focus() {
        Some(ctx)
    } else {
        None
    }
}

pub fn write(scope: &[u8], focus: &[u8], source: &[u8], is_dir: bool) -> bool {
    let scope = if scope.is_empty() {
        if focus.is_empty() {
            b"/graph".as_slice()
        } else {
            focus
        }
    } else {
        scope
    };
    let focus = if focus.is_empty() { scope } else { focus };
    let source = if source.is_empty() {
        b"system".as_slice()
    } else {
        source
    };

    let mut file = [0u8; FILE_CAP];
    let mut len = 0usize;
    len = write_line(&mut file, len, b"scope=", scope);
    len = write_line(&mut file, len, b"focus=", focus);
    len = write_line(&mut file, len, b"source=", source);
    len = write_line(
        &mut file,
        len,
        b"kind=",
        if is_dir { b"dir" } else { b"file" },
    );

    let fd = runtime::vfs_create(CONTEXT_PATH);
    if fd == u64::MAX {
        return false;
    }
    let written = runtime::vfs_write(fd, &file[..len]) as usize;
    runtime::vfs_close(fd);
    written == len
}

pub fn leaf_name(path: &[u8]) -> &[u8] {
    if path.is_empty() || path == b"/" {
        return b"/";
    }
    let mut start = path.len();
    while start > 0 {
        if path[start - 1] == b'/' {
            break;
        }
        start -= 1;
    }
    &path[start..]
}

pub fn parent_path(path: &[u8]) -> &[u8] {
    if path.len() <= 1 {
        return b"/";
    }
    let mut end = path.len().saturating_sub(1);
    while end > 1 && path[end] != b'/' {
        end -= 1;
    }
    &path[..end]
}

fn copy_field<const N: usize>(dst: &mut [u8; N], src: &[u8]) -> usize {
    let len = src.len().min(N);
    dst[..len].copy_from_slice(&src[..len]);
    len
}

fn write_line(out: &mut [u8; FILE_CAP], mut offset: usize, key: &[u8], value: &[u8]) -> usize {
    let key_len = key.len().min(FILE_CAP.saturating_sub(offset));
    out[offset..offset + key_len].copy_from_slice(&key[..key_len]);
    offset += key_len;

    let value_len = value.len().min(FILE_CAP.saturating_sub(offset));
    out[offset..offset + value_len].copy_from_slice(&value[..value_len]);
    offset += value_len;

    if offset < FILE_CAP {
        out[offset] = b'\n';
        offset += 1;
    }
    offset
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0usize;
    while start < bytes.len() && matches!(bytes[start], b' ' | b'\r' | b'\t') {
        start += 1;
    }

    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], b' ' | b'\r' | b'\t') {
        end -= 1;
    }
    &bytes[start..end]
}
