// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Built-in in-memory filesystem used for the kernel MVP path.
//!
//! This is intentionally small and boring: fixed-size file table, fixed-size
//! file contents, no directories below the mount root, and no persistence.
//! The point is to give GraphOS a real writable namespace for smoke tests,
//! diagnostics, and early service hand-off instead of a VFS facade with no
//! backend.

use spin::Mutex;

use super::{FileMeta, FileType, FsOps, VfsError};

pub const BUILTIN_MOUNT: &[u8] = b"/tmp";

const MAX_FILES: usize = 32;
const MAX_PATH_LEN: usize = 63;
const MAX_FILE_SIZE: usize = 4096;

#[derive(Clone, Copy)]
struct RamFile {
    path: [u8; MAX_PATH_LEN + 1],
    path_len: usize,
    data: [u8; MAX_FILE_SIZE],
    size: usize,
    created_at: u64,
    active: bool,
}

impl RamFile {
    const EMPTY: Self = Self {
        path: [0; MAX_PATH_LEN + 1],
        path_len: 0,
        data: [0; MAX_FILE_SIZE],
        size: 0,
        created_at: 0,
        active: false,
    };
}

struct RamFsState {
    files: [RamFile; MAX_FILES],
    count: usize,
    next_stamp: u64,
}

impl RamFsState {
    const fn new() -> Self {
        Self {
            files: [RamFile::EMPTY; MAX_FILES],
            count: 0,
            next_stamp: 1,
        }
    }

    fn lookup_index(&self, path: &[u8]) -> Option<usize> {
        self.files.iter().position(|entry| {
            entry.active && entry.path_len == path.len() && &entry.path[..entry.path_len] == path
        })
    }

    fn ensure_file(&mut self, path: &[u8]) -> Result<usize, VfsError> {
        if let Some(idx) = self.lookup_index(path) {
            return Ok(idx);
        }

        if self.count >= MAX_FILES {
            return Err(VfsError::IoError);
        }

        if let Some(idx) = self.files.iter().position(|entry| !entry.active) {
            let entry = &mut self.files[idx];
            entry.path[..path.len()].copy_from_slice(path);
            entry.path_len = path.len();
            entry.size = 0;
            entry.created_at = self.next_stamp;
            entry.active = true;
            self.next_stamp += 1;
            self.count += 1;
            return Ok(idx);
        }

        Err(VfsError::IoError)
    }
}

static RAMFS: Mutex<RamFsState> = Mutex::new(RamFsState::new());

fn is_root(path: &[u8]) -> bool {
    path.is_empty() || path == b"/"
}

fn validate_path(path: &[u8]) -> Result<(), VfsError> {
    if is_root(path) {
        return Ok(());
    }
    if path.is_empty() || path[0] != b'/' || path.len() > MAX_PATH_LEN {
        return Err(VfsError::InvalidPath);
    }

    let mut prev = 0u8;
    for &byte in path {
        if byte == 0 {
            return Err(VfsError::InvalidPath);
        }
        if prev == b'/' && byte == b'/' {
            return Err(VfsError::InvalidPath);
        }
        prev = byte;
    }

    if path.windows(2).any(|pair| pair == b"..") {
        return Err(VfsError::InvalidPath);
    }

    Ok(())
}

fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    validate_path(path)?;
    if is_root(path) {
        let state = RAMFS.lock();
        return Ok(FileMeta {
            file_type: FileType::Directory,
            size: state.count as u64,
            node_id: 0,
            created_at: 0,
        });
    }

    let state = RAMFS.lock();
    if let Some(idx) = state.lookup_index(path) {
        let entry = &state.files[idx];
        Ok(FileMeta {
            file_type: FileType::Regular,
            size: entry.size as u64,
            node_id: 0,
            created_at: entry.created_at,
        })
    } else {
        Err(VfsError::NotFound)
    }
}

fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) {
        return Err(VfsError::NotSupported);
    }

    let state = RAMFS.lock();
    let Some(idx) = state.lookup_index(path) else {
        return Err(VfsError::NotFound);
    };
    let entry = &state.files[idx];

    let offset = offset as usize;
    if offset >= entry.size {
        return Ok(0);
    }

    let available = entry.size - offset;
    let to_copy = available.min(buf.len());
    buf[..to_copy].copy_from_slice(&entry.data[offset..offset + to_copy]);
    Ok(to_copy)
}

fn write(path: &[u8], offset: u64, data: &[u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) {
        return Err(VfsError::NotSupported);
    }

    let offset = offset as usize;
    if offset > MAX_FILE_SIZE {
        return Err(VfsError::IoError);
    }

    let end = offset.saturating_add(data.len());
    if end > MAX_FILE_SIZE {
        return Err(VfsError::IoError);
    }

    let mut state = RAMFS.lock();
    let idx = state.ensure_file(path)?;
    let entry = &mut state.files[idx];

    if offset > entry.size {
        entry.data[entry.size..offset].fill(0);
    }

    if !data.is_empty() {
        entry.data[offset..end].copy_from_slice(data);
    }
    if end > entry.size {
        entry.size = end;
    }

    Ok(data.len())
}

fn fs_name() -> &'static [u8] {
    b"ramfs"
}

/// Create a directory entry (ramfs treats directories as empty named entries).
fn mkdir(path: &[u8]) -> Result<(), VfsError> {
    validate_path(path)?;
    if is_root(path) {
        return Ok(()); // root already exists
    }
    let mut state = RAMFS.lock();
    state.ensure_file(path).map(|_| ())
}

/// Remove a file or directory entry.
fn unlink(path: &[u8]) -> Result<(), VfsError> {
    validate_path(path)?;
    let mut state = RAMFS.lock();
    let idx = state.lookup_index(path).ok_or(VfsError::NotFound)?;
    state.files[idx] = RamFile::EMPTY;
    state.count = state.count.saturating_sub(1);
    Ok(())
}

pub fn builtin_ops() -> FsOps {
    FsOps {
        lookup,
        read,
        write,
        fs_name,
        mkdir,
        unlink,
    }
}

pub fn seed_boot_files() -> Result<(), VfsError> {
    let mut state = RAMFS.lock();
    let idx = state.ensure_file(b"/README.txt")?;
    let entry = &mut state.files[idx];
    let contents = b"GraphOS MVP ramfs: writable scratch space online.\n";
    entry.data[..contents.len()].copy_from_slice(contents);
    entry.size = contents.len();
    Ok(())
}
