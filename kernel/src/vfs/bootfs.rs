// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Read-only boot filesystem backed by loader-provided boot modules.
//!
//! This gives the kernel a grown-up boundary for protected userspace:
//! ring-3 payloads arrive as boot modules from the UEFI loader and are exposed
//! under `/boot` instead of being compiled directly into the task launcher.

use spin::Mutex;

use crate::bootinfo::BootInfo;

use super::{FileMeta, FileType, FsOps, VfsError};

pub const BOOT_MOUNT: &[u8] = b"/boot";

const MAX_BOOT_FILES: usize = 16;
const MAX_BOOT_PATH_LEN: usize = 31;

#[derive(Clone, Copy)]
struct BootFile {
    path: [u8; MAX_BOOT_PATH_LEN + 1],
    path_len: usize,
    phys_start: u64,
    size: usize,
    active: bool,
}

impl BootFile {
    const EMPTY: Self = Self {
        path: [0; MAX_BOOT_PATH_LEN + 1],
        path_len: 0,
        phys_start: 0,
        size: 0,
        active: false,
    };
}

struct BootFsState {
    files: [BootFile; MAX_BOOT_FILES],
    count: usize,
}

impl BootFsState {
    const fn new() -> Self {
        Self {
            files: [BootFile::EMPTY; MAX_BOOT_FILES],
            count: 0,
        }
    }

    fn reset(&mut self) {
        self.files = [BootFile::EMPTY; MAX_BOOT_FILES];
        self.count = 0;
    }

    fn lookup_index(&self, path: &[u8]) -> Option<usize> {
        self.files.iter().position(|entry| {
            entry.active && entry.path_len == path.len() && &entry.path[..entry.path_len] == path
        })
    }
}

static BOOTFS: Mutex<BootFsState> = Mutex::new(BootFsState::new());

fn is_root(path: &[u8]) -> bool {
    path.is_empty() || path == b"/"
}

fn is_services_dir(path: &[u8]) -> bool {
    path == b"/services"
}

fn is_config_dir(path: &[u8]) -> bool {
    path == b"/config"
}

fn validate_path(path: &[u8]) -> Result<(), VfsError> {
    if is_root(path) || is_services_dir(path) || is_config_dir(path) {
        return Ok(());
    }
    if path.is_empty() || path[0] != b'/' || path.len() > MAX_BOOT_PATH_LEN {
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

    Ok(())
}

pub fn init_from_bootinfo(boot_info: &BootInfo) -> Result<(), VfsError> {
    let modules = unsafe { boot_info.boot_modules() };
    if modules.len() > MAX_BOOT_FILES {
        return Err(VfsError::IoError);
    }

    let mut state = BOOTFS.lock();
    state.reset();

    for module in modules {
        let path = module.path_bytes();
        validate_path(path)?;
        if path.is_empty() || path.len() > MAX_BOOT_PATH_LEN || module.size == 0 {
            return Err(VfsError::InvalidPath);
        }

        let idx = state.count;
        let entry = &mut state.files[idx];
        entry.path[..path.len()].copy_from_slice(path);
        entry.path_len = path.len();
        entry.phys_start = module.phys_start;
        entry.size = module.size as usize;
        entry.active = true;
        state.count += 1;
    }

    Ok(())
}

fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_services_dir(path) || is_config_dir(path) {
        let state = BOOTFS.lock();
        return Ok(FileMeta {
            file_type: FileType::Directory,
            size: state.count as u64,
            node_id: 0,
            created_at: 0,
        });
    }

    let state = BOOTFS.lock();
    if let Some(idx) = state.lookup_index(path) {
        let entry = &state.files[idx];
        Ok(FileMeta {
            file_type: FileType::Regular,
            size: entry.size as u64,
            node_id: 0,
            created_at: 0,
        })
    } else {
        Err(VfsError::NotFound)
    }
}

fn read(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_services_dir(path) || is_config_dir(path) {
        return Err(VfsError::NotSupported);
    }

    let state = BOOTFS.lock();
    let Some(idx) = state.lookup_index(path) else {
        return Err(VfsError::NotFound);
    };
    let entry = &state.files[idx];

    let offset = offset as usize;
    if offset >= entry.size {
        return Ok(0);
    }

    let to_copy = (entry.size - offset).min(buf.len());
    let src = unsafe { core::slice::from_raw_parts(entry.phys_start as *const u8, entry.size) };
    buf[..to_copy].copy_from_slice(&src[offset..offset + to_copy]);
    Ok(to_copy)
}

fn write(_: &[u8], _: u64, _: &[u8]) -> Result<usize, VfsError> {
    Err(VfsError::NotSupported)
}

fn fs_name() -> &'static [u8] {
    b"bootfs"
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
