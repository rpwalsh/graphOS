// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! VFS — virtual file system abstraction layer.
//!
//! Provides a unified namespace for all filesystem-like resources in GraphOS.
//! Concrete backends (graphfs, persistfs, posixfs, pkgfs, bootfs, ramfs)
//! implement the [`FileSystem`] trait and are mounted into the VFS namespace
//! via [`mount()`].
//!
//! ## Design
//! - Mount table: static array of mount points (no heap).
//! - Path resolution: iterative prefix matching against mount table.
//! - File descriptors: per-task, indices into a global open-file table.
//! - No POSIX compatibility layer — the API is intentionally minimal.
//! - Future: read/write/seek via fd, directory iteration, stat.

use crate::arch::serial;
use crate::bootinfo::BootInfo;
use spin::Mutex;

mod bootfs;
pub(crate) mod ext2fs;
pub(crate) mod fat32fs;
mod graphfs;
mod persistfs;
mod pkgfs;
mod posixfs;
pub(crate) mod ramfs;

// ════════════════════════════════════════════════════════════════════
// Error codes
// ════════════════════════════════════════════════════════════════════

/// VFS error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VfsError {
    /// File or directory not found.
    NotFound = 1,
    /// Permission denied.
    PermissionDenied = 2,
    /// Mount table full.
    MountTableFull = 3,
    /// File descriptor table full.
    TooManyOpen = 4,
    /// Invalid path.
    InvalidPath = 5,
    /// I/O error from underlying filesystem.
    IoError = 6,
    /// Operation not supported by this filesystem.
    NotSupported = 7,
    /// Bad file descriptor.
    BadFd = 8,
}

// ════════════════════════════════════════════════════════════════════
// File metadata
// ════════════════════════════════════════════════════════════════════

/// File type tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FileType {
    /// Regular file.
    Regular = 0,
    /// Directory.
    Directory = 1,
    /// Device node (e.g., /dev/serial0).
    Device = 2,
    /// Symlink / graph reference.
    Link = 3,
}

/// File metadata (stat-like).
#[derive(Debug, Clone, Copy)]
pub struct FileMeta {
    /// File type.
    pub file_type: FileType,
    /// Size in bytes (0 for directories and devices).
    pub size: u64,
    /// Graph node ID backing this file (0 if none).
    pub node_id: u64,
    /// Creation timestamp (arena generation).
    pub created_at: u64,
}

impl FileMeta {
    pub const EMPTY: Self = Self {
        file_type: FileType::Regular,
        size: 0,
        node_id: 0,
        created_at: 0,
    };
}

// ════════════════════════════════════════════════════════════════════
// Filesystem trait (function-pointer based, no dyn dispatch)
// ════════════════════════════════════════════════════════════════════

type FsReadFn = fn(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError>;
type FsWriteFn = fn(path: &[u8], offset: u64, data: &[u8]) -> Result<usize, VfsError>;

/// Filesystem operations table.
///
/// Each concrete filesystem provides these function pointers at
/// registration time. Paths passed to these functions are relative
/// to the mount point (the VFS strips the mount prefix).
#[derive(Clone, Copy)]
pub struct FsOps {
    /// Look up a path relative to this mount. Returns metadata.
    pub lookup: fn(path: &[u8]) -> Result<FileMeta, VfsError>,
    /// Read up to `buf.len()` bytes from `path` starting at `offset`.
    /// Returns number of bytes read.
    pub read: FsReadFn,
    /// Write `data` to `path` starting at `offset`.
    /// Returns number of bytes written.
    pub write: FsWriteFn,
    /// Return the name of the filesystem type (e.g., b"graphfs").
    pub fs_name: fn() -> &'static [u8],
    /// Create a directory at `path` relative to this mount.
    /// Returns 0-size Ok on success.
    pub mkdir: fn(path: &[u8]) -> Result<(), VfsError>,
    /// Remove a file (or empty directory) at `path` relative to this mount.
    pub unlink: fn(path: &[u8]) -> Result<(), VfsError>,
}

// ════════════════════════════════════════════════════════════════════
// Mount table
// ════════════════════════════════════════════════════════════════════

/// Maximum number of simultaneous mounts.
const MAX_MOUNTS: usize = 16;

/// Maximum mount path prefix length.
const MAX_PREFIX_LEN: usize = 63;

/// A single mount table entry.
struct MountEntry {
    /// Path prefix (e.g., b"/dev", b"/graph"). Null-padded.
    prefix: [u8; MAX_PREFIX_LEN + 1],
    /// Length of the prefix (excluding null padding).
    prefix_len: usize,
    /// Filesystem operations.
    ops: FsOps,
    /// Whether this slot is occupied.
    active: bool,
}

impl MountEntry {
    const EMPTY: Self = Self {
        prefix: [0; MAX_PREFIX_LEN + 1],
        prefix_len: 0,
        ops: FsOps {
            lookup: stub_lookup,
            read: stub_read,
            write: stub_write,
            fs_name: stub_name,
            mkdir: stub_mkdir,
            unlink: stub_unlink,
        },
        active: false,
    };
}

fn stub_lookup(_: &[u8]) -> Result<FileMeta, VfsError> {
    Err(VfsError::NotSupported)
}
fn stub_read(_: &[u8], _: u64, _: &mut [u8]) -> Result<usize, VfsError> {
    Err(VfsError::NotSupported)
}
fn stub_write(_: &[u8], _: u64, _: &[u8]) -> Result<usize, VfsError> {
    Err(VfsError::NotSupported)
}
fn stub_mkdir(_: &[u8]) -> Result<(), VfsError> {
    Err(VfsError::NotSupported)
}
fn stub_unlink(_: &[u8]) -> Result<(), VfsError> {
    Err(VfsError::NotSupported)
}
fn stub_name() -> &'static [u8] {
    b"none"
}

struct MountTable {
    mounts: [MountEntry; MAX_MOUNTS],
    count: usize,
}

impl MountTable {
    const fn new() -> Self {
        Self {
            mounts: [MountEntry::EMPTY; MAX_MOUNTS],
            count: 0,
        }
    }
}

static MOUNTS: Mutex<MountTable> = Mutex::new(MountTable::new());

// ════════════════════════════════════════════════════════════════════
// Open file table
// ════════════════════════════════════════════════════════════════════

/// Maximum open file descriptors (global).
const MAX_OPEN_FILES: usize = 128;

/// An open file descriptor entry.
struct OpenFile {
    /// Index into the mount table.
    mount_idx: u8,
    /// Path relative to mount (null-padded).
    rel_path: [u8; MAX_PREFIX_LEN + 1],
    rel_path_len: usize,
    /// Current read/write offset.
    offset: u64,
    /// Whether this slot is in use.
    active: bool,
}

impl OpenFile {
    const EMPTY: Self = Self {
        mount_idx: 0,
        rel_path: [0; MAX_PREFIX_LEN + 1],
        rel_path_len: 0,
        offset: 0,
        active: false,
    };
}

struct OpenFileTable {
    files: [OpenFile; MAX_OPEN_FILES],
    count: usize,
}

impl OpenFileTable {
    const fn new() -> Self {
        Self {
            files: [OpenFile::EMPTY; MAX_OPEN_FILES],
            count: 0,
        }
    }
}

static OPEN_FILES: Mutex<OpenFileTable> = Mutex::new(OpenFileTable::new());

// ════════════════════════════════════════════════════════════════════
// Public API
// ════════════════════════════════════════════════════════════════════

/// Mount a filesystem at the given path prefix.
///
/// `prefix` must start with b'/' and be at most MAX_PREFIX_LEN bytes.
pub fn mount(prefix: &[u8], ops: FsOps) -> Result<(), VfsError> {
    if prefix.is_empty() || prefix[0] != b'/' || prefix.len() > MAX_PREFIX_LEN {
        return Err(VfsError::InvalidPath);
    }
    serial::write_bytes(b"[vfs] mounting ");
    serial::write_bytes((ops.fs_name)());
    serial::write_bytes(b" at ");
    serial::write_bytes(prefix);
    serial::write_bytes(b"\n");

    let mut table = MOUNTS.lock();
    for entry in table.mounts.iter() {
        if entry.active
            && entry.prefix_len == prefix.len()
            && entry.prefix[..entry.prefix_len] == *prefix
        {
            return Ok(());
        }
    }
    if table.count >= MAX_MOUNTS {
        return Err(VfsError::MountTableFull);
    }
    for entry in table.mounts.iter_mut() {
        if !entry.active {
            entry.prefix[..prefix.len()].copy_from_slice(prefix);
            entry.prefix_len = prefix.len();
            entry.ops = ops;
            entry.active = true;
            table.count += 1;
            return Ok(());
        }
    }
    Err(VfsError::MountTableFull)
}

/// Resolve a full path to a mount entry and relative path.
///
/// Returns (mount_index, relative_path_start) or VfsError::NotFound.
fn resolve(path: &[u8]) -> Result<(usize, usize), VfsError> {
    if path.is_empty() || path[0] != b'/' {
        return Err(VfsError::InvalidPath);
    }
    let table = MOUNTS.lock();

    // Longest-prefix match.
    let mut best_idx: Option<usize> = None;
    let mut best_len: usize = 0;

    for (i, entry) in table.mounts.iter().enumerate() {
        if !entry.active {
            continue;
        }
        let plen = entry.prefix_len;
        if path.len() >= plen
            && path[..plen] == entry.prefix[..plen]
            && (path.len() == plen || path[plen] == b'/')
            && plen > best_len
        {
            best_idx = Some(i);
            best_len = plen;
        }
    }

    match best_idx {
        Some(idx) => Ok((idx, best_len)),
        None => Err(VfsError::NotFound),
    }
}

/// Look up metadata for a path.
pub fn lookup(path: &[u8]) -> Result<FileMeta, VfsError> {
    let (mount_idx, rel_start) = resolve(path)?;
    let rel = &path[rel_start..];
    let ops = {
        let table = MOUNTS.lock();
        table.mounts[mount_idx].ops
    };
    (ops.lookup)(rel)
}

/// Read from a file path without allocating a file descriptor.
pub fn read_at(path: &[u8], offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
    let (mount_idx, rel_start) = resolve(path)?;
    let rel = &path[rel_start..];
    let ops = {
        let table = MOUNTS.lock();
        table.mounts[mount_idx].ops
    };
    (ops.read)(rel, offset, buf)
}

/// Create a file if it does not already exist, then open it.
pub fn create(path: &[u8]) -> Result<u32, VfsError> {
    let (mount_idx, rel_start) = resolve(path)?;
    let rel = &path[rel_start..];
    let ops = {
        let table = MOUNTS.lock();
        table.mounts[mount_idx].ops
    };

    match (ops.lookup)(rel) {
        Ok(_) => {}
        Err(VfsError::NotFound) => {
            (ops.write)(rel, 0, &[])?;
        }
        Err(err) => return Err(err),
    }

    open(path)
}

/// Open a file and return a file descriptor.
pub fn open(path: &[u8]) -> Result<u32, VfsError> {
    let (mount_idx, rel_start) = resolve(path)?;
    let rel = &path[rel_start..];
    let ops = {
        let table = MOUNTS.lock();
        table.mounts[mount_idx].ops
    };

    // Verify the file exists.
    (ops.lookup)(rel)?;

    let mut ft = OPEN_FILES.lock();
    if ft.count >= MAX_OPEN_FILES {
        return Err(VfsError::TooManyOpen);
    }
    for (i, slot) in ft.files.iter_mut().enumerate() {
        if !slot.active {
            slot.mount_idx = mount_idx as u8;
            let copy_len = rel.len().min(MAX_PREFIX_LEN);
            slot.rel_path[..copy_len].copy_from_slice(&rel[..copy_len]);
            slot.rel_path_len = copy_len;
            slot.offset = 0;
            slot.active = true;
            ft.count += 1;
            return Ok(i as u32);
        }
    }
    Err(VfsError::TooManyOpen)
}

/// Close a file descriptor.
pub fn close(fd: u32) -> Result<(), VfsError> {
    let mut ft = OPEN_FILES.lock();
    let idx = fd as usize;
    if idx >= MAX_OPEN_FILES || !ft.files[idx].active {
        return Err(VfsError::BadFd);
    }
    ft.files[idx].active = false;
    ft.count -= 1;
    Ok(())
}

/// Read from a file descriptor. Returns bytes read.
pub fn read(fd: u32, buf: &mut [u8]) -> Result<usize, VfsError> {
    let idx = fd as usize;
    let (mount_idx, rel_len, rel_path, offset) = {
        let ft = OPEN_FILES.lock();
        if idx >= MAX_OPEN_FILES || !ft.files[idx].active {
            return Err(VfsError::BadFd);
        }
        (
            ft.files[idx].mount_idx,
            ft.files[idx].rel_path_len,
            ft.files[idx].rel_path,
            ft.files[idx].offset,
        )
    };
    let ops = {
        let table = MOUNTS.lock();
        table.mounts[mount_idx as usize].ops
    };

    let n = (ops.read)(&rel_path[..rel_len], offset, buf)?;

    let mut ft = OPEN_FILES.lock();
    if idx >= MAX_OPEN_FILES || !ft.files[idx].active {
        return Err(VfsError::BadFd);
    }
    ft.files[idx].offset = offset + n as u64;
    Ok(n)
}
/// Write to a file descriptor. Returns bytes written.
pub fn write(fd: u32, data: &[u8]) -> Result<usize, VfsError> {
    let idx = fd as usize;
    let (mount_idx, rel_len, rel_path, offset) = {
        let ft = OPEN_FILES.lock();
        if idx >= MAX_OPEN_FILES || !ft.files[idx].active {
            return Err(VfsError::BadFd);
        }
        (
            ft.files[idx].mount_idx,
            ft.files[idx].rel_path_len,
            ft.files[idx].rel_path,
            ft.files[idx].offset,
        )
    };
    let ops = {
        let table = MOUNTS.lock();
        table.mounts[mount_idx as usize].ops
    };

    let n = (ops.write)(&rel_path[..rel_len], offset, data)?;

    let mut ft = OPEN_FILES.lock();
    if idx >= MAX_OPEN_FILES || !ft.files[idx].active {
        return Err(VfsError::BadFd);
    }
    ft.files[idx].offset = offset + n as u64;
    Ok(n)
}
/// Number of active mounts.
pub fn mount_count() -> usize {
    MOUNTS.lock().count
}

/// Create a directory at the given absolute path.
///
/// Resolves the mount point from the path prefix, strips the prefix, and
/// delegates to the filesystem's `mkdir` operation.
pub fn mkdir(path: &[u8]) -> Result<(), VfsError> {
    let (mount_idx, rel_start) = resolve(path)?;
    let rel = &path[rel_start..];
    let ops = { MOUNTS.lock().mounts[mount_idx].ops };
    (ops.mkdir)(rel)
}

/// Remove a file or empty directory at the given absolute path.
///
/// Resolves the mount point from the path prefix, strips the prefix, and
/// delegates to the filesystem's `unlink` operation.
pub fn unlink(path: &[u8]) -> Result<(), VfsError> {
    let (mount_idx, rel_start) = resolve(path)?;
    let rel = &path[rel_start..];
    let ops = { MOUNTS.lock().mounts[mount_idx].ops };
    (ops.unlink)(rel)
}

/// Unmount the filesystem at the given path prefix.
///
/// Deactivates the first mount entry whose prefix exactly matches `prefix`.
/// Returns `VfsError::NotFound` if no matching mount exists.
pub fn umount(prefix: &[u8]) -> Result<(), VfsError> {
    let mut table = MOUNTS.lock();
    for entry in table.mounts.iter_mut() {
        if entry.active
            && entry.prefix_len == prefix.len()
            && entry.prefix[..entry.prefix_len] == *prefix
        {
            entry.active = false;
            table.count -= 1;
            return Ok(());
        }
    }
    Err(VfsError::NotFound)
}

/// Number of open file descriptors.
pub fn open_fd_count() -> usize {
    OPEN_FILES.lock().count
}

/// Initialise the VFS subsystem.
pub fn init(boot_info: &BootInfo) {
    serial::write_line(b"[vfs] init: seeding pkgfs");
    if let Err(err) = pkgfs::init_from_bootinfo(boot_info) {
        serial::write_bytes(b"[vfs] pkgfs seed failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    } else if let Err(err) = mount(pkgfs::PACKAGE_MOUNT, pkgfs::ops()) {
        serial::write_bytes(b"[vfs] pkgfs mount failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    }

    serial::write_line(b"[vfs] init: seeding bootfs");
    if let Err(err) = bootfs::init_from_bootinfo(boot_info) {
        serial::write_bytes(b"[vfs] bootfs seed failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    } else if let Err(err) = mount(bootfs::BOOT_MOUNT, bootfs::ops()) {
        serial::write_bytes(b"[vfs] bootfs mount failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    }

    serial::write_line(b"[vfs] init: mounting ramfs");
    if let Err(err) = mount(ramfs::BUILTIN_MOUNT, ramfs::builtin_ops()) {
        serial::write_bytes(b"[vfs] ramfs mount failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    } else if let Err(err) = ramfs::seed_boot_files() {
        serial::write_bytes(b"[vfs] ramfs seed failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    }

    serial::write_line(b"[vfs] init: mounting persistfs");
    persistfs::init();
    if let Err(err) = mount(persistfs::PERSIST_MOUNT, persistfs::ops()) {
        serial::write_bytes(b"[vfs] persistfs mount failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    }

    serial::write_line(b"[vfs] init: mounting graphfs");
    if let Err(err) = mount(graphfs::GRAPH_MOUNT, graphfs::ops()) {
        serial::write_bytes(b"[vfs] graphfs mount failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    }

    serial::write_line(b"[vfs] init: mounting posixfs");
    if let Err(err) = mount(posixfs::POSIX_MOUNT, posixfs::ops()) {
        serial::write_bytes(b"[vfs] posixfs mount failed: ");
        serial::write_u64_dec(err as u64);
        serial::write_bytes(b"\n");
    }

    serial::write_line(b"[vfs] init: probing ext2 on block device");
    if ext2fs::try_mount() {
        if let Err(err) = mount(b"/data", ext2fs::OPS) {
            serial::write_bytes(b"[vfs] ext2 mount failed: ");
            serial::write_u64_dec(err as u64);
            serial::write_bytes(b"\n");
        }
    } else {
        serial::write_line(
            b"[vfs] ext2 not found on block device (no virtio-blk or no ext2 signature)",
        );
    }

    // Probe FAT32 — typically the ESP partition (second block device or same device).
    serial::write_line(b"[vfs] init: probing fat32 on block device");
    if fat32fs::try_mount() {
        if let Err(err) = mount(b"/boot", fat32fs::OPS) {
            serial::write_bytes(b"[vfs] fat32 mount failed: ");
            serial::write_u64_dec(err as u64);
            serial::write_bytes(b"\n");
        }
    } else {
        serial::write_line(b"[vfs] fat32 not found on block device");
    }

    serial::write_bytes(b"[vfs] subsystem initialised mounts=");
    serial::write_u64_dec(mount_count() as u64);
}
