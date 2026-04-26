// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Tiny persistent metadata filesystem.
//!
//! This is not a general-purpose filesystem. It exposes the metadata store as
//! a writable flat namespace so the kernel can persist small graph/bootstrap
//! records while the real storage stack is still under construction.

use super::{FileMeta, FileType, FsOps, VfsError};

pub const PERSIST_MOUNT: &[u8] = b"/persist";

fn is_root(path: &[u8]) -> bool {
    path.is_empty() || path == b"/"
}

fn validate_path(path: &[u8]) -> Result<(), VfsError> {
    if is_root(path) {
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
    if is_root(path) {
        return Ok(FileMeta {
            file_type: FileType::Directory,
            size: crate::storage::meta::entry_count() as u64,
            node_id: 0,
            created_at: 0,
        });
    }

    let Some(size) = crate::storage::meta::size(path) else {
        return Err(VfsError::NotFound);
    };

    Ok(FileMeta {
        file_type: FileType::Regular,
        size: size as u64,
        node_id: 0,
        created_at: 0,
    })
}

fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) {
        return Err(VfsError::NotSupported);
    }

    let mut scratch = [0u8; 425];
    let Some(size) = crate::storage::meta::get(path, &mut scratch) else {
        return Err(VfsError::NotFound);
    };
    let offset = offset as usize;
    if offset >= size {
        return Ok(0);
    }

    let to_copy = (size - offset).min(buf.len());
    buf[..to_copy].copy_from_slice(&scratch[offset..offset + to_copy]);
    Ok(to_copy)
}

fn write(path: &[u8], offset: u64, data: &[u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) {
        return Err(VfsError::NotSupported);
    }

    if offset != 0 {
        let mut existing = [0u8; 425];
        let current = crate::storage::meta::get(path, &mut existing).ok_or(VfsError::NotFound)?;
        let offset = offset as usize;
        if offset > current || offset.saturating_add(data.len()) > existing.len() {
            return Err(VfsError::IoError);
        }
        existing[offset..offset + data.len()].copy_from_slice(data);
        if !crate::storage::meta::put(path, &existing[..current.max(offset + data.len())]) {
            return Err(VfsError::IoError);
        }
        return Ok(data.len());
    }

    if !crate::storage::meta::put(path, data) {
        return Err(VfsError::IoError);
    }
    Ok(data.len())
}

fn fs_name() -> &'static [u8] {
    b"persistfs"
}

pub fn init() {
    crate::storage::meta::init();
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
