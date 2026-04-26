// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Thin POSIX-compatibility facade over the graph-first namespaces.
//!
//! This is intentionally small. It provides the handful of stable paths we
//! need for bootstrap and for future POSIX-like userland code, but it does not
//! pretend to be a full Unix filesystem implementation.

use alloc::vec;
use alloc::vec::Vec;

use super::{FileMeta, FileType, FsOps, VfsError};

pub const POSIX_MOUNT: &[u8] = b"/fs";

enum Target {
    Static(&'static [u8]),
    Dynamic(Vec<u8>),
    SynthStatus,
}

fn is_root(path: &[u8]) -> bool {
    path.is_empty() || path == b"/"
}

fn is_dir(path: &[u8]) -> bool {
    path == b"/proc"
        || path == b"/proc/self"
        || path == b"/var"
        || path == b"/var/lib"
        || path == b"/var/lib/graphos"
        || path == b"/tmp"
        || path == b"/graph"
}

fn validate_path(path: &[u8]) -> Result<(), VfsError> {
    if is_root(path) || is_dir(path) {
        return Ok(());
    }
    if path.is_empty() || path[0] != b'/' || path.len() > 63 {
        return Err(VfsError::InvalidPath);
    }
    if path.windows(2).any(|pair| pair == b"..") {
        return Err(VfsError::InvalidPath);
    }
    Ok(())
}

fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_dir(path) {
        return Ok(FileMeta {
            file_type: FileType::Directory,
            size: 0,
            node_id: 0,
            created_at: 0,
        });
    }

    match translate(path)? {
        Target::SynthStatus => Ok(FileMeta {
            file_type: FileType::Regular,
            size: synthetic_status_len() as u64,
            node_id: 0,
            created_at: 0,
        }),
        target => crate::vfs::lookup(target_path(&target)),
    }
}

fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_dir(path) {
        return Err(VfsError::NotSupported);
    }

    match translate(path)? {
        Target::SynthStatus => slice_read(&synthetic_status(), offset, buf),
        target => {
            let target = target_path(&target);
            crate::vfs::read_at(target, offset, buf)
        }
    }
}

fn write(path: &[u8], offset: u64, data: &[u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_dir(path) {
        return Err(VfsError::NotSupported);
    }

    if offset != 0 {
        return Err(VfsError::NotSupported);
    }

    if path.starts_with(b"/proc/") || path.starts_with(b"/graph/") {
        return Err(VfsError::NotSupported);
    }

    match translate(path)? {
        Target::SynthStatus => Err(VfsError::NotSupported),
        target => {
            let target = target_path(&target);
            let fd = if let Ok(fd) = crate::vfs::open(target) {
                fd
            } else {
                crate::vfs::create(target)?
            };
            let result = crate::vfs::write(fd, data);
            let _ = crate::vfs::close(fd);
            result
        }
    }
}

fn fs_name() -> &'static [u8] {
    b"posixfs"
}

pub fn ops() -> FsOps {
    FsOps {
        lookup,
        read,
        write,
        fs_name,
        mkdir: |_| Err(VfsError::NotSupported),
        unlink: |_| Err(VfsError::NotSupported),
    }
}

fn translate(path: &[u8]) -> Result<Target, VfsError> {
    if path == b"/proc/self/status" {
        return Ok(Target::SynthStatus);
    }
    if path == b"/proc/self/graph" {
        return Ok(Target::Static(b"/persist/bootstrap.graph"));
    }
    if path == b"/proc/self/manifest" {
        return Ok(Target::Static(b"/graph/bootstrap/manifest"));
    }
    if let Some(rest) = path.strip_prefix(b"/graph") {
        return Ok(Target::Dynamic(join_path(b"/graph", rest)?));
    }
    if let Some(rest) = path.strip_prefix(b"/tmp") {
        return Ok(Target::Dynamic(join_path(b"/tmp", rest)?));
    }
    if let Some(rest) = path.strip_prefix(b"/var/lib/graphos") {
        return Ok(Target::Dynamic(join_path(b"/persist", rest)?));
    }
    Err(VfsError::NotFound)
}

fn target_path(target: &Target) -> &[u8] {
    match target {
        Target::Static(path) => path,
        Target::Dynamic(path) => path.as_slice(),
        Target::SynthStatus => b"/proc/self/status",
    }
}

fn join_path(prefix: &[u8], rest: &[u8]) -> Result<Vec<u8>, VfsError> {
    let mut out = Vec::with_capacity(prefix.len() + rest.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(rest);
    if out.len() > 63 {
        return Err(VfsError::InvalidPath);
    }
    Ok(out)
}

fn synthetic_status() -> Vec<u8> {
    let mut out = Vec::with_capacity(160);
    out.extend_from_slice(b"posix-status-v1\n");
    out.extend_from_slice(b"system=operational-bootstrap\n");
    out.extend_from_slice(b"services=");
    append_u64(&mut out, crate::graph::bootstrap::service_count() as u64);
    out.push(b'\n');
    out.extend_from_slice(b"graph=/graph/bootstrap/state\n");
    out.extend_from_slice(b"persist=/persist/bootstrap.status\n");
    out
}

fn synthetic_status_len() -> usize {
    let services = crate::graph::bootstrap::service_count() as u64;
    "posix-status-v1\n".len()
        + "system=operational-bootstrap\n".len()
        + "services=\n".len()
        + decimal_len(services)
        + "\n".len()
        + "graph=/graph/bootstrap/state\n".len()
        + "persist=/persist/bootstrap.status\n".len()
}

fn slice_read(content: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    let offset = offset as usize;
    if offset >= content.len() {
        return Ok(0);
    }
    let to_copy = (content.len() - offset).min(buf.len());
    buf[..to_copy].copy_from_slice(&content[offset..offset + to_copy]);
    Ok(to_copy)
}

fn read_whole_file(path: &[u8]) -> Option<Vec<u8>> {
    let meta = crate::vfs::lookup(path).ok()?;
    if meta.file_type != FileType::Regular {
        return None;
    }
    let size = meta.size as usize;
    let fd = crate::vfs::open(path).ok()?;
    let mut out = vec![0u8; size];
    let mut filled = 0usize;
    while filled < out.len() {
        match crate::vfs::read(fd, &mut out[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => {
                let _ = crate::vfs::close(fd);
                return None;
            }
        }
    }
    let _ = crate::vfs::close(fd);
    if filled != out.len() {
        return None;
    }
    Some(out)
}

fn append_u64(out: &mut Vec<u8>, mut value: u64) {
    if value == 0 {
        out.push(b'0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    while value > 0 {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        out.push(digits[len]);
    }
}

fn decimal_len(mut value: u64) -> usize {
    if value == 0 {
        return 1;
    }
    let mut len = 0usize;
    while value > 0 {
        len += 1;
        value /= 10;
    }
    len
}
