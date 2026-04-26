// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Persistent package filesystem backed by a loader-staged store image.
//!
//! The store image itself lives on the ESP as `GRAPHOSP.PKG`. The UEFI loader
//! stages that byte-for-byte into low memory and the kernel mounts it at `/pkg`
//! so protected services are loaded from a persistent package catalog instead
//! of one-boot-only per-service payload blobs.

use spin::Mutex;

use crate::arch::serial;
use crate::bootinfo::BootInfo;

use super::{FileMeta, FileType, FsOps, VfsError};

pub const PACKAGE_MOUNT: &[u8] = b"/pkg";

const MAGIC: &[u8; 8] = b"GPKSTORE";
const FORMAT_VERSION: u32 = 2;
const MAX_PACKAGE_FILES: usize = 32;
const MAX_PACKAGE_PATH_LEN: usize = 63;
const HEADER_SIZE: usize = 32;
const ENTRY_SIZE: usize = 88;
const IMAGE_SIZE_OFFSET: usize = 16;
const CHECKSUM_OFFSET: usize = 24;
const SUPPORTED_FLAGS: u32 = 0x1;

#[derive(Clone, Copy)]
struct PackageFile {
    path: [u8; MAX_PACKAGE_PATH_LEN + 1],
    path_len: usize,
    data_ptr: u64,
    size: usize,
    flags: u32,
    active: bool,
}

impl PackageFile {
    const EMPTY: Self = Self {
        path: [0; MAX_PACKAGE_PATH_LEN + 1],
        path_len: 0,
        data_ptr: 0,
        size: 0,
        flags: 0,
        active: false,
    };
}

struct PackageFsState {
    files: [PackageFile; MAX_PACKAGE_FILES],
    count: usize,
}

impl PackageFsState {
    const fn new() -> Self {
        Self {
            files: [PackageFile::EMPTY; MAX_PACKAGE_FILES],
            count: 0,
        }
    }

    fn reset(&mut self) {
        self.files = [PackageFile::EMPTY; MAX_PACKAGE_FILES];
        self.count = 0;
    }

    fn lookup_index(&self, path: &[u8]) -> Option<usize> {
        let mut idx = 0usize;
        while idx < self.count {
            let entry = &self.files[idx];
            if entry.active && path_bytes_equal(&entry.path, entry.path_len, path) {
                return Some(idx);
            }
            idx += 1;
        }
        None
    }
}

static PKGFS: Mutex<PackageFsState> = Mutex::new(PackageFsState::new());

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
    if path.is_empty() || path[0] != b'/' || path.len() > MAX_PACKAGE_PATH_LEN {
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

fn read_u32_le(data: &[u8], start: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(start..start + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn read_u64_le(data: &[u8], start: usize) -> Option<u64> {
    let bytes: [u8; 8] = data.get(start..start + 8)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}

fn nul_terminated_len(data: &[u8]) -> usize {
    let mut len = 0usize;
    while len < data.len() {
        if data[len] == 0 {
            break;
        }
        len += 1;
    }
    len
}

fn path_bytes_equal(
    stored: &[u8; MAX_PACKAGE_PATH_LEN + 1],
    stored_len: usize,
    path: &[u8],
) -> bool {
    if stored_len != path.len() || stored_len > MAX_PACKAGE_PATH_LEN {
        return false;
    }
    let mut idx = 0usize;
    while idx < stored_len {
        if stored[idx] != path[idx] {
            return false;
        }
        idx += 1;
    }
    true
}

fn hash_store_with_zeroed_checksum(data: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for (idx, &byte) in data.iter().enumerate() {
        let effective = if (CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8).contains(&idx) {
            0
        } else {
            byte
        };
        hash ^= effective as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub fn init_from_bootinfo(boot_info: &BootInfo) -> Result<(), VfsError> {
    let store = unsafe { boot_info.package_store() };
    if store.is_empty() {
        return Err(VfsError::NotFound);
    }
    if store.len() < HEADER_SIZE || &store[..8] != MAGIC {
        return Err(VfsError::IoError);
    }

    let version = read_u32_le(store, 8).ok_or(VfsError::IoError)?;
    if version != FORMAT_VERSION {
        return Err(VfsError::IoError);
    }
    let image_size = read_u64_le(store, IMAGE_SIZE_OFFSET).ok_or(VfsError::IoError)? as usize;
    if image_size != store.len() {
        return Err(VfsError::IoError);
    }
    let expected_checksum = read_u64_le(store, CHECKSUM_OFFSET).ok_or(VfsError::IoError)?;
    if expected_checksum != hash_store_with_zeroed_checksum(store) {
        return Err(VfsError::IoError);
    }
    let entry_count = read_u32_le(store, 12).ok_or(VfsError::IoError)? as usize;
    if entry_count > MAX_PACKAGE_FILES {
        return Err(VfsError::IoError);
    }

    let entries_bytes = entry_count
        .checked_mul(ENTRY_SIZE)
        .ok_or(VfsError::IoError)?;
    let table_end = HEADER_SIZE
        .checked_add(entries_bytes)
        .ok_or(VfsError::IoError)?;
    if table_end > store.len() {
        return Err(VfsError::IoError);
    }

    let base = store.as_ptr() as u64;
    let mut state = PKGFS.lock();
    state.reset();

    for idx in 0..entry_count {
        let start = HEADER_SIZE + idx * ENTRY_SIZE;
        let raw_path = &store[start..start + 64];
        let path_len = nul_terminated_len(raw_path);
        let path = &raw_path[..path_len];
        validate_path(path)?;

        let offset = read_u64_le(store, start + 64).ok_or(VfsError::IoError)? as usize;
        let size = read_u64_le(store, start + 72).ok_or(VfsError::IoError)? as usize;
        let flags = read_u32_le(store, start + 80).ok_or(VfsError::IoError)?;
        if flags & !SUPPORTED_FLAGS != 0 {
            return Err(VfsError::IoError);
        }
        let end = offset.checked_add(size).ok_or(VfsError::IoError)?;
        if offset < table_end || size == 0 || end > store.len() || !offset.is_multiple_of(16) {
            return Err(VfsError::IoError);
        }
        let mut dup_idx = 0usize;
        while dup_idx < state.count {
            let existing = &state.files[dup_idx];
            if existing.active && path_bytes_equal(&existing.path, existing.path_len, path) {
                return Err(VfsError::IoError);
            }
            dup_idx += 1;
        }
        let mut existing_idx = 0usize;
        while existing_idx < state.count {
            let existing = &state.files[existing_idx];
            if !existing.active {
                existing_idx += 1;
                continue;
            }
            let existing_start = (existing.data_ptr - base) as usize;
            let existing_end = existing_start + existing.size;
            if offset < existing_end && existing_start < end {
                return Err(VfsError::IoError);
            }
            existing_idx += 1;
        }
        if path.is_empty() {
            return Err(VfsError::IoError);
        }

        let slot_idx = state.count;
        let slot = &mut state.files[slot_idx];
        slot.path[..path_len].copy_from_slice(path);
        slot.path_len = path_len;
        slot.data_ptr = base + offset as u64;
        slot.size = size;
        slot.flags = flags;
        slot.active = true;
        state.count += 1;
    }

    serial::write_bytes(b"[pkgfs] catalog validated entries=");
    serial::write_u64_dec(entry_count as u64);
    Ok(())
}

fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    validate_path(path)?;
    if is_root(path) || is_services_dir(path) || is_config_dir(path) {
        let state = PKGFS.lock();
        return Ok(FileMeta {
            file_type: FileType::Directory,
            size: state.count as u64,
            node_id: 0,
            created_at: 0,
        });
    }

    let state = PKGFS.lock();
    if let Some(idx) = state.lookup_index(path) {
        let entry = &state.files[idx];
        Ok(FileMeta {
            file_type: FileType::Regular,
            size: entry.size as u64,
            node_id: entry.flags as u64,
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

    let state = PKGFS.lock();
    let Some(idx) = state.lookup_index(path) else {
        return Err(VfsError::NotFound);
    };
    let entry = &state.files[idx];

    let offset = offset as usize;
    if offset >= entry.size {
        return Ok(0);
    }

    let to_copy = (entry.size - offset).min(buf.len());
    let src = unsafe { core::slice::from_raw_parts(entry.data_ptr as *const u8, entry.size) };
    buf[..to_copy].copy_from_slice(&src[offset..offset + to_copy]);
    Ok(to_copy)
}

fn write(_: &[u8], _: u64, _: &[u8]) -> Result<usize, VfsError> {
    Err(VfsError::NotSupported)
}

fn fs_name() -> &'static [u8] {
    b"pkgfs"
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
